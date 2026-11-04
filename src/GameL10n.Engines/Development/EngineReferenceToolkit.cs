using System.IO.Compression;
using System.Security.Cryptography;
using System.Text;
using System.Text.Json;
using System.Diagnostics;
using GameL10n.Core.Isolation;
using GameL10n.Core.Storage;

namespace GameL10n.Core.Development;

public sealed record EngineReferencePin(string Revision, string SourceSha256, string LibrarySha256, string Kind = "galtransl");
public sealed record EnginePythonDependency(EngineReferencePin Reference, string PythonVersion, string PythonSha256);
public sealed record EnginePythonPaths(string Python, string EngineLibrary);
public sealed record AdapterToolPaths(string? Python, string? EngineLibrary, string? VnTextPatch);

/// <summary>One shared source archive and portable interpreter, outside game/project directories.</summary>
public sealed class EngineReferenceToolkit(string rootDirectory, HttpClient? httpClient = null,
    Action<string, string>? reportProgress = null, BuiltinAdapterAssets? parserAssets = null,
    string? bundledDirectory = null, bool allowDownloads = false)
{
    public const string PythonVersion = "3.14.10";
    public const string DependencyPath = "tools/engine-reference.json";
    private const string Repository = "GalTransl/GalTransl-GalGameReverse-Skill";
    private const int MaximumDownload = 96 * 1024 * 1024;
    private static readonly HttpClient Http = new(new HttpClientHandler { UseCookies = false }) { Timeout = TimeSpan.FromMinutes(3) };
    private static readonly SemaphoreSlim Preparation = new(1, 1);
    private static readonly JsonSerializerOptions Json = new(JsonSerializerDefaults.Web);
    private readonly string root = Path.GetFullPath(rootDirectory);
    private readonly string bundle = Path.GetFullPath(bundledDirectory ?? Path.Combine(AppContext.BaseDirectory, "tools", "engine-tools"));
    private string Current => Path.Combine(root, "current.json");

    public EngineReferencePin? CurrentReference()
    {
        var path = File.Exists(Current) ? Current : Path.Combine(bundle, "current.json");
        return File.Exists(path) ? JsonSerializer.Deserialize<EngineReferencePin>(File.ReadAllText(path), Json) : null;
    }

    public string LibraryPath(EngineReferencePin pin)
    {
        ValidatePin(pin);
        var relative = $"references/{pin.Revision}/library.zip";
        var local = WorkspaceFiles.Resolve(root, relative);
        var bundled = WorkspaceFiles.Resolve(bundle, relative);
        return !File.Exists(local) && pin.Kind != "siglus" && File.Exists(bundled) ? bundled : local;
    }

    public async Task<EngineReferencePin> EnsureReferenceAsync(EngineReferencePin? pin, CancellationToken ct)
    {
        if (pin?.Kind == "siglus")
        {
            var path = LibraryPath(pin);
            if (File.Exists(path)) await RequireHashAsync(path, pin.LibrarySha256, ct);
            else
            {
                await Preparation.WaitAsync(ct);
                try
                {
                    if (File.Exists(path)) await RequireHashAsync(path, pin.LibrarySha256, ct);
                    else
                    {
                        if (pin != SiglusAssets.Pin) throw new InvalidDataException("缺少此适配器固定版本的 Siglus 工具，请使用对应版本的程序恢复工具文件。");
                        reportProgress?.Invoke("准备 Siglus 工具", "正在准备内置的脚本解析与回填工具。");
                        await WriteAtomicAsync(path, SiglusAssets.Library, ct);
                    }
                }
                finally { Preparation.Release(); }
            }
            await EnsureSiglusNativeAsync(path, ct);
            return pin;
        }
        if (pin is not null && pin.Kind != "galtransl") throw new InvalidDataException("不支持的引擎工具类型。");
        pin ??= CurrentReference();
        // A different project's first download must not block tasks using an already installed version.
        if (pin is not null && File.Exists(LibraryPath(pin)))
        {
            reportProgress?.Invoke("检查共享工具", "正在校验本地引擎工具库。");
            await RequireHashAsync(LibraryPath(pin), pin.LibrarySha256, ct);
            return pin;
        }
        reportProgress?.Invoke("准备共享工具", "等待共享工具准备完成，可切换到其他项目。");
        await Preparation.WaitAsync(ct);
        try
        {
            pin ??= CurrentReference();
            if (pin is not null)
            {
                var existing = LibraryPath(pin);
                if (File.Exists(existing))
                {
                    await RequireHashAsync(existing, pin.LibrarySha256, ct);
                    return pin;
                }
            }
            var revision = pin?.Revision;
            if (revision is null)
            {
                var json = await DownloadAsync($"https://api.github.com/repos/{Repository}/commits/main", 1024 * 1024, ct, "GalTransl 引擎工具版本");
                using var document = JsonDocument.Parse(json);
                revision = document.RootElement.GetProperty("sha").GetString();
                if (!IsHex(revision, 40)) throw new InvalidDataException("引擎工具库未返回有效的版本号。");
            }
            var source = await DownloadAsync($"https://codeload.github.com/{Repository}/zip/{revision}", MaximumDownload, ct, "GalTransl 引擎脚本与指南");
            var sourceHash = Hash(source);
            if (pin is not null && pin.SourceSha256 != sourceHash)
                throw new InvalidDataException("下载的引擎工具源码与适配器固定版本不一致。");
            reportProgress?.Invoke("整理共享工具", "下载完成，正在校验和保存引擎工具库。");
            var library = FilterLibrary(source);
            var result = new EngineReferencePin(revision!, sourceHash, Hash(library));
            if (pin is not null && pin != result) throw new InvalidDataException("引擎工具库内容与已验证版本不一致。");
            await WriteAtomicAsync(LibraryPath(result), library, ct);
            if (!File.Exists(Current)) await LocalFiles.WriteJsonAsync(Current, result, ct);
            return result;
        }
        finally { Preparation.Release(); }
    }

    public string Read(EngineReferencePin pin, string path)
    {
        ValidateEntry(path);
        using var zip = ZipFile.OpenRead(LibraryPath(pin));
        var entry = zip.GetEntry(path) ?? throw new FileNotFoundException("工具库中没有该文件：" + path);
        if (entry.Length > 4 * 1024 * 1024) throw new InvalidDataException("请读取更具体的参考文件。");
        using var reader = new StreamReader(entry.Open(), Encoding.UTF8);
        return reader.ReadToEnd();
    }

    public async Task<(EnginePythonDependency Dependency, EnginePythonPaths Paths)> EnsurePythonAsync(
        EngineReferencePin pin, EnginePythonDependency? dependency, CancellationToken ct)
    {
        ValidatePin(pin);
        if (dependency is not null && (dependency.Reference != pin || dependency.PythonVersion != PythonVersion
            || !IsHex(dependency.PythonSha256, 64)))
            throw new InvalidDataException("不支持的 Python 引擎工具依赖版本。");
        reportProgress?.Invoke("检查解析运行环境", "正在确认本地解析工具是否完整。");
        if (await InstalledPythonAsync(pin, dependency, ct) is { } installed) return installed;
        reportProgress?.Invoke("准备解析运行环境", "等待共享运行环境准备完成，可切换到其他项目。");
        await Preparation.WaitAsync(ct);
        try
        {
            var directory = WorkspaceFiles.Resolve(root, "python/" + PythonVersion);
            if (await InstalledPythonAsync(pin, dependency, ct) is { } reused) return reused;
            var bytes = await DownloadAsync($"https://www.python.org/ftp/python/{PythonVersion}/python-{PythonVersion}-embed-amd64.zip", MaximumDownload, ct, "Python " + PythonVersion + " Windows x64 嵌入式运行环境");
            var archiveHash = Hash(bytes);
            if (dependency is not null && dependency.PythonSha256 != archiveHash)
                throw new InvalidDataException("下载的 Python 与适配器固定版本不一致。");
            var pending = directory + ".download";
            reportProgress?.Invoke("准备解析运行环境", "下载完成，正在解压并校验运行环境。");
            WorkspaceFiles.RejectLinkAncestors(pending);
            if (Directory.Exists(pending)) Directory.Delete(pending, true);
            Directory.CreateDirectory(pending);
            try
            {
                var files = new Dictionary<string, string>(StringComparer.OrdinalIgnoreCase);
                using var zip = new ZipArchive(new MemoryStream(bytes));
                long expanded = 0;
                foreach (var entry in zip.Entries)
                {
                    ct.ThrowIfCancellationRequested();
                    ValidateEntry(entry.FullName);
                    expanded += entry.Length;
                    if (expanded > 256 * 1024 * 1024 || files.Count > 256) throw new InvalidDataException("Python 压缩包大小异常。");
                    if (entry.FullName.EndsWith('/')) continue;
                    if (!files.TryAdd(entry.FullName, "")) throw new InvalidDataException("Python 压缩包包含重复文件。");
                    var path = WorkspaceFiles.Resolve(pending, entry.FullName);
                    Directory.CreateDirectory(Path.GetDirectoryName(path)!);
                    await using (var input = entry.Open())
                    await using (var output = File.Create(path)) await input.CopyToAsync(output, ct);
                    files[entry.FullName] = await LocalFiles.HashFileAsync(path, ct);
                }
                if (!files.ContainsKey("python.exe") || !files.ContainsKey("python314.zip"))
                    throw new InvalidDataException("Python 嵌入式运行时文件不完整。");
                // Restore missing files without replacing files another game's Python process may be using.
                foreach (var (file, hash) in files)
                {
                    var target = WorkspaceFiles.Resolve(directory, file);
                    if (File.Exists(target)) await RequireHashAsync(target, hash, ct);
                }
                foreach (var file in files.Keys)
                {
                    ct.ThrowIfCancellationRequested();
                    var target = WorkspaceFiles.Resolve(directory, file);
                    if (File.Exists(target)) continue;
                    Directory.CreateDirectory(Path.GetDirectoryName(target)!);
                    File.Move(WorkspaceFiles.Resolve(pending, file), target);
                }
                await LocalFiles.WriteJsonAsync(Path.Combine(directory, "runtime.json"), new PythonReceipt(archiveHash, files), ct);
            }
            finally { if (Directory.Exists(pending)) GeneratedGameAdapter.DeleteStagedTree(pending); }
            return (new(pin, PythonVersion, archiveHash), new(Path.Combine(directory, "python.exe"), LibraryPath(pin)));
        }
        finally { Preparation.Release(); }
    }

    private async Task<(EnginePythonDependency Dependency, EnginePythonPaths Paths)?> InstalledPythonAsync(
        EngineReferencePin pin, EnginePythonDependency? dependency, CancellationToken ct)
    {
        var directory = WorkspaceFiles.Resolve(root, "python/" + PythonVersion);
        if (!File.Exists(Path.Combine(directory, "runtime.json"))) directory = WorkspaceFiles.Resolve(bundle, "python/" + PythonVersion);
        var receiptPath = Path.Combine(directory, "runtime.json");
        if (!File.Exists(receiptPath)) return null;
        var receipt = JsonSerializer.Deserialize<PythonReceipt>(await File.ReadAllTextAsync(receiptPath, ct), Json)
            ?? throw new InvalidDataException("Python 工具记录无效。");
        if (!IsHex(receipt.ArchiveSha256, 64) || dependency is not null && dependency.PythonSha256 != receipt.ArchiveSha256)
            throw new InvalidDataException("共享 Python 与适配器固定版本不一致。");
        if (receipt.Files is null || !receipt.Files.ContainsKey("python.exe") || !receipt.Files.ContainsKey("python314.zip"))
            throw new InvalidDataException("Python 工具文件记录不完整。");
        foreach (var (file, hash) in receipt.Files)
        {
            var path = WorkspaceFiles.Resolve(directory, file);
            if (!File.Exists(path)) return null;
            await RequireHashAsync(path, hash, ct);
        }
        return (new(pin, PythonVersion, receipt.ArchiveSha256), new(Path.Combine(directory, "python.exe"), LibraryPath(pin)));
    }

    public async Task<string?> ResolveVnTextPatchAsync(string packageDirectory, CancellationToken ct,
        BuiltinAdapterAssets? assets = null)
    {
        var path = WorkspaceFiles.Resolve(packageDirectory, BuiltinAdapterAssets.SharedDependencyPath);
        if (!File.Exists(path)) return null;
        var dependency = await LocalFiles.ReadJsonAsync<BuiltinParserDependency>(path, ct)
            ?? throw new InvalidDataException("共享 VNTextPatch 依赖记录无效。");
        return await (assets ?? parserAssets ?? new BuiltinAdapterAssets()).ResolveSharedAsync(root, dependency, ct);
    }

    public async Task<AdapterToolPaths?> ResolvePackageAsync(string packageDirectory, CancellationToken ct)
    {
        var parser = await ResolveVnTextPatchAsync(packageDirectory, ct);
        var path = WorkspaceFiles.Resolve(packageDirectory, DependencyPath);
        if (!File.Exists(path)) return parser is null ? null : new(null, null, parser);
        var dependency = await LocalFiles.ReadJsonAsync<EnginePythonDependency>(path, ct)
            ?? throw new InvalidDataException("引擎工具依赖记录无效。");
        await EnsureReferenceAsync(dependency.Reference, ct);
        var paths = (await EnsurePythonAsync(dependency.Reference, dependency, ct)).Paths;
        return new(paths.Python, paths.EngineLibrary, parser);
    }

    public static IReadOnlyList<string> PythonArguments(EnginePythonPaths paths, string script, IReadOnlyList<string> arguments) =>
        ["-I", "-B", "-X", "utf8", "-c", Bootstrap, paths.EngineLibrary, script, .. arguments];

    public static IReadOnlyList<string> PythonModuleArguments(EnginePythonPaths paths, string module, IReadOnlyList<string> arguments) =>
        ["-I", "-B", "-X", "utf8", "-c", ModuleBootstrap, paths.EngineLibrary, module, .. arguments];

    // Explicit sys.path insertion works with the embeddable distribution's isolated ._pth configuration.
    public const string Bootstrap = "import sys,runpy,os; sys.path.insert(0,sys.argv.pop(1)); p=sys.argv.pop(1); sys.path.append(os.path.dirname(p)); sys.argv[0]=p; runpy.run_path(p,run_name='__main__')";
    public const string ModuleBootstrap = "import sys,runpy; sys.path.insert(0,sys.argv.pop(1)); p=sys.argv.pop(1); sys.argv[0]=p; runpy.run_module(p,run_name='__main__',alter_sys=True)";

    private async Task<byte[]> DownloadAsync(string url, int maximum, CancellationToken ct, string name)
    {
        if (!allowDownloads)
            throw new InvalidDataException("发行包缺少所需的固定版本工具（" + name + "）。请使用包含 tools/engine-tools 的完整发行包；智能适配不会自动下载内置依赖。");
        using var lifetime = CancellationTokenSource.CreateLinkedTokenSource(ct);
        lifetime.CancelAfter(TimeSpan.FromMinutes(3));
        try
        {
            reportProgress?.Invoke("下载共享工具", "正在连接下载服务，获取" + name + "。首次准备需要联网。");
            using var request = new HttpRequestMessage(HttpMethod.Get, url);
            request.Headers.UserAgent.ParseAdd("GameL10n/1.0");
            using var response = await (httpClient ?? Http).SendAsync(request, HttpCompletionOption.ResponseHeadersRead, lifetime.Token);
            response.EnsureSuccessStatusCode();
            if (response.Content.Headers.ContentLength > maximum) throw new InvalidDataException("工具下载超出大小限制。");
            await using var input = await response.Content.ReadAsStreamAsync(lifetime.Token);
            using var output = new MemoryStream();
            var buffer = new byte[64 * 1024];
            var watch = Stopwatch.StartNew();
            var total = response.Content.Headers.ContentLength;
            void ReportDownload() => reportProgress?.Invoke("下载共享工具", name + $" · 已下载 {output.Length / 1048576.0:F1} MiB"
                + (total > 0 ? $" / {total.Value / 1048576.0:F1} MiB" : ""));
            ReportDownload();
            int count;
            while ((count = await input.ReadAsync(buffer, lifetime.Token)) > 0)
            {
                if (output.Length + count > maximum) throw new InvalidDataException("工具下载超出大小限制。");
                output.Write(buffer, 0, count);
                if (watch.ElapsedMilliseconds >= 500) { ReportDownload(); watch.Restart(); }
            }
            ReportDownload();
            return output.ToArray();
        }
        catch (OperationCanceledException error) when (!ct.IsCancellationRequested)
        { throw new HttpRequestException("共享引擎工具下载超时，可在网络恢复后重试。", error); }
    }

    private static byte[] FilterLibrary(byte[] source)
    {
        using var input = new ZipArchive(new MemoryStream(source));
        using var memory = new MemoryStream();
        using (var output = new ZipArchive(memory, ZipArchiveMode.Create, leaveOpen: true))
        {
            var paths = new HashSet<string>(StringComparer.OrdinalIgnoreCase);
            long expanded = 0;
            foreach (var entry in input.Entries.OrderBy(entry => entry.FullName, StringComparer.Ordinal))
            {
                ValidateEntry(entry.FullName);
                var slash = entry.FullName.IndexOf('/');
                if (slash < 0 || slash == entry.FullName.Length - 1) continue;
                var path = entry.FullName[(slash + 1)..];
                // Ship source, catalog and notices only. No hooks, fonts, media, or executables.
                var name = path.Split('/')[^1];
                var notice = name.StartsWith("LICENSE", StringComparison.OrdinalIgnoreCase)
                    || name.StartsWith("NOTICE", StringComparison.OrdinalIgnoreCase)
                    || name.StartsWith("COPYING", StringComparison.OrdinalIgnoreCase);
                if (!(path.StartsWith("python/", StringComparison.Ordinal) && (path.EndsWith(".py") || path.EndsWith('/'))
                    || new[] { "engines/", "guides/", "catalog/", "provenance/" }.Any(path.StartsWith)
                        && (path.EndsWith(".md") || path.EndsWith(".json") || path.EndsWith(".txt"))
                    || notice)) continue;
                if (!paths.Add(path)) throw new InvalidDataException("引擎工具源码包含重复路径。");
                expanded += entry.Length;
                if (expanded > 64 * 1024 * 1024 || paths.Count > 4096) throw new InvalidDataException("引擎工具源码大小异常。");
                var target = output.CreateEntry(path, CompressionLevel.Optimal);
                target.LastWriteTime = new DateTimeOffset(2000, 1, 1, 0, 0, 0, TimeSpan.Zero);
                using var from = entry.Open();
                using var to = target.Open();
                from.CopyTo(to);
            }
            if (!paths.Contains("catalog/engines.json") || !paths.Contains("python/detect.py")
                || !paths.Any(path => path.StartsWith("LICENSE")) || !paths.Contains("provenance/NOTICE.md"))
                throw new InvalidDataException("引擎工具源码缺少目录、探测模块或许可信息。");
        }
        return memory.ToArray();
    }

    private static void ValidateEntry(string path)
    {
        if (string.IsNullOrEmpty(path) || path.StartsWith('/') || path.Contains('\\') || path.Contains(':')
            || path.Split('/').Any(part => part is "." or "..")) throw new InvalidDataException("工具压缩包路径无效。");
    }
    private static bool IsHex(string? value, int length) => value?.Length == length && value.All(char.IsAsciiHexDigit);
    private static async Task EnsureSiglusNativeAsync(string libraryPath, CancellationToken ct)
    {
        const string name = "siglus_ssu/native_accel.cp314-win_amd64.pyd";
        using var archive = ZipFile.OpenRead(libraryPath);
        var entry = archive.GetEntry(name) ?? throw new InvalidDataException("内置 Siglus 加速模块缺失。");
        if (entry.Length > 8 * 1024 * 1024) throw new InvalidDataException("Siglus 加速模块大小异常。");
        using var buffer = new MemoryStream();
        await using (var input = entry.Open()) await input.CopyToAsync(buffer, ct);
        var bytes = buffer.ToArray();
        var target = WorkspaceFiles.Resolve(Path.GetDirectoryName(libraryPath)!, "native/" + name);
        if (File.Exists(target)) { await RequireHashAsync(target, Hash(bytes), ct); return; }
        await Preparation.WaitAsync(ct);
        try
        {
            if (File.Exists(target)) await RequireHashAsync(target, Hash(bytes), ct);
            else await WriteAtomicAsync(target, bytes, ct);
        }
        finally { Preparation.Release(); }
    }
    private static void ValidatePin(EngineReferencePin pin)
    {
        if (!IsHex(pin.Revision, 40) || !IsHex(pin.SourceSha256, 64) || !IsHex(pin.LibrarySha256, 64)
            || pin.Kind is not ("galtransl" or "siglus"))
            throw new InvalidDataException("引擎工具依赖指纹无效。");
    }
    private static string Hash(byte[] bytes) => Convert.ToHexStringLower(SHA256.HashData(bytes));
    private static async Task RequireHashAsync(string path, string expected, CancellationToken ct)
    {
        WorkspaceFiles.RejectLinkAncestors(path);
        if (!IsHex(expected, 64) || await LocalFiles.HashFileAsync(path, ct) != expected)
            throw new InvalidDataException("共享工具内容已改变，请恢复固定版本：" + path);
    }
    private static async Task WriteAtomicAsync(string path, byte[] bytes, CancellationToken ct)
    {
        WorkspaceFiles.RejectLinkAncestors(path);
        Directory.CreateDirectory(Path.GetDirectoryName(path)!);
        var temporary = path + ".download";
        try
        {
            await File.WriteAllBytesAsync(temporary, bytes, ct);
            File.Move(temporary, path, overwrite: true);
        }
        finally { if (File.Exists(temporary)) File.Delete(temporary); }
    }
    private sealed record PythonReceipt(string ArchiveSha256, Dictionary<string, string> Files);
}
