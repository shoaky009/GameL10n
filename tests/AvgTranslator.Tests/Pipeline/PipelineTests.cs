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
        foreach (var file in Directory.GetFiles(Path.Combine(Data, "memory", session.Id), "*.json")) File.Delete(file);
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
        await Assert.ThrowsAsync<TranslationProviderException>(() => CreatePipeline().RunAsync(session, failing, policy));
        var provider = new FakeProvider();
        await Assert.ThrowsAsync<UncertainRequestException>(() => CreatePipeline().RunAsync(session, provider, policy, new RunOptions(1)));
        Assert.Equal(0, provider.Calls);
        var result = await CreatePipeline().RunAsync(session, provider, policy, new RunOptions(1, true));
        Assert.True(result.Validation.Passed);
        Assert.Equal(2, provider.Calls);
    }

    [Fact]
    public async Task ExplicitProviderRejectionDoesNotBecomeAnUnknownOutcome()
    {
        await WriteGame(("1", "hello"));
        var session = await CreatePipeline().AnalyzeAsync(Game, new JsonGameAdapter());
        await Assert.ThrowsAsync<TranslationProviderException>(() => CreatePipeline().RunAsync(session,
            new FakeProvider { Failure = new TranslationProviderException("HTTP401") }, policy));
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
    }

    [Fact]
    public async Task InstalledGameReadsBackupAndRestoresOriginal()
    {
        await WriteGame(("1", "hello"));
        var original = await File.ReadAllBytesAsync(Path.Combine(Game, "story.json"));
        var session = await CreatePipeline().AnalyzeAsync(Game, new JsonGameAdapter());
        var result = await CreatePipeline().RunAsync(session, new FakeProvider(), policy);
        var installer = new PatchInstaller();
        await installer.InstallAsync(Game, result.PatchDirectory);
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
        await Assert.ThrowsAsync<IOException>(() => new PatchInstaller().InstallAsync(Game, result.PatchDirectory));
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
        await CreatePipeline().RunAsync(session, new FakeProvider(), policy, new RunOptions(TrialUnitIds: ["1"]));
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
        Assert.Single(Directory.GetFiles(Path.Combine(Data, "memory", session.Id), "*.json"));
        Assert.Single(Directory.GetFiles(Path.Combine(session.ProjectDirectory, "responses"), "*.refusal.json"));

        var resumed = new FakeProvider { FailIfCalled = true };
        var error = await Assert.ThrowsAsync<TranslationRefusedException>(() => CreatePipeline().RunAsync(session, resumed, policy,
            new RunOptions(RetryUncertainRequests: false), new ImmediateProgress(updates.Add)));
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

    private sealed record Line(string Id, string Text);

    private sealed class JsonGameAdapter : IGameAdapter
    {
        public string Id => "test-json";
        public string Version => "1";
        public int Builds { get; private set; }
        public Task<GameProbe?> ProbeAsync(string directory, CancellationToken cancellationToken = default) => Task.FromResult<GameProbe?>(
            File.Exists(Path.Combine(directory, "story.json")) ? new GameProbe("test", Id, Version, "test", ["story.json"], []) : null);
        public async Task<ExtractionResult> ExtractAsync(string directory, CancellationToken cancellationToken = default)
        {
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
        public string Identity => "test-provider";
        public int Calls { get; private set; }
        public bool FailIfCalled { get; init; }
        public Action? AfterResponse { get; init; }
        public Exception? Failure { get; init; }
        public Func<TranslationUnit, string> Translate { get; init; } = unit => "翻译 " + unit.SourceText;
        public List<IReadOnlyList<TranslationUnit>> Batches { get; } = [];
        public Task<TranslationResponse> TranslateAsync(TranslationRequest request, CancellationToken cancellationToken = default)
        {
            if (FailIfCalled) throw new InvalidOperationException("Unexpected API request");
            Calls++; Batches.Add(request.Units);
            if (Failure is not null) throw Failure;
            var response = new TranslationResponse(request.Units.ToDictionary(u => u.Id, Translate), new(20, 10));
            AfterResponse?.Invoke();
            return Task.FromResult(response);
        }
    }
    public void Dispose() { if (Directory.Exists(root)) Directory.Delete(root, true); }
}
