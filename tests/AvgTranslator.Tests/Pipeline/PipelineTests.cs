using System.Text.Json;
using AvgTranslator.Core;
using AvgTranslator.Core.Patching;
using AvgTranslator.Core.Pipeline;
using AvgTranslator.Core.Providers;
using AvgTranslator.Core.Storage;
using AvgTranslator.Core.Translation;

namespace AvgTranslator.Tests.Pipeline;

public sealed class PipelineTests : IDisposable
{
    private readonly string root = Path.Combine(Path.GetTempPath(), "avg-pipeline-" + Guid.NewGuid().ToString("N"));
    private readonly TranslationPolicy policy = new("简体中文", "natural", new Dictionary<string, string>());
    private string Game => Path.Combine(root, "game");
    private string Data => Path.Combine(root, "data");
    private TranslationPipeline CreatePipeline() => new(Data);

    private static async Task<MemoryEntry> ReadSingleMemoryEntryAsync(GameSession session)
    {
        var shard = await LocalFiles.ReadJsonAsync<MemoryShard>(Assert.Single(Directory.GetFiles(
            Path.Combine(session.ProjectDirectory, "memory"), "??.json")));
        return Assert.Single(shard!.Entries).Value;
    }

    [Fact]
    public async Task FullTranslationContinuesAfterInvalidTextAndRetriesOnlyFailedAliases()
    {
        await WriteGame(("1", "bad {name}"), ("2", "later one"), ("3", "later two"), ("4", "bad {name}"));
        var adapter = new JsonGameAdapter();
        var session = await CreatePipeline().AnalyzeAsync(Game, adapter);
        var provider = new FakeProvider { Translate = unit => unit.SourceText.StartsWith("bad") ? "缺少占位符" : "翻译 " + unit.SourceText };
        var progress = new List<PipelineProgress>();
        var result = await CreatePipeline().TranslateOnlyAsync(session, [provider], policy, new(1), new ImmediateProgress(progress.Add));
        Assert.Equal(new TranslationRunSummary(4, 2, 2), result);
        Assert.Equal(new[] { "1", "2", "3" }, provider.Batches.Select(batch => batch[0].Id));
        Assert.Equal(0, adapter.Builds);
        Assert.Equal(2, progress[^1].Failed);
        var errors = await CreatePipeline().LoadTranslationFailuresAsync(session, policy);
        Assert.Equal(new[] { "1", "4" }, errors.Keys.Order());
        var retry = new FakeProvider();
        result = await CreatePipeline().TranslateOnlyAsync(session, [retry], policy, new(1));
        Assert.Equal(new TranslationRunSummary(4, 4, 0), result);
        Assert.Equal("1", Assert.Single(Assert.Single(retry.Batches)).Id);
        Assert.NotNull(Assert.Single(retry.Requests).ValidationFeedback);
        Assert.Empty(await CreatePipeline().LoadTranslationFailuresAsync(session, policy));
    }

    [Fact]
    public async Task FullTranslationReconcilesCorrectionsToEarlierFailuresBeforeCompletion()
    {
        await WriteGame(("1", "bad {name}"), ("2", "later"));
        var session = await CreatePipeline().AnalyzeAsync(Game, new JsonGameAdapter());
        var unit = session.Extraction.Units[0];
        var calls = 0;
        var provider = new FakeProvider
        {
            Translate = item => item.Id == "1" ? "缺少标记" : "后续文本",
            AfterResponse = () =>
            {
                if (++calls == 2)
                    new TranslationEdits(1, new Dictionary<string, TranslationEdit>
                    {
                        [unit.Id] = new(TranslationEdits.Signature(unit), policy.TargetLanguage, "人工修正 {name}", true, DateTimeOffset.UtcNow)
                    }).SaveAsync(session.ProjectDirectory).GetAwaiter().GetResult();
            }
        };
        var result = await CreatePipeline().TranslateOnlyAsync(session, [provider], policy, new(1));
        Assert.Equal(new TranslationRunSummary(2, 2, 0), result);
        Assert.Empty(await CreatePipeline().LoadTranslationFailuresAsync(session, policy));
        Assert.Equal("人工修正 {name}", (await TranslationEdits.LoadAsync(session.ProjectDirectory)).Find(unit, policy.TargetLanguage)!.Text);
    }

    [Fact]
    public async Task FullTranslationContinuesAfterRefusalAndDoesNotResendSuccessfulOrRefusedBatches()
    {
        await WriteGame(("1", "refused"), ("2", "allowed"), ("3", "later"));
        var session = await CreatePipeline().AnalyzeAsync(Game, new JsonGameAdapter());
        var provider = new FakeProvider { FailureFor = request => request.Units[0].Id == "1" ? Refusal() : null };
        var result = await CreatePipeline().TranslateOnlyAsync(session, [provider], policy, new(1));
        Assert.Equal(new TranslationRunSummary(3, 2, 1), result);
        Assert.Equal(3, provider.Calls);
        Assert.Equal("1", Assert.Single(await CreatePipeline().LoadTranslationFailuresAsync(session, policy)).Key);
        result = await CreatePipeline().TranslateOnlyAsync(session, [new FakeProvider { FailIfCalled = true }], policy, new(1));
        Assert.Equal(1, result.Failed);
        var fallback = new FakeProvider { Identity = "new-provider" };
        result = await CreatePipeline().TranslateOnlyAsync(session, [new FakeProvider { FailIfCalled = true }, fallback], policy, new(1));
        Assert.Equal(0, result.Failed);
        Assert.Equal("1", Assert.Single(Assert.Single(fallback.Batches)).Id);
        Assert.Null(Assert.Single(fallback.Requests).ValidationFeedback);
        Assert.Empty(await CreatePipeline().LoadTranslationFailuresAsync(session, policy));
    }

    [Fact]
    public async Task FullTranslationContinuesAfterRetryExhaustionWithoutChangingProviderChain()
    {
        await WriteGame(("1", "network failure"), ("2", "later"));
        var session = await CreatePipeline().AnalyzeAsync(Game, new JsonGameAdapter());
        var provider = new FakeProvider { FailureFor = request => request.Units[0].Id == "1"
            ? new TranslationProviderException("HTTP429", statusCode: System.Net.HttpStatusCode.TooManyRequests) : null };
        var fallback = new FakeProvider { Identity = "fallback", FailIfCalled = true };
        var result = await CreatePipeline().TranslateOnlyAsync(session, [provider, fallback], policy, new(1));
        Assert.Equal(new TranslationRunSummary(2, 1, 1), result);
        Assert.Equal(7, provider.Calls);
        Assert.Equal(0, fallback.Calls);
        Assert.Contains("重试", Assert.Single(await CreatePipeline().LoadTranslationFailuresAsync(session, policy)).Value);
        var retry = new FakeProvider();
        result = await CreatePipeline().TranslateOnlyAsync(session, [retry], policy, new(1));
        Assert.Equal(0, result.Failed);
        Assert.Equal("1", Assert.Single(Assert.Single(retry.Batches)).Id);
    }

    [Fact]
    public async Task FullTranslationSkipsUncertainRequestsWithoutBlockingUnrelatedText()
    {
        await WriteGame(("1", "uncertain"), ("2", "later"));
        var session = await CreatePipeline().AnalyzeAsync(Game, new JsonGameAdapter());
        var memory = new TranslationMemory(Path.Combine(session.ProjectDirectory, "memory"));
        var responseDirectory = Path.Combine(session.ProjectDirectory, "responses");
        await LocalFiles.WriteJsonAsync(Path.Combine(responseDirectory, "pending.request.json"),
            new TranslationIntent("pending", "test-provider", DateTimeOffset.UtcNow, [new("1", memory.Key(session.Extraction.Units[0], policy))]));
        var provider = new FakeProvider();
        var result = await CreatePipeline().TranslateOnlyAsync(session, [provider], policy, new(1, RetryUncertainRequests: false));
        Assert.Equal(new TranslationRunSummary(2, 1, 1), result);
        Assert.Equal("2", Assert.Single(Assert.Single(provider.Batches)).Id);
        Assert.True(File.Exists(Path.Combine(responseDirectory, "pending.request.json")));
        Assert.Equal("1", Assert.Single(await CreatePipeline().LoadTranslationFailuresAsync(session, policy)).Key);
    }

    [Fact]
    public async Task OpeningSavedProjectDoesNotProbeExtractOrReadSourceArchives()
    {
        await WriteGame(("1", "hello"));
        var analyzed = await CreatePipeline().AnalyzeAsync(Game, new JsonGameAdapter());
        var adapter = new JsonGameAdapter { FailIfProbed = true };
        using var gameLock = new FileStream(Path.Combine(Game, "story.json"), FileMode.Open, FileAccess.ReadWrite, FileShare.None);
        using var sourceLock = new FileStream(Path.Combine(analyzed.SourceDirectory, "story.json"), FileMode.Open, FileAccess.ReadWrite, FileShare.None);

        var restored = await CreatePipeline().LoadSessionAsync(Game, adapter);

        Assert.NotNull(restored);
        Assert.Equal(analyzed.Extraction.Units, restored.Extraction.Units);
        Assert.Equal(analyzed.Probe.Description, restored.Probe.Description);
        Assert.Equal(0, adapter.Probes);
        Assert.Equal(0, adapter.Extractions);
    }

    [Fact]
    public async Task OpeningUsesSavedVersionUntilExplicitAnalysisAndInstallationRejectsAnUpdatedGame()
    {
        await WriteGame(("1", "hello"));
        await CreatePipeline().AnalyzeAsync(Game, new JsonGameAdapter());
        await WriteGame(("1", "updated"));

        var restored = await CreatePipeline().LoadSessionAsync(Game, new JsonGameAdapter { FailIfProbed = true });
        Assert.Equal("hello", Assert.Single(restored!.Extraction.Units).SourceText);
        var result = await CreatePipeline().RunAsync(restored, new FakeProvider(), policy);
        var error = await Assert.ThrowsAsync<IOException>(() => new PatchInstaller(restored.ProjectDirectory).InstallAsync(Game, result.PatchDirectory));
        Assert.Contains("resource changed", error.Message);
        var refreshed = await CreatePipeline().AnalyzeAsync(Game, new JsonGameAdapter());
        Assert.Equal("updated", Assert.Single(refreshed.Extraction.Units).SourceText);
    }

    [Fact]
    public async Task LegacySnapshotWithoutLaunchMetadataRequiresExplicitRecognitionWithoutProbing()
    {
        await WriteGame(("1", "hello"));
        var original = await CreatePipeline().AnalyzeAsync(Game, new JsonGameAdapter());
        var path = Path.Combine(original.ProjectDirectory, "source.json");
        var snapshot = await LocalFiles.ReadJsonAsync<SourceSnapshot>(path);
        await LocalFiles.WriteJsonAsync(path, snapshot! with { Probe = null });
        var before = await File.ReadAllTextAsync(path);
        var adapter = new JsonGameAdapter { FailIfProbed = true };
        var messages = new List<string>();

        Assert.Null(await CreatePipeline().LoadSessionAsync(Game, adapter,
            reportProgress: (_, message) => messages.Add(message)));
        Assert.Equal(before, await File.ReadAllTextAsync(path));
        Assert.Contains(messages, message => message.Contains("旧版识别记录缺少引擎和启动信息") && message.Contains("请点击“重新识别”"));
        Assert.Equal(0, adapter.Probes);
        Assert.Equal(0, adapter.Extractions);
    }

    [Theory]
    [InlineData(false)]
    [InlineData(true)]
    public async Task FailedOrCancelledReanalysisRetainsCommittedSnapshotAndTranslations(bool cancel)
    {
        await WriteGame(("1", "hello"));
        var original = await CreatePipeline().AnalyzeAsync(Game, new JsonGameAdapter());
        await CreatePipeline().TranslateOnlyAsync(original, [new FakeProvider()], policy);
        var memory = await ReadSingleMemoryEntryAsync(original);
        var snapshotPath = Path.Combine(original.ProjectDirectory, "source.json");
        var before = await File.ReadAllTextAsync(snapshotPath);
        await WriteGame(("1", cancel ? "updated" : ""));
        using var cancellation = new CancellationTokenSource();

        Task<GameSession> Analyze() => CreatePipeline().AnalyzeAsync(Game, new JsonGameAdapter(), cancellation.Token,
            reportProgress: (stage, _) => { if (cancel && stage == "核对已保存译文") cancellation.Cancel(); });
        if (cancel) await Assert.ThrowsAnyAsync<OperationCanceledException>(Analyze);
        else await Assert.ThrowsAsync<InvalidDataException>(Analyze);

        Assert.Equal(before, await File.ReadAllTextAsync(snapshotPath));
        var restored = await CreatePipeline().LoadSessionAsync(Game, new JsonGameAdapter { FailIfProbed = true });
        Assert.NotNull(restored);
        Assert.Equal("hello", Assert.Single(restored.Extraction.Units).SourceText);
        Assert.Equal(memory, await ReadSingleMemoryEntryAsync(restored));
        await CreatePipeline().TranslateOnlyAsync(restored, [new FakeProvider { FailIfCalled = true }], policy);
    }

    [Fact]
    public async Task DeferredAnalysisReplacesSavedSessionOnlyAfterSuccessfulCommit()
    {
        await WriteGame(("1", "hello"));
        var original = await CreatePipeline().AnalyzeAsync(Game, new JsonGameAdapter());
        var path = Path.Combine(original.ProjectDirectory, "source.json");
        var before = await File.ReadAllTextAsync(path);
        await WriteGame(("1", "updated"));

        var prepared = await CreatePipeline().AnalyzeAsync(Game, new JsonGameAdapter(), commitSnapshot: false);

        Assert.Equal(before, await File.ReadAllTextAsync(path));
        var stillOriginal = await CreatePipeline().LoadSessionAsync(Game, new JsonGameAdapter { FailIfProbed = true });
        Assert.Equal("hello", Assert.Single(stillOriginal!.Extraction.Units).SourceText);
        using var cancellation = new CancellationTokenSource();
        cancellation.Cancel();
        await Assert.ThrowsAnyAsync<OperationCanceledException>(() => CreatePipeline().CommitAnalysisAsync(prepared, cancellation.Token));
        Assert.Equal(before, await File.ReadAllTextAsync(path));

        await CreatePipeline().CommitAnalysisAsync(prepared);

        var restored = await CreatePipeline().LoadSessionAsync(Game, new JsonGameAdapter { FailIfProbed = true });
        Assert.NotNull(restored);
        Assert.Equal(prepared.SourceDirectory, restored.SourceDirectory);
        Assert.Equal("updated", Assert.Single(restored.Extraction.Units).SourceText);
    }

    [Theory]
    [InlineData("version")]
    [InlineData("cache")]
    [InlineData("source")]
    public async Task MissingOrIncompatibleSavedAnalysisReportsWhyExplicitRecognitionIsRequired(string change)
    {
        await WriteGame(("1", "hello"));
        var original = await CreatePipeline().AnalyzeAsync(Game, new JsonGameAdapter());
        var adapter = new JsonGameAdapter { FailIfProbed = true, Version = change == "version" ? "2" : "1" };
        if (change == "source") File.Delete(Path.Combine(original.SourceDirectory, "story.json"));
        if (change == "cache")
        {
            var key = ExtractionStore.CacheKey(Path.GetFileName(original.SourceDirectory), adapter.Id, adapter.Version);
            await File.WriteAllTextAsync(Path.Combine(original.ProjectDirectory, "extraction", key, "manifest.json"), "{");
        }
        var messages = new List<string>();
        Assert.Null(await CreatePipeline().LoadSessionAsync(Game, adapter,
            reportProgress: (_, message) => messages.Add(message)));
        Assert.Contains(messages, message => message.Contains(change switch
        {
            "version" => "Adapter 或版本",
            "source" => "缺少资源",
            _ => "文本缓存缺失或已损坏"
        }));
        Assert.Equal(0, adapter.Probes);
        Assert.Equal(0, adapter.Extractions);
    }

    [Fact]
    public async Task TranslationStillRejectsChangedSnapshotBeforeSendingAnyModelRequest()
    {
        await WriteGame(("1", "hello"));
        var original = await CreatePipeline().AnalyzeAsync(Game, new JsonGameAdapter());
        await File.WriteAllTextAsync(Path.Combine(original.SourceDirectory, "story.json"), "damaged snapshot");
        var restored = await CreatePipeline().LoadSessionAsync(Game, new JsonGameAdapter { FailIfProbed = true });
        Assert.NotNull(restored);
        await Assert.ThrowsAsync<InvalidDataException>(() => CreatePipeline().RunAsync(restored,
            new FakeProvider { FailIfCalled = true }, policy));
    }

    [Fact]
    public async Task UnchangedAnalysisReusesVerifiedExtractionAndPersistsRelativePaths()
    {
        await WriteGame(("1", "hello"));
        var adapter = new JsonGameAdapter();
        var first = await CreatePipeline().AnalyzeAsync(Game, adapter);
        var second = await CreatePipeline().AnalyzeAsync(Game, adapter);
        Assert.Equal(1, adapter.Extractions);
        Assert.Equal(first.Extraction.Units, second.Extraction.Units);
        Assert.False(File.Exists(Path.Combine(first.ProjectDirectory, "extraction.json")));
        var snapshot = await LocalFiles.ReadJsonAsync<SourceSnapshot>(Path.Combine(first.ProjectDirectory, "source.json"));
        Assert.False(Path.IsPathRooted(snapshot!.Directory));
        var result = await CreatePipeline().RunAsync(second, new FakeProvider(), policy);
        var stored = await LocalFiles.ReadJsonAsync<PipelineResult>(Path.Combine(first.ProjectDirectory, "latest-result.json"));
        Assert.False(Path.IsPathRooted(stored!.PatchDirectory));
        Assert.Equal(result.PatchDirectory, (await CreatePipeline().LoadLatestResultAsync(first.ProjectDirectory))!.PatchDirectory);
        Assert.Single(Directory.GetDirectories(Path.Combine(first.ProjectDirectory, "tasks")));
    }

    [Fact]
    public async Task AnalysisChecksHashesEvenWhenResourceLengthAndTimestampAreUnchanged()
    {
        await WriteGame(("1", "hello"));
        var adapter = new JsonGameAdapter();
        var first = await CreatePipeline().AnalyzeAsync(Game, adapter);
        var resource = Path.Combine(Game, "story.json");
        var timestamp = File.GetLastWriteTimeUtc(resource);
        await WriteGame(("1", "world"));
        File.SetLastWriteTimeUtc(resource, timestamp);
        var changed = await CreatePipeline().AnalyzeAsync(Game, adapter);
        Assert.Equal(2, adapter.Extractions);
        Assert.NotEqual(first.SourceDirectory, changed.SourceDirectory);
        Assert.Equal("world", Assert.Single(changed.Extraction.Units).SourceText);
    }

    [Fact]
    public async Task DamagedUpdatedCacheReextractsInsteadOfUsingStaleLegacyText()
    {
        await WriteGame(("1", "old text"));
        var adapter = new JsonGameAdapter();
        var old = await CreatePipeline().AnalyzeAsync(Game, adapter);
        await LocalFiles.WriteJsonAsync(Path.Combine(old.ProjectDirectory, "extraction.json"), old.Extraction);
        await WriteGame(("1", "updated text"), ("2", "new text"));
        var updated = await CreatePipeline().AnalyzeAsync(Game, adapter);
        var cacheKey = ExtractionStore.CacheKey(Path.GetFileName(updated.SourceDirectory), adapter.Id, adapter.Version);
        await File.WriteAllTextAsync(Path.Combine(updated.ProjectDirectory, "extraction", cacheKey, "manifest.json"), "{");
        var recovered = await CreatePipeline().AnalyzeAsync(Game, adapter);
        Assert.Equal(3, adapter.Extractions);
        Assert.Equal(["updated text", "new text"], recovered.Extraction.Units.Select(unit => unit.SourceText));
    }

    [Fact]
    public async Task RepeatRunAndRestartReuseTranslationsAndPatch()
    {
        await WriteGame(("1", "hello"), ("2", "goodbye"));
        var adapter = new JsonGameAdapter();
        var provider = new FakeProvider();
        var firstSession = await CreatePipeline().AnalyzeAsync(Game, adapter);
        var first = await CreatePipeline().RunAsync(firstSession, provider, policy);
        var second = await CreatePipeline().RunAsync(await CreatePipeline().AnalyzeAsync(Game, adapter), provider, policy);
        Assert.Equal(1, provider.Calls);
        Assert.Equal(2, second.CacheHits);
        Assert.Equal(0, second.TranslationUsage.TotalTokens);
        Assert.Equal(first.PatchDirectory, second.PatchDirectory);
        Assert.Equal(1, adapter.Builds);
    }

    [Fact]
    public async Task UpdateTranslatesOnlyChangedUnitAndReusesOtherTexts()
    {
        await WriteGame(("1", "hello"), ("2", "goodbye"));
        var provider = new FakeProvider();
        await CreatePipeline().RunAsync(await CreatePipeline().AnalyzeAsync(Game, new JsonGameAdapter()), provider, policy);
        await WriteGame(("inserted", "new"), ("1", "hello"), ("2", "changed"));
        var result = await CreatePipeline().RunAsync(await CreatePipeline().AnalyzeAsync(Game, new JsonGameAdapter()), provider, policy);
        Assert.Equal(2, provider.Calls);
        Assert.Equal(new[] { "new", "changed" }, provider.Batches.Last().Select(u => u.SourceText));
        Assert.Equal(1, result.CacheHits);
    }

    [Fact]
    public async Task ResponseCommittedBeforeCancellationRecoversWithoutRepeatingApi()
    {
        await WriteGame(("1", "hello"), ("2", "goodbye"));
        var session = await CreatePipeline().AnalyzeAsync(Game, new JsonGameAdapter());
        using var cancellation = new CancellationTokenSource();
        var provider = new FakeProvider { AfterResponse = cancellation.Cancel };
        await Assert.ThrowsAnyAsync<OperationCanceledException>(() => CreatePipeline().RunAsync(session, provider, policy, ct: cancellation.Token));
        // Simulate interruption after a response was durably recorded but before all memory entries were committed.
        foreach (var file in Directory.GetFiles(Path.Combine(session.ProjectDirectory, "memory"), "??.json")) File.Delete(file);
        var resumed = await CreatePipeline().RunAsync(session, new FakeProvider { FailIfCalled = true }, policy);
        Assert.Equal(2, resumed.CacheHits);
        Assert.True(resumed.Validation.Passed);
    }

    [Fact]
    public async Task UnknownRequestOutcomeRequiresExplicitResendEvenWhenBatchSizeChanges()
    {
        await WriteGame(("1", "hello"), ("2", "goodbye"));
        var session = await CreatePipeline().AnalyzeAsync(Game, new JsonGameAdapter());
        var failing = new FakeProvider { Failure = new TranslationProviderException("unknown", outcomeUnknown: true) };
        await Assert.ThrowsAsync<TranslationRetriesExhaustedException>(() => CreatePipeline().RunAsync(session, failing, policy));
        Assert.Equal(6, failing.Calls);
        var provider = new FakeProvider();
        await Assert.ThrowsAsync<UncertainRequestException>(() => CreatePipeline().RunAsync(session, provider, policy, new TranslationOptions(1, RetryUncertainRequests: false)));
        Assert.Equal(0, provider.Calls);
        var result = await CreatePipeline().RunAsync(session, provider, policy, new TranslationOptions(1, true));
        Assert.True(result.Validation.Passed);
        Assert.Equal(2, provider.Calls);
    }

    [Fact]
    public async Task ExplicitProviderRejectionDoesNotBecomeAnUnknownOutcome()
    {
        await WriteGame(("1", "hello"));
        var session = await CreatePipeline().AnalyzeAsync(Game, new JsonGameAdapter());
        var failing = new FakeProvider { Failure = new TranslationProviderException("HTTP429", statusCode: System.Net.HttpStatusCode.TooManyRequests) };
        var unused = new FakeProvider { Identity = "unused", FailIfCalled = true };
        await Assert.ThrowsAsync<TranslationRetriesExhaustedException>(() => CreatePipeline().RunAsync(session,
            new ITranslationProvider[] { failing, unused }, policy));
        Assert.Equal(6, failing.Calls);
        Assert.Empty(Directory.GetFiles(Path.Combine(session.ProjectDirectory, "responses"), "*.refusal.json"));
        var result = await CreatePipeline().RunAsync(session, new FakeProvider(), policy);
        Assert.True(result.Validation.Passed);
    }

    [Fact]
    public async Task InvalidPlaceholderRetriesOnlyFailedText()
    {
        await WriteGame(("1", "hello"), ("2", "hello {name}"));
        var session = await CreatePipeline().AnalyzeAsync(Game, new JsonGameAdapter());
        var provider = new FakeProvider { Translate = u => u.Id == "2" ? "错误" : "你好" };
        var logs = Path.Combine(root, "logs");
        var error = await Assert.ThrowsAsync<InvalidDataException>(() => new TranslationPipeline(Data, logs).RunAsync(session, provider, policy));
        Assert.Contains("story.json", error.Message);
        Assert.Contains("占位符或控制标记与原文不一致", error.Message);
        using var log = JsonDocument.Parse(await File.ReadAllTextAsync(Assert.Single(Directory.GetFiles(logs, "validation-*.json"))));
        Assert.Equal("placeholder-mismatch", log.RootElement.GetProperty("issues")[0].GetProperty("code").GetString());
        Assert.Equal("hello {name}", log.RootElement.GetProperty("texts")[0].GetProperty("sourceText").GetString());
        Assert.Equal("错误", log.RootElement.GetProperty("texts")[0].GetProperty("translation").GetString());
        var retry = new FakeProvider();
        await CreatePipeline().RunAsync(session, retry, policy);
        Assert.Equal("2", Assert.Single(Assert.Single(retry.Batches)).Id);
        var feedback = Assert.Single(Assert.Single(retry.Requests).ValidationFeedback!);
        Assert.Equal("2", feedback.Key);
        Assert.Contains("{name}", Assert.Single(feedback.Value));
        Assert.Contains("原文 1 次，译文 0 次", feedback.Value[0]);
        Assert.Empty((await LocalFiles.ReadJsonAsync<ValidationIssue[]>(Path.Combine(session.ProjectDirectory, "translation-errors.json")))!);
    }

    [Theory]
    [InlineData(false)]
    [InlineData(true)]
    public async Task ValidationFeedbackDoesNotApplyToChangedSourceOrPolicy(bool changeSource)
    {
        await WriteGame(("1", "hello {name}"));
        var session = await CreatePipeline().AnalyzeAsync(Game, new JsonGameAdapter());
        await Assert.ThrowsAsync<InvalidDataException>(() => CreatePipeline().RunAsync(session,
            new FakeProvider { Translate = _ => "缺少标记" }, policy));
        var updatedPolicy = policy;
        if (changeSource)
        {
            await WriteGame(("1", "goodbye {name}"));
            session = await CreatePipeline().AnalyzeAsync(Game, new JsonGameAdapter());
        }
        else updatedPolicy = policy with { Style = "concise" };
        var retry = new FakeProvider();
        await CreatePipeline().RunAsync(session, retry, updatedPolicy);
        Assert.Null(Assert.Single(retry.Requests).ValidationFeedback);
    }

    [Theory]
    [InlineData(false)]
    [InlineData(true)]
    public async Task ValidationFeedbackReadsLegacyErrorsAndIgnoresDamagedHints(bool damaged)
    {
        await WriteGame(("1", "hello {name}"));
        var session = await CreatePipeline().AnalyzeAsync(Game, new JsonGameAdapter());
        var path = Path.Combine(session.ProjectDirectory, "translation-errors.json");
        if (damaged) await File.WriteAllTextAsync(path, "{truncated");
        else await LocalFiles.WriteJsonAsync(path, new[] { new ValidationIssue("placeholder-mismatch", "缺少{name}", "1") });
        var provider = new FakeProvider();
        await CreatePipeline().RunAsync(session, provider, policy);
        var feedback = Assert.Single(provider.Requests).ValidationFeedback;
        if (damaged) Assert.Null(feedback);
        else Assert.Equal("缺少{name}", Assert.Single(feedback!["1"]));
    }

    [Fact]
    public async Task PartialResponseCommitsValidEntriesAndRetriesOnlyMissingText()
    {
        await WriteGame(("1", "hello"), ("2", "goodbye"));
        var session = await CreatePipeline().AnalyzeAsync(Game, new JsonGameAdapter());
        var partial = new FakeProvider { ResponseTranslations = _ => new Dictionary<string, string> { ["1"] = "你好" } };
        await Assert.ThrowsAsync<InvalidDataException>(() => CreatePipeline().RunAsync(session, partial, policy));
        var resumed = new FakeProvider();
        var result = await CreatePipeline().RunAsync(session, resumed, policy);
        Assert.Equal(1, result.CacheHits);
        Assert.Equal("2", Assert.Single(Assert.Single(resumed.Batches)).Id);
        Assert.Single(Directory.GetFiles(Path.Combine(session.ProjectDirectory, "logs"), "validation-*.json"));
    }

    [Theory]
    [InlineData(false)]
    [InlineData(true)]
    public async Task InstalledGameReadsBackupAndRestoresOriginal(bool deleteBackup)
    {
        await WriteGame(("1", "hello"));
        var original = await File.ReadAllBytesAsync(Path.Combine(Game, "story.json"));
        var session = await CreatePipeline().AnalyzeAsync(Game, new JsonGameAdapter());
        var result = await CreatePipeline().RunAsync(session, new FakeProvider(), policy);
        var installer = new PatchInstaller(session.ProjectDirectory);
        await installer.InstallAsync(Game, result.PatchDirectory);
        if (deleteBackup) File.Delete(Path.Combine(Game, "story.json.bak"));
        var reimport = await CreatePipeline().AnalyzeAsync(Game, new JsonGameAdapter());
        Assert.Equal("hello", Assert.Single(reimport.Extraction.Units).SourceText);
        var repeat = await CreatePipeline().RunAsync(reimport, new FakeProvider { FailIfCalled = true }, policy);
        Assert.Equal(1, repeat.CacheHits);
        await installer.RestoreAsync(Game);
        Assert.Equal(original, await File.ReadAllBytesAsync(Path.Combine(Game, "story.json")));
    }

    [Fact]
    public async Task PatchRejectsGameUpdatedAfterGeneration()
    {
        await WriteGame(("1", "hello"));
        var session = await CreatePipeline().AnalyzeAsync(Game, new JsonGameAdapter());
        var result = await CreatePipeline().RunAsync(session, new FakeProvider(), policy);
        await WriteGame(("1", "updated"));
        await Assert.ThrowsAsync<IOException>(() => new PatchInstaller(session.ProjectDirectory).InstallAsync(Game, result.PatchDirectory));
        Assert.Contains("updated", await File.ReadAllTextAsync(Path.Combine(Game, "story.json")));
        Assert.False(File.Exists(Path.Combine(Game, "story.json.bak")));
    }

    [Fact]
    public async Task TerminologyChangesInvalidateOnlyAffectedUnits()
    {
        await WriteGame(("1", "Alice hello"), ("2", "goodbye"));
        var session = await CreatePipeline().AnalyzeAsync(Game, new JsonGameAdapter());
        await CreatePipeline().RunAsync(session, new FakeProvider(), policy);
        var provider = new FakeProvider();
        await CreatePipeline().RunAsync(session, provider, policy with { Terminology = new Dictionary<string, string> { ["Alice"] = "爱丽丝" } });
        Assert.Equal("1", Assert.Single(Assert.Single(provider.Batches)).Id);
    }

    private Task WriteGame(params (string Id, string Text)[] lines) => LocalFiles.WriteJsonAsync(Path.Combine(Game, "story.json"),
        lines.Select(line => new Line(line.Id, line.Text)).ToArray());

    [Fact]
    public async Task RetryProgressShowsFailureCategoryWhileKeepingDeveloperDetailsInSavedError()
    {
        await WriteGame(("1", "hello"));
        var session = await CreatePipeline().AnalyzeAsync(Game, new JsonGameAdapter());
        var failure = new TranslationProviderException("Detailed developer error", diagnostic:
            new("translation-duplicate-id", "Repeated property", "Private response content"));
        var provider = new FakeProvider { Failure = failure };
        using var cancellation = new CancellationTokenSource();
        var messages = new List<string>();
        var progress = new ImmediateProgress(update =>
        {
            messages.Add(update.Message);
            if (update.Stage == "等待重试") cancellation.Cancel();
        });
        await Assert.ThrowsAnyAsync<OperationCanceledException>(() => CreatePipeline().RunAsync(session, provider, policy,
            progress: progress, ct: cancellation.Token));
        Assert.Equal(1, provider.Calls);
        Assert.Contains(messages, message => message.Contains("返回格式不正确：文本 ID 重复") && message.Contains("第 1 / 5 次重试"));
        Assert.DoesNotContain(messages, message => message.Contains("Detailed developer error") || message.Contains("Private response content"));
        var savedError = await File.ReadAllTextAsync(Assert.Single(Directory.GetFiles(Path.Combine(session.ProjectDirectory, "responses"), "*.error-*.json")));
        Assert.Contains("Private response content", savedError);
        var exhausted = new TranslationRetriesExhaustedException(failure);
        Assert.Contains("文本 ID 重复", exhausted.Message);
        Assert.DoesNotContain("Detailed developer error", exhausted.Message);
    }

    private sealed class ImmediateProgress(Action<PipelineProgress> report) : IProgress<PipelineProgress>
    {
        public void Report(PipelineProgress value) => report(value);
    }

    [Fact]
    public async Task RefusalPausesImmediatelyAndRestartPreservesCompletedTranslationsWithoutResending()
    {
        await WriteGame(("1", "hello"), ("2", "goodbye"));
        var session = await CreatePipeline().AnalyzeAsync(Game, new JsonGameAdapter());
        await CreatePipeline().RunAsync(session, new FakeProvider(), policy, new TranslationOptions(UnitIds: ["1"]));
        var provider = new FakeProvider { Failure = new TranslationProviderException("Refused", usage: new TokenUsage(10, 5),
            diagnostic: new("model-refusal", ResponsePreview: "I cannot assist with this request.")) };
        var updates = new List<PipelineProgress>();
        await Assert.ThrowsAsync<TranslationRefusedException>(() => CreatePipeline().RunAsync(session, provider, policy,
            progress: new ImmediateProgress(updates.Add)));
        Assert.Equal(1, provider.Calls);
        Assert.Equal("2", Assert.Single(Assert.Single(provider.Batches)).Id);
        Assert.DoesNotContain(updates, update => update.Stage == "等待重试");
        Assert.Equal("已暂停", updates[^1].Stage);
        Assert.Equal(15, updates[^1].TranslationTokens);
        Assert.Equal(1, updates[^1].CacheHits);
        Assert.Single(Directory.GetFiles(Path.Combine(session.ProjectDirectory, "memory"), "??.json"));
        Assert.Single(Directory.GetFiles(Path.Combine(session.ProjectDirectory, "responses"), "*.refusal.json"));

        var resumed = new FakeProvider { FailIfCalled = true };
        var error = await Assert.ThrowsAsync<TranslationRefusedException>(() => CreatePipeline().RunAsync(session, resumed, policy,
            new TranslationOptions(RetryUncertainRequests: false), new ImmediateProgress(updates.Add)));
        Assert.Equal(0, resumed.Calls);
        Assert.Contains("未再次调用 API", error.Message);
        Assert.Equal(0, updates[^1].TranslationTokens);
        Assert.Equal(1, updates[^1].CacheHits);
    }

    [Theory]
    [InlineData("translation-json-syntax", "抱歉，我无法翻译这段内容。")]
    [InlineData("translation-unknown-id", "{\"error\":\"抱歉，我无法翻译这段内容。\"}")]
    public async Task RecoversPreviouslyMisclassifiedRefusalWithoutAnotherApiCall(string code, string content)
    {
        await WriteGame(("1", "hello"));
        var session = await CreatePipeline().AnalyzeAsync(Game, new JsonGameAdapter());
        var legacy = new FakeProvider { Failure = new TranslationProviderException("Old format error", diagnostic: new(code, ResponsePreview: content)) };
        using var cancellation = new CancellationTokenSource();
        await Assert.ThrowsAnyAsync<OperationCanceledException>(() => CreatePipeline().RunAsync(session, legacy, policy,
            progress: new ImmediateProgress(update => { if (update.Stage == "等待重试") cancellation.Cancel(); }), ct: cancellation.Token));
        Assert.Equal(1, legacy.Calls);

        var resumed = new FakeProvider { FailIfCalled = true };
        await Assert.ThrowsAsync<TranslationRefusedException>(() => CreatePipeline().RunAsync(session, resumed, policy));
        Assert.Equal(0, resumed.Calls);
        var record = await LocalFiles.ReadJsonAsync<TranslationRefusal>(Assert.Single(Directory.GetFiles(
            Path.Combine(session.ProjectDirectory, "responses"), "*.refusal.json")));
        Assert.Equal("model-refusal", record!.Diagnostic!.Code);
        Assert.Equal(content, record.Diagnostic.ResponsePreview);
    }


    private static TranslationProviderException Refusal() => new("Refused", usage: new TokenUsage(10, 5, Estimated: true),
        diagnostic: new("model-refusal", ResponsePreview: "I cannot assist with this request."));

    [Fact]
    public async Task ChainPassesSameBatchUntilSuccessAndCountsEveryAttempt()
    {
        await WriteGame(("1", "hello"));
        var session = await CreatePipeline().AnalyzeAsync(Game, new JsonGameAdapter());
        var first = new FakeProvider { Identity = "first", Failure = Refusal() };
        var second = new FakeProvider { Identity = "second", Failure = Refusal() };
        var third = new FakeProvider { Identity = "third" };
        var unused = new FakeProvider { Identity = "unused", FailIfCalled = true };
        var updates = new List<PipelineProgress>();
        var result = await CreatePipeline().RunAsync(session, new ITranslationProvider[] { first, second, third, unused }, policy,
            progress: new ImmediateProgress(updates.Add));
        Assert.True(result.Validation.Passed);
        Assert.Equal(1, first.Calls);
        Assert.Equal(1, second.Calls);
        Assert.Equal(1, third.Calls);
        Assert.Same(first.Batches[0], second.Batches[0]);
        Assert.Same(first.Batches[0], third.Batches[0]);
        Assert.Equal(60, result.TranslationUsage.TotalTokens);
        Assert.True(result.TranslationUsage.Estimated);
        Assert.Equal(2, updates.Count(update => update.Stage == "切换翻译 API"));
        Assert.DoesNotContain(updates, update => update.Stage == "等待重试");
        var entry = await ReadSingleMemoryEntryAsync(session);
        Assert.Equal("third", entry!.Provider);
        Assert.Equal(2, Directory.GetFiles(Path.Combine(session.ProjectDirectory, "responses"), "*.refusal.json").Length);
        var cached = await CreatePipeline().RunAsync(session, new FakeProvider { FailIfCalled = true }, policy);
        Assert.Equal(1, cached.CacheHits);
        Assert.Equal(0, cached.TranslationUsage.TotalTokens);
    }

    [Fact]
    public async Task EachNewBatchStartsAtFirstProviderAndStopsAfterSuccess()
    {
        await WriteGame(("1", "hello"), ("2", "goodbye"));
        var session = await CreatePipeline().AnalyzeAsync(Game, new JsonGameAdapter());
        var first = new FakeProvider { Identity = "first", FailureFor = request => request.Units[0].Id == "1" ? Refusal() : null };
        var second = new FakeProvider { Identity = "second" };
        var unused = new FakeProvider { Identity = "unused", FailIfCalled = true };
        var result = await CreatePipeline().RunAsync(session, new ITranslationProvider[] { first, second, unused }, policy, new TranslationOptions(1));
        Assert.True(result.Validation.Passed);
        Assert.Equal(2, first.Calls);
        Assert.Equal("1", Assert.Single(Assert.Single(second.Batches)).Id);
    }

    [Fact]
    public async Task ExhaustedChainSkipsRecordedRefusalsAndCanResumeWithNewProvider()
    {
        await WriteGame(("1", "hello"));
        var session = await CreatePipeline().AnalyzeAsync(Game, new JsonGameAdapter());
        var first = new FakeProvider { Identity = "first", Failure = Refusal() };
        var second = new FakeProvider { Identity = "second", Failure = Refusal() };
        var updates = new List<PipelineProgress>();
        await Assert.ThrowsAsync<TranslationRefusedException>(() => CreatePipeline().RunAsync(session,
            new ITranslationProvider[] { first, second }, policy, progress: new ImmediateProgress(updates.Add)));
        Assert.Equal(1, first.Calls);
        Assert.Equal(1, second.Calls);
        Assert.Equal(30, updates[^1].TranslationTokens);
        Assert.Equal("已暂停", updates[^1].Stage);
        var recorded = new ITranslationProvider[]
        {
            new FakeProvider { Identity = "first", FailIfCalled = true },
            new FakeProvider { Identity = "second", FailIfCalled = true }
        };
        var error = await Assert.ThrowsAsync<TranslationRefusedException>(() => CreatePipeline().RunAsync(session,
            recorded, policy, new TranslationOptions(RetryUncertainRequests: false), new ImmediateProgress(updates.Add)));
        Assert.Contains("未再次调用 API", error.Message);
        Assert.Equal(0, updates[^1].TranslationTokens);
        var next = new FakeProvider { Identity = "third" };
        var result = await CreatePipeline().RunAsync(session, recorded.Append(next).ToArray(), policy, new TranslationOptions(RetryUncertainRequests: false));
        Assert.Equal(1, next.Calls);
        Assert.Equal(30, result.TranslationUsage.TotalTokens);
        Assert.True(result.Validation.Passed);
    }

    [Theory]
    [InlineData(429, "http-error")]
    [InlineData(401, "http-error")]
    [InlineData(500, "http-error")]
    [InlineData(0, "network-error")]
    [InlineData(0, "request-timeout")]
    [InlineData(0, "translation-json-syntax")]
    [InlineData(0, "output-token-limit")]
    [InlineData(0, "empty-content")]
    [InlineData(0, "responses-incomplete")]
    public async Task NonRefusalRetriesCurrentProviderWithoutAdvancingChain(int status, string code)
    {
        await WriteGame(("1", "hello"));
        var session = await CreatePipeline().AnalyzeAsync(Game, new JsonGameAdapter());
        var first = new FakeProvider { Identity = "first", Failure = Refusal() };
        var current = new FakeProvider { Identity = "current", Failure = new TranslationProviderException("failure",
            statusCode: status == 0 ? null : (System.Net.HttpStatusCode)status, diagnostic: new(code)) };
        var unused = new FakeProvider { Identity = "unused", FailIfCalled = true };
        using var cancellation = new CancellationTokenSource();
        var updates = new List<PipelineProgress>();
        var progress = new ImmediateProgress(update =>
        {
            updates.Add(update);
            if (update.Stage == "等待重试") cancellation.Cancel();
        });
        await Assert.ThrowsAnyAsync<OperationCanceledException>(() => CreatePipeline().RunAsync(session,
            new ITranslationProvider[] { first, current, unused }, policy, progress: progress, ct: cancellation.Token));
        Assert.Equal(1, first.Calls);
        Assert.Equal(1, current.Calls);
        Assert.Contains(updates, update => update.Stage == "等待重试" && update.Message.Contains("第 2 / 3 个翻译 API"));
        Assert.Single(Directory.GetFiles(Path.Combine(session.ProjectDirectory, "responses"), "*.refusal.json"));
    }

    [Fact]
    public async Task CancellationBetweenProvidersStopsImmediatelyAndResumeSkipsRefusedProvider()
    {
        await WriteGame(("1", "hello"));
        var session = await CreatePipeline().AnalyzeAsync(Game, new JsonGameAdapter());
        var first = new FakeProvider { Identity = "first", Failure = Refusal() };
        using var cancellation = new CancellationTokenSource();
        await Assert.ThrowsAnyAsync<OperationCanceledException>(() => CreatePipeline().RunAsync(session,
            new ITranslationProvider[] { first, new FakeProvider { Identity = "second", FailIfCalled = true } }, policy,
            progress: new ImmediateProgress(update => { if (update.Stage == "切换翻译 API") cancellation.Cancel(); }),
            ct: cancellation.Token));
        var second = new FakeProvider { Identity = "second" };
        var result = await CreatePipeline().RunAsync(session, new ITranslationProvider[]
        {
            new FakeProvider { Identity = "first", FailIfCalled = true }, second
        }, policy, new TranslationOptions(RetryUncertainRequests: false));
        Assert.Equal(1, second.Calls);
        Assert.True(result.Validation.Passed);
    }

    [Fact]
    public async Task FallbackResponseRecoversAfterCancellationWithoutCallingAnyProvider()
    {
        await WriteGame(("1", "hello"));
        var session = await CreatePipeline().AnalyzeAsync(Game, new JsonGameAdapter());
        using var cancellation = new CancellationTokenSource();
        await Assert.ThrowsAnyAsync<OperationCanceledException>(() => CreatePipeline().RunAsync(session,
            new ITranslationProvider[]
            {
                new FakeProvider { Identity = "first", Failure = Refusal() },
                new FakeProvider { Identity = "second", AfterResponse = cancellation.Cancel }
            }, policy, ct: cancellation.Token));
        foreach (var file in Directory.GetFiles(Path.Combine(session.ProjectDirectory, "memory"), "??.json")) File.Delete(file);
        var result = await CreatePipeline().RunAsync(session, new ITranslationProvider[]
        {
            new FakeProvider { Identity = "first", FailIfCalled = true },
            new FakeProvider { Identity = "second", FailIfCalled = true }
        }, policy, new TranslationOptions(RetryUncertainRequests: false));
        Assert.Equal(1, result.CacheHits);
        var entry = await ReadSingleMemoryEntryAsync(session);
        Assert.Equal("second", entry!.Provider);
    }

    [Fact]
    public async Task WorkbenchTranslationDoesNotBuildAndManualEditsWinWithoutChangingModelMemory()
    {
        await WriteGame(("1", "hello"), ("2", "goodbye"));
        var adapter = new JsonGameAdapter();
        var session = await CreatePipeline().AnalyzeAsync(Game, adapter);
        var provider = new FakeProvider();
        await CreatePipeline().TranslateOnlyAsync(session, [provider], policy);
        Assert.Equal(0, adapter.Builds);
        Assert.False(File.Exists(Path.Combine(session.ProjectDirectory, "latest-result.json")));
        var unit = session.Extraction.Units[0];
        await new TranslationEdits(1, new Dictionary<string, TranslationEdit>
        {
            [unit.Id] = new(TranslationEdits.Signature(unit), policy.TargetLanguage, "人工校对", true, DateTimeOffset.UtcNow)
        }).SaveAsync(session.ProjectDirectory);
        await CreatePipeline().TranslateOnlyAsync(session, [new FakeProvider { FailIfCalled = true }], policy);
        var patch = await CreatePipeline().BuildFromSavedAsync(session, policy, reviewedOnly: true);
        var output = await LocalFiles.ReadJsonAsync<Line[]>(Path.Combine(patch.PatchDirectory, "story.json"));
        Assert.Equal("人工校对", output![0].Text);
        Assert.Equal("goodbye", output[1].Text);
        Assert.Equal("翻译 hello", await new TranslationMemory(Path.Combine(session.ProjectDirectory, "memory")).FindAsync(unit, policy));
        var index = await LocalFiles.ReadJsonAsync<PatchTranslationSnapshot>(Path.Combine(patch.PatchDirectory, "translation-snapshot.json"));
        Assert.Equal("人工校对", Assert.Single(index!.Texts).Value);
    }

    [Fact]
    public async Task WorkbenchInvalidDraftIsProtectedButCannotBuildOrApplyToChangedSource()
    {
        await WriteGame(("1", "hello {name}"));
        var session = await CreatePipeline().AnalyzeAsync(Game, new JsonGameAdapter());
        var unit = Assert.Single(session.Extraction.Units);
        var edits = new TranslationEdits(1, new Dictionary<string, TranslationEdit>
        {
            [unit.Id] = new(TranslationEdits.Signature(unit), policy.TargetLanguage, "草稿", false, DateTimeOffset.UtcNow)
        });
        await edits.SaveAsync(session.ProjectDirectory);
        await CreatePipeline().TranslateOnlyAsync(session, [new FakeProvider { FailIfCalled = true }], policy);
        await Assert.ThrowsAsync<InvalidDataException>(() => CreatePipeline().BuildFromSavedAsync(session, policy));
        Assert.Null(edits.Find(unit with { SourceText = "updated" }, policy.TargetLanguage));
        Assert.Null(edits.Find(unit with { Context = "another scene" }, policy.TargetLanguage));
        Assert.Null(edits.Find(unit, "English"));
    }

    [Fact]
    public async Task WorkbenchCandidateDoesNotReplaceSavedTextAndIsNotRecoveredAsAcceptedTranslation()
    {
        await WriteGame(("1", "hello"));
        var session = await CreatePipeline().AnalyzeAsync(Game, new JsonGameAdapter());
        var unit = Assert.Single(session.Extraction.Units);
        var memory = new TranslationMemory(Path.Combine(session.ProjectDirectory, "memory"));
        await memory.StoreAsync(unit, policy, "已有译文", "test");
        var candidate = await CreatePipeline().CreateCandidateAsync(session, unit, [new FakeProvider { Translate = _ => "新候选" }], policy);
        Assert.Equal("新候选", candidate);
        Assert.Equal("已有译文", await new TranslationMemory(Path.Combine(session.ProjectDirectory, "memory")).FindAsync(unit, policy));
        // An interrupted candidate receipt must never become an accepted model translation.
        var responses = Path.Combine(session.ProjectDirectory, "responses");
        await LocalFiles.WriteJsonAsync(Path.Combine(responses, "candidate.request.json"), new TranslationIntent("candidate", "test", DateTimeOffset.UtcNow,
            [new(unit.Id, memory.Key(unit, policy with { Style = "other" }))], CandidateOnly: true));
        await LocalFiles.WriteJsonAsync(Path.Combine(responses, "candidate.json"), new TranslationResponse(new Dictionary<string, string> { [unit.Id] = "未采用候选" }, new(0, 0)));
        var provider = new FakeProvider();
        await CreatePipeline().TranslateOnlyAsync(session, [provider], policy with { Style = "other" });
        Assert.Equal(1, provider.Calls);
    }

    [Fact]
    public async Task WorkbenchPatchSnapshotRemainsUnchangedAfterLaterEditAndSelectionKeepsOtherSources()
    {
        await WriteGame(("1", "hello"), ("2", "goodbye"));
        var session = await CreatePipeline().AnalyzeAsync(Game, new JsonGameAdapter());
        var unit = session.Extraction.Units[0];
        async Task SaveEdit(string text) => await new TranslationEdits(1, new Dictionary<string, TranslationEdit>
        { [unit.Id] = new(TranslationEdits.Signature(unit), policy.TargetLanguage, text, false, DateTimeOffset.UtcNow) }).SaveAsync(session.ProjectDirectory);
        await SaveEdit("第一稿");
        var first = await CreatePipeline().BuildFromSavedAsync(session, policy, [unit.Id]);
        await SaveEdit("第二稿");
        var second = await CreatePipeline().BuildFromSavedAsync(session, policy, [unit.Id]);
        Assert.NotEqual(first.PatchDirectory, second.PatchDirectory);
        Assert.Equal("第一稿", (await LocalFiles.ReadJsonAsync<Line[]>(Path.Combine(first.PatchDirectory, "story.json")))![0].Text);
        var output = (await LocalFiles.ReadJsonAsync<Line[]>(Path.Combine(second.PatchDirectory, "story.json")))!;
        Assert.Equal("第二稿", output[0].Text); Assert.Equal("goodbye", output[1].Text);
    }

    [Fact]
    public async Task WorkbenchSkipsCorrectionsSavedWhileEarlierBatchWasInFlight()
    {
        await WriteGame(("1", "hello"), ("2", "goodbye"));
        var session = await CreatePipeline().AnalyzeAsync(Game, new JsonGameAdapter());
        var unit = session.Extraction.Units[1];
        var provider = new FakeProvider { AfterResponse = () => new TranslationEdits(1, new Dictionary<string, TranslationEdit>
        { [unit.Id] = new(TranslationEdits.Signature(unit), policy.TargetLanguage, "手工译文", false, DateTimeOffset.UtcNow) }).SaveAsync(session.ProjectDirectory).GetAwaiter().GetResult() };
        await CreatePipeline().TranslateOnlyAsync(session, [provider], policy, new(1));
        Assert.Equal(1, provider.Calls);
        Assert.Equal("手工译文", (await TranslationEdits.LoadAsync(session.ProjectDirectory)).Find(unit, policy.TargetLanguage)!.Text);
    }

    [Fact]
    public async Task SingleUnitProviderLimitCommitsEachTranslationAndResumesAfterInterruption()
    {
        await WriteGame(("1", "hello"), ("2", "goodbye"), ("3", "welcome"));
        var session = await CreatePipeline().AnalyzeAsync(Game, new JsonGameAdapter());
        var first = new FakeProvider { Identity = "refusing", Failure = Refusal() };
        var local = new FakeProvider
        {
            Identity = "local", MaximumBatchSize = 1,
            FailureFor = request => request.Units[0].Id == "2" ? new OperationCanceledException() : null
        };
        await Assert.ThrowsAsync<OperationCanceledException>(() => CreatePipeline().TranslateOnlyAsync(session, [first, local], policy, new(50)));
        Assert.All(local.Batches, batch => Assert.Single(batch));
        var resumed = new FakeProvider { Identity = "local", MaximumBatchSize = 1 };
        await CreatePipeline().TranslateOnlyAsync(session, [first, resumed], policy, new(50));
        Assert.Equal(new[] { "2", "3" }, resumed.Batches.Select(batch => Assert.Single(batch).Id));
        Assert.All(first.Batches, batch => Assert.Single(batch));
        var memory = new TranslationMemory(Path.Combine(session.ProjectDirectory, "memory"));
        foreach (var unit in session.Extraction.Units)
            Assert.Equal("翻译 " + unit.SourceText, await memory.FindAsync(unit, policy));
    }

    [Fact]
    public async Task RetranslationReplacesOnlySelectionEvenWhenUnselectedTextSharesItsMemoryKey()
    {
        await WriteGame(("1", "same"), ("2", "same"), ("3", "other"));
        var session = await CreatePipeline().AnalyzeAsync(Game, new JsonGameAdapter());
        await CreatePipeline().TranslateOnlyAsync(session, [new FakeProvider()], policy);
        var first = session.Extraction.Units[0];
        await new TranslationEdits(1, new Dictionary<string, TranslationEdit>
        {
            [first.Id] = new(TranslationEdits.Signature(first), policy.TargetLanguage, "旧校对", true, DateTimeOffset.UtcNow)
        }).SaveAsync(session.ProjectDirectory);
        var provider = new FakeProvider { Translate = _ => "新译文" };
        var options = new TranslationOptions(50, UnitIds: [first.Id]) { RetranslationId = Guid.NewGuid().ToString("N") };
        await CreatePipeline().TranslateOnlyAsync(session, [provider], policy, options);
        Assert.Equal(first.Id, Assert.Single(Assert.Single(provider.Batches)).Id);
        var edit = (await TranslationEdits.LoadAsync(session.ProjectDirectory)).Find(first, policy.TargetLanguage)!;
        Assert.Equal("新译文", edit.Text); Assert.False(edit.Reviewed); Assert.True(edit.IsGenerated);
        Assert.Equal(options.RetranslationId, edit.RetranslationId);
        var patch = await CreatePipeline().BuildFromSavedAsync(session, policy);
        var output = (await LocalFiles.ReadJsonAsync<Line[]>(Path.Combine(patch.PatchDirectory, "story.json")))!;
        Assert.Equal(new[] { "新译文", "翻译 same", "翻译 other" }, output.Select(line => line.Text));
        await CreatePipeline().TranslateOnlyAsync(session, [new FakeProvider { FailIfCalled = true }], policy, options);
        var fresh = new FakeProvider { Translate = _ => "下一轮" };
        await CreatePipeline().TranslateOnlyAsync(session, [fresh], policy, options with { RetranslationId = Guid.NewGuid().ToString("N") });
        Assert.Equal(1, fresh.Calls);
    }

    [Fact]
    public async Task RetranslationKeepsOldTextOnValidationFailureAndResumesOnlyFailedUnits()
    {
        await WriteGame(("1", "hello {name}"), ("2", "goodbye {name}"));
        var session = await CreatePipeline().AnalyzeAsync(Game, new JsonGameAdapter());
        await CreatePipeline().TranslateOnlyAsync(session, [new FakeProvider()], policy);
        var options = new TranslationOptions(50, UnitIds: ["1", "2"]) { RetranslationId = Guid.NewGuid().ToString("N") };
        var provider = new FakeProvider { Translate = unit => unit.Id == "1" ? "你好 {name}" : "再见" };
        await Assert.ThrowsAsync<InvalidDataException>(() => CreatePipeline().TranslateOnlyAsync(session, [provider], policy, options));
        var saved = await TranslationEdits.LoadAsync(session.ProjectDirectory);
        Assert.Equal("你好 {name}", saved.Entries["1"].Text); Assert.False(saved.Entries.ContainsKey("2"));
        var memory = new TranslationMemory(Path.Combine(session.ProjectDirectory, "memory"));
        Assert.Equal("翻译 goodbye {name}", await memory.FindAsync(session.Extraction.Units[1], policy));
        // A review made after pausing remains accepted when this operation resumes.
        await (saved with { Entries = saved.Entries.ToDictionary(pair => pair.Key,
            pair => pair.Value with { Text = "手工修订 {name}", Reviewed = true, IsGenerated = false }) }).SaveAsync(session.ProjectDirectory);
        var resumed = new FakeProvider { Translate = _ => "再见 {name}" };
        await CreatePipeline().TranslateOnlyAsync(session, [resumed], policy, options);
        Assert.Equal("2", Assert.Single(Assert.Single(resumed.Batches)).Id);
        saved = await TranslationEdits.LoadAsync(session.ProjectDirectory);
        Assert.Equal("手工修订 {name}", saved.Entries["1"].Text); Assert.True(saved.Entries["1"].Reviewed);
        Assert.Equal("再见 {name}", saved.Entries["2"].Text);
    }

    [Fact]
    public async Task RetranslationSavedReceiptRecoversWithoutApiAndDoesNotLeakIntoNormalMemory()
    {
        await WriteGame(("1", "hello"));
        var session = await CreatePipeline().AnalyzeAsync(Game, new JsonGameAdapter());
        await CreatePipeline().TranslateOnlyAsync(session, [new FakeProvider()], policy);
        var unit = session.Extraction.Units[0];
        var memory = new TranslationMemory(Path.Combine(session.ProjectDirectory, "memory"));
        var operation = Guid.NewGuid().ToString("N");
        var provider = new FakeProvider { FailIfCalled = true };
        var key = LocalFiles.HashText(System.Text.Json.JsonSerializer.Serialize(new
        {
            Provider = provider.Identity, Units = new[] { new { unit.Id, Key = memory.Key(unit, policy) } }
        }) + "\nretranslate:" + operation);
        var responses = Path.Combine(session.ProjectDirectory, "responses");
        await LocalFiles.WriteJsonAsync(Path.Combine(responses, key + ".request.json"), new TranslationIntent(key, provider.Identity, DateTimeOffset.UtcNow,
            [new(unit.Id, memory.Key(unit, policy))], RetranslationId: operation));
        await LocalFiles.WriteJsonAsync(Path.Combine(responses, key + ".json"), new TranslationResponse(new Dictionary<string, string> { [unit.Id] = "已返回新译文" }, new(20, 10)));
        await CreatePipeline().TranslateOnlyAsync(session, [provider], policy);
        Assert.True(File.Exists(Path.Combine(responses, key + ".json")));
        await CreatePipeline().TranslateOnlyAsync(session, [provider], policy,
            new(50, UnitIds: [unit.Id]) { RetranslationId = operation });
        Assert.Equal("已返回新译文", (await TranslationEdits.LoadAsync(session.ProjectDirectory)).Entries[unit.Id].Text);
        Assert.Equal("翻译 hello", await memory.FindAsync(unit, policy));
        Assert.False(File.Exists(Path.Combine(responses, key + ".json")));
    }

    private sealed record Line(string Id, string Text);

    private sealed class JsonGameAdapter : IGameAdapter
    {
        public int Extractions { get; private set; }
        public string Id => "test-json";
        public string Version { get; init; } = "1";
        public bool FailIfProbed { get; init; }
        public int Probes { get; private set; }
        public int Builds { get; private set; }
        public Task<GameProbe?> ProbeAsync(string directory, CancellationToken cancellationToken = default)
        {
            if (FailIfProbed) throw new InvalidOperationException("Opening a saved analysis must not invoke the parser.");
            Probes++;
            return Task.FromResult<GameProbe?>(File.Exists(Path.Combine(directory, "story.json"))
                ? new GameProbe("test", Id, Version, "test", ["story.json"], []) : null);
        }
        public async Task<ExtractionResult> ExtractAsync(string directory, CancellationToken cancellationToken = default)
        {
            Extractions++;
            var lines = await LocalFiles.ReadJsonAsync<Line[]>(Path.Combine(directory, "story.json"), cancellationToken);
            return new ExtractionResult(lines!.Select(line => new TranslationUnit(line.Id, "story.json", line.Id, line.Text, "scene")).ToArray(), []);
        }
        public async Task<AdapterBuildResult> BuildPatchAsync(string sourceDirectory, IReadOnlyDictionary<string, string> translations,
            string patchDirectory, CancellationToken cancellationToken = default)
        {
            Builds++;
            var source = await ExtractAsync(sourceDirectory, cancellationToken);
            await LocalFiles.WriteJsonAsync(Path.Combine(patchDirectory, "story.json"), source.Units.Select(u => new Line(u.Id, translations[u.Id])).ToArray(), cancellationToken);
            return new AdapterBuildResult(["story.json"], []);
        }
        public async Task<ValidationResult> ValidatePatchAsync(string sourceDirectory, string patchDirectory, CancellationToken cancellationToken = default)
        {
            var source = await ExtractAsync(sourceDirectory, cancellationToken);
            var output = await ExtractAsync(patchDirectory, cancellationToken);
            return new ValidationResult(source.Units.Select(u => u.Id).SequenceEqual(output.Units.Select(u => u.Id)), []);
        }
    }
    private sealed class FakeProvider : ITranslationProvider
    {
        public string Identity { get; init; } = "test-provider";
        public int MaximumBatchSize { get; init; } = 256;
        public int Calls { get; private set; }
        public bool FailIfCalled { get; init; }
        public Action? AfterResponse { get; init; }
        public Exception? Failure { get; init; }
        public Func<TranslationRequest, Exception?>? FailureFor { get; init; }
        public Func<TranslationRequest, IReadOnlyDictionary<string, string>>? ResponseTranslations { get; init; }
        public Func<TranslationUnit, string> Translate { get; init; } = unit => "翻译 " + unit.SourceText;
        public List<IReadOnlyList<TranslationUnit>> Batches { get; } = [];
        public List<TranslationRequest> Requests { get; } = [];
        public Task<TranslationResponse> TranslateAsync(TranslationRequest request, CancellationToken cancellationToken = default)
        {
            if (FailIfCalled) throw new InvalidOperationException("Unexpected API request");
            Calls++; Batches.Add(request.Units); Requests.Add(request);
            if ((FailureFor?.Invoke(request) ?? Failure) is { } failure) throw failure;
            var response = new TranslationResponse(ResponseTranslations?.Invoke(request) ?? request.Units.ToDictionary(u => u.Id, Translate), new(20, 10));
            AfterResponse?.Invoke();
            return Task.FromResult(response);
        }
    }
    public void Dispose() { if (Directory.Exists(root)) Directory.Delete(root, true); }
}
