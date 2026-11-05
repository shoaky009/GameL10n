using System.Text.Json;
using GameL10n.Core.Development;
using GameL10n.Core.Serialization;

namespace GameL10n.Tests.Development;

public sealed partial class AutomaticAdaptationTests
{
    [Fact]
    public async Task ResourceTrialsIsolateSelectedFilesAndEvidenceAndCacheOnlyTheirInputs()
    {
        var observed = new List<string[]>();
        var runner = new ScriptedRunner { ObserveRequest = call =>
        {
            if (string.IsNullOrEmpty(call.StandardInput)) return;
            using var input = JsonDocument.Parse(call.StandardInput);
            if (!input.RootElement.TryGetProperty("sourceDirectory", out var directory)) return;
            observed.Add(Directory.GetFiles(directory.GetString()!, "*", SearchOption.AllDirectories)
                .Select(path => Path.GetRelativePath(directory.GetString()!, path).Replace('\\', '/')).Order().ToArray());
        } };
        var (request, state, workspace, store) = await VerificationCacheFixtureAsync(runner);
        await File.WriteAllTextAsync(Path.Combine(request.GameDirectory, "other.txt"), "Other chapter");
        request = request with { BaselineUnits = request.BaselineUnits!
            .Append(new("other-0", "other.txt", "line:0", "Other chapter", "fixture")).ToArray() };
        state.TextCoverage = new([new("script.txt", "line:0", "First line"), new("other.txt", "line:0", "Other chapter")], []);
        var trace = new List<JsonElement>();
        var workflow = new AdapterDevelopmentWorkflow(request, state, workspace, Runtime(runner), store,
            trace: entry => { trace.Add(JsonSerializer.SerializeToElement(entry, Json)); return Task.CompletedTask; });
        var tools = new AdaptationTools(request, state, workspace, Runtime(runner), store, development: workflow);
        await tools.ExecuteAsync(new("stage", "stage_resources", """{"files":["other.txt"]}"""), default);
        var arguments = CacheTestArguments[..^1] + ",\"trialFiles\":[\"script.txt\"]}";
        using var trial = JsonDocument.Parse(await tools.ExecuteAsync(new("trial", "test_adapter", arguments), default));
        var result = trial.RootElement.GetProperty("result");
        Assert.True(result.GetProperty("passed").GetBoolean(), result.ToString());
        Assert.Equal("trial", result.GetProperty("stage").GetString());
        Assert.Equal("trial", result.GetProperty("scope").GetString());
        Assert.Equal(1, result.GetProperty("trialResourceCount").GetInt32());
        Assert.Equal(new FileInfo(Path.Combine(request.GameDirectory, "script.txt")).Length, result.GetProperty("trialBytes").GetInt64());
        Assert.NotEmpty(observed);
        Assert.All(observed, files => Assert.Equal(new[] { "script.txt" }, files));
        Assert.NotEmpty(trace);
        Assert.All(trace, entry => Assert.Equal("trial", entry.GetProperty("scope").GetString()));
        Assert.Null(state.VerifiedPackage);
        Assert.Null(state.VerificationCache);
        Assert.Empty(state.VerificationDiagnostics);
        Assert.NotNull(state.TrialVerificationCache);
        Assert.Equal(2, state.ResourceHashes.Count);
        Assert.Equal(2, state.TextCoverage.Samples.Count);
        Assert.Empty(Directory.EnumerateDirectories(Path.Combine(workspace, "work"), ".resource-trial-*"));
        await Assert.ThrowsAsync<InvalidDataException>(() => tools.ExecuteAsync(new("finish", "finish", """{"summary":"Trial only."}"""), default));

        state = JsonSerializer.Deserialize(JsonSerializer.Serialize(state, EngineJson.TypeInfo<AdaptationSession>(EngineJson.Storage)),
            EngineJson.TypeInfo<AdaptationSession>(EngineJson.Storage))!;
        // An unrelated stale resource must not force a scoped trial to process the entire game.
        await File.AppendAllTextAsync(Path.Combine(request.GameDirectory, "other.txt"), " changed");
        state.TextCoverage = state.TextCoverage with { Samples = state.TextCoverage.Samples
            .Select(sample => sample.ResourcePath == "other.txt" ? sample with { SourceText = "Other chapter changed" } : sample).ToArray() };
        request = request with { BaselineUnits = request.BaselineUnits!
            .Select(unit => unit.ResourcePath == "other.txt" ? unit with { Context = "Unrelated evidence updated" } : unit).ToArray() };
        tools = new(request, state, workspace, Runtime(runner), store);
        var previousRuns = observed.Count;
        using var repeated = JsonDocument.Parse(await tools.ExecuteAsync(new("cached", "test_adapter", arguments), default));
        Assert.True(repeated.RootElement.GetProperty("result").GetProperty("reused").GetBoolean());
        Assert.Equal(previousRuns, observed.Count);
        await File.AppendAllTextAsync(Path.Combine(request.GameDirectory, "script.txt"), "\nExtra line");
        var stale = await Assert.ThrowsAsync<InvalidDataException>(() => tools.ExecuteAsync(new("stale", "test_adapter", arguments), default));
        Assert.Contains("重新 stage_resources", stale.Message);
        await tools.ExecuteAsync(new("stage-selected", "stage_resources", """{"files":["script.txt"]}"""), default);
        using var changed = JsonDocument.Parse(await tools.ExecuteAsync(new("changed", "test_adapter", arguments), default));
        Assert.True(changed.RootElement.GetProperty("result").GetProperty("passed").GetBoolean());
        Assert.False(changed.RootElement.GetProperty("result").GetProperty("reused").GetBoolean());
        Assert.True(observed.Count > previousRuns);
        var trialCache = state.TrialVerificationCache;
        // Full verification still checks the unselected original, then its baseline and coverage.
        await Assert.ThrowsAsync<InvalidDataException>(() => tools.ExecuteAsync(new("full-stale", "test_adapter", CacheTestArguments), default));
        await tools.ExecuteAsync(new("stage-rest", "stage_resources", """{"files":["other.txt"]}"""), default);
        using var full = JsonDocument.Parse(await tools.ExecuteAsync(new("full", "test_adapter", CacheTestArguments), default));
        Assert.False(full.RootElement.GetProperty("result").GetProperty("passed").GetBoolean());
        Assert.False(full.RootElement.GetProperty("result").GetProperty("reused").GetBoolean());
        Assert.Contains(observed, files => files.Contains("other.txt"));
        Assert.Same(trialCache, state.TrialVerificationCache);
        Assert.NotNull(state.VerificationCache);
        Assert.Null(state.VerifiedPackage);
        Assert.Null(store.LoadActive(request.GameId));
    }
}
