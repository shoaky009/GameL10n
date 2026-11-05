using System.Text.Json;
using GameL10n.Core.Adapters;

namespace GameL10n.Core.Development;

public sealed partial class AdaptationTools
{
    private BuiltinAdapterAssets BuiltinAssets => Development.BuiltinAssets;
    private AdapterComponentAssets ComponentAssets => componentAssets ?? new AdapterComponentAssets();

    private async Task<object> ListAdaptersAsync(JsonElement args, CancellationToken ct)
    {
        if (session.BuiltinDiscovery is null || Boolean(args, "refreshCandidates", false))
        {
            session.BuiltinDiscovery = await BuiltinAdapterDiscovery.InspectAsync(request.GameDirectory, ct);
            session.BuiltinDiscoveryObservedAt = DateTimeOffset.UtcNow;
        }
        var query = Text(args, "query", "").Trim();
        bool Matches(string text) => query.Length == 0 || text.Contains(query, StringComparison.OrdinalIgnoreCase);
        var disabled = store.ReadDisabledFamilyIds();
        // A project can repair its pinned version even when the family is disabled for new games.
        var pinned = store.LoadActive(request.GameId);
        var published = (await store.ReadDiscoveryCandidatesAsync(ct: ct)).AsEnumerable();
        if (pinned is not null) published = published.Prepend(new(pinned.Id, pinned.Version, null,
            new(pinned.ResourceFiles, pinned.ResourcePatterns, pinned.Recognition), new(RecognitionStatus.Unknown, 0), pinned.Family));
        return new
        {
            candidates = session.BuiltinDiscovery with { Candidates = session.BuiltinDiscovery.Candidates.Where(candidate => !disabled.Contains(candidate.AdapterId)).ToArray() },
            observedAt = session.BuiltinDiscoveryObservedAt,
            regularProbes = session.BuiltinProbes,
            published = published.DistinctBy(package => (package.Id, package.Version))
                .Where(package => Matches(package.Id + " " + package.Family?.Name))
                .Select(package => new { package.Id, package.Version, package.Family,
                    pinned = package.Id == pinned?.Id && package.Version == pinned.Version,
                    enabledForNewGames = !disabled.Contains(package.Id) }),
            builtIns = VnTextPatchFormatCatalog.All.Where(format => !disabled.Contains(format.Family.AdapterId)
                && Matches(format.Key + " " + format.Family.Name + " " + format.Family.AdapterId))
                .Select(format => new
                {
                    id = format.Family.AdapterId, version = VnTextPatchFormatCatalog.Version, format = format.Key,
                    name = format.Family.Name, extensions = format.Extensions, companions = format.CompanionFileNames
                }),
            components = ComponentAssets.List(query),
            engineComponents = EngineArchiveComponents.All.Where(component => Matches(component.Id + " " + component.Format)),
            templates = AdapterTemplates.All.Where(template => Matches(template.Id + " " + template.Format + " " + template.Family.Name)),
            siglus = !disabled.Contains(SiglusAssets.Family.AdapterId) && Matches("Siglus engine.siglus Scene.pck FKDS") ? SiglusAssets.Describe() : null,
            usage = "候选是本地采样线索，不代表兼容验证。inspect_builtin_adapter 查看相关能力；try_builtin_adapter 在少量工作副本上验证；copy_adapter 复用解析器。封包优先 inspect_engine_component / run_engine_component 直接调用；文本语义、编码、字体、启动处理可用 inspect_adapter_component 查看适用条件，再 copy_adapter_components 直接复用代码。组件不替代当前游戏验收。"
        };
    }

    private object InspectBuiltinAdapter(JsonElement args)
    {
        var format = Text(args, "format");
        if (format.Equals("Siglus", StringComparison.OrdinalIgnoreCase) || format == "engine.siglus")
            return args.TryGetProperty("path", out _) ? SiglusAssets.ReadSource(Text(args, "path"),
                Number(args, "offset", 0, 0, int.MaxValue), Number(args, "length", 8000, 1, 16000)) : SiglusAssets.Describe();
        if (format.Equals("NekoSDK", StringComparison.OrdinalIgnoreCase))
            return new { format, status = "reference-only", engine = "nekosdk",
                usage = "请调用 inspect_engine_reference(engine=nekosdk)。共享 Python 已提供 NEKOPACK4 和 ADVSCRIPT2 工具；尚无经过当前游戏验证的内置中文运行适配器。" };
        if (format.Equals("BSScriptRev6", StringComparison.OrdinalIgnoreCase) || format == "reference.bsscript-rev6")
            return new { format = "BSScriptRev6", status = "reference-only", usage = BuiltinAdapterDiscovery.BsscriptGuidance };
        if (args.TryGetProperty("path", out _))
            return BuiltinAssets.ReadSource(format, Text(args, "path"),
                Number(args, "offset", 0, 0, int.MaxValue), Number(args, "length", 12000, 1, 24000));
        object? engineInspection = null;
        if (format.Equals("Kaguya", StringComparison.OrdinalIgnoreCase))
        {
            var candidates = session.BuiltinDiscovery?.Candidates.Where(candidate => candidate.Format.Equals("Kaguya", StringComparison.OrdinalIgnoreCase))
                .SelectMany(candidate => candidate.SampleFiles) ?? [];
            var probes = session.BuiltinProbes.Where(probe => probe.Format.Equals("Kaguya", StringComparison.OrdinalIgnoreCase))
                .SelectMany(probe => probe.Resources);
            engineInspection = KaguyaMessage40Guidance.Inspect(request.GameDirectory, candidates.Concat(probes));
        }
        return BuiltinAssets.Describe(format, engineInspection);
    }

    private object InspectAdapterComponent(JsonElement args) => args.TryGetProperty("path", out _)
        ? ComponentAssets.ReadSource(Text(args, "id"), Text(args, "path"), Number(args, "offset", 0, 0, int.MaxValue),
            Number(args, "length", 12000, 1, 24000))
        : ComponentAssets.Describe(Text(args, "id"));

    private async Task<object> CopyAdapterComponentsAsync(JsonElement args, CancellationToken ct)
    {
        session.VerifiedPackage = null;
        var copied = new List<AdapterReleaseFile>();
        var result = await ComponentAssets.CopyAsync(Strings(args, "ids"), workspace, ct, copied);
        Development.RegisterReleaseFiles(copied);
        return result;
    }

    private async Task<object> TryBuiltinAdapterAsync(JsonElement args, CancellationToken ct)
    {
        var directory = Text(args, "directory");
        var files = Strings(args, "files");
        var result = await (builtinTrial ?? new BuiltinAdapterTrial(runtime.Runner)).RunAsync(Text(args, "format"),
            workspace, directory, files, Boolean(args, "checkRoundtrip", true), ct);
        RecordBuiltinTrial(result, directory, files);
        return result;
    }

    private void RecordBuiltinTrial(BuiltinAdapterTrialResult result, string directory, IReadOnlyList<string> files,
        string? fingerprint = null, IReadOnlyList<BuiltinAdapterTrialSample>? samples = null)
    {
        var cause = result.Diagnostic;
        while (cause?.Inner is { } inner) cause = inner;
        var diagnostic = cause is null ? null : Limit(
            $"{result.Diagnostic?.ResourcePath ?? cause.ResourcePath} {cause.ErrorType}: {cause.Message}", 1500);
        // Keep the latest observation per parser, not a new report file for every attempt.
        session.BuiltinTrials.RemoveAll(item => item.Format == result.Format);
        session.BuiltinTrials.Add(new(result.Format, directory, files, result.Status, result.RoundtripStatus,
            result.UnitCount, result.Error, diagnostic,
            result.SourceHashes, DateTimeOffset.UtcNow, fingerprint, samples));
        if (session.BuiltinTrials.Count > 6) session.BuiltinTrials.RemoveAt(0);
    }

    private static bool Boolean(JsonElement args, string name, bool defaultValue)
    {
        if (!args.TryGetProperty(name, out var value)) return defaultValue;
        if (value.ValueKind is not (JsonValueKind.True or JsonValueKind.False))
            throw new InvalidDataException(name + " 必须为布尔值。");
        return value.GetBoolean();
    }

    private async Task<object> ComposeAdapterTemplateAsync(JsonElement args, CancellationToken ct)
    {
        var composed = await Development.ComposeAsync(Text(args, "id"),
            args.TryGetProperty("resourcePatterns", out _) ? Strings(args, "resourcePatterns") : null,
            Text(args, "filter", ""), ct);
        var template = composed.Template;
        return new { template, files = composed.Files, releaseFiles = session.ReleaseFiles,
            testArguments = new { kind = template.Family.Kind, key = template.Family.Key, name = template.Family.Name, resourcePatterns = composed.ResourcePatterns },
            limitations = "只连接已声明的源脚本与封包组件。脚本编码沿用解析器；不生成未经当前游戏验证的字体、转区或运行时补丁。",
            nextAction = "暂存与规则匹配的真实资源，用返回的 testArguments 调用完整 test_adapter。模板生成不代表已验证，也不会发布。" };
    }
}

public sealed record BuiltinAdapterTrialObservation(string Format, string Directory, IReadOnlyList<string> Files,
    string Status, string RoundtripStatus, int UnitCount, string? Error, string? Diagnostic,
    IReadOnlyDictionary<string, string> SourceHashes, DateTimeOffset ObservedAt, string? Fingerprint = null,
    IReadOnlyList<BuiltinAdapterTrialSample>? Samples = null);

public sealed record BuiltinAdapterProbeObservation(string Format, string Status, IReadOnlyList<string> Resources,
    string? Diagnostic, DateTimeOffset ObservedAt);
