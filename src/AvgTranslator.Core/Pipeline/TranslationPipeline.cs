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
    long TranslationTokens, string Message);

public sealed record PipelineResult(string PatchDirectory, string? LaunchExecutable,
    int TotalUnits, int CacheHits, TokenUsage TranslationUsage, ValidationResult Validation,
    IReadOnlyList<string> Warnings, string? SourceFingerprint = null,
    bool IsTrial = false, int? TranslatedUnits = null);

public sealed record RunOptions(int BatchSize = 24, bool RetryUncertainRequests = true,
    IReadOnlyList<string>? TrialUnitIds = null);
public sealed record SourceSnapshot(string Directory, string AdapterId, string AdapterVersion, IReadOnlyList<SourceFile> Files);
public sealed record PendingTranslation(string Id, string MemoryKey);
public sealed record TranslationIntent(string RequestKey, string Provider, DateTimeOffset SentAt,
    IReadOnlyList<PendingTranslation> Units);
public sealed record TranslationRefusal(TranslationIntent Request, DateTimeOffset RecordedAt, ProviderResponseDiagnostic? Diagnostic);

public sealed class UncertainRequestException(string message) : Exception(message);
public sealed class TranslationRetriesExhaustedException(TranslationProviderException error)
    : Exception("API 请求重试 5 次后仍失败，任务已暂停。已完成译文已保存，可继续相同范围的翻译。最后一次失败：" + error.FailureSummary + "。", error);

public sealed class TranslationRefusedException(bool alreadyRecorded = false)
    : Exception((alreadyRecorded ? "这批内容已有模型拒绝记录，本次未再次调用 API。" : "模型拒绝处理这批内容（服务商内容限制），已停止自动重试。") +
        "任务已暂停，已完成译文已保存。请先处理被拒绝的内容；拒绝说明保存在日志和任务记录中。");

public sealed class TranslationPipeline(string dataDirectory, string? diagnosticDirectory = null)
{
    public string DataDirectory { get; } = Path.GetFullPath(dataDirectory);

    public static string GameId(string gameDirectory) => LocalFiles.HashText(
        Path.GetFullPath(gameDirectory).TrimEnd(Path.DirectorySeparatorChar).ToUpperInvariant())[..24];

    public async Task<GameSession> AnalyzeAsync(string gameDirectory, IGameAdapter adapter,
        CancellationToken ct = default)
    {
        var gameRoot = Path.GetFullPath(gameDirectory);
        if (!Directory.Exists(gameRoot)) throw new DirectoryNotFoundException("游戏目录不存在。");
        var probe = await adapter.ProbeAsync(gameRoot, ct)
            ?? throw new InvalidDataException("当前 Adapter 不支持此游戏版本。");
        var id = GameId(gameRoot);
        var project = Path.Combine(DataDirectory, "games", id);
        Directory.CreateDirectory(project);
        await using var projectLock = AcquireProjectLock(project);
        var installer = new PatchInstaller();
        var sourceFiles = new List<(SourceFile File, string Path)>();
        foreach (var relative in probe.RequiredFiles.Distinct(StringComparer.OrdinalIgnoreCase))
        {
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
        await VerifySourcesAsync(snapshotDirectory, files, ct);
        var executableAnalysis = new List<ExecutableAnalysis>();
        foreach (var file in files.Where(file => file.RelativePath.EndsWith(".exe", StringComparison.OrdinalIgnoreCase)))
            executableAnalysis.Add(ExecutableChecks.Analyze(file.RelativePath,
                await File.ReadAllBytesAsync(LocalFiles.UnderRoot(snapshotDirectory, file.RelativePath), ct)));
        await LocalFiles.WriteJsonAsync(Path.Combine(project, "executable-analysis.json"), executableAnalysis, ct);
        var snapshot = new SourceSnapshot(snapshotDirectory, adapter.Id, adapter.Version, files);
        await LocalFiles.WriteJsonAsync(Path.Combine(project, "source.json"), snapshot, ct);
        var extraction = await adapter.ExtractAsync(snapshotDirectory, ct);
        if (extraction.Units.Count == 0) throw new InvalidDataException("未提取到可翻译文本，不能报告为汉化成功。");
        if (extraction.Units.Any(u => string.IsNullOrWhiteSpace(u.Id) || string.IsNullOrWhiteSpace(u.SourceText)) ||
            extraction.Units.Select(u => u.Id).Distinct(StringComparer.Ordinal).Count() != extraction.Units.Count)
            throw new InvalidDataException("提取结果包含空文本或重复的文本 ID。");
        await LocalFiles.WriteJsonAsync(Path.Combine(project, "extraction.json"), extraction, ct);
        return new GameSession(id, gameRoot, project, snapshotDirectory, probe, files, extraction, adapter);
    }

    public async Task<PipelineResult> RunAsync(GameSession session, ITranslationProvider provider,
        TranslationPolicy policy, RunOptions? options = null, IProgress<PipelineProgress>? progress = null,
        CancellationToken ct = default)
    {
        options ??= new();
        if (options.BatchSize is < 1 or > 80) throw new ArgumentOutOfRangeException(nameof(options), "每批文本数必须在 1 到 80 之间。");
        if (string.IsNullOrWhiteSpace(policy.TargetLanguage)) throw new ArgumentException("目标语言不能为空。", nameof(policy));
        var allUnits = session.Extraction.Units;
        var trialIds = options.TrialUnitIds?.ToHashSet(StringComparer.Ordinal);
        IReadOnlyList<TranslationUnit> units = trialIds is null ? allUnits : allUnits.Where(u => trialIds.Contains(u.Id)).ToArray();
        if (trialIds is not null && (trialIds.Count == 0 || units.Count != trialIds.Count))
            throw new ArgumentException("请重新选择试译文本：选区为空或包含不属于当前游戏的文本。", nameof(options));
        var isTrial = trialIds is not null;
        await using var projectLock = AcquireProjectLock(session.ProjectDirectory);
        var memory = new TranslationMemory(Path.Combine(DataDirectory, "memory", session.Id));
        var completed = new Dictionary<string, string>(StringComparer.Ordinal);
        var cacheHits = 0;
        long inputTokens = 0, outputTokens = 0;
        var estimated = false;
        var stage = "准备";
        var runId = Guid.NewGuid().ToString("N");
        var started = DateTimeOffset.UtcNow;
        var responseDirectory = Path.Combine(session.ProjectDirectory, "responses");
        Directory.CreateDirectory(responseDirectory);
        async Task ReportAsync(string nextStage, string message, CancellationToken token = default)
        {
            stage = nextStage;
            var update = new PipelineProgress(stage, completed.Count, units.Count, cacheHits,
                inputTokens + outputTokens, message);
            await LocalFiles.WriteJsonAsync(Path.Combine(session.ProjectDirectory, "job.json"), new
            {
                runId, started, updatedAt = DateTimeOffset.UtcNow, session.GameDirectory,
                adapterId = session.Adapter.Id, adapterVersion = session.Adapter.Version,
                sourceHashes = session.Sources, isTrial, trialUnitIds = trialIds, progress = update
            }, token);
            progress?.Report(update);
        }
        try
        {
            await VerifySourcesAsync(session.SourceDirectory, session.Sources, ct);
            if (isTrial)
                await LocalFiles.WriteJsonAsync(Path.Combine(session.ProjectDirectory, "trial-selection.json"), units.Select(u => u.Id).ToArray(), ct);
            await ReportAsync("查询缓存", "正在复用已提交且上下文兼容的译文。", ct);
            await RecoverResponsesAsync(responseDirectory, memory, units, policy, session.Adapter, ct);
            foreach (var unit in units)
            {
                ct.ThrowIfCancellationRequested();
                var translation = await memory.FindAsync(unit, policy, ct);
                if (translation is not null && ValidateTranslation(session.Adapter, unit, translation).Count == 0)
                {
                    completed.Add(unit.Id, translation);
                    cacheHits++;
                }
            }
            var groups = units.Where(u => !completed.ContainsKey(u.Id)).GroupBy(u => memory.Key(u, policy))
                .Select(g => g.ToArray()).ToArray();
            if (!options.RetryUncertainRequests)
            {
                var missingKeys = groups.Select(g => memory.Key(g[0], policy)).ToHashSet(StringComparer.Ordinal);
                foreach (var intentFile in Directory.EnumerateFiles(responseDirectory, "*.request.json"))
                {
                    var intent = await LocalFiles.ReadJsonAsync<TranslationIntent>(intentFile, ct);
                    var receipt = intentFile[..^".request.json".Length] + ".json";
                    if (!File.Exists(receipt) && !File.Exists(intentFile[..^".request.json".Length] + ".refusal.json") &&
                        intent?.Units?.Any(u => missingKeys.Contains(u.MemoryKey)) == true)
                        throw new UncertainRequestException("上次请求的部分文本没有响应记录，服务商可能已处理或计费。请确认是否重新发送；调整批大小也不会自动重发这些文本。");
                }
            }
            foreach (var batch in Batch(groups, options.BatchSize))
            {
                ct.ThrowIfCancellationRequested();
                var representatives = batch.Select(g => g[0]).ToArray();
                var requestKey = LocalFiles.HashText(JsonSerializer.Serialize(new
                {
                    Provider = provider.Identity,
                    Units = representatives.Select(u => new { u.Id, Key = memory.Key(u, policy) })
                }));
                var responsePath = Path.Combine(responseDirectory, requestKey + ".json");
                var intentPath = Path.Combine(responseDirectory, requestKey + ".request.json");
                var intent = new TranslationIntent(requestKey, provider.Identity, DateTimeOffset.UtcNow,
                    representatives.Select(u => new PendingTranslation(u.Id, memory.Key(u, policy))).ToArray());
                var response = await LocalFiles.ReadJsonAsync<TranslationResponse>(responsePath, ct);
                if (response is null)
                {
                    if (await FindRecordedRefusalAsync(responseDirectory, intent, ct) is not null)
                        throw new TranslationRefusedException(alreadyRecorded: true);
                    if (File.Exists(intentPath) && !options.RetryUncertainRequests)
                        throw new UncertainRequestException("上次翻译请求未记录到响应，服务商可能已处理或计费。请确认是否重新发送这批文本；已保存译文不会重发。");
                    await ReportAsync("翻译", $"正在翻译 {representatives.Length} 条新文本；已复用 {cacheHits} 条。", ct);
                    await LocalFiles.WriteJsonAsync(intentPath, intent, ct);
                    const int maximumRetries = 5;
                    for (var attempt = 0; ; attempt++)
                    {
                        ct.ThrowIfCancellationRequested();
                        try
                        {
                            response = await provider.TranslateAsync(new TranslationRequest(representatives,
                                policy.TargetLanguage, policy.Terminology, policy.Style), ct);
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
                                throw new TranslationRefusedException();
                            }
                            if (attempt == maximumRetries)
                            {
                                File.Delete(intentPath);
                                throw new TranslationRetriesExhaustedException(error);
                            }
                            await ReportAsync("等待重试", $"API 请求失败：{error.FailureSummary}。10 秒后进行第 {attempt + 1} / {maximumRetries} 次重试。", ct);
                            await Task.Delay(TimeSpan.FromSeconds(10), ct);
                            await ReportAsync("翻译", $"正在进行第 {attempt + 1} / {maximumRetries} 次重试。", ct);
                        }
                    }
                    // Persist the complete response before committing entries. A restart can finish the commit without another API call.
                    await LocalFiles.WriteJsonAsync(responsePath, response, CancellationToken.None);
                }
                inputTokens += response.Usage.InputTokens;
                outputTokens += response.Usage.OutputTokens;
                estimated |= response.Usage.Estimated;
                var expected = representatives.Select(u => u.Id).ToHashSet(StringComparer.Ordinal);
                if (response.Translations.Count != expected.Count || response.Translations.Keys.Any(id => !expected.Contains(id)))
                {
                    File.Move(responsePath, responsePath + ".invalid-" + Guid.NewGuid().ToString("N"));
                    File.Delete(intentPath);
                    throw new InvalidDataException("模型返回的文本 ID 与请求不一致，响应已保存，尚未回填资源。");
                }
                var issues = new List<ValidationIssue>();
                foreach (var group in batch)
                {
                    var representative = group[0];
                    if (!response.Translations.TryGetValue(representative.Id, out var translation))
                    {
                        issues.Add(new("missing-unit", "模型遗漏文本。", representative.Id));
                        continue;
                    }
                    var validation = ValidateTranslation(session.Adapter, representative, translation);
                    if (validation.Count != 0) { issues.AddRange(validation); continue; }
                    await memory.StoreAsync(representative, policy, translation, provider.Identity, CancellationToken.None);
                    foreach (var unit in group) completed[unit.Id] = translation;
                    cacheHits += group.Length - 1;
                }
                if (issues.Count != 0)
                {
                    await LocalFiles.WriteJsonAsync(Path.Combine(session.ProjectDirectory, "translation-errors.json"), issues, CancellationToken.None);
                    if (diagnosticDirectory is not null)
                    {
                        try
                        {
                            await LocalFiles.WriteJsonAsync(Path.Combine(diagnosticDirectory, $"validation-{DateTime.UtcNow:yyyyMMdd-HHmmssfff}-{Guid.NewGuid():N}.json"),
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
                    File.Delete(intentPath);
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
                    throw new InvalidDataException($"{failedUnits} 条译文未通过格式检查：{location}{reason} 有效译文已保存，继续时只重试失败文本。");
                }
                await ReportAsync("翻译", $"已完成 {completed.Count} / {units.Count} 条文本。", ct);
            }
            await VerifySourcesAsync(session.SourceDirectory, session.Sources, ct);
            // Original-text fallbacks belong only to the patch; they are never committed as translations.
            var buildTexts = isTrial
                ? allUnits.ToDictionary(u => u.Id, u => completed.GetValueOrDefault(u.Id, u.SourceText), StringComparer.Ordinal)
                : completed;
            var buildKey = LocalFiles.HashText(JsonSerializer.Serialize(new
            {
                session.Adapter.Id, session.Adapter.Version, session.Sources, isTrial,
                TrialUnitIds = trialIds?.Order(StringComparer.Ordinal).ToArray(),
                Translations = buildTexts.OrderBy(k => k.Key, StringComparer.Ordinal)
            }));
            var patchDirectory = Path.Combine(session.ProjectDirectory, "patches", buildKey);
            var reportPath = Path.Combine(patchDirectory, "translation-report.json");
            var existing = await LocalFiles.ReadJsonAsync<PipelineResult>(reportPath, ct);
            if (existing is not null && await PatchIsIntactAsync(patchDirectory, ct))
            {
                var manifest = await LocalFiles.ReadJsonAsync<PatchManifest>(Path.Combine(patchDirectory, PatchInstaller.ManifestFilename), ct);
                var executableCheck = await ValidateExecutablesAsync(session, patchDirectory, manifest!.Files.Select(file => file.RelativePath).ToArray(), ct);
                if (!executableCheck.Passed)
                    throw new InvalidDataException("已有补丁未通过程序加载检查：" + string.Join("；", executableCheck.Issues.Select(issue => issue.Message)));
                await RecordCompatibilityAsync(session, existing, ct);
                var reused = existing with { CacheHits = cacheHits, TranslationUsage = new(inputTokens, outputTokens, estimated) };
                await LocalFiles.WriteJsonAsync(Path.Combine(session.ProjectDirectory, "latest-result.json"), reused, ct);
                await ReportAsync(isTrial ? "试译完成" : "完成", isTrial
                    ? "已复用试译补丁。可安装后进入所选片段检查显示效果；其余文本保留原文。"
                    : "译文和补丁均未变化，直接复用已完成结果。", ct);
                return reused;
            }
            await ReportAsync("生成补丁", "正在回填译文并生成游戏目录所需文件。", ct);
            var staging = patchDirectory + ".pending-" + Guid.NewGuid().ToString("N");
            Directory.CreateDirectory(staging);
            var build = await session.Adapter.BuildPatchAsync(session.SourceDirectory, buildTexts, staging, ct);
            if (build.Files.Count == 0) throw new InvalidDataException("Adapter 未生成补丁文件。");
            if (build.Files.Distinct(StringComparer.OrdinalIgnoreCase).Count() != build.Files.Count)
                throw new InvalidDataException("Adapter 生成了重复的文件路径。");
            await ReportAsync("校验补丁", "正在校验资源结构、文本和文件哈希。", ct);
            var validationResult = await session.Adapter.ValidatePatchAsync(session.SourceDirectory, staging, ct);
            var executableValidation = await ValidateExecutablesAsync(session, staging, build.Files, ct);
            validationResult = validationResult with
            {
                Passed = validationResult.Passed && executableValidation.Passed,
                Issues = validationResult.Issues.Concat(executableValidation.Issues).ToArray()
            };
            if (!validationResult.Passed)
            {
                await LocalFiles.WriteJsonAsync(Path.Combine(staging, "validation-errors.json"), validationResult, ct);
                throw new InvalidDataException("补丁未通过结构校验：" + string.Join("；", validationResult.Issues.Select(i => i.Message).Take(5)));
            }
            var patchFiles = new List<PatchFile>();
            foreach (var relative in build.Files)
                patchFiles.Add(new(relative, await LocalFiles.HashFileAsync(LocalFiles.UnderRoot(staging, relative), ct),
                    session.Sources.FirstOrDefault(f => f.RelativePath.Equals(relative, StringComparison.OrdinalIgnoreCase))?.Sha256));
            await LocalFiles.WriteJsonAsync(Path.Combine(staging, PatchInstaller.ManifestFilename), new PatchManifest(1, patchFiles), ct);
            var result = new PipelineResult(patchDirectory, build.LaunchExecutable ?? session.Probe.OriginalExecutable,
                allUnits.Count, cacheHits, new(inputTokens, outputTokens, estimated), validationResult,
                session.Extraction.Warnings.Concat(build.Warnings).Distinct().ToArray(), Path.GetFileName(session.SourceDirectory),
                isTrial, completed.Count);
            await LocalFiles.WriteJsonAsync(Path.Combine(staging, "translation-report.json"), result, ct);
            if (isTrial)
                await LocalFiles.WriteJsonAsync(Path.Combine(staging, "trial-texts.json"), units.Select(u => new
                {
                    u.Id, u.ResourcePath, u.Locator, u.Kind, u.Speaker, u.SourceText, translation = completed[u.Id]
                }).ToArray(), ct);
            if (Directory.Exists(patchDirectory))
                Directory.Move(patchDirectory, patchDirectory + ".damaged-" + Guid.NewGuid().ToString("N"));
            Directory.Move(staging, patchDirectory);
            await LocalFiles.WriteJsonAsync(Path.Combine(session.ProjectDirectory, "latest-result.json"), result, ct);
            await RecordCompatibilityAsync(session, result, ct);
            await ReportAsync(isTrial ? "试译完成" : "完成", isTrial
                ? $"已生成 {completed.Count} 条文本的试译补丁，其余文本保留原文。请在“汉化补丁”安装后进入所选片段验证。"
                : "汉化补丁已生成。结构校验通过；游戏运行效果尚未验证。", ct);
            return result;
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
        var byKey = units.GroupBy(u => memory.Key(u, policy)).ToDictionary(g => g.Key, g => g.First(), StringComparer.Ordinal);
        foreach (var intentPath in Directory.EnumerateFiles(directory, "*.request.json"))
        {
            var intent = await LocalFiles.ReadJsonAsync<TranslationIntent>(intentPath, ct);
            if (intent?.Units is null) continue;
            var responsePath = intentPath[..^".request.json".Length] + ".json";
            var response = await LocalFiles.ReadJsonAsync<TranslationResponse>(responsePath, ct);
            if (response is null) continue;
            if (response.Translations.Count != intent.Units.Count ||
                response.Translations.Keys.Any(id => !intent.Units.Any(u => u.Id == id))) continue;
            foreach (var requested in intent.Units)
            {
                if (!byKey.TryGetValue(requested.MemoryKey, out var current) ||
                    !response.Translations.TryGetValue(requested.Id, out var translated) ||
                    ValidateTranslation(adapter, current, translated).Count != 0) continue;
                if (await memory.FindAsync(current, policy, ct) is null)
                    await memory.StoreAsync(current, policy, translated, intent.Provider, ct);
            }
        }
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
