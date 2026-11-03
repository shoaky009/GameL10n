using System.Security.Cryptography;
using AvgTranslator.Core.Adapters;
using AvgTranslator.Core.Isolation;

namespace AvgTranslator.Core.Development;

public sealed record BuiltinAdapterTrialSample(string ResourcePath, string Locator, string Text, string Kind);

public sealed record BuiltinAdapterTrialResult(string Format, string Status, string RoundtripStatus, int UnitCount,
    IReadOnlyList<BuiltinAdapterTrialSample> Samples, IReadOnlyList<string> Warnings,
    IReadOnlyDictionary<string, string> SourceHashes, string? Error = null, VnTextPatchDiagnostic? Diagnostic = null)
{
    public string RuntimeStatus => "not-tested";
}

/// <summary>Tests explicitly selected workspace samples without publishing or activating an adapter.</summary>
public sealed class BuiltinAdapterTrial(IAdapterProcessRunner runner, string? workerExecutablePath = null,
    string? workerAssemblyPath = null)
{
    internal const int Revision = 1;
    private const int MaximumFiles = 12;
    private const long MaximumBytes = 64L * 1024 * 1024;
    private readonly VnTextPatchClient client = new(runner, workerExecutablePath, workerAssemblyPath);
    internal Task<string> FingerprintAsync(CancellationToken ct) => client.FingerprintAsync(ct);

    public async Task<BuiltinAdapterTrialResult> RunAsync(string format, string workspace, string directory,
        IReadOnlyList<string> files, bool checkRoundtrip = true, CancellationToken ct = default)
    {
        var hashes = new Dictionary<string, string>(StringComparer.OrdinalIgnoreCase);
        IReadOnlyList<BuiltinAdapterTrialSample> samples = [];
        IReadOnlyList<string> warnings = [];
        string statusOnFailure = "rejected";
        string roundtrip = checkRoundtrip ? "not-run" : "skipped";
        string? source = null;
        string? output = null;
        int unitCount = 0;
        BuiltinAdapterTrialResult result;
        try
        {
            ct.ThrowIfCancellationRequested();
            var definition = VnTextPatchFormatCatalog.All.FirstOrDefault(candidate =>
                candidate.Key.Equals(format, StringComparison.OrdinalIgnoreCase))
                ?? throw new InvalidDataException("未找到该格式的内置解析器，请使用内置格式目录中的 format 标识。");
            format = definition.Key;
            source = ResolveSampleDirectory(workspace, directory);
            if (files.Count == 0) throw new InvalidDataException("请在 files 中明确指定要试提取的样本文件。");
            if (files.Count > MaximumFiles)
                return Result("limit-exceeded", $"本次选择了 {files.Count} 个文件；单次试提取最多 {MaximumFiles} 个。请分批选择样本，这不是引擎不受支持的结论。");
            var resources = files.Select(Normalize).ToArray();
            if (resources.Distinct(StringComparer.OrdinalIgnoreCase).Count() != resources.Length)
                throw new InvalidDataException("样本文件列表包含重复路径，请每个文件只指定一次。");
            long totalBytes = 0;
            foreach (var resource in resources)
            {
                ct.ThrowIfCancellationRequested();
                var path = WorkspaceFiles.Resolve(source, resource);
                if (!File.Exists(path)) throw new FileNotFoundException("找不到所选样本文件：" + resource, resource);
                long length = new FileInfo(path).Length;
                if (length > MaximumBytes - totalBytes)
                    return Result("limit-exceeded", $"所选样本合计超过单次 64 MiB 限制（此前 {totalBytes:N0} 字节，{resource} 为 {length:N0} 字节）。请缩小样本范围，这不是引擎不受支持的结论。");
                totalBytes += length;
            }
            RequireCompanions(definition, source, resources);
            foreach (var resource in resources)
                hashes.Add(resource, await HashAsync(WorkspaceFiles.Resolve(source, resource), ct));

            statusOnFailure = "failed";
            // Explicit format trials parse each sample once; normal autodetection's probe is intentionally unnecessary here.
            var extraction = (await client.InvokeAsync("extract", format, source, resources, null, null, ct)).Extraction
                ?? throw new InvalidDataException("内置解析器未返回提取结果。");
            if (extraction.Units is null || extraction.Warnings is null)
                throw new InvalidDataException("内置解析器返回了无效提取结果。");
            var seen = new HashSet<string>(StringComparer.Ordinal);
            var resourceSet = resources.ToHashSet(StringComparer.OrdinalIgnoreCase);
            foreach (var unit in extraction.Units)
            {
                if (unit is null || string.IsNullOrWhiteSpace(unit.Id) || unit.SourceText is null || unit.Locator is null
                    || unit.Kind is null || !seen.Add(unit.Id) || !resourceSet.Contains(Normalize(unit.ResourcePath)))
                    throw new InvalidDataException("内置解析器返回了重复文本 ID 或样本范围之外的文本。");
            }
            unitCount = extraction.Units.Count;
            samples = extraction.Units.Take(5).Select(unit =>
                new BuiltinAdapterTrialSample(unit.ResourcePath, unit.Locator, Limit(unit.SourceText, 300), unit.Kind)).ToArray();
            warnings = extraction.Warnings.Append("本次仅检查所选样本；原文回填通过不代表中文写入或游戏运行已验证。")
                .Distinct(StringComparer.Ordinal).Take(12).Select(warning => Limit(warning, 500)).ToArray();
            await RequireUnchangedAsync(source, hashes, ct);

            if (checkRoundtrip)
            {
                roundtrip = "failed";
                output = Path.Combine(Path.GetTempPath(), "avg-translator-builtin-trial-" + Guid.NewGuid().ToString("N"));
                WorkspaceFiles.RejectLinkAncestors(output);
                var build = (await client.InvokeAsync("build", format, source, resources, output,
                    new Dictionary<string, string>(), ct)).Build
                    ?? throw new InvalidDataException("内置解析器未返回原文回填结果。");
                var actualFiles = GeneratedGameAdapter.EnumerateFiles(output)
                    .Select(path => Normalize(Path.GetRelativePath(output, path))).ToHashSet(StringComparer.OrdinalIgnoreCase);
                if (build.Files is null || build.Files.Count != resourceSet.Count
                    || !resourceSet.SetEquals(build.Files.Select(Normalize)) || !resourceSet.SetEquals(actualFiles)
                    || build.LaunchExecutable is not null)
                    throw new InvalidDataException("原文回填产生的完整文件集合与所选样本不一致。");
                foreach (var (resource, expectedHash) in hashes)
                {
                    if (await HashAsync(WorkspaceFiles.Resolve(output, resource), ct) != expectedHash)
                        throw new FileIntegrityException("原文回填改变了样本字节：" + resource, resource);
                }
                roundtrip = "passed";
            }
            result = Result(unitCount == 0 ? "no-text" : "passed");
        }
        catch (Exception error) when (error is not OperationCanceledException and not OutOfMemoryException)
        {
            result = Result(statusOnFailure, Limit(error.Message, 2000), Describe(error));
        }
        finally
        {
            if (output is not null) GeneratedGameAdapter.DeleteStagedTree(output);
        }

        // Check failed operations too, so a parser failure cannot hide a changed sample.
        if (source is not null && hashes.Count > 0)
        {
            try { await RequireUnchangedAsync(source, hashes, ct); }
            catch (Exception error) when (error is not OperationCanceledException and not OutOfMemoryException)
            {
                result = result with { Status = "failed", Error = error.Message, Diagnostic = Describe(error) };
            }
        }
        return result;

        BuiltinAdapterTrialResult Result(string status, string? error = null, VnTextPatchDiagnostic? diagnostic = null) =>
            new(format, status, roundtrip, unitCount, samples, warnings, hashes, error, diagnostic);
    }

    private static string ResolveSampleDirectory(string workspace, string directory)
    {
        var relative = Normalize(directory);
        if (relative.Split('/')[0] is not ("resources" or "work"))
            throw new InvalidDataException("试提取目录只能是当前工作区中的 resources、work 或它们的子目录。");
        var resolved = WorkspaceFiles.Resolve(workspace, relative);
        if (!Directory.Exists(resolved)) throw new DirectoryNotFoundException("试提取目录不存在：" + relative);
        return resolved;
    }

    private static void RequireCompanions(VnTextPatchFormat format, string source, IReadOnlyList<string> resources)
    {
        var selected = resources.ToHashSet(StringComparer.OrdinalIgnoreCase);
        Span<byte> signature = stackalloc byte[4];
        foreach (var script in resources.Where(path => format.Extensions.Contains(Path.GetExtension(path), StringComparer.OrdinalIgnoreCase)))
        {
            bool required = format.Key == "Softpal";
            if (format.Key == "Yuris")
            {
                using var stream = File.OpenRead(WorkspaceFiles.Resolve(source, script));
                required = stream.Read(signature) == 4 && signature.SequenceEqual("YSTB"u8);
                if (!required) continue;
            }
            foreach (var name in format.CompanionFileNames)
            {
                var companion = Normalize(Path.Combine(Path.GetDirectoryName(script) ?? "", name));
                var path = WorkspaceFiles.Resolve(source, companion);
                if (!File.Exists(path))
                {
                    if (required) throw new FileNotFoundException($"{script} 缺少同目录伴随文件 {companion}；请先将它复制到工作区再试提取。", companion);
                    continue;
                }
                if (!selected.Contains(companion))
                    throw new FileNotFoundException($"解析 {script} 会读取伴随文件 {companion}；请将它一并加入 files，确保样本范围和大小可核验。", companion);
            }
        }
    }

    private static async Task RequireUnchangedAsync(string source, IReadOnlyDictionary<string, string> hashes, CancellationToken ct)
    {
        foreach (var (resource, expected) in hashes)
        {
            var path = WorkspaceFiles.Resolve(source, resource);
            if (!File.Exists(path) || await HashAsync(path, ct) != expected)
                throw new FileIntegrityException("试提取期间源样本发生变化，结果不能作为验证依据：" + resource, resource);
        }
    }

    private static async Task<string> HashAsync(string path, CancellationToken ct)
    {
        await using var stream = new FileStream(path, FileMode.Open, FileAccess.Read, FileShare.Read,
            128 * 1024, FileOptions.Asynchronous | FileOptions.SequentialScan);
        return Convert.ToHexStringLower(await SHA256.HashDataAsync(stream, ct));
    }

    private static VnTextPatchDiagnostic Describe(Exception error) => error is VnTextPatchException { Diagnostic: { } diagnostic }
        ? diagnostic : new(error.GetType().FullName ?? error.GetType().Name, Limit(error.Message, 2000),
            error is FileNotFoundException missing ? missing.FileName : error is FileIntegrityException changed ? changed.ResourcePath : null,
            StackTrace: error.StackTrace is null ? null : Limit(error.StackTrace, 6000));

    private static string Normalize(string path)
    {
        if (string.IsNullOrWhiteSpace(path)) throw new InvalidDataException("样本路径不能为空。");
        return path.Replace('\\', '/');
    }

    private static string Limit(string text, int maximum)
    {
        if (text.Length <= maximum) return text;
        int count = maximum - 1;
        if (char.IsHighSurrogate(text[count - 1])) count--;
        return text[..count] + "…";
    }

    private sealed class FileIntegrityException(string message, string resourcePath) : Exception(message)
    {
        public string ResourcePath { get; } = resourcePath;
    }
}
