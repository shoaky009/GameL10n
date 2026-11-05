using System.Text.Json;
using GameL10n.Core.Adapters;
using GameL10n.Core.Isolation;

namespace GameL10n.Core.Development;

public sealed record AutomaticTemplateReport(int Revision, string Status, string? TemplateId, string Stage,
    IReadOnlyList<string> Diagnostics, IReadOnlyList<string> EvidenceFiles,
    long? ResourceBytes = null, long? SelectedBytes = null, long? ArchiveReadBytes = null);

public sealed partial class AdapterDevelopmentWorkflow
{
    private const long AutomaticTemplateBytes = 256L * 1024 * 1024;
    private const string AutomaticTemplateRuntimeCheck = "游戏启动、中文显示、字体及排版尚未实机验证。";
    // Generic data/text extensions do not establish a second script format. Recognizable formats
    // and containers do require investigation before a source-only template can finish Develop.
    private static readonly HashSet<string> AutomaticTemplateOtherResources = new(
        VnTextPatchFormatCatalog.All.SelectMany(format => format.Extensions)
            .Except(["", ".txt", ".bin", ".dat", ".map"], StringComparer.OrdinalIgnoreCase)
            .Concat([".rpyc", ".rpym", ".rpymc", ".scn", ".xp3", ".rpa", ".arc", ".pack", ".pak", ".ypf", ".int", ".pfs", ".pck"]),
        StringComparer.OrdinalIgnoreCase);

    /// <summary>Only a fresh Develop task may create a template; all acceptance and publication use the normal tools.</summary>
    internal async Task RunAutomaticTemplateAsync(IReadOnlySet<string> disabledFamilies, Func<Task> save, CancellationToken ct)
    {
        if (session.Objective != AdaptationObjective.Develop || request.ImproveExisting || request.BaselineUnits is { Count: > 0 }
            || !string.IsNullOrWhiteSpace(request.UserInstruction) || !string.IsNullOrWhiteSpace(session.UserInstruction)
            || session.TotalTurns != 0 || session.Messages.Count > 0 || session.PendingCalls.Count > 0
            || session.VerifiedPackage is not null || session.ReleaseFiles is { Count: > 0 } || session.ReuseFailures.Count > 0
            || session.AutomaticTemplate is not null
            || Directory.Exists(SourceDirectory) && Directory.EnumerateFileSystemEntries(SourceDirectory).Any()) return;
        if (session.BuiltinDiscovery?.Candidates.Any(candidate => candidate.Format is "Renpy" or "KirikiriKs") != true) return;

        using var budget = CancellationTokenSource.CreateLinkedTokenSource(ct);
        budget.CancelAfter(TimeSpan.FromMinutes(5));
        var token = budget.Token;
        var diagnostics = new List<string>();
        var evidence = new List<string>();
        AdapterTemplateDefinition? template = null;
        long? resourceBytes = null, selectedBytes = null, archiveReadBytes = null;
        var stage = "resources";
        try
        {
            await RecordAsync("running");
            reportProgress?.Invoke("检查自动组合条件", "正在检查完整资源列表及模板未覆盖的脚本、封包和启动线索。");
            var inventory = GameDiscoverySession.Scan(request.GameDirectory, token, maximumEntries: 20_000);
            if (!inventory.Complete) throw new InvalidDataException("资源目录未完整读取，不能自动确认模板适用范围。" + string.Join("；", inventory.Diagnostics));
            var paths = inventory.Select(["**/*"], token);
            var xp3 = paths.Where(path => Path.GetExtension(path).Equals(".xp3", StringComparison.OrdinalIgnoreCase)).ToArray();
            if (xp3.Length > 0)
                throw new InvalidDataException("XP3 缺少独立的无过滤器证据，不能根据零标记或可解包推断 filter=none；保留模型流程确认：" + Describe(xp3));
            var archives = paths.Where(path => Path.GetExtension(path).Equals(".rpa", StringComparison.OrdinalIgnoreCase)).ToArray();
            var renpy = paths.Where(path => Path.GetExtension(path).Equals(".rpy", StringComparison.OrdinalIgnoreCase)).ToArray();
            var kirikiri = paths.Where(path => Path.GetExtension(path).Equals(".ks", StringComparison.OrdinalIgnoreCase)).ToArray();
            if (archives.Length > 0 && (renpy.Length > 0 || kirikiri.Length > 0) || renpy.Length > 0 && kirikiri.Length > 0)
                throw new InvalidDataException("发现封包与裸脚本或不同脚本格式混用，单个现有模板不能覆盖这些资源：" + Describe(archives.Concat(renpy).Concat(kirikiri)));
            template = AdapterTemplates.Find(archives.Length > 0 ? "rpa3-renpy" : renpy.Length > 0 ? "loose-renpy" : "loose-kirikiri-ks");
            var format = VnTextPatchFormatCatalog.All.Single(format => format.Key == template.Format);
            if (disabledFamilies.Contains(template.Family.AdapterId) || disabledFamilies.Contains(format.Family.AdapterId))
                throw new InvalidDataException("模板或其内置解析器已禁用，未自动组合：" + template.Id);
            var resources = inventory.Select(template.ResourcePatterns, token);
            if (resources.Count is < 1 or > 1024 || archives.Length > 16)
                throw new InvalidDataException("自动组合需要 1～1,024 个资源，且最多 16 个封包；当前资源数量超出范围。");
            RequireSupportedPaths(paths, allowArchives: template.ArchiveComponent is not null);
            var otherCandidate = session.BuiltinDiscovery!.Candidates.FirstOrDefault(candidate => candidate.Format != template.Format
                && candidate.EvidenceKind is "script-signature" or "container-signature" or "format-signature" or "container-header");
            if (otherCandidate is not null)
                throw new InvalidDataException("另有未被模板覆盖的格式证据：" + otherCandidate.Format + "；" + string.Join("；", otherCandidate.Evidence));
            if (session.LaunchInspection is { } launch && (launch.Partial || launch.LocaleProfiles.Count > 0
                || launch.LaunchScripts.Count > 0 || launch.ErrorLogs.Count > 0))
                throw new InvalidDataException("存在尚需解释的启动配置、启动脚本、错误日志或不完整启动检查；先交给模型核对："
                    + Describe(launch.LocaleProfiles.Select(item => item.Path).Concat(launch.LaunchScripts.Select(item => item.Path))
                        .Concat(launch.ErrorLogs.Select(item => item.Path))));
            if (session.TextCoverage.Pending.Count > 0 || session.TextCoverage.RuntimeChecks.Count > 0)
                throw new InvalidDataException("已有覆盖或运行待办需要继续处理，未自动完成任务："
                    + string.Join("；", session.TextCoverage.Pending.Concat(session.TextCoverage.RuntimeChecks)));

            var lengths = resources.ToDictionary(path => path, path => new FileInfo(WorkspaceFiles.Resolve(request.GameDirectory, path)).Length,
                StringComparer.OrdinalIgnoreCase);
            resourceBytes = lengths.Values.Sum();
            var inputBudget = template.ArchiveComponent is null ? AutomaticTemplateBytes : EngineArchiveComponents.RpaMaximumArchiveBytes;
            if (lengths.Values.Any(length => length < 0 || length > inputBudget) || resourceBytes > inputBudget)
                throw new InvalidDataException($"完整资源超过自动组合的 {inputBudget / (1024 * 1024):N0} MiB 输入预算，未缩小为少量样本发布。");
            var hashes = await SampleHashesAsync(request.GameDirectory, lengths, token);
            stage = "stage";
            await RecordAsync("running");
            await StageAutomaticSamplesAsync(lengths, hashes, token);
            if (template.ArchiveComponent is not null)
            {
                stage = "archive-inspection";
                await RecordAsync("running");
                selectedBytes = 0;
                archiveReadBytes = 0;
                var entries = 0;
                var scripts = 0;
                var inspection = "work/automatic-template-" + Guid.NewGuid().ToString("N");
                foreach (var resource in resources)
                {
                    reportProgress?.Invoke("检查自动组合封包", "正在检查 " + resource + " 的全部成员及支持条件。");
                    var output = inspection + "/" + evidence.Count.ToString("D2");
                    // Inspect the complete index; media payloads do not consume the script budget.
                    var prepared = await PythonAsync(token);
                    var outputPath = WorkspaceFiles.Resolve(workspace, output);
                    var inspected = await new EngineArchiveComponentClient(runtime.Runner).InvokeAsync(prepared.Paths,
                        new(1, template.ArchiveComponent, "inspect", WorkspaceFiles.Resolve(ResourceDirectory, resource),
                            Path.Combine(outputPath, "manifest.json"), Extensions: template.ScriptExtensions), workspace, token);
                    evidence.Add(output + "/manifest.json");
                    await RecordAsync("running");
                    if (!inspected.GetProperty("success").GetBoolean())
                        throw new InvalidDataException(resource + "：" + inspected.GetProperty("error").GetString());
                    using var manifest = JsonDocument.Parse(WorkspaceFiles.ReadBounded(workspace, evidence[^1], 8 * 1024 * 1024));
                    var members = manifest.RootElement.GetProperty("entries").EnumerateArray().ToArray();
                    entries += members.Length;
                    selectedBytes += manifest.RootElement.GetProperty("selectedBytes").GetInt64();
                    archiveReadBytes += manifest.RootElement.GetProperty("readBytes").GetInt64();
                    if (entries > 10_000 || selectedBytes > AutomaticTemplateBytes)
                        throw new InvalidDataException("全部封包合计超过自动组合的 10,000 成员／256 MiB 脚本预算。");
                    if (members.Any(member => template.ScriptExtensions.Contains(Path.GetExtension(member.GetProperty("name").GetString()!), StringComparer.OrdinalIgnoreCase)
                        && member.GetProperty("length").GetInt64() > 64L * 1024 * 1024))
                        throw new InvalidDataException("单个脚本超过封包组件的 64 MiB 读取预算。");
                    RequireSupportedPaths(members.Select(member => member.GetProperty("name").GetString()!), allowArchives: false, resource + " → ");
                    scripts += manifest.RootElement.GetProperty("selectedCount").GetInt32();
                }
                if (scripts == 0) throw new InvalidDataException("全部封包中都没有模板支持的 .rpy 源脚本；未生成或发布适配器。");
            }

            stage = "compose";
            await RecordAsync("running");
            reportProgress?.Invoke("自动组合适配器", "资源符合 " + template.Id + " 的条件，正在连接现有组件。");
            await ComposeAsync(template.Id, null, null, token);
            session.TextCoverage = session.TextCoverage with
            { RuntimeChecks = session.TextCoverage.RuntimeChecks.Append(AutomaticTemplateRuntimeCheck).ToArray() };
            stage = "verification";
            await RecordAsync("running");
            reportProgress?.Invoke("验证自动组合适配器", "正在对全部匹配资源检查原字节往返、完整中文回填及部分试译。");
            var tested = await TestAsync(template.Family, template.ResourcePatterns, false, token);
            diagnostics.AddRange(session.VerificationDiagnostics);
            if (!tested.ReadyToPublish)
            { await RecordAsync("incomplete"); return; }
            stage = "publish";
            await RecordAsync("running");
            await PublishAsync("已自动组合 " + template.Family.Name + "，全部匹配资源已通过提取、原字节往返、完整中文及部分中文回填验收。"
                    + "支持范围仅为此模板选中的源脚本；独立文字覆盖" + (session.TextCoverage.Samples.Count == 0 ? "尚未评估" : "仅核对已保留样本")
                    + "，不代表整款游戏文字已完整覆盖。", false, token, async publicationToken =>
            {
                stage = "scope-recheck";
                await RecordAsync("running");
                // Test/finish check the selected resource patterns. Automatic selection additionally
                // depends on the absence of other formats, including files added during verification.
                var current = GameDiscoverySession.Scan(request.GameDirectory, publicationToken, maximumEntries: 20_000);
                var currentPaths = current.Select(["**/*"], publicationToken);
                if (!current.Complete || !paths.SequenceEqual(currentPaths, StringComparer.Ordinal))
                {
                    ClearVerification();
                    throw new InvalidDataException("自动组合期间游戏目录发生变化或未完整读取，需重新确认模板适用范围；未自动发布。新增／移除："
                        + Describe(currentPaths.Except(paths, StringComparer.Ordinal).Concat(paths.Except(currentPaths, StringComparer.Ordinal))));
                }
            });
            await RecordAsync("activated");
        }
        catch (OperationCanceledException) when (!ct.IsCancellationRequested)
        {
            diagnostics.Add("自动组合达到 5 分钟预算；保留现有文件与证据，未完成的步骤交给模型继续，不代表格式不受支持。");
            await RecordAsync("timed-out");
        }
        catch (OperationCanceledException)
        { await RecordAsync("cancelled"); throw; }
        catch (Exception error) when (error is not OperationCanceledException)
        {
            diagnostics.Add(Limit(error.Message, 2000));
            await RecordAsync("incomplete");
        }

        async Task RecordAsync(string status)
        {
            session.AutomaticTemplate = new(2, status, template?.Id, stage,
                diagnostics.TakeLast(12).Select(item => Limit(item, 2000)).ToArray(), evidence.ToArray(),
                resourceBytes, selectedBytes, archiveReadBytes);
            await save();
        }

        void RequireSupportedPaths(IEnumerable<string> paths, bool allowArchives, string prefix = "")
        {
            var unsupported = paths.Where(path => AutomaticTemplateOtherResources.Contains(Path.GetExtension(path))
                && !template!.ScriptExtensions.Contains(Path.GetExtension(path), StringComparer.OrdinalIgnoreCase)
                && !(allowArchives && Path.GetExtension(path).Equals(".rpa", StringComparison.OrdinalIgnoreCase))).ToArray();
            if (unsupported.Length > 0)
                throw new InvalidDataException("发现模板未覆盖的封包或脚本，需要继续确认，未自动发布：" + prefix + Describe(unsupported));
        }

        static string Describe(IEnumerable<string> paths) => string.Join("、", paths.Take(8).Select(path => Limit(path, 200)));
    }
}
