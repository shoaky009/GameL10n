using System.Runtime.InteropServices;
using System.Text;
using System.Xml;
using AvgTranslator.Core.Isolation;

namespace AvgTranslator.Core.Development;

public sealed record LocaleLaunchProfile(string Path, string Executable, bool ExecutableExists,
    string? Location, bool? RunAsAdmin, bool? RunWithSuspend, string? Error);
public sealed record LaunchTextSample(string Path, string Encoding, string Content, bool Truncated, string? Error);
public sealed record GameLaunchInspection(uint? SystemAnsiCodePage, IReadOnlyList<LocaleLaunchProfile> LocaleProfiles,
    IReadOnlyList<LaunchTextSample> LaunchScripts, IReadOnlyList<LaunchTextSample> ErrorLogs, bool Partial);

/// <summary>Bounded, read-only evidence for adaptation. Does not launch games or interpret scripts.</summary>
public static class GameLaunchInspector
{
    private const int MaximumFiles = 20_000;
    private static readonly EnumerationOptions Enumeration = new()
    { RecurseSubdirectories = true, AttributesToSkip = FileAttributes.ReparsePoint, IgnoreInaccessible = true };

    public static async Task<GameLaunchInspection> InspectAsync(string gameDirectory, CancellationToken ct = default)
    {
        var files = Enumerate(gameDirectory, ct).Take(MaximumFiles + 1).ToArray();
        var partial = files.Length > MaximumFiles;
        files = files.Take(MaximumFiles).ToArray();
        var profiles = new List<LocaleLaunchProfile>();
        var candidates = files.Where(path => path.EndsWith(".exe.le.config", StringComparison.OrdinalIgnoreCase)).ToArray();
        partial |= candidates.Length > 8;
        foreach (var path in candidates.Take(8))
        {
            var executable = path[..^".le.config".Length];
            var exists = File.Exists(WorkspaceFiles.Resolve(gameDirectory, executable));
            try
            {
                await using var input = File.OpenRead(WorkspaceFiles.Resolve(gameDirectory, path));
                if (input.Length > 32 * 1024) throw new InvalidDataException("转区配置超过 32 KB，请单独检查。");
                var document = new XmlDocument { XmlResolver = null };
                using var xml = XmlReader.Create(input, new XmlReaderSettings
                { DtdProcessing = DtdProcessing.Prohibit, XmlResolver = null, MaxCharactersInDocument = 32 * 1024 });
                document.Load(xml);
                var profile = document.SelectSingleNode("/LEConfig/Profiles/Profile")
                    ?? throw new InvalidDataException("未找到 Locale Emulator 配置项。");
                string? Value(string name) => profile.SelectSingleNode(name)?.InnerText.Trim();
                bool? Flag(string name) => bool.TryParse(Value(name), out var value) ? value : null;
                profiles.Add(new(path, executable, exists, Value("Location"), Flag("RunAsAdmin"), Flag("RunWithSuspend"), null));
            }
            catch (Exception error) when (error is IOException or UnauthorizedAccessException or XmlException or InvalidDataException)
            { profiles.Add(new(path, executable, exists, null, null, null, error.Message)); }
            ct.ThrowIfCancellationRequested();
        }

        uint? codePage = OperatingSystem.IsWindows() ? GetACP() : null;
        var fallback = profiles.Any(profile => profile.ExecutableExists && profile.Location == "ja-JP") ? 932 : (int?)codePage;
        var scriptPaths = files.Where(path => Path.GetExtension(path).ToLowerInvariant() is ".cmd" or ".bat" or ".ps1")
            .OrderBy(path => path.Count(character => character == '/')).ThenBy(path => path, StringComparer.Ordinal).ToArray();
        partial |= scriptPaths.Length > 4;
        var scripts = new List<LaunchTextSample>();
        foreach (var path in scriptPaths.Take(4)) scripts.Add(await ReadSampleAsync(gameDirectory, path, fallback, tail: false, ct));
        // Known error-log names only; do not read saves, arbitrary logs or game dialogue as launch evidence.
        var logPaths = files.Where(path => Path.GetFileName(path).ToLowerInvariant() is "errorlog.txt" or "error.log" or "error.txt")
            .OrderBy(path => path.Count(character => character == '/')).ThenBy(path => path, StringComparer.Ordinal).ToArray();
        partial |= logPaths.Length > 2;
        var logs = new List<LaunchTextSample>();
        foreach (var path in logPaths.Take(2)) logs.Add(await ReadSampleAsync(gameDirectory, path, fallback, tail: true, ct));
        return new(codePage, profiles, scripts, logs, partial);
    }

    public static object FindFile(string gameDirectory, string relativePathOrName, CancellationToken ct = default)
    {
        var normalized = relativePathOrName.Replace('\\', '/');
        var exact = WorkspaceFiles.Resolve(gameDirectory, normalized);
        var name = Path.GetFileName(normalized);
        var matches = new List<string>();
        var scanned = 0;
        var partial = false;
        foreach (var path in Enumerate(gameDirectory, ct))
        {
            if (++scanned > MaximumFiles) { partial = true; break; }
            if (!Path.GetFileName(path).Equals(name, StringComparison.OrdinalIgnoreCase)) continue;
            if (matches.Count == 20) { partial = true; break; }
            matches.Add(path);
        }
        return new { requested = normalized, exactPathExists = File.Exists(exact), matches, partial,
            note = "文件存在不代表游戏进程能够按相同路径访问；还需核对工作目录、转区环境与脚本引用的原始字节。" };
    }

    private static IEnumerable<string> Enumerate(string root, CancellationToken ct)
    {
        WorkspaceFiles.RejectLinkAncestors(root);
        foreach (var file in Directory.EnumerateFiles(root, "*", Enumeration))
        {
            ct.ThrowIfCancellationRequested();
            var relative = Path.GetRelativePath(root, file).Replace('\\', '/');
            if (!relative.StartsWith(".avg-translator/", StringComparison.OrdinalIgnoreCase)) yield return relative;
        }
    }

    private static async Task<LaunchTextSample> ReadSampleAsync(string root, string relative, int? fallback, bool tail, CancellationToken ct)
    {
        try
        {
            await using var input = File.OpenRead(WorkspaceFiles.Resolve(root, relative));
            var size = (int)Math.Min(input.Length, 4096);
            var header = new byte[(int)Math.Min(3, input.Length)];
            await input.ReadExactlyAsync(header, ct);
            Encoding? unicode = header.Length >= 2 && header[0] == 0xff && header[1] == 0xfe ? Encoding.Unicode
                : header.Length >= 2 && header[0] == 0xfe && header[1] == 0xff ? Encoding.BigEndianUnicode : null;
            var offset = tail ? input.Length - size : 0;
            if (unicode is not null && offset % 2 != 0) { offset++; size--; }
            input.Position = offset;
            var data = new byte[size];
            await input.ReadExactlyAsync(data, ct);
            // Start a truncated log at a whole line, avoiding a split multibyte character.
            var newline = Array.IndexOf(data, (byte)'\n');
            if (unicode is null && offset > 0 && newline >= 0) data = data[(newline + 1)..];
            string content;
            var encoding = unicode?.WebName ?? "utf8";
            try { content = (unicode ?? new UTF8Encoding(false, true)).GetString(data).TrimStart('\uFEFF'); }
            catch (DecoderFallbackException)
            {
                Encoding.RegisterProvider(CodePagesEncodingProvider.Instance);
                if (fallback is not { } codePage)
                    return new(relative, "unknown", "", input.Length > size, "编码未确认，请用 read_file 指定编码读取。");
                content = Encoding.GetEncoding(codePage).GetString(data);
                encoding = "cp" + codePage + " (inferred; verify if garbled)";
            }
            var truncated = input.Length > size || content.Length > 1200;
            if (content.Length > 1200) content = tail ? content[^1200..] : content[..1200];
            return new(relative, encoding, content, truncated, null);
        }
        catch (Exception error) when (error is IOException or UnauthorizedAccessException)
        { return new(relative, "unknown", "", false, error.Message); }
    }

    [DllImport("kernel32.dll")]
    private static extern uint GetACP();
}
