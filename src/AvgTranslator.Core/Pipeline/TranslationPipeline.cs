using System.Text.Json;
using AvgTranslator.Core.Patching;
using AvgTranslator.Core.Storage;
using AvgTranslator.Core.Translation;
using AvgTranslator.Core.Providers;
using AvgTranslator.Core.Adapters;
using AvgTranslator.Core.Development;
using AvgTranslator.Core.Validation;

namespace AvgTranslator.Core.Pipeline;

public sealed record SourceFile(string RelativePath, string Sha256);
public sealed record GameSession(string Id, string GameDirectory, string ProjectDirectory,
    string SourceDirectory, GameProbe Probe, IReadOnlyList<SourceFile> Sources,
    ExtractionResult Extraction, IGameAdapter Adapter);

public sealed record PipelineProgress(string Stage, int Completed, int Total, int CacheHits,
    long TranslationTokens, string Message, int Failed = 0);

public sealed record TranslationRunSummary(int Total, int Completed, int Failed);

public sealed record PipelineResult(string PatchDirectory, string? LaunchExecutable,
    int TotalUnits, int CacheHits, TokenUsage TranslationUsage, ValidationResult Validation,
    IReadOnlyList<string> Warnings, string? SourceFingerprint = null,
    bool IsTrial = false, int? TranslatedUnits = null);

public sealed record RunOptions(int BatchSize = 24, bool RetryUncertainRequests = true,
    IReadOnlyList<string>? TrialUnitIds = null)
{
    public IProgress<IReadOnlyDictionary<string, string>>? TranslationUpdates { get; init; }
    public IProgress<IReadOnlyDictionary<string, string?>>? FailureUpdates { get; init; }
    internal bool CandidateOnly { get; init; }
    public string? RetranslationId { get; init; }
}
public sealed record SourceSnapshot(string Directory, string AdapterId, string AdapterVersion, IReadOnlyList<SourceFile> Files,
    GameProbe? Probe = null);
public sealed record PendingTranslation(string Id, string MemoryKey);
public sealed record TranslationIntent(string RequestKey, string Provider, DateTimeOffset SentAt,
    IReadOnlyList<PendingTranslation> Units, bool CandidateOnly = false, string? RetranslationId = null);
public sealed record TranslationRefusal(TranslationIntent Request, DateTimeOffset RecordedAt, ProviderResponseDiagnostic? Diagnostic);

public sealed class UncertainRequestException(string message) : Exception(message);
public sealed class TranslationRetriesExhaustedException(TranslationProviderException error)
    : Exception("API 请求重试 5 次后仍失败，任务已暂停。已完成译文已保存，可继续相同范围的翻译。最后一次失败：" + error.FailureSummary + "。", error);

public sealed class TranslationRefusedException(bool alreadyRecorded = false)
    : Exception((alreadyRecorded ? "这批内容在翻译 API 链中的所有接口均已有拒绝记录，本次未再次调用 API。" : "翻译 API 链中的所有接口均拒绝处理这批内容（服务商内容限制），已停止尝试。") +
        "任务已暂停，已完成译文已保存。可调整翻译 API 链或处理被拒绝的内容后继续；拒绝说明保存在日志和任务记录中。");

public sealed partial class TranslationPipeline(string dataDirectory, string? diagnosticDirectory = null)
{
    public string DataDirectory { get; } = Path.GetFullPath(dataDirectory);

    public static string GameId(string gameDirectory) => LocalFiles.HashText(
        Path.GetFullPath(gameDirectory).TrimEnd(Path.DirectorySeparatorChar).ToUpperInvariant())[..24];

    /// <summary>Opens the saved analysis, without scanning the game or hashing source archives.
    /// RunAsync verifies snapshot bytes before using them; explicit analysis discovers game updates.</summary>
    public async Task<GameSession?> LoadSessionAsync(string gameDirectory, IGameAdapter adapter,
        CancellationToken ct = default, string? projectId = null, Action<string, string>? reportProgress = null)
    {
        var gameRoot = Path.GetFullPath(gameDirectory);
        var id = projectId ?? GameId(gameRoot);
        var project = LocalFiles.UnderRoot(Path.Combine(DataDirectory, "games"), id);
        GameSession? NeedsAnalysis(string reason)
        {
            reportProgress?.Invoke("需要重新识别", reason + " 请点击“重新识别”；已保存译文会继续保留。");
            return null;
        }
        if (!Directory.Exists(project)) return NeedsAnalysis("项目尚无已保存的识别结果。");
        await using var projectLock = AcquireProjectLock(project);
        try
        {
            var snapshotPath = Path.Combine(project, "source.json");
            var snapshot = await LocalFiles.ReadJsonAsync<SourceSnapshot>(snapshotPath, ct);
            if (snapshot is null) return NeedsAnalysis("项目缺少已保存的原文快照记录。");
            if (snapshot.AdapterId != adapter.Id || snapshot.AdapterVersion != adapter.Version)
                return NeedsAnalysis("已保存识别结果的 Adapter 或版本与当前项目不一致。");
            if (snapshot.Files is not { Count: > 0 } || string.IsNullOrWhiteSpace(snapshot.Directory))
                return NeedsAnalysis("已保存的原文快照缺少资源目录或文件列表。");
            if (snapshot.Files.Any(file => file is null || string.IsNullOrWhiteSpace(file.RelativePath)
                || file.Sha256 is not { Length: 64 } || file.Sha256.Any(character => !Uri.IsHexDigit(character))))
                return NeedsAnalysis("已保存的原文快照包含无效的资源路径或校验值。");
            var probe = snapshot.Probe;
            if (probe is null)
                return NeedsAnalysis("旧版识别记录缺少引擎和启动信息，需要重新识别补充。");
            var source = LocalFiles.UnderRoot(project, snapshot.Directory);
            var fingerprint = LocalFiles.HashText(JsonSerializer.Serialize(snapshot.Files.OrderBy(file => file.RelativePath)));
            if (Path.GetFileName(source) != fingerprint)
                return NeedsAnalysis("已保存的原文快照目录与资源指纹不一致。");
            var resources = new HashSet<string>(StringComparer.OrdinalIgnoreCase);
            foreach (var file in snapshot.Files)
            {
                ct.ThrowIfCancellationRequested();
                if (!resources.Add(file.RelativePath))
                    return NeedsAnalysis("已保存的原文快照包含重复资源：" + file.RelativePath);
                if (!File.Exists(LocalFiles.UnderRoot(source, file.RelativePath)))
                    return NeedsAnalysis("已保存的原文快照缺少资源：" + file.RelativePath);
            }
            reportProgress?.Invoke("加载已保存文本", "正在读取上次识别结果；游戏更新后可点击“重新识别”。");
            var extraction = await new ExtractionStore(project).LoadAsync(fingerprint, adapter.Id, adapter.Version, ct);
            if (extraction is null || extraction.Units.Count == 0)
                return NeedsAnalysis("与已保存原文快照对应的文本缓存缺失或已损坏。");
            if (probe.AdapterId != adapter.Id || probe.AdapterVersion != adapter.Version
                || probe.RequiredFiles is null || probe.Warnings is null || !resources.SetEquals(probe.RequiredFiles)
                || (probe.OriginalExecutable is { } executable && !resources.Contains(executable)))
                return NeedsAnalysis("已保存的引擎、启动信息或资源列表与原文快照不一致。");
            if (adapter is GeneratedGameAdapter generated) generated.RestoreProbe(probe);
            extraction = extraction with { Warnings = probe.Warnings.Concat(extraction.Warnings).Distinct(StringComparer.Ordinal).ToArray() };
            return new(id, gameRoot, project, source, probe, snapshot.Files, extraction, adapter);
        }
        catch (JsonException)
        {
            return NeedsAnalysis("已保存的识别记录不是有效的 JSON，无法恢复。");
        }
        catch (InvalidDataException error)
        {
            return NeedsAnalysis("已保存的识别记录无效：" + error.Message);
        }
        catch (Exception error) when (error is FileNotFoundException or DirectoryNotFoundException)
        {
            return NeedsAnalysis("已保存的识别记录或文本缓存文件缺失。");
        }
    }

    public async Task<GameSession> AnalyzeAsync(string gameDirectory, IGameAdapter adapter,
        CancellationToken ct = default, string? projectId = null, GameProbe? knownProbe = null,
        Action<string, string>? reportProgress = null, bool commitSnapshot = true)
    {
        var gameRoot = Path.GetFullPath(gameDirectory);
        if (!Directory.Exists(gameRoot)) throw new DirectoryNotFoundException("游戏目录不存在。");
        reportProgress?.Invoke("识别游戏资源", "正在确认引擎和需要处理的资源文件。");
        var probe = knownProbe ?? await adapter.ProbeAsync(gameRoot, ct)
            ?? throw new InvalidDataException("当前 Adapter 不支持此游戏版本。");
        if (probe.AdapterId != adapter.Id || probe.AdapterVersion != adapter.Version)
            throw new InvalidDataException("识别结果与当前 Adapter 不一致，请重新识别游戏。");
        var id = projectId ?? GameId(gameRoot);
        var project = LocalFiles.UnderRoot(Path.Combine(DataDirectory, "games"), id);
        Directory.CreateDirectory(project);
        await using var projectLock = AcquireProjectLock(project);
        var installer = new PatchInstaller(project);
        var sourceFiles = new List<(SourceFile File, string Path)>();
        foreach (var relative in probe.RequiredFiles.Distinct(StringComparer.OrdinalIgnoreCase))
        {
            reportProgress?.Invoke("校验原始资源", $"{sourceFiles.Count + 1} / {probe.RequiredFiles.Count} · {relative}");
            LocalFiles.UnderRoot(gameRoot, relative);
            var sourcePath = await installer.ResolveOriginalFileAsync(gameRoot, relative, ct);
            sourceFiles.Add((new SourceFile(relative, await LocalFiles.HashFileAsync(sourcePath, ct)), sourcePath));
        }
        if (sourceFiles.Count == 0) throw new InvalidDataException("Adapter 未声明需要处理的资源文件。");
        var fingerprint = LocalFiles.HashText(JsonSerializer.Serialize(sourceFiles.Select(f => f.File).OrderBy(f => f.RelativePath)));
        var snapshotDirectory = Path.Combine(project, "sources", fingerprint);
        if (!Directory.Exists(snapshotDirectory))
        {
            var staging = snapshotDirectory + ".pending-" + Guid.NewGuid().ToString("N");
            Directory.CreateDirectory(staging);
            foreach (var (file, sourcePath) in sourceFiles)
            {
                reportProgress?.Invoke("保存原文快照", "正在复制并校验：" + file.RelativePath);
                var target = LocalFiles.UnderRoot(staging, file.RelativePath);
                Directory.CreateDirectory(Path.GetDirectoryName(target)!);
                File.Copy(sourcePath, target, false);
                if (await LocalFiles.HashFileAsync(target, ct) != file.Sha256 ||
                    await LocalFiles.HashFileAsync(sourcePath, ct) != file.Sha256)
                    throw new IOException("扫描期间游戏资源发生变化，请等待游戏更新完成后重试。");
            }
            Directory.Move(staging, snapshotDirectory);
        }
        var files = sourceFiles.Select(f => f.File).ToArray();
        reportProgress?.Invoke("校验原文快照", "正在确认已保存资源完整，文件较大时需要一些时间。");
        await VerifySourcesAsync(snapshotDirectory, files, ct);
        var executableAnalysis = new List<ExecutableAnalysis>();
        foreach (var file in files.Where(file => file.RelativePath.EndsWith(".exe", StringComparison.OrdinalIgnoreCase)))
            executableAnalysis.Add(ExecutableChecks.Analyze(file.RelativePath,
                await File.ReadAllBytesAsync(LocalFiles.UnderRoot(snapshotDirectory, file.RelativePath), ct)));
        await LocalFiles.WriteJsonAsync(Path.Combine(project, "executable-analysis.json"), executableAnalysis, ct);
        var store = new ExtractionStore(project);
        reportProgress?.Invoke("加载文本缓存", "正在读取已有的提取结果。");
        var extraction = await store.LoadAsync(fingerprint, adapter.Id, adapter.Version, ct);
        var cachedExtraction = extraction is not null;
        // Legacy extraction.json has no fingerprint of its own and may belong to an older
        // source snapshot. Only a verified cache can replace extraction from current resources.
        if (extraction is null)
        {
            reportProgress?.Invoke("提取剧情文本", "正在从游戏资源提取对白、名称和选项。");
            extraction = await adapter.ExtractAsync(snapshotDirectory, ct);
        }
        if (extraction.Units.Count == 0) throw new InvalidDataException("未提取到可翻译文本，不能报告为汉化成功。");
        if (extraction.Units.Any(u => string.IsNullOrWhiteSpace(u.Id) || string.IsNullOrWhiteSpace(u.SourceText)) ||
            extraction.Units.Select(u => u.Id).Distinct(StringComparer.Ordinal).Count() != extraction.Units.Count)
            throw new InvalidDataException("提取结果包含空文本或重复的文本 ID。");
        reportProgress?.Invoke("整理项目文本", $"已读取 {extraction.Units.Count:N0} 条文本，正在保存项目记录。");
        if (!cachedExtraction) await store.StoreAsync(fingerprint, adapter.Id, adapter.Version, extraction, ct);
        if (Directory.Exists(Path.Combine(project, "memory")))
        {
            reportProgress?.Invoke("核对已保存译文", "正在核对已提交的翻译记录，已有译文会继续复用。");
            await CleanupCommittedResponsesAsync(project, new TranslationMemory(Path.Combine(project, "memory")), ct);
        }
        // Probe may exclude unsupported files outside the snapshot; keep that coverage visible on cache hits too.
        extraction = extraction with { Warnings = probe.Warnings.Concat(extraction.Warnings).Distinct(StringComparer.Ordinal).ToArray() };
        var session = new GameSession(id, gameRoot, project, snapshotDirectory, probe, files, extraction, adapter);
        ct.ThrowIfCancellationRequested();
        if (commitSnapshot) await CommitAnalysisCoreAsync(session, ct);
        return session;
    }

    /// <summary>Commits a completed analysis after the caller's additional validation has succeeded.</summary>
    public async Task CommitAnalysisAsync(GameSession session, CancellationToken ct = default)
    {
        ct.ThrowIfCancellationRequested();
        await using var projectLock = AcquireProjectLock(session.ProjectDirectory);
        await CommitAnalysisCoreAsync(session, ct);
    }

    private static Task CommitAnalysisCoreAsync(GameSession session, CancellationToken ct)
    {
        ct.ThrowIfCancellationRequested();
        var snapshot = new SourceSnapshot(Path.GetRelativePath(session.ProjectDirectory, session.SourceDirectory),
            session.Probe.AdapterId, session.Probe.AdapterVersion, session.Sources, session.Probe);
        return LocalFiles.WriteJsonAsync(Path.Combine(session.ProjectDirectory, "source.json"), snapshot, ct);
    }

    public async Task<PipelineResult?> LoadLatestResultAsync(string projectDirectory, CancellationToken ct = default)
    {
        var result = await LocalFiles.ReadRecoverableJsonAsync<PipelineResult>(Path.Combine(projectDirectory, "latest-result.json"), ct);
        return result is null ? null : ResolveResult(projectDirectory, result);
    }

    private static PipelineResult ResolveResult(string project, PipelineResult result) => result with
    {
        PatchDirectory = Path.IsPathRooted(result.PatchDirectory) ? result.PatchDirectory : LocalFiles.UnderRoot(project, result.PatchDirectory)
    };

    private static PipelineResult PersistedResult(string project, PipelineResult result) => result with
    {
        PatchDirectory = Path.GetRelativePath(project, result.PatchDirectory)
    };

    public Task<PipelineResult> RunAsync(GameSession session, ITranslationProvider provider,
        TranslationPolicy policy, RunOptions? options = null, IProgress<PipelineProgress>? progress = null,
        CancellationToken ct = default) => RunAsync(session, new[] { provider }, policy, options, progress, ct);

    public async Task<PipelineResult> RunAsync(GameSession session, IReadOnlyList<ITranslationProvider> translationProviders,
        TranslationPolicy policy, RunOptions? options = null, IProgress<PipelineProgress>? progress = null,
        CancellationToken ct = default) =>
        (await RunCoreAsync(session, translationProviders, policy, options, progress, true, ct)).Patch!;

    public async Task<TranslationRunSummary> TranslateOnlyAsync(GameSession session, IReadOnlyList<ITranslationProvider> providers,
        TranslationPolicy policy, RunOptions? options = null, IProgress<PipelineProgress>? progress = null,
        CancellationToken ct = default) => (await RunCoreAsync(session, providers, policy, options, progress, false, ct)).Summary;

    private sealed record TranslationRunOutcome(PipelineResult? Patch, TranslationRunSummary Summary);

    private async Task<TranslationRunOutcome> RunCoreAsync(GameSession session, IReadOnlyList<ITranslationProvider> translationProviders,
        TranslationPolicy policy, RunOptions? options, IProgress<PipelineProgress>? progress, bool buildPatch,
        CancellationToken ct)
    {
        ArgumentNullException.ThrowIfNull(translationProviders);
        var providers = translationProviders.ToArray();
        if (providers.Length == 0 || providers.Any(provider => provider is null))
            throw new ArgumentException("翻译 API 链至少需要一个有效配置。", nameof(translationProviders));
        options ??= new();
        if (options.BatchSize is < 1 or > 80) throw new ArgumentOutOfRangeException(nameof(options), "每批文本数必须在 1 到 80 之间。");
        if (providers.Any(provider => provider.MaximumBatchSize < 1))
            throw new ArgumentException("翻译 API 的批次上限必须大于零。", nameof(translationProviders));
        var batchSize = Math.Min(options.BatchSize, providers.Min(provider => provider.MaximumBatchSize));
        var retranslating = options.RetranslationId is not null;
        if (retranslating && (options.CandidateOnly || buildPatch || options.TrialUnitIds is null
            || !Guid.TryParseExact(options.RetranslationId, "N", out _)))
            throw new ArgumentException("重新翻译必须指定有效任务标识和文本选区。", nameof(options));
        if (string.IsNullOrWhiteSpace(policy.TargetLanguage)) throw new ArgumentException("目标语言不能为空。", nameof(policy));
        var allUnits = session.Extraction.Units;
        var trialIds = options.TrialUnitIds?.ToHashSet(StringComparer.Ordinal);
        IReadOnlyList<TranslationUnit> units = trialIds is null ? allUnits : allUnits.Where(u => trialIds.Contains(u.Id)).ToArray();
        if (trialIds is not null && (trialIds.Count == 0 || units.Count != trialIds.Count))
            throw new ArgumentException("请重新选择试译文本：选区为空或包含不属于当前游戏的文本。", nameof(options));
        var isTrial = trialIds is not null;
        var continueOnTextError = !isTrial && !options.CandidateOnly;
        Exception? firstBatchError = null;
        var failedIds = new HashSet<string>(StringComparer.Ordinal);
        await using var projectLock = AcquireProjectLock(session.ProjectDirectory);
        var memory = new TranslationMemory(Path.Combine(session.ProjectDirectory, "memory"), Path.Combine(DataDirectory, "memory", session.Id));
        var completed = new Dictionary<string, string>(StringComparer.Ordinal);
        var cacheHits = 0;
        long inputTokens = 0, outputTokens = 0;
        var estimated = false;
        var stage = "准备";
        var runId = Guid.NewGuid().ToString("N");
        var taskDirectory = Path.Combine(session.ProjectDirectory, "tasks", runId);
        Directory.CreateDirectory(taskDirectory);
        var logDirectory = diagnosticDirectory ?? Path.Combine(session.ProjectDirectory, "logs");
        var started = DateTimeOffset.UtcNow;
        var responseDirectory = Path.Combine(session.ProjectDirectory, "responses");
        Directory.CreateDirectory(responseDirectory);
        var validationFeedbackPath = Path.Combine(session.ProjectDirectory, "translation-errors.json");
        var previousIssues = new List<RecordedTranslationIssue>();
        async Task ReportAsync(string nextStage, string message, CancellationToken token = default)
        {
            stage = nextStage;
            var update = new PipelineProgress(stage, completed.Count, units.Count, cacheHits,
                inputTokens + outputTokens, message, failedIds.Count);
            var state = new
            {
                runId, started, updatedAt = DateTimeOffset.UtcNow, session.GameDirectory,
                adapterId = session.Adapter.Id, adapterVersion = session.Adapter.Version,
                sourceHashes = session.Sources, isTrial, trialUnitIds = trialIds, progress = update
            };
            await LocalFiles.WriteJsonAsync(Path.Combine(taskDirectory, "job.json"), state, token);
            await LocalFiles.WriteJsonAsync(Path.Combine(session.ProjectDirectory, "job.json"), state, token);
            progress?.Report(update);
        }
        async Task MarkBatchFailedAsync(IEnumerable<TranslationUnit> failedUnits, Exception error, string code, string reason)
        {
            ct.ThrowIfCancellationRequested();
            firstBatchError ??= error;
            var failed = failedUnits.ToArray();
            var ids = failed.Select(unit => unit.Id).ToHashSet(StringComparer.Ordinal);
            previousIssues.RemoveAll(issue => issue.UnitId is { } id && ids.Contains(id));
            previousIssues.AddRange(failed.Select(unit => new RecordedTranslationIssue(code, reason, unit.Id, memory.Key(unit, policy))));
            failedIds.UnionWith(ids);
            await LocalFiles.WriteJsonAsync(validationFeedbackPath, previousIssues, CancellationToken.None);
            options.FailureUpdates?.Report(failed.ToDictionary(unit => unit.Id, _ => (string?)reason, StringComparer.Ordinal));
            await ReportAsync("翻译", $"{failed.Length} 条文本翻译失败：{reason} 已标记失败，继续后续文本。", ct);
        }

        string ApiLabel(int index) => providers[index].DisplayName is { Length: > 0 } name
            ? $"{name}({index + 1}/{providers.Length})"
            : $"第 {index + 1} / {providers.Length} 个翻译 API";

        async Task<(TranslationResponse Response, ITranslationProvider Provider, string ResponsePath, string IntentPath)>
            TranslateBatchAsync(TranslationUnit[] representatives)
        {
            var feedback = representatives.Select(unit => new
            {
                unit.Id,
                Messages = (IReadOnlyList<string>)previousIssues.Where(issue => issue.UnitId == unit.Id
                    && (issue.MemoryKey is null || issue.MemoryKey == memory.Key(unit, policy))
                    && issue.Code is not ("provider-refusal" or "provider-request-failed" or "request-outcome-unknown"))
                    .Select(issue => issue.Message).Distinct(StringComparer.Ordinal).ToArray()
            }).Where(item => item.Messages.Count > 0).ToDictionary(item => item.Id, item => item.Messages, StringComparer.Ordinal);
            var request = new TranslationRequest(representatives, policy.TargetLanguage, policy.Terminology, policy.Style)
            { ValidationFeedback = feedback.Count > 0 ? feedback : null };
            var anyNewRefusal = false;
            for (var index = 0; index < providers.Length; index++)
            {
                ct.ThrowIfCancellationRequested();
                var provider = providers[index];
                var providerLabel = ApiLabel(index);
                var requestKey = LocalFiles.HashText(JsonSerializer.Serialize(new
                {
                    Provider = provider.Identity,
                    Units = representatives.Select(u => new { u.Id, Key = memory.Key(u, policy) })
                }) + (options.CandidateOnly ? "\ncandidate" : "")
                    + (retranslating ? "\nretranslate:" + options.RetranslationId : ""));
                var responsePath = Path.Combine(responseDirectory, requestKey + ".json");
                var intentPath = Path.Combine(responseDirectory, requestKey + ".request.json");
                var intent = new TranslationIntent(requestKey, provider.Identity, DateTimeOffset.UtcNow,
                    representatives.Select(u => new PendingTranslation(u.Id, memory.Key(u, policy))).ToArray(), options.CandidateOnly, options.RetranslationId);
                var response = await LocalFiles.ReadRecoverableJsonAsync<TranslationResponse>(responsePath, ct);
                if (response is not null) return (response, provider, responsePath, intentPath);
                var recordedRefusal = await FindRecordedRefusalAsync(responseDirectory, intent, ct);
                if (recordedRefusal is null)
                {
                    if (File.Exists(intentPath) && !options.RetryUncertainRequests)
                        throw new UncertainRequestException("上次翻译请求未记录到响应，服务商可能已处理或计费。请确认是否重新发送这批文本；已保存译文不会重发。");
                    await ReportAsync("翻译", $"正在使用 {providerLabel} 翻译 {representatives.Length} 条新文本。", ct);
                    await LocalFiles.WriteJsonAsync(intentPath, intent, ct);
                    const int maximumRetries = 5;
                    for (var attempt = 0; ; attempt++)
                    {
                        ct.ThrowIfCancellationRequested();
                        try
                        {
                            response = await provider.TranslateAsync(request, ct);
                            break;
                        }
                        catch (TranslationProviderException error)
                        {
                            if (error.Usage is not null)
                            {
                                inputTokens += error.Usage.InputTokens; outputTokens += error.Usage.OutputTokens;
                                estimated |= error.Usage.Estimated;
                            }
                            await LocalFiles.WriteJsonAsync(Path.Combine(responseDirectory, requestKey + ".error-" + Guid.NewGuid().ToString("N") + ".json"),
                                new { attempt = attempt + 1, error.Message, error.FailureSummary, error.Usage, error.OutcomeUnknown, error.StatusCode, error.Diagnostic,
                                    failedAt = DateTimeOffset.UtcNow }, CancellationToken.None);
                            if (error.IsModelRefusal)
                            {
                                await LocalFiles.WriteJsonAsync(Path.Combine(responseDirectory, requestKey + ".refusal.json"),
                                    new TranslationRefusal(intent, DateTimeOffset.UtcNow, error.Diagnostic), CancellationToken.None);
                                DeleteReceipt(intentPath);
                                anyNewRefusal = true;
                                break;
                            }
                            // Operational and response-format errors retry this provider only.
                            if (attempt == maximumRetries)
                            {
                                if (!error.OutcomeUnknown) DeleteReceipt(intentPath);
                                throw new TranslationRetriesExhaustedException(error);
                            }
                            await ReportAsync("等待重试", $"{providerLabel} 请求失败：{error.FailureSummary}。10 秒后进行第 {attempt + 1} / {maximumRetries} 次重试。", ct);
                            await Task.Delay(TimeSpan.FromSeconds(10), ct);
                            await ReportAsync("翻译", $"正在对 {providerLabel} 进行第 {attempt + 1} / {maximumRetries} 次重试。", ct);
                        }
                    }
                    if (response is not null)
                    {
                        // Persist before committing entries so a restart can recover without another API call.
                        await LocalFiles.WriteJsonAsync(responsePath, response, CancellationToken.None);
                        return (response, provider, responsePath, intentPath);
                    }
                }
                if (index + 1 < providers.Length)
                    await ReportAsync("切换翻译 API", $"{providerLabel} " +
                        (recordedRefusal is null ? "拒绝处理当前内容" : "已有当前内容的拒绝记录，已跳过") +
                        $"，正在交给 {ApiLabel(index + 1)}。", ct);
            }
            throw new TranslationRefusedException(alreadyRecorded: !anyNewRefusal);
        }

        try
        {
            await VerifySourcesAsync(session.SourceDirectory, session.Sources, ct);
            await memory.InitializeAsync(ct);
            previousIssues = await ReadTranslationIssuesAsync(validationFeedbackPath, ct);
            if (isTrial && !options.CandidateOnly)
                await LocalFiles.WriteJsonAsync(Path.Combine(session.ProjectDirectory, "trial-selection.json"), units.Select(u => u.Id).ToArray(), ct);
            await ReportAsync("查询缓存", "正在加载历史译文。", ct);
            if (!options.CandidateOnly && !retranslating)
                await RecoverResponsesAsync(responseDirectory, memory, units, policy, session.Adapter, ct);
            var edits = await TranslationEdits.LoadAsync(session.ProjectDirectory, ct);
            await CleanupCommittedResponsesAsync(session.ProjectDirectory, memory, ct);
            foreach (var unit in units)
            {
                ct.ThrowIfCancellationRequested();
                if (options.CandidateOnly) continue;
                var edit = edits.Find(unit, policy.TargetLanguage);
                if (retranslating && edit?.RetranslationId != options.RetranslationId) continue;
                var translation = edit?.Text ?? await memory.FindAsync(unit, policy, ct);
                // Unfinished manual corrections are protected from model replacement too.
                if (translation is not null && ((!retranslating && edit is not null) || ValidateTranslation(session.Adapter, unit, translation).Count == 0))
                {
                    completed.Add(unit.Id, translation);
                    cacheHits++;
                }
            }
            if (previousIssues.RemoveAll(issue => issue.UnitId is { } id && completed.ContainsKey(id)) > 0)
                await LocalFiles.WriteJsonAsync(validationFeedbackPath, previousIssues, CancellationToken.None);
            options.FailureUpdates?.Report(completed.Keys.ToDictionary(id => id, _ => (string?)null, StringComparer.Ordinal));
            options.TranslationUpdates?.Report(units.Where(unit => completed.ContainsKey(unit.Id)
                && (retranslating || edits.Find(unit, policy.TargetLanguage) is null)).ToDictionary(unit => unit.Id, unit => completed[unit.Id]));
            await ReportAsync("查询缓存", retranslating ? $"重新翻译所选 {units.Count} 条文本；本次任务已完成 {cacheHits} 条，其余将重新请求 API。"
                : $"已加载 {cacheHits} 条历史译文。", ct);
            if (batchSize < options.BatchSize)
                await ReportAsync("翻译", $"根据翻译 API 支持的输入格式，每次提交 {batchSize} 条文本，成功后立即保存。", ct);
            var groups = units.Where(u => !completed.ContainsKey(u.Id)).GroupBy(u => memory.Key(u, policy))
                .Select(g => g.ToArray()).ToArray();
            if (!options.RetryUncertainRequests)
            {
                var missingKeys = groups.Select(g => memory.Key(g[0], policy)).ToHashSet(StringComparer.Ordinal);
                var uncertainKeys = new HashSet<string>(StringComparer.Ordinal);
                foreach (var intentFile in Directory.EnumerateFiles(responseDirectory, "*.request.json"))
                {
                    var intent = await LocalFiles.ReadJsonAsync<TranslationIntent>(intentFile, ct);
                    var receipt = intentFile[..^".request.json".Length] + ".json";
                    if (!File.Exists(receipt) && !File.Exists(intentFile[..^".request.json".Length] + ".refusal.json") &&
                        intent?.CandidateOnly == options.CandidateOnly && intent.RetranslationId == options.RetranslationId
                        && intent.Units?.Any(u => missingKeys.Contains(u.MemoryKey)) == true)
                    {
                        if (!continueOnTextError)
                            throw new UncertainRequestException("上次请求的部分文本没有响应记录，服务商可能已处理或计费。请确认是否重新发送；调整批大小也不会自动重发这些文本。");
                        uncertainKeys.UnionWith(intent.Units.Where(unit => missingKeys.Contains(unit.MemoryKey)).Select(unit => unit.MemoryKey));
                    }
                }
                if (uncertainKeys.Count > 0)
                {
                    const string reason = "上次请求结果未知，未授权重新发送；请确认后重试。";
                    await MarkBatchFailedAsync(groups.Where(group => uncertainKeys.Contains(memory.Key(group[0], policy))).SelectMany(group => group),
                        new UncertainRequestException(reason), "request-outcome-unknown", reason);
                    groups = groups.Where(group => !uncertainKeys.Contains(memory.Key(group[0], policy))).ToArray();
                }
            }
            foreach (var plannedBatch in Batch(groups, batchSize))
            {
                ct.ThrowIfCancellationRequested();
                var batch = plannedBatch;
                if (!options.CandidateOnly && !retranslating)
                {
                    edits = await TranslationEdits.LoadAsync(session.ProjectDirectory, ct);
                    batch = plannedBatch.Select(group => group.Where(unit =>
                    {
                        if (edits.Find(unit, policy.TargetLanguage) is not { } edit) return true;
                        completed[unit.Id] = edit.Text; cacheHits++; return false;
                    }).ToArray()).Where(group => group.Length > 0).ToArray();
                    if (batch.Length == 0) continue;
                }
                var representatives = batch.Select(g => g[0]).ToArray();
                TranslationResponse response;
                ITranslationProvider provider;
                string responsePath, intentPath;
                try { (response, provider, responsePath, intentPath) = await TranslateBatchAsync(representatives); }
                catch (Exception error) when (continueOnTextError && error is (TranslationRefusedException or TranslationRetriesExhaustedException))
                {
                    var code = error is TranslationRefusedException ? "provider-refusal" : "provider-request-failed";
                    var reason = error is TranslationRetriesExhaustedException { InnerException: TranslationProviderException failure }
                        ? "API 重试后仍失败：" + failure.FailureSummary : "翻译 API 链均拒绝处理此批内容。";
                    await MarkBatchFailedAsync(batch.SelectMany(group => group), error, code, reason);
                    continue;
                }
                inputTokens += response.Usage.InputTokens;
                outputTokens += response.Usage.OutputTokens;
                estimated |= response.Usage.Estimated;
                var expected = representatives.Select(u => u.Id).ToHashSet(StringComparer.Ordinal);
                var issues = new List<ValidationIssue>();
                if (response.Translations.Keys.Any(id => !expected.Contains(id)))
                    issues.Add(new("unexpected-unit", "模型返回了请求之外的文本 ID。"));
                var validEntries = new List<MemoryEntry>();
                foreach (var group in batch)
                {
                    var representative = group[0];
                    if (!response.Translations.TryGetValue(representative.Id, out var translation))
                    {
                        issues.Add(new("missing-unit", "模型遗漏文本。", representative.Id));
                        continue;
                    }
                    var validation = ValidateTranslation(session.Adapter, representative, translation);
                    if (validation.Count != 0) { issues.AddRange(validation.Select(issue => issue with { UnitId = representative.Id })); continue; }
                    validEntries.Add(new(memory.Key(representative, policy), representative.SourceText, representative.Context,
                        translation, provider.Identity, DateTimeOffset.UtcNow));
                    foreach (var unit in group) completed[unit.Id] = translation;
                    cacheHits += group.Length - 1;
                }
                if (retranslating && validEntries.Count > 0)
                {
                    // Selected units can share a memory key with unselected units. Store accepted
                    // retranslations by unit ID, leaving unrelated texts and old drafts intact.
                    edits = await TranslationEdits.LoadAsync(session.ProjectDirectory, CancellationToken.None);
                    var replacements = new Dictionary<string, TranslationEdit>(edits.Entries, StringComparer.Ordinal);
                    var candidates = new Dictionary<string, TranslationEdit>(edits.Candidates ?? new Dictionary<string, TranslationEdit>(), StringComparer.Ordinal);
                    foreach (var unit in batch.SelectMany(group => group).Where(unit => completed.ContainsKey(unit.Id)))
                    {
                        replacements[unit.Id] = new(TranslationEdits.Signature(unit), policy.TargetLanguage,
                            completed[unit.Id], false, DateTimeOffset.UtcNow, options.RetranslationId, IsGenerated: true);
                        candidates.Remove(unit.Id);
                    }
                    await new TranslationEdits(1, replacements, candidates).SaveAsync(session.ProjectDirectory, CancellationToken.None);
                }
                else if (!options.CandidateOnly && !retranslating) await memory.StoreBatchAsync(validEntries, CancellationToken.None);
                options.TranslationUpdates?.Report(batch.SelectMany(group => group)
                    .Where(unit => completed.ContainsKey(unit.Id)).ToDictionary(unit => unit.Id, unit => completed[unit.Id]));
                var batchIds = batch.SelectMany(group => group).Select(unit => unit.Id).ToHashSet(StringComparer.Ordinal);
                var feedbackChanged = previousIssues.RemoveAll(issue => issue.UnitId is { } id && batchIds.Contains(id)) > 0;
                foreach (var group in batch)
                    foreach (var issue in issues.Where(issue => issue.UnitId == group[0].Id))
                        previousIssues.AddRange(group.Select(unit => new RecordedTranslationIssue(issue.Code, issue.Message, unit.Id, memory.Key(unit, policy))));
                previousIssues.AddRange(issues.Where(issue => issue.UnitId is null)
                    .Select(issue => new RecordedTranslationIssue(issue.Code, issue.Message, null)));
                var failureUpdates = batch.SelectMany(group => group).ToDictionary(unit => unit.Id,
                    unit => completed.ContainsKey(unit.Id) ? null : previousIssues.FirstOrDefault(issue => issue.UnitId == unit.Id)?.Message,
                    StringComparer.Ordinal);
                failedIds.UnionWith(batchIds.Where(id => !completed.ContainsKey(id)));
                options.FailureUpdates?.Report(failureUpdates);
                if (feedbackChanged || issues.Count > 0)
                    await LocalFiles.WriteJsonAsync(validationFeedbackPath, previousIssues, CancellationToken.None);
                if (issues.Count != 0)
                {
                    if (logDirectory is not null)
                    {
                        try
                        {
                            await LocalFiles.WriteJsonAsync(Path.Combine(logDirectory, $"validation-{DateTime.UtcNow:yyyyMMdd-HHmmssfff}-{Guid.NewGuid():N}.json"),
                                new
                                {
                                    timestamp = DateTimeOffset.UtcNow, session.Id, adapterId = session.Adapter.Id, adapterVersion = session.Adapter.Version,
                                    issues,
                                    texts = representatives.Where(unit => issues.Any(issue => issue.UnitId == unit.Id)).Select(unit => new
                                    {
                                        unit.Id, unit.ResourcePath, unit.Locator, unit.SourceText, translation = response.Translations.GetValueOrDefault(unit.Id)
                                    })
                                }, CancellationToken.None);
                        }
                        catch (Exception logError) when (logError is IOException or UnauthorizedAccessException)
                        {
                            // The project's mandatory validation record above remains available if portable logs are unwritable.
                            System.Diagnostics.Debug.WriteLine("Could not write validation diagnostic log: " + logError.GetType().Name);
                        }
                    }
                    // Valid entries survive; the failed subset receives a new batch key on retry.
                    File.Move(responsePath, responsePath + ".invalid-" + Guid.NewGuid().ToString("N"));
                    DeleteReceipt(responsePath);
                    DeleteReceipt(intentPath);
                    int failedUnits = issues.Select(issue => issue.UnitId).Distinct().Count();
                    var first = issues[0];
                    var failedUnit = representatives.FirstOrDefault(unit => unit.Id == first.UnitId);
                    string location = failedUnit is null ? "" : $"{failedUnit.ResourcePath}（{failedUnit.Locator}）：";
                    string reason = first.Code switch
                    {
                        "unsupported-character" or "empty-translation" or "nul-character" or "unexpected-newline" or "placeholder-mismatch" => first.Message,
                        "missing-unit" => "模型遗漏了文本。",
                        "script-text-invalid" => "译文不符合脚本规则，请检查控制标记、括号、引号或换行。",
                        _ => "译文不符合引擎格式要求，详情见 logs 日志。"
                    };
                    var error = new InvalidDataException($"{failedUnits} 条译文未通过格式检查：{location}{reason} 有效译文已保存，继续时只重试失败文本。");
                    if (!continueOnTextError) throw error;
                    // Unexpected extra response IDs do not invalidate correctly matched translations.
                    if (batchIds.Any(id => !completed.ContainsKey(id))) firstBatchError ??= error;
                    var failures = batchIds.Count(id => !completed.ContainsKey(id));
                    await ReportAsync("翻译", failures > 0
                        ? $"本批 {failures} 条文本翻译失败；有效译文已保存，继续后续文本。{location}{reason}"
                        : "本批译文已保存，模型返回的多余文本已忽略，继续后续文本。", ct);
                }
                await ReportAsync("翻译", $"已完成 {completed.Count} / {units.Count} 条文本" + (failedIds.Count > 0 ? $"，失败 {failedIds.Count} 条。" : "。"), ct);
                // Translations and usage are durable. Receipts are only needed until this point.
                DeleteReceipt(responsePath);
                DeleteReceipt(intentPath);
            }
            if (continueOnTextError && failedIds.Count > 0)
            {
                // A user can correct an earlier failed unit while later batches are running.
                edits = await TranslationEdits.LoadAsync(session.ProjectDirectory, ct);
                var resolved = new Dictionary<string, string?>();
                foreach (var unit in units.Where(unit => failedIds.Contains(unit.Id)))
                    if (edits.Find(unit, policy.TargetLanguage) is { } edit && ValidateTranslation(session.Adapter, unit, edit.Text).Count == 0)
                    {
                        completed[unit.Id] = edit.Text;
                        failedIds.Remove(unit.Id);
                        resolved[unit.Id] = null;
                    }
                if (resolved.Count > 0)
                {
                    previousIssues.RemoveAll(issue => issue.UnitId is { } id && resolved.ContainsKey(id));
                    await LocalFiles.WriteJsonAsync(validationFeedbackPath, previousIssues, CancellationToken.None);
                    options.FailureUpdates?.Report(resolved);
                }
            }
            var summary = new TranslationRunSummary(units.Count, completed.Count, failedIds.Count);
            if (!buildPatch)
            {
                if (summary.Failed > 0)
                    await ReportAsync("翻译完成（有失败）", $"已处理全部文本：成功 {summary.Completed} 条，失败 {summary.Failed} 条。可筛选“翻译失败”查看原因，继续任务时复用已成功译文。", ct);
                else await ReportAsync("翻译完成", retranslating ? "所选文本已重新翻译并保存，可以继续校对或生成补丁。"
                    : options.CandidateOnly ? "候选译文已生成，采用后才会替换当前译文。"
                    : "本次翻译已保存，可以继续校对或生成补丁。", ct);
                return new(null, summary);
            }
            // Automatic patch workflows must not silently publish an incomplete full translation.
            // All remaining batches have been attempted before reporting the first failure.
            if (failedIds.Count > 0 && firstBatchError is not null) System.Runtime.ExceptionServices.ExceptionDispatchInfo.Capture(firstBatchError).Throw();
            edits = await TranslationEdits.LoadAsync(session.ProjectDirectory, ct);
            foreach (var unit in units)
                if (edits.Find(unit, policy.TargetLanguage) is { } edit) completed[unit.Id] = edit.Text;
            var result = await BuildSnapshotAsync(session, completed, cacheHits, new(inputTokens, outputTokens, estimated), progress, ct);
            await ReportAsync(result.IsTrial ? "试译完成" : "完成", "补丁已生成并通过结构校验，可安装后进入游戏验证。", ct);
            return new(result, summary);

        }
        catch (OperationCanceledException)
        {
            await ReportAsync("已暂停", "已提交译文已保存。再次开始可继续处理。");
            throw;
        }
        catch (TranslationRefusedException error)
        {
            await ReportAsync("已暂停", error.Message);
            throw;
        }
        catch (TranslationRetriesExhaustedException error)
        {
            await ReportAsync("已暂停", error.Message + (isTrial
                ? " 再次打开“选择文本试译”会保留本次选区。"
                : " 点击“开始 / 继续全量翻译”可继续任务。"));
            throw;
        }
        catch (Exception error)
        {
            await ReportAsync("失败", error.Message);
            throw;
        }
    }

    private sealed record RecordedTranslationIssue(string Code, string Message, string? UnitId, string? MemoryKey = null);

    private static async Task<List<RecordedTranslationIssue>> ReadTranslationIssuesAsync(string path, CancellationToken ct)
    {
        try
        {
            var issues = await LocalFiles.ReadJsonAsync<List<RecordedTranslationIssue>>(path, ct);
            return issues?.Where(issue => issue is not null && !string.IsNullOrWhiteSpace(issue.Message)).ToList() ?? [];
        }
        catch (Exception error) when (error is IOException or UnauthorizedAccessException or JsonException)
        {
            // Optional retry hints must not prevent translating from the source and committed memory.
            System.Diagnostics.Debug.WriteLine("Could not read translation feedback: " + error.GetType().Name);
            return [];
        }
    }

    private static async Task<TranslationRefusal?> FindRecordedRefusalAsync(string directory, TranslationIntent request, CancellationToken ct)
    {
        string path = Path.Combine(directory, request.RequestKey + ".refusal.json");
        var record = await LocalFiles.ReadJsonAsync<TranslationRefusal>(path, ct);
        if (record is not null)
        {
            if (record.Request.RequestKey != request.RequestKey || record.Request.Provider != request.Provider ||
                !record.Request.Units.SequenceEqual(request.Units))
                throw new InvalidDataException("拒绝记录与当前请求不一致，请保留记录后检查任务数据。");
            return record;
        }
        // Older versions recorded plain-text/error-envelope refusals as JSON failures.
        // The request hash binds provider, unit IDs, source, context and translation policy.
        foreach (var file in new DirectoryInfo(directory).EnumerateFiles(request.RequestKey + ".error-*.json").OrderByDescending(file => file.LastWriteTimeUtc))
        {
            var failure = await LocalFiles.ReadJsonAsync<RecordedProviderFailure>(file.FullName, ct);
            if (!ModelRefusal.IsRecordedRefusal(failure?.Diagnostic, request.Units.Select(unit => unit.Id))) continue;
            record = new TranslationRefusal(request, DateTimeOffset.UtcNow, failure!.Diagnostic! with
            {
                Code = "model-refusal", Detail = "Recognized refusal from a previously recorded failed response."
            });
            await LocalFiles.WriteJsonAsync(path, record, CancellationToken.None);
            return record;
        }
        return null;
    }

    private sealed record RecordedProviderFailure(ProviderResponseDiagnostic? Diagnostic);

    private static async Task<ValidationResult> ValidateExecutablesAsync(GameSession session, string directory,
        IReadOnlyList<string> files, CancellationToken ct)
    {
        var issues = new List<ValidationIssue>();
        var reports = new List<object>();
        foreach (var file in files.Where(file => file.EndsWith(".exe", StringComparison.OrdinalIgnoreCase)))
        {
            var source = session.Sources.FirstOrDefault(source => source.RelativePath.Equals(file, StringComparison.OrdinalIgnoreCase))?.RelativePath;
            if (source is null && session.Probe.OriginalExecutable is { } originalName &&
                Path.GetFileNameWithoutExtension(file).StartsWith(Path.GetFileNameWithoutExtension(originalName) + ".", StringComparison.OrdinalIgnoreCase) &&
                session.Sources.Any(source => source.RelativePath.Equals(originalName, StringComparison.OrdinalIgnoreCase)))
                source = originalName;
            var original = source is null ? null : await File.ReadAllBytesAsync(LocalFiles.UnderRoot(session.SourceDirectory, source), ct);
            var output = await File.ReadAllBytesAsync(LocalFiles.UnderRoot(directory, file), ct);
            var validation = ExecutableChecks.Validate(original, output);
            issues.AddRange(validation.Issues.Select(issue => issue with { Message = file + ": " + issue.Message }));
            reports.Add(new { file, originalFile = source, validation, coverage = source is null
                ? "PE structure only; no matching original executable" : "PE structure and native x86 relocation hazards" });
        }
        if (reports.Count != 0)
            await LocalFiles.WriteJsonAsync(Path.Combine(directory, "executable-validation.json"), reports, ct);
        return new(issues.Count == 0, issues);
    }

    private Task RecordCompatibilityAsync(GameSession session, PipelineResult result, CancellationToken ct)
    {
        if (session.Adapter.CatalogEntry is not { } entry) return Task.CompletedTask;
        return new LocalAdapterPackageStore(Path.Combine(DataDirectory, "extensions")).RecordCompatibilityAsync(entry.Family,
            new GameCompatibility(session.Id, Path.GetFileName(Path.TrimEndingDirectorySeparator(session.GameDirectory)),
                result.IsTrial ? "已生成试译产物" : "已生成汉化产物", session.Adapter.Version, session.Sources.ToDictionary(file => file.RelativePath, file => file.Sha256),
                "patch-validated", result.Validation.RuntimeStatus, result.PatchDirectory), ct);
    }

    private static async Task RecoverResponsesAsync(string directory, TranslationMemory memory,
        IReadOnlyList<TranslationUnit> units, TranslationPolicy policy, IGameAdapter adapter, CancellationToken ct)
    {
        var byKey = new Dictionary<string, TranslationUnit>(StringComparer.Ordinal);
        foreach (var unit in units)
            if (await memory.FindAsync(unit, policy, ct) is null) byKey.TryAdd(memory.Key(unit, policy), unit);
        if (byKey.Count == 0) return;
        var recovered = new Dictionary<string, MemoryEntry>(StringComparer.Ordinal);
        foreach (var intentPath in Directory.EnumerateFiles(directory, "*.request.json"))
        {
            var intent = await LocalFiles.ReadRecoverableJsonAsync<TranslationIntent>(intentPath, ct);
            if (intent?.Units is null || intent.CandidateOnly || intent.RetranslationId is not null) continue;
            if (!intent.Units.Any(unit => byKey.ContainsKey(unit.MemoryKey))) continue;
            var responsePath = intentPath[..^".request.json".Length] + ".json";
            var response = await LocalFiles.ReadRecoverableJsonAsync<TranslationResponse>(responsePath, ct);
            if (response is null) continue;
            foreach (var requested in intent.Units)
            {
                if (!byKey.TryGetValue(requested.MemoryKey, out var current) ||
                    !response.Translations.TryGetValue(requested.Id, out var translated) ||
                    ValidateTranslation(adapter, current, translated).Count != 0) continue;
                recovered[requested.MemoryKey] = new(requested.MemoryKey, current.SourceText, current.Context,
                    translated, intent.Provider, DateTimeOffset.UtcNow);
            }
        }
        await memory.StoreBatchAsync(recovered.Values, ct);
    }

    public static async Task CleanupCommittedResponsesAsync(string projectDirectory, TranslationMemory memory, CancellationToken ct = default)
    {
        try
        {
            await memory.InitializeAsync(ct);
            foreach (var path in await FindCommittedResponseFilesAsync(Path.Combine(projectDirectory, "responses"), memory.Entries, ct))
                DeleteReceipt(path);
        }
        catch (Exception error) when (error is IOException or UnauthorizedAccessException or JsonException or InvalidDataException)
        { System.Diagnostics.Debug.WriteLine("Could not clean translation receipts: " + error.Message); }
    }

    internal static async Task<IReadOnlyList<string>> FindCommittedResponseFilesAsync(string directory,
        IReadOnlyDictionary<string, MemoryEntry> entries, CancellationToken ct)
    {
        var files = new List<string>();
        if (!Directory.Exists(directory)) return files;
        foreach (var intentPath in Directory.EnumerateFiles(directory, "*.request.json"))
        {
            ct.ThrowIfCancellationRequested();
            try
            {
                var key = Path.GetFileName(intentPath)[..^".request.json".Length];
                if (key.Length != 64 || !key.All(Uri.IsHexDigit)) continue;
                var intent = await LocalFiles.ReadJsonAsync<TranslationIntent>(intentPath, ct);
                if (intent?.RequestKey != key || intent.CandidateOnly || intent.RetranslationId is not null || intent.Units is not { Count: > 0 } ||
                    intent.Units.Any(unit => unit is null || unit.Id is null || unit.MemoryKey is null || !entries.ContainsKey(unit.MemoryKey))) continue;
                var responsePath = Path.Combine(directory, key + ".json");
                var response = await LocalFiles.ReadJsonAsync<TranslationResponse>(responsePath, ct);
                // Only discard confirmed successes. Failed, incomplete or unrecognizable records stay available.
                if (response?.Translations is null || response.Translations.Count != intent.Units.Count ||
                    intent.Units.Select(unit => unit.Id).Distinct(StringComparer.Ordinal).Count() != intent.Units.Count ||
                    intent.Units.Any(unit => !response.Translations.TryGetValue(unit.Id, out var text) ||
                        text != entries[unit.MemoryKey].Translation)) continue;
                files.Add(responsePath);
                files.Add(intentPath);
            }
            catch (Exception error) when (error is IOException or UnauthorizedAccessException or JsonException)
            {
                System.Diagnostics.Debug.WriteLine("Could not inspect translation receipt: " + error.Message);
            }
        }
        return files;
    }

    private static void DeleteReceipt(string path)
    {
        // An old recovery copy must not resurrect a completed request. Failure to tidy up
        // does not invalidate committed translations; the next run can try again.
        foreach (var candidate in new[] { path + ".bak", path })
            try { File.Delete(candidate); }
            catch (Exception error) when (error is IOException or UnauthorizedAccessException)
            { System.Diagnostics.Debug.WriteLine("Could not remove translation receipt: " + error.Message); }
    }

    private static IReadOnlyList<ValidationIssue> ValidateTranslation(IGameAdapter adapter, TranslationUnit unit, string text) =>
        TextValidation.Validate(unit, text).Concat(adapter.ValidateTranslation(unit, text)).ToArray();

    private static IEnumerable<TranslationUnit[][]> Batch(TranslationUnit[][] groups, int maximum)
    {
        var batch = new List<TranslationUnit[]>();
        var characters = 0;
        foreach (var group in groups)
        {
            var length = group[0].SourceText.Length + group[0].Context.Length;
            if (batch.Count != 0 && (batch.Count >= maximum || characters + length > 12_000))
            { yield return batch.ToArray(); batch.Clear(); characters = 0; }
            batch.Add(group); characters += length;
        }
        if (batch.Count != 0) yield return batch.ToArray();
    }

    private static FileStream AcquireProjectLock(string project)
    {
        try { return new FileStream(Path.Combine(project, ".task.lock"), FileMode.OpenOrCreate, FileAccess.ReadWrite, FileShare.None); }
        catch (IOException error) { throw new IOException("这个游戏已有任务正在运行，请等待完成或暂停后重试。", error); }
    }

    private static async Task VerifySourcesAsync(string directory, IReadOnlyList<SourceFile> files, CancellationToken ct)
    {
        foreach (var file in files)
            if (await LocalFiles.HashFileAsync(LocalFiles.UnderRoot(directory, file.RelativePath), ct) != file.Sha256)
                throw new InvalidDataException("原文快照校验失败，请重新识别游戏。");
    }

    private static async Task<bool> PatchIsIntactAsync(string directory, CancellationToken ct)
    {
        var manifest = await LocalFiles.ReadJsonAsync<PatchManifest>(Path.Combine(directory, PatchInstaller.ManifestFilename), ct);
        if (manifest?.SchemaVersion != 1 || manifest.Files.Count == 0) return false;
        foreach (var file in manifest.Files)
        {
            var path = LocalFiles.UnderRoot(directory, file.RelativePath);
            if (!File.Exists(path) || await LocalFiles.HashFileAsync(path, ct) != file.Sha256) return false;
        }
        return true;
    }
}
