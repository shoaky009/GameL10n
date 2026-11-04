using GameL10n.Core.Development;
using GameL10n.Core.Isolation;

namespace GameL10n.Core.Adapters;

/// <summary>Runs one upstream script parser in a fresh process so its static encoding and reader state cannot leak.</summary>
public sealed class VnTextPatchAdapter : IGameAdapter
{
    private readonly VnTextPatchFormat format;
    private readonly VnTextPatchClient client;
    private readonly VnTextPatchDiscovery discovery;
    private static readonly string[] Warnings =
    [
        "此适配器处理已解包的独立脚本，不包含资源包解包或重打包；需要确认游戏能加载这些脚本。"
    ];

    public VnTextPatchAdapter(VnTextPatchFormat format, IAdapterProcessRunner? runner = null,
        string? workerExecutablePath = null, string? workerAssemblyPath = null)
        : this(format, new VnTextPatchClient(runner ?? new AdapterProcessRunner(), workerExecutablePath, workerAssemblyPath),
            new VnTextPatchDiscovery()) { }

    internal VnTextPatchAdapter(VnTextPatchFormat format, VnTextPatchClient client, VnTextPatchDiscovery discovery)
    {
        format.Family.Validate();
        this.format = format;
        this.client = client;
        this.discovery = discovery;
    }

    public string Id => format.Family.AdapterId;
    public string FormatKey => format.Key;
    public string Version => VnTextPatchFormatCatalog.Version;
    public AdapterCatalogEntry CatalogEntry => new(format.Family, Version, []);
    public string? LastProbeDiagnostic { get; private set; }

    public void ResetDiscovery() => discovery.Reset();

    public Task<GameProbe?> ProbeAsync(string gameDirectory, CancellationToken cancellationToken = default) =>
        ProbeAsync(gameDirectory, null, cancellationToken);

    internal async Task<GameProbe?> ProbeAsync(string gameDirectory, Func<VnTextPatchFormat, IReadOnlyList<string>, Task>? reportResources,
        CancellationToken cancellationToken)
    {
        LastProbeDiagnostic = null;
        try
        {
            var resources = discovery.ForProbe(gameDirectory, format, cancellationToken);
            if (reportResources is not null) await reportResources(format, resources);
            if (resources.Count == 0) return null;
            var response = await client.InvokeAsync("probe", format.Key, gameDirectory, resources, null, null, cancellationToken);
            if (response.Probe is not { } probe) return null;
            if (probe.RequiredFiles is null || probe.Warnings is null || probe.RequiredFiles.Count == 0)
                throw new VnTextPatchException("内置脚本工具未声明有效资源列表。");
            RequirePaths(gameDirectory, probe.RequiredFiles, resources);
            if (probe.OriginalExecutable is not null)
                throw new VnTextPatchException("独立脚本适配器不能声明修改游戏启动程序。");
            return probe with
            {
                EngineId = format.Family.Key, AdapterId = Id, AdapterVersion = Version,
                Description = format.Family.Name + "（独立脚本，运行未验证）", Warnings = WithWarnings(probe.Warnings)
            };
        }
        catch (Exception error) when (error is VnTextPatchException or AdapterProcessLimitException or IOException or InvalidDataException)
        {
            LastProbeDiagnostic = VnTextPatchClient.Diagnostic(error.Message);
            return null;
        }
    }

    public async Task<ExtractionResult> ExtractAsync(string sourceDirectory, CancellationToken cancellationToken = default)
    {
        var resources = VnTextPatchDiscovery.Discover(sourceDirectory, format, cancellationToken);
        var result = (await client.InvokeAsync("extract", format.Key, sourceDirectory, resources, null, null, cancellationToken)).Extraction
            ?? throw new VnTextPatchException("内置脚本工具未返回提取结果。");
        if (result.Units is null || result.Warnings is null || result.Units.Count == 0
            || result.Units.Any(unit => unit is null || string.IsNullOrWhiteSpace(unit.Id) || string.IsNullOrWhiteSpace(unit.SourceText)
                || unit.Locator is null || unit.Context is null || unit.Kind is null)
            || result.Units.Select(unit => unit.Id).Distinct(StringComparer.Ordinal).Count() != result.Units.Count)
            throw new VnTextPatchException("内置脚本工具返回了空文本、重复 ID 或无效提取结果。");
        var resourceSet = resources.ToHashSet(StringComparer.OrdinalIgnoreCase);
        foreach (var path in result.Units.Select(unit => unit.ResourcePath).Distinct(StringComparer.OrdinalIgnoreCase))
            RequirePath(sourceDirectory, path, resourceSet);
        return result with { Warnings = WithWarnings(result.Warnings) };
    }

    public async Task<AdapterBuildResult> BuildPatchAsync(string sourceDirectory, IReadOnlyDictionary<string, string> translations,
        string patchDirectory, CancellationToken cancellationToken = default)
    {
        WorkspaceFiles.RejectLinkAncestors(patchDirectory);
        if (Directory.Exists(patchDirectory) && Directory.EnumerateFileSystemEntries(patchDirectory).Any())
            throw new InvalidDataException("补丁输出目录必须为空，请为本次构建选择新的目录。");
        var resources = VnTextPatchDiscovery.Discover(sourceDirectory, format, cancellationToken);
        var result = (await client.InvokeAsync("build", format.Key, sourceDirectory, resources, patchDirectory, translations,
            cancellationToken)).Build ?? throw new VnTextPatchException("内置脚本工具未返回补丁结果。");
        if (result.Files is null || result.Warnings is null || result.Files.Count == 0 || result.LaunchExecutable is not null)
            throw new VnTextPatchException("内置脚本工具返回了无效补丁结果。");
        RequirePaths(patchDirectory, result.Files, resources);
        if (result.Files.Count != resources.Count)
            throw new VnTextPatchException("内置脚本工具遗漏了原始资源文件。");
        var actual = GeneratedGameAdapter.EnumerateFiles(patchDirectory)
            .Select(file => Normalize(Path.GetRelativePath(patchDirectory, file))).ToHashSet(StringComparer.OrdinalIgnoreCase);
        if (!actual.SetEquals(result.Files.Select(Normalize)))
            throw new VnTextPatchException("实际补丁文件与内置脚本工具声明的文件列表不一致。");
        return result with { Warnings = WithWarnings(result.Warnings) };
    }

    public async Task<ValidationResult> ValidatePatchAsync(string sourceDirectory, string patchDirectory,
        CancellationToken cancellationToken = default)
    {
        var resources = VnTextPatchDiscovery.Discover(sourceDirectory, format, cancellationToken);
        var result = (await client.InvokeAsync("validate", format.Key, sourceDirectory, resources, patchDirectory, null,
            cancellationToken)).Validation ?? throw new VnTextPatchException("内置脚本工具未返回验证结果。");
        if (result.Issues is null || result.Issues.Any(issue => issue is null || string.IsNullOrWhiteSpace(issue.Code) || issue.Message is null))
            throw new VnTextPatchException("内置脚本工具返回了无效验证结果。");
        return result with { Passed = result.Passed && result.Issues.Count == 0, RuntimeStatus = "not-tested" };
    }

    private IReadOnlyList<string> WithWarnings(IReadOnlyList<string> warnings) =>
        warnings.Concat(Warnings).Concat(format.Key == "Kaguya" ? [KaguyaMessage40Guidance.EncodingWarning] : [])
            .Distinct(StringComparer.Ordinal).ToArray();

    private static void RequirePaths(string root, IReadOnlyList<string> paths, IReadOnlyList<string> resources)
    {
        if (paths.Any(string.IsNullOrWhiteSpace))
            throw new VnTextPatchException("内置脚本工具返回了空资源路径。");
        if (paths.Select(Normalize).Distinct(StringComparer.OrdinalIgnoreCase).Count() != paths.Count)
            throw new VnTextPatchException("内置脚本工具返回了重复资源路径。");
        var resourceSet = resources.ToHashSet(StringComparer.OrdinalIgnoreCase);
        foreach (var path in paths) RequirePath(root, path, resourceSet);
    }

    private static void RequirePath(string root, string path, IReadOnlySet<string> resources)
    {
        _ = WorkspaceFiles.Resolve(root, path);
        if (!resources.Contains(Normalize(path)))
            throw new VnTextPatchException("内置脚本工具引用了未发现的资源文件。");
    }

    private static string Normalize(string path) => path.Replace('\\', '/');
}
