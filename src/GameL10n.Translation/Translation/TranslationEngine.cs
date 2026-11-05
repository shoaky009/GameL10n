using System.Text.Json;
using GameL10n.Core.Providers;
using GameL10n.Core.Storage;
using System.Text.Json.Nodes;
using GameL10n.Core.Serialization;

namespace GameL10n.Core.Translation;

/// <summary>Batch translation, memory reuse, validation and durable request recovery, independent of game resources.</summary>
public sealed class TranslationEngine(string workingDirectory, string? diagnosticDirectory = null,
    string? legacyMemoryDirectory = null)
{
    public async Task<IReadOnlyDictionary<string, string>> LoadFailuresAsync(IReadOnlyList<TranslationUnit> units,
        TranslationPolicy policy, CancellationToken ct = default)
    {
        var issues = await ReadTranslationIssuesAsync(Path.Combine(workingDirectory, "translation-errors.json"), ct);
        var byId = units.ToDictionary(unit => unit.Id, StringComparer.Ordinal);
        var memory = new TranslationMemory(Path.Combine(workingDirectory, "memory"));
        return issues.Where(issue => issue.UnitId is { } id && byId.TryGetValue(id, out var unit)
                && (issue.MemoryKey is null || issue.MemoryKey == memory.Key(unit, policy)))
            .GroupBy(issue => issue.UnitId!, StringComparer.Ordinal)
            .ToDictionary(group => group.Key, group => string.Join("；", group.Select(issue => issue.Message).Distinct()), StringComparer.Ordinal);
    }

    /// <summary>Translates text using a host-owned working directory. The host serializes operations against that directory.
    /// Optional resource-specific validation runs after the shared text checks; no adapter is loaded by this library.</summary>
    public async Task<TranslationRunResult> RunAsync(IReadOnlyList<TranslationUnit> allUnits,
        IReadOnlyList<ITranslationProvider> translationProviders, TranslationPolicy policy,
        TranslationOptions? options = null,
        Func<TranslationProgress, CancellationToken, Task>? reportProgress = null,
        Func<TranslationUnit, string, IReadOnlyList<ValidationIssue>>? additionalValidation = null,
        CancellationToken ct = default)
    {
        ArgumentNullException.ThrowIfNull(allUnits);
        ArgumentNullException.ThrowIfNull(policy);
        if (allUnits.Any(unit => unit is null || string.IsNullOrWhiteSpace(unit.Id)
                || string.IsNullOrWhiteSpace(unit.SourceText) || unit.Context is null)
            || allUnits.Select(unit => unit.Id).Distinct(StringComparer.Ordinal).Count() != allUnits.Count)
            throw new ArgumentException("文本必须包含唯一且非空的 ID、原文及有效上下文。", nameof(allUnits));
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
        if (retranslating && (options.CandidateOnly || options.UnitIds is null
            || !Guid.TryParseExact(options.RetranslationId, "N", out _)))
            throw new ArgumentException("重新翻译必须指定有效任务标识和文本选区。", nameof(options));
        if (string.IsNullOrWhiteSpace(policy.TargetLanguage)) throw new ArgumentException("目标语言不能为空。", nameof(policy));
        var trialIds = options.UnitIds?.ToHashSet(StringComparer.Ordinal);
        IReadOnlyList<TranslationUnit> units = trialIds is null ? allUnits : allUnits.Where(u => trialIds.Contains(u.Id)).ToArray();
        if (trialIds is not null && (trialIds.Count == 0 || units.Count != trialIds.Count))
            throw new ArgumentException("请重新选择试译文本：选区为空或包含不属于当前游戏的文本。", nameof(options));
        var isTrial = trialIds is not null;
        var continueOnTextError = !isTrial && !options.CandidateOnly;
        Exception? firstBatchError = null;
        var failedIds = new HashSet<string>(StringComparer.Ordinal);
        var memory = new TranslationMemory(Path.Combine(workingDirectory, "memory"), legacyMemoryDirectory);
        var completed = new Dictionary<string, string>(StringComparer.Ordinal);
        var cacheHits = 0;
        long inputTokens = 0, outputTokens = 0;
        var estimated = false;
        var logDirectory = diagnosticDirectory ?? Path.Combine(workingDirectory, "logs");
        var responseDirectory = Path.Combine(workingDirectory, "responses");
        Directory.CreateDirectory(responseDirectory);
        var validationFeedbackPath = Path.Combine(workingDirectory, "translation-errors.json");
        var previousIssues = new List<RecordedTranslationIssue>();
        Task ReportAsync(string stage, string message, CancellationToken token = default) =>
            reportProgress?.Invoke(new(stage, completed.Count, units.Count, cacheHits,
                new(inputTokens, outputTokens, estimated), message, failedIds.Count), token) ?? Task.CompletedTask;
        IReadOnlyList<ValidationIssue> Validate(TranslationUnit unit, string text) =>
            TextValidation.Validate(unit, text).Concat(additionalValidation?.Invoke(unit, text) ?? []).ToArray();

        async Task MarkBatchFailedAsync(IEnumerable<TranslationUnit> failedUnits, Exception error, string code, string reason)
        {
            ct.ThrowIfCancellationRequested();
            firstBatchError ??= error;
            var failed = failedUnits.ToArray();
            var ids = failed.Select(unit => unit.Id).ToHashSet(StringComparer.Ordinal);
            previousIssues.RemoveAll(issue => issue.UnitId is { } id && ids.Contains(id));
            previousIssues.AddRange(failed.Select(unit => new RecordedTranslationIssue(code, reason, unit.Id, memory.Key(unit, policy))));
            failedIds.UnionWith(ids);
            await LocalFiles.WriteJsonAsync(validationFeedbackPath, previousIssues, TranslationJson.TypeInfo<List<RecordedTranslationIssue>>(TranslationJson.Storage), CancellationToken.None);
            options.FailureUpdates?.Report(failed.ToDictionary(unit => unit.Id, _ => (string?)reason, StringComparer.Ordinal));
            await ReportAsync("翻译", $"{failed.Length} 条文本翻译失败：{reason} 已标记失败，继续后续文本。", ct);
        }

        string ApiLabel(int index) => providers[index].DisplayName is { Length: > 0 } name
            ? $"{name}({index + 1}/{providers.Length})"
            : $"第 {index + 1} / {providers.Length} 个翻译 API";

        async Task<(TranslationResponse Response, ITranslationProvider Provider, string ResponsePath, string IntentPath)>
            TranslateBatchAsync(TranslationUnit[] representatives)
        {
            var feedback = representatives.Select(unit => (
                unit.Id,
                Messages: (IReadOnlyList<string>)previousIssues.Where(issue => issue.UnitId == unit.Id
                    && (issue.MemoryKey is null || issue.MemoryKey == memory.Key(unit, policy))
                    && issue.Code is not ("provider-refusal" or "provider-request-failed" or "request-outcome-unknown"))
                    .Select(issue => issue.Message).Distinct(StringComparer.Ordinal).ToArray()
            )).Where(item => item.Messages.Count > 0).ToDictionary(item => item.Id, item => item.Messages, StringComparer.Ordinal);
            var request = new TranslationRequest(representatives, policy.TargetLanguage, policy.Terminology, policy.Style)
            { ValidationFeedback = feedback.Count > 0 ? feedback : null };
            var anyNewRefusal = false;
            for (var index = 0; index < providers.Length; index++)
            {
                ct.ThrowIfCancellationRequested();
                var provider = providers[index];
                var providerLabel = ApiLabel(index);
                var requestKey = LocalFiles.HashText(JsonSerializer.Serialize(new JsonObject
                {
                    ["Provider"] = provider.Identity,
                    ["Units"] = TranslationJson.Array(representatives.Select(u => new JsonObject
                    {
                        ["Id"] = u.Id,
                        ["Key"] = memory.Key(u, policy)
                    }))
                }, TranslationJson.TypeInfo<JsonObject>(TranslationJson.Default)) + (options.CandidateOnly ? "\ncandidate" : "")
                    + (retranslating ? "\nretranslate:" + options.RetranslationId : ""));
                var responsePath = Path.Combine(responseDirectory, requestKey + ".json");
                var intentPath = Path.Combine(responseDirectory, requestKey + ".request.json");
                var intent = new TranslationIntent(requestKey, provider.Identity, DateTimeOffset.UtcNow,
                    representatives.Select(u => new PendingTranslation(u.Id, memory.Key(u, policy))).ToArray(), options.CandidateOnly, options.RetranslationId);
                var response = await LocalFiles.ReadRecoverableJsonAsync<TranslationResponse>(responsePath, TranslationJson.TypeInfo<TranslationResponse>(TranslationJson.Storage), ct);
                if (response is not null) return (response, provider, responsePath, intentPath);
                var recordedRefusal = await FindRecordedRefusalAsync(responseDirectory, intent, ct);
                if (recordedRefusal is null)
                {
                    if (File.Exists(intentPath) && !options.RetryUncertainRequests)
                        throw new UncertainRequestException("上次翻译请求未记录到响应，服务商可能已处理或计费。请确认是否重新发送这批文本；已保存译文不会重发。");
                    await ReportAsync("翻译", $"正在使用 {providerLabel} 翻译 {representatives.Length} 条新文本。", ct);
                    await LocalFiles.WriteJsonAsync(intentPath, intent, TranslationJson.TypeInfo<TranslationIntent>(TranslationJson.Storage), ct);
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
                                new JsonObject
                                {
                                    ["attempt"] = attempt + 1,
                                    ["message"] = error.Message,
                                    ["failureSummary"] = error.FailureSummary,
                                    ["usage"] = TranslationJson.Node(error.Usage, TranslationJson.Storage),
                                    ["outcomeUnknown"] = error.OutcomeUnknown,
                                    ["statusCode"] = TranslationJson.Node(error.StatusCode, TranslationJson.Storage),
                                    ["diagnostic"] = TranslationJson.Node(error.Diagnostic, TranslationJson.Storage),
                                    ["failedAt"] = DateTimeOffset.UtcNow
                                }, TranslationJson.TypeInfo<JsonObject>(TranslationJson.Storage), CancellationToken.None);
                            if (error.IsModelRefusal)
                            {
                                await LocalFiles.WriteJsonAsync(Path.Combine(responseDirectory, requestKey + ".refusal.json"),
                                    new TranslationRefusal(intent, DateTimeOffset.UtcNow, error.Diagnostic), TranslationJson.TypeInfo<TranslationRefusal>(TranslationJson.Storage), CancellationToken.None);
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
                        await LocalFiles.WriteJsonAsync(responsePath, response, TranslationJson.TypeInfo<TranslationResponse>(TranslationJson.Storage), CancellationToken.None);
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
            await memory.InitializeAsync(ct);
            previousIssues = await ReadTranslationIssuesAsync(validationFeedbackPath, ct);
            await ReportAsync("查询缓存", "正在加载历史译文。", ct);
            if (!options.CandidateOnly && !retranslating)
                await RecoverResponsesAsync(responseDirectory, memory, units, policy, Validate, ct);
            var edits = await TranslationEdits.LoadAsync(workingDirectory, ct);
            await CleanupCommittedResponsesAsync(workingDirectory, memory, ct);
            foreach (var unit in units)
            {
                ct.ThrowIfCancellationRequested();
                if (options.CandidateOnly) continue;
                var edit = edits.Find(unit, policy.TargetLanguage);
                if (retranslating && edit?.RetranslationId != options.RetranslationId) continue;
                var translation = edit?.Text ?? await memory.FindAsync(unit, policy, ct);
                // Unfinished manual corrections are protected from model replacement too.
                if (translation is not null && ((!retranslating && edit is not null) || Validate(unit, translation).Count == 0))
                {
                    completed.Add(unit.Id, translation);
                    cacheHits++;
                }
            }
            if (previousIssues.RemoveAll(issue => issue.UnitId is { } id && completed.ContainsKey(id)) > 0)
                await LocalFiles.WriteJsonAsync(validationFeedbackPath, previousIssues, TranslationJson.TypeInfo<List<RecordedTranslationIssue>>(TranslationJson.Storage), CancellationToken.None);
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
                    var intent = await LocalFiles.ReadJsonAsync<TranslationIntent>(intentFile, TranslationJson.TypeInfo<TranslationIntent>(TranslationJson.Storage), ct);
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
                    edits = await TranslationEdits.LoadAsync(workingDirectory, ct);
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
                    var validation = Validate(representative, translation);
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
                    edits = await TranslationEdits.LoadAsync(workingDirectory, CancellationToken.None);
                    var replacements = new Dictionary<string, TranslationEdit>(edits.Entries, StringComparer.Ordinal);
                    var candidates = new Dictionary<string, TranslationEdit>(edits.Candidates ?? new Dictionary<string, TranslationEdit>(), StringComparer.Ordinal);
                    foreach (var unit in batch.SelectMany(group => group).Where(unit => completed.ContainsKey(unit.Id)))
                    {
                        replacements[unit.Id] = new(TranslationEdits.Signature(unit), policy.TargetLanguage,
                            completed[unit.Id], false, DateTimeOffset.UtcNow, options.RetranslationId, IsGenerated: true);
                        candidates.Remove(unit.Id);
                    }
                    await new TranslationEdits(1, replacements, candidates).SaveAsync(workingDirectory, CancellationToken.None);
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
                    await LocalFiles.WriteJsonAsync(validationFeedbackPath, previousIssues, TranslationJson.TypeInfo<List<RecordedTranslationIssue>>(TranslationJson.Storage), CancellationToken.None);
                if (issues.Count != 0)
                {
                    if (logDirectory is not null)
                    {
                        try
                        {
                            await LocalFiles.WriteJsonAsync(Path.Combine(logDirectory, $"validation-{DateTime.UtcNow:yyyyMMdd-HHmmssfff}-{Guid.NewGuid():N}.json"),
                                new JsonObject
                                {
                                    ["timestamp"] = DateTimeOffset.UtcNow,
                                    ["workId"] = Path.GetFileName(workingDirectory),
                                    ["issues"] = TranslationJson.Node(issues, TranslationJson.Storage),
                                    ["texts"] = TranslationJson.Array(representatives.Where(unit => issues.Any(issue => issue.UnitId == unit.Id)).Select(unit => new JsonObject
                                    {
                                        ["id"] = unit.Id,
                                        ["resourcePath"] = unit.ResourcePath,
                                        ["locator"] = unit.Locator,
                                        ["sourceText"] = unit.SourceText,
                                        ["translation"] = response.Translations.GetValueOrDefault(unit.Id)
                                    }))
                                }, TranslationJson.TypeInfo<JsonObject>(TranslationJson.Storage), CancellationToken.None);
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
                edits = await TranslationEdits.LoadAsync(workingDirectory, ct);
                var resolved = new Dictionary<string, string?>();
                foreach (var unit in units.Where(unit => failedIds.Contains(unit.Id)))
                    if (edits.Find(unit, policy.TargetLanguage) is { } edit && Validate(unit, edit.Text).Count == 0)
                    {
                        completed[unit.Id] = edit.Text;
                        failedIds.Remove(unit.Id);
                        resolved[unit.Id] = null;
                    }
                if (resolved.Count > 0)
                {
                    previousIssues.RemoveAll(issue => issue.UnitId is { } id && resolved.ContainsKey(id));
                    await LocalFiles.WriteJsonAsync(validationFeedbackPath, previousIssues, TranslationJson.TypeInfo<List<RecordedTranslationIssue>>(TranslationJson.Storage), CancellationToken.None);
                    options.FailureUpdates?.Report(resolved);
                }
            }
            var summary = new TranslationRunSummary(units.Count, completed.Count, failedIds.Count);
            // A host that builds a complete artifact can require every translation to pass,
            // after all remaining batches have been attempted and successful results saved.
            if (options.RequireAllTranslations && failedIds.Count > 0 && firstBatchError is not null)
                System.Runtime.ExceptionServices.ExceptionDispatchInfo.Capture(firstBatchError).Throw();
            if (summary.Failed > 0)
                await ReportAsync("翻译完成（有失败）", $"已处理全部文本：成功 {summary.Completed} 条，失败 {summary.Failed} 条。可筛选“翻译失败”查看原因，继续任务时复用已成功译文。", ct);
            else await ReportAsync("翻译完成", retranslating ? "所选文本已重新翻译并保存，可以继续校对或生成补丁。"
                : options.CandidateOnly ? "候选译文已生成，采用后才会替换当前译文。"
                : "本次翻译已保存，可以继续校对或生成补丁。", ct);
            return new(completed, cacheHits, new(inputTokens, outputTokens, estimated), summary);

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
            await ReportAsync("已暂停", error.Message);
            throw;
        }
        catch (Exception error)
        {
            await ReportAsync("失败", error.Message);
            throw;
        }
    }

    internal sealed record RecordedTranslationIssue(string Code, string Message, string? UnitId, string? MemoryKey = null);

    private static async Task<List<RecordedTranslationIssue>> ReadTranslationIssuesAsync(string path, CancellationToken ct)
    {
        try
        {
            var issues = await LocalFiles.ReadJsonAsync<List<RecordedTranslationIssue>>(path, TranslationJson.TypeInfo<List<RecordedTranslationIssue>>(TranslationJson.Storage), ct);
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
        var record = await LocalFiles.ReadJsonAsync<TranslationRefusal>(path, TranslationJson.TypeInfo<TranslationRefusal>(TranslationJson.Storage), ct);
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
            var failure = await LocalFiles.ReadJsonAsync<RecordedProviderFailure>(file.FullName, TranslationJson.TypeInfo<RecordedProviderFailure>(TranslationJson.Storage), ct);
            if (!ModelRefusal.IsRecordedRefusal(failure?.Diagnostic, request.Units.Select(unit => unit.Id))) continue;
            record = new TranslationRefusal(request, DateTimeOffset.UtcNow, failure!.Diagnostic! with
            {
                Code = "model-refusal", Detail = "Recognized refusal from a previously recorded failed response."
            });
            await LocalFiles.WriteJsonAsync(path, record, TranslationJson.TypeInfo<TranslationRefusal>(TranslationJson.Storage), CancellationToken.None);
            return record;
        }
        return null;
    }

    internal sealed record RecordedProviderFailure(ProviderResponseDiagnostic? Diagnostic);

    private static async Task RecoverResponsesAsync(string directory, TranslationMemory memory,
        IReadOnlyList<TranslationUnit> units, TranslationPolicy policy, Func<TranslationUnit, string, IReadOnlyList<ValidationIssue>> validate, CancellationToken ct)
    {
        var byKey = new Dictionary<string, TranslationUnit>(StringComparer.Ordinal);
        foreach (var unit in units)
            if (await memory.FindAsync(unit, policy, ct) is null) byKey.TryAdd(memory.Key(unit, policy), unit);
        if (byKey.Count == 0) return;
        var recovered = new Dictionary<string, MemoryEntry>(StringComparer.Ordinal);
        foreach (var intentPath in Directory.EnumerateFiles(directory, "*.request.json"))
        {
            var intent = await LocalFiles.ReadRecoverableJsonAsync<TranslationIntent>(intentPath, TranslationJson.TypeInfo<TranslationIntent>(TranslationJson.Storage), ct);
            if (intent?.Units is null || intent.CandidateOnly || intent.RetranslationId is not null) continue;
            if (!intent.Units.Any(unit => byKey.ContainsKey(unit.MemoryKey))) continue;
            var responsePath = intentPath[..^".request.json".Length] + ".json";
            var response = await LocalFiles.ReadRecoverableJsonAsync<TranslationResponse>(responsePath, TranslationJson.TypeInfo<TranslationResponse>(TranslationJson.Storage), ct);
            if (response is null) continue;
            foreach (var requested in intent.Units)
            {
                if (!byKey.TryGetValue(requested.MemoryKey, out var current) ||
                    !response.Translations.TryGetValue(requested.Id, out var translated) ||
                    validate(current, translated).Count != 0) continue;
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
                var intent = await LocalFiles.ReadJsonAsync<TranslationIntent>(intentPath, TranslationJson.TypeInfo<TranslationIntent>(TranslationJson.Storage), ct);
                if (intent?.RequestKey != key || intent.CandidateOnly || intent.RetranslationId is not null || intent.Units is not { Count: > 0 } ||
                    intent.Units.Any(unit => unit is null || unit.Id is null || unit.MemoryKey is null || !entries.ContainsKey(unit.MemoryKey))) continue;
                var responsePath = Path.Combine(directory, key + ".json");
                var response = await LocalFiles.ReadJsonAsync<TranslationResponse>(responsePath, TranslationJson.TypeInfo<TranslationResponse>(TranslationJson.Storage), ct);
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

}
