using System.Text.Json;
using GameL10n.Core.Patching;
using GameL10n.Core.Storage;
using GameL10n.Core.Translation;
using GameL10n.Core.Providers;
using GameL10n.Core.Adapters;
using GameL10n.Core.Development;
using GameL10n.Core.Validation;

namespace GameL10n.Core.Pipeline;

public sealed record SourceFile(string RelativePath, string Sha256);
public sealed record GameSession(string Id, string GameDirectory, string ProjectDirectory,
    string SourceDirectory, GameProbe Probe, IReadOnlyList<SourceFile> Sources,
    ExtractionResult Extraction, IGameAdapter Adapter);

public sealed record PipelineProgress(string Stage, int Completed, int Total, int CacheHits,
    long TranslationTokens, string Message, int Failed = 0);

public sealed record PipelineResult(string PatchDirectory, string? LaunchExecutable,
    int TotalUnits, int CacheHits, TokenUsage TranslationUsage, ValidationResult Validation,
    IReadOnlyList<string> Warnings, string? SourceFingerprint = null,
    bool IsTrial = false, int? TranslatedUnits = null);

public sealed record SourceSnapshot(string Directory, string AdapterId, string AdapterVersion, IReadOnlyList<SourceFile> Files,
    GameProbe? Probe = null);

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
        Action<string, string>? reportProgress = null, bool commitSnapshot = true, Func<object, Task>? trace = null)
    {
        var timing = new AdapterOperationTrace(trace, "analysis-phase", adapter.Id, adapter.Version);
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
        await timing.MeasureAsync("source-hash", async () =>
        {
            foreach (var relative in probe.RequiredFiles.Distinct(StringComparer.OrdinalIgnoreCase))
            {
                reportProgress?.Invoke("校验原始资源", $"{sourceFiles.Count + 1} / {probe.RequiredFiles.Count} · {relative}");
                LocalFiles.UnderRoot(gameRoot, relative);
                var sourcePath = await installer.ResolveOriginalFileAsync(gameRoot, relative, ct);
                var hash = await timing.FileAsync("source-file-hash", relative, sourcePath, () => LocalFiles.HashFileAsync(sourcePath, ct));
                sourceFiles.Add((new SourceFile(relative, hash), sourcePath));
            }
        }, new { files = probe.RequiredFiles.Count });
        if (sourceFiles.Count == 0) throw new InvalidDataException("Adapter 未声明需要处理的资源文件。");
        var fingerprint = LocalFiles.HashText(JsonSerializer.Serialize(sourceFiles.Select(f => f.File).OrderBy(f => f.RelativePath)));
        var snapshotDirectory = Path.Combine(project, "sources", fingerprint);
        var reusedSnapshot = Directory.Exists(snapshotDirectory);
        await timing.MeasureAsync("source-snapshot", async () =>
        {
            if (reusedSnapshot) return;
            var staging = snapshotDirectory + ".pending-" + Guid.NewGuid().ToString("N");
            Directory.CreateDirectory(staging);
            foreach (var (file, sourcePath) in sourceFiles)
            {
                reportProgress?.Invoke("保存原文快照", "正在复制并校验：" + file.RelativePath);
                var target = LocalFiles.UnderRoot(staging, file.RelativePath);
                Directory.CreateDirectory(Path.GetDirectoryName(target)!);
                await timing.FileAsync("snapshot-file-copy", file.RelativePath, sourcePath, () =>
                {
                    File.Copy(sourcePath, target, false);
                    return Task.CompletedTask;
                });
                await timing.FileAsync("snapshot-copy-check", file.RelativePath, sourcePath, async () =>
                {
                    if (await LocalFiles.HashFileAsync(target, ct) != file.Sha256 ||
                        await LocalFiles.HashFileAsync(sourcePath, ct) != file.Sha256)
                        throw new IOException("扫描期间游戏资源发生变化，请等待游戏更新完成后重试。");
                });
            }
            Directory.Move(staging, snapshotDirectory);
        }, new { reused = reusedSnapshot });
        var files = sourceFiles.Select(f => f.File).ToArray();
        reportProgress?.Invoke("校验原文快照", "正在确认已保存资源完整，文件较大时需要一些时间。");
        await timing.MeasureAsync("snapshot-verify", () => VerifySourcesAsync(snapshotDirectory, files, ct, timing));
        await timing.MeasureAsync("executable-analysis", async () =>
        {
            var executableAnalysis = new List<ExecutableAnalysis>();
            foreach (var file in files.Where(file => file.RelativePath.EndsWith(".exe", StringComparison.OrdinalIgnoreCase)))
                executableAnalysis.Add(ExecutableChecks.Analyze(file.RelativePath,
                    await File.ReadAllBytesAsync(LocalFiles.UnderRoot(snapshotDirectory, file.RelativePath), ct)));
            await LocalFiles.WriteJsonAsync(Path.Combine(project, "executable-analysis.json"), executableAnalysis, ct);
        });
        var store = new ExtractionStore(project);
        reportProgress?.Invoke("加载文本缓存", "正在读取已有的提取结果。");
        var extraction = await timing.MeasureAsync("extraction-cache", () => store.LoadAsync(fingerprint, adapter.Id, adapter.Version, ct));
        var cachedExtraction = extraction is not null;
        // Legacy extraction.json has no fingerprint of its own and may belong to an older
        // source snapshot. Only a verified cache can replace extraction from current resources.
        if (extraction is null)
        {
            reportProgress?.Invoke("提取剧情文本", "正在从游戏资源提取对白、名称和选项。");
            extraction = await timing.MeasureAsync("extract", () => adapter.ExtractAsync(snapshotDirectory, ct));
        }
        if (extraction.Units.Count == 0) throw new InvalidDataException("未提取到可翻译文本，不能报告为汉化成功。");
        if (extraction.Units.Any(u => string.IsNullOrWhiteSpace(u.Id) || string.IsNullOrWhiteSpace(u.SourceText)) ||
            extraction.Units.Select(u => u.Id).Distinct(StringComparer.Ordinal).Count() != extraction.Units.Count)
            throw new InvalidDataException("提取结果包含空文本或重复的文本 ID。");
        reportProgress?.Invoke("整理项目文本", $"已读取 {extraction.Units.Count:N0} 条文本，正在保存项目记录。");
        if (trace is not null) await trace(new { operation = "extraction-result", adapterId = adapter.Id,
            version = adapter.Version, cached = cachedExtraction, units = extraction.Units.Count });
        if (!cachedExtraction) await timing.MeasureAsync("extraction-store", () => store.StoreAsync(fingerprint, adapter.Id, adapter.Version, extraction, ct));
        if (Directory.Exists(Path.Combine(project, "memory")))
        {
            reportProgress?.Invoke("核对已保存译文", "正在核对已提交的翻译记录，已有译文会继续复用。");
            await timing.MeasureAsync("translation-cleanup", () => TranslationEngine.CleanupCommittedResponsesAsync(project,
                new TranslationMemory(Path.Combine(project, "memory")), ct));
        }
        // Probe may exclude unsupported files outside the snapshot; keep that coverage visible on cache hits too.
        extraction = extraction with { Warnings = probe.Warnings.Concat(extraction.Warnings).Distinct(StringComparer.Ordinal).ToArray() };
        var session = new GameSession(id, gameRoot, project, snapshotDirectory, probe, files, extraction, adapter);
        ct.ThrowIfCancellationRequested();
        if (commitSnapshot) await timing.MeasureAsync("commit", () => CommitAnalysisCoreAsync(session, ct));
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
        TranslationPolicy policy, TranslationOptions? options = null, IProgress<PipelineProgress>? progress = null,
        CancellationToken ct = default) => RunAsync(session, new[] { provider }, policy, options, progress, ct);

    public async Task<PipelineResult> RunAsync(GameSession session, IReadOnlyList<ITranslationProvider> translationProviders,
        TranslationPolicy policy, TranslationOptions? options = null, IProgress<PipelineProgress>? progress = null,
        CancellationToken ct = default) =>
        (await RunCoreAsync(session, translationProviders, policy, options, progress, true, ct)).Patch!;

    public async Task<TranslationRunSummary> TranslateOnlyAsync(GameSession session, IReadOnlyList<ITranslationProvider> providers,
        TranslationPolicy policy, TranslationOptions? options = null, IProgress<PipelineProgress>? progress = null,
        CancellationToken ct = default) => (await RunCoreAsync(session, providers, policy, options, progress, false, ct)).Summary;

    private sealed record TranslationRunOutcome(PipelineResult? Patch, TranslationRunSummary Summary);

    private async Task<TranslationRunOutcome> RunCoreAsync(GameSession session, IReadOnlyList<ITranslationProvider> translationProviders,
        TranslationPolicy policy, TranslationOptions? options, IProgress<PipelineProgress>? progress, bool buildPatch,
        CancellationToken ct)
    {
        if (buildPatch && session.Adapter is UnavailableGameAdapter unavailable)
            throw new GeneratedAdapterException(unavailable.Message);
        options = (options ?? new()) with { RequireAllTranslations = buildPatch || options?.RequireAllTranslations == true };
        if (buildPatch && options.RetranslationId is not null)
            throw new ArgumentException("重新翻译只更新所选译文，请另行生成补丁。", nameof(options));
        await using var projectLock = AcquireProjectLock(session.ProjectDirectory);
        var runId = Guid.NewGuid().ToString("N");
        var started = DateTimeOffset.UtcNow;
        var taskDirectory = Path.Combine(session.ProjectDirectory, "tasks", runId);
        var trialIds = options.UnitIds?.ToHashSet(StringComparer.Ordinal);
        async Task ReportAsync(TranslationProgress update, CancellationToken token)
        {
            var local = new PipelineProgress(update.Stage, update.Completed, update.Total, update.CacheHits,
                update.Usage.TotalTokens, update.Message, update.Failed);
            var state = new
            {
                runId, started, updatedAt = DateTimeOffset.UtcNow, session.GameDirectory,
                adapterId = session.Adapter.Id, adapterVersion = session.Adapter.Version,
                sourceHashes = session.Sources, isTrial = trialIds is not null, trialUnitIds = trialIds, progress = local
            };
            await LocalFiles.WriteJsonAsync(Path.Combine(taskDirectory, "job.json"), state, token);
            await LocalFiles.WriteJsonAsync(Path.Combine(session.ProjectDirectory, "job.json"), state, token);
            progress?.Report(local);
        }
        try
        {
            await VerifySourcesAsync(session.SourceDirectory, session.Sources, ct);
            if (trialIds is not null && !options.CandidateOnly)
            {
                if (trialIds.Count == 0 || !trialIds.IsSubsetOf(session.Extraction.Units.Select(unit => unit.Id)))
                    throw new ArgumentException("请重新选择试译文本：选区为空或包含不属于当前游戏的文本。", nameof(options));
                await LocalFiles.WriteJsonAsync(Path.Combine(session.ProjectDirectory, "trial-selection.json"),
                    session.Extraction.Units.Where(unit => trialIds.Contains(unit.Id)).Select(unit => unit.Id).ToArray(), ct);
            }
        }
        catch (Exception error)
        {
            await ReportAsync(new(error is OperationCanceledException ? "已暂停" : "失败", 0,
                trialIds?.Count ?? session.Extraction.Units.Count, 0, new(0, 0), error.Message), CancellationToken.None);
            throw;
        }
        var engine = new TranslationEngine(session.ProjectDirectory, diagnosticDirectory,
            Path.Combine(DataDirectory, "memory", session.Id));
        var translated = await engine.RunAsync(session.Extraction.Units, translationProviders, policy, options,
            ReportAsync, session.Adapter.ValidateTranslation, ct);
        if (!buildPatch) return new(null, translated.Summary);
        try
        {
            var completed = new Dictionary<string, string>(translated.Translations, StringComparer.Ordinal);
            var edits = await TranslationEdits.LoadAsync(session.ProjectDirectory, ct);
            foreach (var unit in session.Extraction.Units.Where(unit => completed.ContainsKey(unit.Id)))
                if (edits.Find(unit, policy.TargetLanguage) is { } edit) completed[unit.Id] = edit.Text;
            var result = await BuildSnapshotAsync(session, completed, translated.CacheHits, translated.Usage, progress, ct);
            await ReportAsync(new(result.IsTrial ? "试译完成" : "完成", completed.Count, completed.Count,
                translated.CacheHits, translated.Usage, "补丁已生成并通过结构校验，可安装后进入游戏验证。"), ct);
            return new(result, translated.Summary);
        }
        catch (Exception error)
        {
            await ReportAsync(new(error is OperationCanceledException ? "已暂停" : "失败", translated.Translations.Count,
                translated.Translations.Count, translated.CacheHits, translated.Usage, error.Message), CancellationToken.None);
            throw;
        }
    }

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
                result.IsTrial ? "已生成试译产物" : "已生成汉化产物", session.Adapter.Version,
                "patch-validated", result.Validation.RuntimeStatus), ct);
    }

    private static IReadOnlyList<ValidationIssue> ValidateTranslation(IGameAdapter adapter, TranslationUnit unit, string text) =>
        TextValidation.Validate(unit, text).Concat(adapter.ValidateTranslation(unit, text)).ToArray();

    private static FileStream AcquireProjectLock(string project)
    {
        try { return new FileStream(Path.Combine(project, ".task.lock"), FileMode.OpenOrCreate, FileAccess.ReadWrite, FileShare.None); }
        catch (IOException error) { throw new IOException("这个游戏已有任务正在运行，请等待完成或暂停后重试。", error); }
    }

    private static async Task VerifySourcesAsync(string directory, IReadOnlyList<SourceFile> files, CancellationToken ct,
        AdapterOperationTrace? timing = null)
    {
        foreach (var file in files)
        {
            var path = LocalFiles.UnderRoot(directory, file.RelativePath);
            var hash = timing is null ? await LocalFiles.HashFileAsync(path, ct)
                : await timing.FileAsync("snapshot-file-hash", file.RelativePath, path, () => LocalFiles.HashFileAsync(path, ct));
            if (hash != file.Sha256)
                throw new InvalidDataException("原文快照校验失败，请重新识别游戏。");
        }
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
