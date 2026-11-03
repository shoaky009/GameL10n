using System.Text.Json;
using GameL10n.Core.Development;
using GameL10n.Core.Isolation;

namespace GameL10n.Core.Adapters;

/// <summary>Shared, pinned Siglus reader/writer; game-specific rules use the same JSON worker.</summary>
public sealed class SiglusAdapter(EngineReferenceToolkit toolkit, IAdapterProcessRunner? runner = null,
    Action<string, string>? reportProgress = null) : IGameAdapter
{
    private readonly SiglusClient client = new(toolkit, runner ?? new AdapterProcessRunner(), reportProgress);
    public string Id => SiglusAssets.Family.AdapterId;
    public string Version => SiglusAssets.Version;
    public AdapterCatalogEntry CatalogEntry => new(SiglusAssets.Family, Version, []);

    public async Task<GameProbe?> ProbeAsync(string gameDirectory, CancellationToken cancellationToken = default)
    {
        if (!File.Exists(Path.Combine(gameDirectory, "Scene.pck")) || !File.Exists(Path.Combine(gameDirectory, "Gameexe.dat"))) return null;
        await using (var input = File.OpenRead(Path.Combine(gameDirectory, "Scene.pck")))
        {
            var header = new byte[4];
            if (await input.ReadAsync(header, cancellationToken) != 4 || System.Buffers.Binary.BinaryPrimitives.ReadUInt32LittleEndian(header) != 92) return null;
        }
        var result = await client.InvokeAsync("probe", gameDirectory, null, null, null, cancellationToken);
        return Read<GameProbe>(result, "probe");
    }

    public async Task<ExtractionResult> ExtractAsync(string sourceDirectory, CancellationToken cancellationToken = default)
    {
        var result = Read<ExtractionResult>(await client.InvokeAsync("extract", sourceDirectory, null, null, null, cancellationToken), "extraction")
            ?? throw new InvalidDataException("Siglus 未返回提取结果。");
        if (result.Units.Count == 0 || result.Units.Select(u => u.Id).Distinct(StringComparer.Ordinal).Count() != result.Units.Count)
            throw new InvalidDataException("Siglus 返回了空文本集或重复文本 ID。");
        return result;
    }

    public async Task<AdapterBuildResult> BuildPatchAsync(string sourceDirectory, IReadOnlyDictionary<string, string> translations,
        string patchDirectory, CancellationToken cancellationToken = default) =>
        Read<AdapterBuildResult>(await client.InvokeAsync("build", sourceDirectory, patchDirectory, translations, null, cancellationToken), "build")
        ?? throw new InvalidDataException("Siglus 未返回补丁结果。");

    public async Task<ValidationResult> ValidatePatchAsync(string sourceDirectory, string patchDirectory, CancellationToken cancellationToken = default) =>
        Read<ValidationResult>(await client.InvokeAsync("validate", sourceDirectory, patchDirectory, null, null, cancellationToken), "validation")
        ?? throw new InvalidDataException("Siglus 未返回验证结果。");

    public IReadOnlyList<ValidationIssue> ValidateTranslation(TranslationUnit unit, string text) =>
        text.Any(ch => char.IsSurrogate(ch) || ch == '\0')
            ? [new("siglus-character", "当前 Siglus 方案不支持此译文中的补充平面字符或空字符。", unit.Id)] : [];

    private static T? Read<T>(JsonElement response, string property) => response.GetProperty(property).Deserialize<T>(SiglusClient.Json);
}

internal sealed class SiglusClient(EngineReferenceToolkit toolkit, IAdapterProcessRunner runner, Action<string, string>? progress = null)
{
    internal static readonly JsonSerializerOptions Json = new(JsonSerializerDefaults.Web);

    internal async Task<JsonElement> InvokeAsync(string operation, string sourceDirectory, string? outputDirectory,
        IReadOnlyDictionary<string, string>? translations, JsonElement? inspection, CancellationToken ct)
    {
        var source = Path.GetFullPath(sourceDirectory);
        var resources = DiscoverResources(source);
        if (outputDirectory is not null)
        {
            var output = Path.GetFullPath(outputDirectory);
            RequireSeparate(source, output);
            WorkspaceFiles.RejectLinkAncestors(output);
        }
        var pin = await toolkit.EnsureReferenceAsync(SiglusAssets.Pin, ct);
        var prepared = await toolkit.EnsurePythonAsync(pin, null, ct);
        var work = Path.Combine(Path.GetTempPath(), "gamel10n-siglus-" + Guid.NewGuid().ToString("N"));
        RequireSeparate(source, work);
        WorkspaceFiles.RejectLinkAncestors(work);
        Directory.CreateDirectory(work);
        try
        {
            var responsePath = Path.Combine(work, "response.json");
            var translationsPath = translations is null ? null : Path.Combine(work, "translations.json");
            if (translationsPath is not null)
            {
                await using var stream = File.Create(translationsPath);
                await JsonSerializer.SerializeAsync(stream, translations, Json, ct);
            }
            var request = new Dictionary<string, object?>
            {
                ["protocolVersion"] = 1, ["operation"] = operation, ["sourceDirectory"] = source,
                ["outputDirectory"] = outputDirectory is null ? null : Path.GetFullPath(outputDirectory),
                ["resourceFiles"] = resources, ["responseFile"] = responsePath,
                ["translationsFile"] = translationsPath, ["compactOutput"] = true
            };
            if (inspection is { } options)
                foreach (var name in new[] { "source", "query", "offset", "length" })
                    if (options.TryGetProperty(name, out var value)) request[name] = value;
            var stage = operation switch
            {
                "probe" => "检查 Siglus 资源", "extract" => "提取 Siglus 文本", "build" => "回填脚本和中文字库",
                "validate" => "验证 Siglus 补丁", _ => "分析 Siglus 显示规则"
            };
            progress?.Invoke(stage, operation == "build" ? "正在回填译文并补充缺少的中文字形。" : "正在使用本地工具检查剧情脚本和显示规则。");
            var process = await runner.RunAsync(new(prepared.Paths.Python,
                ["-I", "-B", "-X", "utf8", "-c", "import sys;sys.path.insert(0,sys.argv[1]);from gamel10n_siglus import main;sys.exit(main())", prepared.Paths.EngineLibrary],
                work, JsonSerializer.Serialize(request, Json), TimeSpan.FromMinutes(10), 1024 * 1024), ct);
            if (!File.Exists(responsePath)) throw new InvalidDataException("Siglus 工具未返回结果：" + VnTextPatchClient.Diagnostic(process.StandardError));
            WorkspaceFiles.RejectReparsePoints(responsePath);
            await using var input = File.OpenRead(responsePath);
            using var response = await JsonDocument.ParseAsync(input, cancellationToken: ct);
            var valueRoot = response.RootElement;
            if (valueRoot.GetProperty("protocolVersion").GetInt32() != 1 || !valueRoot.GetProperty("success").GetBoolean() || process.ExitCode != 0)
                throw new InvalidDataException(valueRoot.TryGetProperty("error", out var error) ? error.GetString() : "Siglus 工具执行失败。");
            return valueRoot.Clone();
        }
        finally { GeneratedGameAdapter.DeleteStagedTree(work); }
    }

    internal static IReadOnlyList<string> DiscoverResources(string source)
    {
        WorkspaceFiles.RejectLinkAncestors(source);
        var files = Directory.EnumerateFiles(source).Where(path => Path.GetFileName(path).Equals("Scene.pck", StringComparison.OrdinalIgnoreCase)
            || Path.GetFileName(path).Equals("Gameexe.dat", StringComparison.OrdinalIgnoreCase)
            || path.EndsWith(".exe", StringComparison.OrdinalIgnoreCase) && !path.EndsWith(".zh.exe", StringComparison.OrdinalIgnoreCase)
            || path.EndsWith(".exe.le.config", StringComparison.OrdinalIgnoreCase)).ToList();
        var glyphs = Path.Combine(source, "g00");
        if (Directory.Exists(glyphs))
        {
            WorkspaceFiles.RejectLinkAncestors(glyphs);
            files.AddRange(Directory.EnumerateFiles(glyphs, "*.g00").Where(path =>
                Path.GetFileName(path).StartsWith("_moji_AN48_", StringComparison.OrdinalIgnoreCase)
                || Path.GetFileName(path).StartsWith("_moji_SE48_", StringComparison.OrdinalIgnoreCase)));
        }
        if (files.Count > 1000) throw new InvalidDataException("Siglus 候选资源过多，请使用智能适配确认资源规则。");
        foreach (var path in files) WorkspaceFiles.RejectReparsePoints(path);
        return files.Select(path => Path.GetRelativePath(source, path).Replace('\\', '/')).Order(StringComparer.Ordinal).ToArray();
    }

    private static void RequireSeparate(string source, string output)
    {
        var left = Path.TrimEndingDirectorySeparator(Path.GetFullPath(source));
        var right = Path.TrimEndingDirectorySeparator(Path.GetFullPath(output));
        if (left.Equals(right, StringComparison.OrdinalIgnoreCase)
            || left.StartsWith(right + Path.DirectorySeparatorChar, StringComparison.OrdinalIgnoreCase)
            || right.StartsWith(left + Path.DirectorySeparatorChar, StringComparison.OrdinalIgnoreCase))
            throw new InvalidDataException("Siglus 原始资源、补丁和临时目录必须相互独立。");
    }
}
