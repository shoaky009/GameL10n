using System.Security.Cryptography;
using System.Text.Json;
using GameL10n.Core.Isolation;
using GameL10n.Core.Adapters;
using GameL10n.Core.Validation;
using System.Text.Json.Nodes;
using GameL10n.Core.Serialization;

namespace GameL10n.Core.Development;

/// <summary>Runs standalone adapters against host snapshots. Probes receive bounded candidate ranges, never the game directory.</summary>
public sealed class GeneratedGameAdapter(
    GeneratedAdapterPackage package,
    PinnedDotNetToolchain toolchain,
    IAdapterProcessRunner runner,
    string workRoot,
    EngineReferenceToolkit? engineToolkit = null,
    Action<string, string>? reportProgress = null, GameDiscoverySession? discovery = null) : IGameAdapter
{
    private const int ProbeHeaderBytes = 64 * 1024;
    private static readonly JsonSerializerOptions Json = new(EngineJson.Web) { MaxDepth = 64 };
    private string? originalExecutable;

    public string Id => package.Id;
    public string Version => package.Version;
    public GeneratedAdapterPackage Package => package;
    internal static string LimitDiagnostic(string value) => value.Length > 8192 ? value[..8192] + "\n[diagnostic truncated]" : value;
    public AdapterCatalogEntry? CatalogEntry => package.Family is { } family ? new(family, Version, []) : null;

    public IReadOnlyList<string> DiscoverResources(string sourceDirectory) => GeneratedAdapterResources.Discover(package, sourceDirectory);

    internal void RestoreProbe(GameProbe probe)
    {
        if (probe.AdapterId != Id || probe.AdapterVersion != Version)
            throw new InvalidDataException("保存的识别结果与当前适配器不一致。");
        if (probe.OriginalExecutable is { } executable) RequireResourcePath(executable, probe.RequiredFiles);
        originalExecutable = probe.OriginalExecutable;
    }

    public async Task<GameProbe?> ProbeAsync(string gameDirectory, CancellationToken cancellationToken = default)
    {
        using var deadline = CancellationTokenSource.CreateLinkedTokenSource(cancellationToken);
        deadline.CancelAfter(TimeSpan.FromSeconds(45));
        try
        {
            var resources = discovery is { Complete: true } && Path.GetFullPath(gameDirectory).Equals(discovery.DirectoryPath, StringComparison.OrdinalIgnoreCase)
                ? discovery.Resources(new(package.ResourceFiles, package.ResourcePatterns, package.Recognition), deadline.Token)
                : DiscoverResources(gameDirectory);
            if (resources.Count == 0 || resources.Any(path => !File.Exists(WorkspaceFiles.Resolve(gameDirectory, path)))) return null;
            var response = await InvokeAsync("probe", gameDirectory, resources, null, null, deadline.Token);
            if (response.Probe is null) return null;
            var probe = response.Probe;
            if (probe.RequiredFiles is null || probe.Warnings is null || probe.RequiredFiles.Count == 0
                || probe.RequiredFiles.Select(GeneratedAdapterResources.Normalize).Distinct(StringComparer.OrdinalIgnoreCase).Count() != probe.RequiredFiles.Count)
                throw new GeneratedAdapterException("The adapter returned an invalid probe result.");
            foreach (var path in probe.RequiredFiles) RequireResourcePath(path, resources);
            if (probe.OriginalExecutable is { } executable)
            {
                RequireResourcePath(executable, resources);
                if (!probe.RequiredFiles.Contains(executable, StringComparer.OrdinalIgnoreCase))
                    throw new GeneratedAdapterException("The original executable must be included in requiredFiles so the host preserves its source.");
            }
            originalExecutable = probe.OriginalExecutable;
            return probe with { EngineId = package.Family?.Key ?? probe.EngineId, AdapterId = Id, AdapterVersion = Version };
        }
        catch (OperationCanceledException) when (!cancellationToken.IsCancellationRequested)
        { throw new AdapterProcessLimitException("适配器探测超过 45 秒，本次未完成检查，可使用智能适配进一步分析。"); }
    }

    public async Task<ExtractionResult> ExtractAsync(string sourceDirectory, CancellationToken cancellationToken = default)
    {
        var resources = DiscoverResources(sourceDirectory);
        var result = (await InvokeAsync("extract", sourceDirectory, resources, null, null, cancellationToken)).Extraction
            ?? throw new GeneratedAdapterException("The adapter returned no extraction result.");
        if (result.Units is null || result.Warnings is null || result.Units.Count == 0)
            throw new GeneratedAdapterException("The adapter returned an empty or invalid extraction.");
        var identities = new Dictionary<string, TranslationUnit>(StringComparer.Ordinal);
        var whitespaceCount = 0;
        foreach (var unit in result.Units)
        {
            if (unit is null || string.IsNullOrWhiteSpace(unit.Id) || unit.Id.Length > 512
                || unit.SourceText is null || unit.Locator is null || unit.Context is null || string.IsNullOrWhiteSpace(unit.ResourcePath)
                || unit.ProtectedTokens is { } markers && (markers.Count > 64 || markers.Any(marker =>
                    string.IsNullOrEmpty(marker) || marker.Length > 256 || !unit.SourceText.Contains(marker, StringComparison.Ordinal))))
                throw new GeneratedAdapterException("适配器返回了无效文本记录：" + (unit is null ? "空记录" : UnitLocation(unit)));
            if (!identities.TryAdd(unit.Id, unit))
                throw new GeneratedAdapterException("适配器返回了重复的文本 ID：" + unit.Id + "；位置："
                    + UnitLocation(identities[unit.Id]) + " / " + UnitLocation(unit)
                    + "。相同原文可以保留多条记录，但不同位置必须有独立 ID。");
            if (string.IsNullOrWhiteSpace(unit.SourceText)) whitespaceCount++;
        }
        foreach (var path in result.Units.Select(unit => unit.ResourcePath).Distinct(StringComparer.Ordinal))
            RequireResourcePath(path, resources);
        // Empty display slots and spacing literals are script data, not translation targets. Validate raw records first:
        // filtering must never hide an ID collision or invalid path, renumber locations, or remove bytes from the script.
        if (whitespaceCount == result.Units.Count)
            throw new GeneratedAdapterException($"适配器返回的 {result.Units.Count:N0} 条记录全部为空文本或纯空白，没有可翻译内容。");
        return whitespaceCount == 0 ? result : result with
        { Units = result.Units.Where(unit => !string.IsNullOrWhiteSpace(unit.SourceText)).ToArray() };
    }

    private static string UnitLocation(TranslationUnit unit) =>
        LimitDiagnostic(unit.ResourcePath + "（" + unit.Locator + "）");

    public async Task<AdapterBuildResult> BuildPatchAsync(string sourceDirectory,
        IReadOnlyDictionary<string, string> translations, string patchDirectory, CancellationToken cancellationToken = default) =>
        (await InvokeAsync("build", sourceDirectory, DiscoverResources(sourceDirectory), translations, patchDirectory, cancellationToken)).Build
            ?? throw new GeneratedAdapterException("The adapter returned no build result.");

    public async Task<ValidationResult> ValidatePatchAsync(string sourceDirectory, string patchDirectory, CancellationToken cancellationToken = default)
    {
        var validation = (await InvokeAsync("validate", sourceDirectory, DiscoverResources(sourceDirectory), null, patchDirectory, cancellationToken)).Validation
            ?? throw new GeneratedAdapterException("The adapter returned no validation result.");
        if (validation.Issues is null || validation.Issues.Any(issue => issue is null || issue.Code is null || issue.Message is null))
            throw new GeneratedAdapterException("The adapter returned an invalid validation result.");
        if (validation.Passed && validation.Issues.Count > 0)
            throw new GeneratedAdapterException("适配器返回了矛盾的校验结果：passed=true，但 issues 非空。issues 只用于阻止通过的错误；"
                + "错误应返回 passed=false，覆盖范围、字体差异或尚未实机测试等说明应放入 probe/extraction/build.warnings，"
                + "运行状态使用 runtimeStatus=not-tested。首条 issue："
                + LimitDiagnostic(validation.Issues[0].Code + ": " + validation.Issues[0].Message));
        var issues = validation.Issues.Concat(ValidateExecutables(sourceDirectory, patchDirectory, EnumerateFiles(patchDirectory)
            .Select(path => GeneratedAdapterResources.Normalize(Path.GetRelativePath(patchDirectory, path))), null)).ToArray();
        return validation with { Passed = validation.Passed && issues.Length == 0, Issues = issues, RuntimeStatus = "not-tested" };
    }

    private async Task<CliResponse> InvokeAsync(string operation, string sourceDirectory, IReadOnlyList<string> resources,
        IReadOnlyDictionary<string, string>? translations, string? patchDirectory, CancellationToken cancellationToken)
    {
        GeneratedAdapterResources.VerifyPackage(package);
        var dependency = WorkspaceFiles.Resolve(package.DirectoryPath, EngineReferenceToolkit.DependencyPath);
        if ((File.Exists(dependency) || File.Exists(WorkspaceFiles.Resolve(package.DirectoryPath, BuiltinAdapterAssets.SharedDependencyPath))) && engineToolkit is null)
            throw new GeneratedAdapterException("该适配器依赖共享引擎工具，请通过完整应用加载。");
        var toolPaths = engineToolkit is null ? null : await engineToolkit.ResolvePackageAsync(package.DirectoryPath, cancellationToken);
        if (package.IsPython && toolPaths is not null)
            toolPaths = toolPaths with { HostCommand = EngineReferenceToolkit.AdapterHostCommand(toolchain) };
        var python = package.IsPython ? toolPaths is { Python: not null, EngineLibrary: not null }
            ? new EnginePythonPaths(toolPaths.Python, toolPaths.EngineLibrary)
            : throw new GeneratedAdapterException("Python 适配器缺少固定版本的共享解释器或引擎库。") : null;
        var stage = operation switch { "probe" => "识别引擎", "extract" => "提取游戏文本", "build" => "生成补丁文件", _ => "校验补丁资源" };
        reportProgress?.Invoke(stage, "正在准备资源并运行 " + (package.Family?.Name ?? package.Id) + " 适配器。");
        Directory.CreateDirectory(workRoot);
        var workspace = WorkspaceFiles.Resolve(workRoot, "run-" + Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(workspace);
        var source = Path.GetFullPath(sourceDirectory);
        var output = Path.Combine(workspace, "output");
        Directory.CreateDirectory(output);
        try
        {
            IReadOnlyList<ProbeFile>? probeFiles = null;
            var probeReads = operation == "probe" && package.ManifestVersion >= 2
                ? new GeneratedProbeReads(sourceDirectory, resources, workspace) : null;
            if (operation == "probe")
            {
                source = Path.Combine(workspace, "headers");
                Directory.CreateDirectory(source);
                var headers = new List<ProbeFile>();
                foreach (var resource in resources)
                {
                    cancellationToken.ThrowIfCancellationRequested();
                    var original = WorkspaceFiles.Resolve(sourceDirectory, resource);
                    var destination = WorkspaceFiles.Resolve(source, resource);
                    Directory.CreateDirectory(Path.GetDirectoryName(destination)!);
                    if (package.ManifestVersion >= 2 && discovery is not null
                        && Path.GetFullPath(sourceDirectory).Equals(discovery.DirectoryPath, StringComparison.OrdinalIgnoreCase))
                    {
                        var cachedLength = new FileInfo(original).Length;
                        int cachedHeaderLength = (int)Math.Min(cachedLength, ProbeHeaderBytes);
                        var cached = cachedHeaderLength == 0 ? [] : await discovery.ReadRangeAsync(resource, 0, cachedHeaderLength, cancellationToken);
                        if (cached is not null)
                        {
                            await File.WriteAllBytesAsync(destination, cached, cancellationToken);
                            headers.Add(new(resource, cachedLength, cachedHeaderLength));
                            continue;
                        }
                    }
                    await using var input = new FileStream(original, FileMode.Open, FileAccess.Read, FileShare.Read,
                        ProbeHeaderBytes, FileOptions.Asynchronous | FileOptions.SequentialScan);
                    var length = input.Length;
                    // Existing v1 packages predate truncated probes and retain their original input contract.
                    var headerLength = package.ManifestVersion == 1 ? length : Math.Min(length, ProbeHeaderBytes);
                    await using var staged = new FileStream(destination, FileMode.CreateNew, FileAccess.Write, FileShare.None);
                    var buffer = new byte[(int)Math.Min(headerLength, ProbeHeaderBytes)];
                    long remaining = headerLength;
                    while (remaining > 0)
                    {
                        var count = await input.ReadAsync(buffer.AsMemory(0, (int)Math.Min(buffer.Length, remaining)), cancellationToken);
                        if (count == 0) throw new IOException("A probe resource changed while its header was being read.");
                        await staged.WriteAsync(buffer.AsMemory(0, count), cancellationToken);
                        remaining -= count;
                    }
                    headers.Add(new(resource, length, headerLength));
                }
                probeFiles = headers;
            }
            var baseline = await ResourceHashesAsync(source, resources, cancellationToken);
            Dictionary<string, string>? patchBaseline = null;
            if (operation == "validate")
            {
                if (patchDirectory is null) throw new ArgumentException("Validation requires a patch directory.");
                output = Path.GetFullPath(patchDirectory);
                patchBaseline = await AllHashesAsync(output, cancellationToken);
            }
            var responsePath = Path.Combine(workspace, "response.json");
            string? translationsPath = null;
            if (translations is not null && package.ManifestVersion >= 2)
            {
                translationsPath = Path.Combine(workspace, "translations.json");
                await using var stream = new FileStream(translationsPath, FileMode.CreateNew, FileAccess.Write, FileShare.None);
                await JsonSerializer.SerializeAsync(stream, translations, EngineJson.TypeInfo<IReadOnlyDictionary<string, string>>(Json), cancellationToken);
            }
            CliResponse response;
            while (true)
            {
                // Each additional round is a local adapter invocation, not a model request.
                if (File.Exists(responsePath)) File.Delete(responsePath);
                var request = JsonSerializer.Serialize(new JsonObject
                {
                    ["protocolVersion"] = package.ProtocolVersion,
                    ["operation"] = operation,
                    ["sourceDirectory"] = source,
                    ["outputDirectory"] = output,
                    ["adapterDirectory"] = package.IsPython ? package.DirectoryPath : null,
                    ["resourceFiles"] = EngineJson.Node((resources).ToArray(), Json),
                    ["probeFiles"] = EngineJson.Node((probeFiles)?.ToArray(), Json),
                    ["probeRanges"] = EngineJson.Node((probeReads?.Ranges)?.ToArray(), Json),
                    ["probeReadLimits"] = EngineJson.Node(probeReads is null ? null : GeneratedProbeReads.Limits, Json),
                    ["translations"] = EngineJson.Node(translationsPath is null ? translations : null, Json),
                    ["translationsFile"] = translationsPath,
                    ["responseFile"] = responsePath,
                    ["toolPaths"] = EngineJson.Node(toolPaths, Json)
                }, EngineJson.TypeInfo<JsonObject>(Json));
                var result = await runner.RunAsync(new AdapterProcessRequest(python?.Python ?? toolchain.ExecutablePath,
                    python is null ? [Path.Combine(package.DirectoryPath, "adapter.dll")]
                        : EngineReferenceToolkit.PythonArguments(python, WorkspaceFiles.Resolve(package.DirectoryPath, package.EntryFile), []), workspace, request,
                    Timeout: TimeSpan.FromMinutes(10), MaximumOutputBytes: package.ManifestVersion >= 2 ? 8L * 1024 * 1024 : 128L * 1024 * 1024,
                    VnTextPatchAssemblyPath: toolPaths?.VnTextPatch), cancellationToken);
                reportProgress?.Invoke(stage, "资源处理完成，正在校验结果与原文件完整性。");
                if (!HashesEqual(baseline, await ResourceHashesAsync(source, resources, cancellationToken)))
                    throw new GeneratedAdapterException("The adapter modified host source resources.");
                if (patchBaseline is not null && !HashesEqual(patchBaseline, await AllHashesAsync(output, cancellationToken)))
                    throw new GeneratedAdapterException("The adapter modified patch files during validation.");
                if (result.ExitCode != 0)
                    throw new GeneratedAdapterException("The adapter process failed: " + LimitDiagnostic(result.StandardError));
                probeReads?.VerifyUnchanged();
                try
                {
                    if (File.Exists(responsePath))
                    {
                        WorkspaceFiles.RejectReparsePoints(responsePath);
                        await using var stream = new FileStream(responsePath, FileMode.Open, FileAccess.Read, FileShare.Read,
                            128 * 1024, FileOptions.Asynchronous | FileOptions.SequentialScan);
                        response = await JsonSerializer.DeserializeAsync<CliResponse>(stream, EngineJson.TypeInfo<CliResponse>(Json), cancellationToken) ?? throw new JsonException("Empty JSON.");
                    }
                    else response = JsonSerializer.Deserialize<CliResponse>(result.StandardOutput, EngineJson.TypeInfo<CliResponse>(Json)) ?? throw new JsonException("Empty JSON.");
                }
                catch (JsonException exception) { throw new GeneratedAdapterException("The adapter returned invalid protocol JSON: " + exception.Message); }
                if (response.ProtocolVersion != package.ProtocolVersion || !response.Success)
                    throw new GeneratedAdapterException("The adapter rejected the operation: " + LimitDiagnostic(response.Error ?? "Invalid protocol version or unsuccessful response."));
                if (response.ProbeReads is not { Count: > 0 } reads) break;
                if (probeReads is null || response.Probe is not null)
                    throw new GeneratedAdapterException("probeReads 仅可用于新版 probe 请求，不能同时返回 probe 结果。");
                await probeReads.SupplyAsync(reads, cancellationToken);
                }
            if (operation == "build")
            {
                if (response.Build is not { Files: { } files, Warnings: not null } || patchDirectory is null)
                    throw new GeneratedAdapterException("Missing or invalid build response.");
                if (files.Count == 0 || files.Select(GeneratedAdapterResources.Normalize).Distinct(StringComparer.OrdinalIgnoreCase).Count() != files.Count)
                    throw new GeneratedAdapterException("The adapter returned no build files or duplicate paths.");
                var actual = EnumerateFiles(output).Select(file => GeneratedAdapterResources.Normalize(Path.GetRelativePath(output, file))).ToHashSet(StringComparer.OrdinalIgnoreCase);
                var declared = files.Select(GeneratedAdapterResources.Normalize).ToHashSet(StringComparer.OrdinalIgnoreCase);
                if (!actual.SetEquals(declared)) throw new GeneratedAdapterException("Build output does not match its declared file list.");
                foreach (var file in files)
                {
                    _ = WorkspaceFiles.Resolve(output, file);
                    var destination = WorkspaceFiles.Resolve(patchDirectory, file);
                    if (File.Exists(destination) || Directory.Exists(destination)) throw new IOException("The patch destination already exists: " + file);
                }
                if (response.Build.LaunchExecutable is { } launch && !declared.Contains(GeneratedAdapterResources.Normalize(launch)))
                    throw new GeneratedAdapterException("The launch executable is not among the declared build files.");
                if (string.IsNullOrWhiteSpace(response.Build.LaunchExecutable) && files.Any(file =>
                    Path.GetExtension(file).ToLowerInvariant() is ".exe" or ".cmd" or ".bat" or ".ps1"
                    && !File.Exists(WorkspaceFiles.Resolve(sourceDirectory, file))))
                    throw new GeneratedAdapterException("补丁新增了可执行文件或脚本，但未声明启动入口。请在 build.launchExecutable 指定原游戏 EXE 或独立汉化 EXE；需要字体或转区启动器时，不能只输出 CMD/PowerShell 脚本。");
                var executableIssues = ValidateExecutables(sourceDirectory, output, files, response.Build.LaunchExecutable);
                if (executableIssues.Count != 0)
                    throw new GeneratedAdapterException("Executable static validation failed: " + string.Join("; ", executableIssues.Select(issue => issue.Message)));
                foreach (var file in files)
                    await GeneratedAdapterResources.CopyAsync(WorkspaceFiles.Resolve(output, file), WorkspaceFiles.Resolve(patchDirectory, file), cancellationToken);
            }
            return response;
        }
        catch (GeneratedAdapterException error) when (operation == "probe" && package.ManifestVersion >= 2)
        {
            throw new GeneratedAdapterException("探测失败：" + error.Message
                + "\nprobe 的 sourceDirectory 仅含每个候选文件前 65536 字节，probeFiles.length 是原始长度，headerBytes 是实际可读长度。"
                + "包尾索引或 PE 资源可通过返回 probeReads 请求有限片段，下一次本地 probe 从 probeRanges[].dataPath 读取。不要把截断文件头当成原文件损坏，也不要为读取索引复制整个媒体包。");
        }
        finally { DeleteStagedTree(workspace); }
    }

    private IReadOnlyList<ValidationIssue> ValidateExecutables(string sourceDirectory, string outputDirectory,
        IEnumerable<string> files, string? launchExecutable)
    {
        var executableFiles = files.Where(file => file.EndsWith(".exe", StringComparison.OrdinalIgnoreCase)
            || string.Equals(file, launchExecutable, StringComparison.OrdinalIgnoreCase)).ToArray();
        var issues = new List<ValidationIssue>();
        foreach (var file in executableFiles)
        {
            var original = WorkspaceFiles.Resolve(sourceDirectory, file);
            if (!File.Exists(original) && originalExecutable is { } selected)
                original = WorkspaceFiles.Resolve(sourceDirectory, selected);
            if (!File.Exists(original))
            {
                var candidates = DiscoverResources(sourceDirectory).Where(path => path.EndsWith(".exe", StringComparison.OrdinalIgnoreCase)).ToArray();
                if (candidates.Length == 1) original = WorkspaceFiles.Resolve(sourceDirectory, candidates[0]);
            }
            issues.AddRange(ExecutableChecks.Validate(File.Exists(original) ? File.ReadAllBytes(original) : null,
                File.ReadAllBytes(WorkspaceFiles.Resolve(outputDirectory, file))).Issues);
        }
        return issues;
    }

    private static void RequireResourcePath(string path, IReadOnlyList<string> resources)
    {
        _ = WorkspaceFiles.Resolve(Path.GetTempPath(), path);
        if (!resources.Contains(GeneratedAdapterResources.Normalize(path), StringComparer.OrdinalIgnoreCase))
            throw new GeneratedAdapterException("The adapter referenced a file outside its discovered resource set.");
    }

    private static async Task<Dictionary<string, string>> ResourceHashesAsync(string root, IReadOnlyList<string> resources, CancellationToken cancellationToken)
    {
        var hashes = new Dictionary<string, string>(StringComparer.Ordinal);
        foreach (var file in resources)
        {
            await using var stream = new FileStream(WorkspaceFiles.Resolve(root, file), FileMode.Open, FileAccess.Read, FileShare.Read,
                128 * 1024, FileOptions.Asynchronous | FileOptions.SequentialScan);
            hashes.Add(file, Convert.ToHexStringLower(await SHA256.HashDataAsync(stream, cancellationToken)));
        }
        return hashes;
    }

    private static Task<Dictionary<string, string>> AllHashesAsync(string root, CancellationToken cancellationToken) =>
        ResourceHashesAsync(root, EnumerateFiles(root).Select(path => GeneratedAdapterResources.Normalize(Path.GetRelativePath(root, path))).ToArray(), cancellationToken);

    private static bool HashesEqual(Dictionary<string, string> left, Dictionary<string, string> right) =>
        left.Count == right.Count && left.All(pair => right.TryGetValue(pair.Key, out var value) && pair.Value == value);

    internal static IEnumerable<string> EnumerateFiles(string directory)
    {
        WorkspaceFiles.RejectLinkAncestors(directory);
        foreach (var child in Directory.EnumerateFileSystemEntries(directory))
        {
            WorkspaceFiles.RejectReparsePoints(child);
            if (Directory.Exists(child))
            {
                foreach (var file in EnumerateFiles(child)) yield return file;
            }
            else yield return child;
        }
    }

    internal static void DeleteStagedTree(string directory)
    {
        if (!Directory.Exists(directory)) return;
        try
        {
            WorkspaceFiles.RejectLinkAncestors(directory);
            var files = EnumerateFiles(directory).ToArray();
            foreach (var file in files) File.SetAttributes(file, File.GetAttributes(file) & ~FileAttributes.ReadOnly);
            Directory.Delete(directory, true);
        }
        catch (Exception exception) when (exception is IOException or UnauthorizedAccessException or InvalidDataException)
        {
            // A failed cleanup cannot replace the operation's diagnostic or escape its staging root.
        }
    }

    internal sealed record ProbeFile(string Path, long Length, long HeaderBytes);
    internal sealed record CliResponse(int ProtocolVersion, bool Success, string? Error,
        GameProbe? Probe, ExtractionResult? Extraction, AdapterBuildResult? Build, ValidationResult? Validation,
        IReadOnlyList<ProbeRead>? ProbeReads = null);
}
