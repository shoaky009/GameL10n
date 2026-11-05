using System.Text.Json;
using GameL10n.Core;
using GameL10n.Core.Development;
using GameL10n.Core.Storage;

namespace GameL10n.Tests.Development;

public sealed partial class AutomaticAdaptationTests
{
    private const string CacheTestArguments = """{"kind":"template","key":"line-script","name":"Line script","resourcePatterns":["*.txt"]}""";

    private async Task<(AdaptationRequest Request, AdaptationSession State, string Workspace, LocalAdapterPackageStore Store)>
        VerificationCacheFixtureAsync(ScriptedRunner runner)
    {
        var request = Request() with { Objective = AdaptationObjective.Repair,
            BaselineUnits = [new("line-0", "script.txt", "line:0", "First line", "fixture")] };
        var state = new AdaptationSession { Objective = request.Objective };
        var workspace = Path.Combine(root, "workspace");
        var store = new LocalAdapterPackageStore(Path.Combine(root, "store"));
        var tools = new AdaptationTools(request, state, workspace, Runtime(runner), store);
        await tools.ExecuteAsync(new("source", "write_file", """{"path":"src/adapter.py","content":"# fixture"}"""), default);
        await tools.ExecuteAsync(new("dependency", "write_file", """{"path":"src/tools/parser.py","content":"# fixture","releaseRole":"runtime","purpose":"Parser"}"""), default);
        await tools.ExecuteAsync(new("stage", "stage_resources", """{"files":["script.txt"]}"""), default);
        return (request, state, workspace, store);
    }

    [Fact]
    public async Task IdenticalVerificationSurvivesCheckpointAndRestartAndRefreshesPendingNotes()
    {
        var runs = 0;
        var runner = new ScriptedRunner { ObserveRequest = _ => runs++ };
        var (request, state, workspace, store) = await VerificationCacheFixtureAsync(runner);
        var tools = new AdaptationTools(request, state, workspace, Runtime(runner), store);
        using var first = JsonDocument.Parse(await tools.ExecuteAsync(new("test", "test_adapter", CacheTestArguments), default));
        Assert.True(first.RootElement.GetProperty("result").GetProperty("passed").GetBoolean());
        var initialRuns = runs;
        var initialPackage = state.VerifiedPackage!;
        await tools.ExecuteAsync(new("checkpoint", "checkpoint", """{"summary":"Full checks passed; publish next."}"""), default);
        Directory.CreateDirectory(Path.Combine(workspace, "work"));
        await File.WriteAllTextAsync(Path.Combine(workspace, "work/notes.txt"), "Investigation only.");
        await File.WriteAllTextAsync(Path.Combine(workspace, "src/Unused.py"), "not selected for release");
        var sessionPath = Path.Combine(workspace, "session.json");
        await LocalFiles.WriteJsonAsync(sessionPath, state, FixtureJson.TypeInfo<global::GameL10n.Core.Development.AdaptationSession>(FixtureJson.Storage));
        state = (await LocalFiles.ReadJsonAsync<AdaptationSession>(sessionPath, FixtureJson.TypeInfo<global::GameL10n.Core.Development.AdaptationSession>(FixtureJson.Storage)))!;
        var resumed = new AdaptationTools(request, state, workspace, Runtime(runner), store);
        using var repeated = JsonDocument.Parse(await resumed.ExecuteAsync(new("repeat", "test_adapter", """
            {"kind":"template","key":"line-script","name":"Line script","resourcePatterns":["*.txt"],
             "pendingCoverage":["Options menu not covered."],"runtimeChecks":["Confirm Chinese font in game."]}
            """), default));
        var result = repeated.RootElement.GetProperty("result");
        Assert.True(result.GetProperty("reused").GetBoolean());
        Assert.False(result.GetProperty("runtimeTested").GetBoolean());
        Assert.Equal(0, result.GetProperty("compileMs").GetInt32());
        Assert.Equal("Options menu not covered.", Assert.Single(result.GetProperty("pendingCoverage").EnumerateArray()).GetString());
        Assert.Equal("Confirm Chinese font in game.", Assert.Single(result.GetProperty("runtimeChecks").EnumerateArray()).GetString());
        Assert.Equal(initialRuns, runs);
        Assert.Equal(initialPackage.Version, state.VerifiedPackage!.Version);
        Assert.Equal(GeneratedAdapterVerifier.Revision, state.VerificationRevision);
        Assert.Equal(state.TextCoverage.Fingerprint(), state.VerifiedCoverageFingerprint);
        Assert.NotNull(state.VerifiedBaselineFingerprint);
        await resumed.ExecuteAsync(new("finish", "finish", """{"summary":"Repair verified; menu coverage remains pending.","allowPartial":true}"""), default);
        Assert.Equal(initialPackage.Version, store.LoadActive(request.GameId)!.Version);
    }

    [Theory]
    [InlineData("source")]
    [InlineData("dependency")]
    [InlineData("resources")]
    [InlineData("new-game-candidate")]
    [InlineData("new-staged-candidate")]
    [InlineData("patterns")]
    [InlineData("recognition")]
    [InlineData("release")]
    [InlineData("coverage")]
    [InlineData("objective")]
    [InlineData("baseline")]
    [InlineData("revision")]
    [InlineData("artifact")]
    [InlineData("missing-build")]
    public async Task VerificationCacheRejectsChangedInputsAndDamagedArtifacts(string change)
    {
        var runs = 0;
        var runner = new ScriptedRunner { ObserveRequest = _ => runs++ };
        var (request, state, workspace, store) = await VerificationCacheFixtureAsync(runner);
        var tools = new AdaptationTools(request, state, workspace, Runtime(runner), store);
        await tools.ExecuteAsync(new("test", "test_adapter", CacheTestArguments), default);
        Assert.NotNull(state.VerifiedPackage);
        var initialRuns = runs;
        var arguments = CacheTestArguments;
        switch (change)
        {
            case "source":
            case "dependency":
                var file = Path.Combine(workspace, change == "source" ? "src/adapter.py" : "src/tools/parser.py");
                var timestamp = File.GetLastWriteTimeUtc(file);
                // Same length and timestamp still require content-based invalidation, including external edits.
                await File.WriteAllTextAsync(file, (await File.ReadAllTextAsync(file)).Replace("fixture", "changed"));
                File.SetLastWriteTimeUtc(file, timestamp);
                break;
            case "resources":
                await File.WriteAllTextAsync(Path.Combine(request.GameDirectory, "script.txt"), "First line\nUpdated second line");
                await tools.ExecuteAsync(new("stage", "stage_resources", """{"files":["script.txt"]}"""), default);
                break;
            case "new-game-candidate":
                await File.WriteAllTextAsync(Path.Combine(request.GameDirectory, "extra.txt"), "extra candidate");
                break;
            case "new-staged-candidate":
                await File.WriteAllTextAsync(Path.Combine(workspace, "resources/extra.txt"), "extra candidate");
                break;
            case "patterns":
                arguments = arguments.Replace("[\"*.txt\"]", "[\"*.txt\",\"*.dat\"]");
                break;
            case "recognition":
                state.Recognition = new(1, [new(["*.txt"])], []);
                break;
            case "release":
                await tools.ExecuteAsync(new("release", "update_release_files", """{"files":[{"path":"tools/parser.py","role":"runtime","purpose":"Updated purpose"}]}"""), default);
                break;
            case "coverage":
                state.TextCoverage = new([new("script.txt", "line:1", "Second line")], []);
                break;
            case "objective":
                state.Objective = AdaptationObjective.Develop;
                break;
            case "baseline":
                request = request with { BaselineUnits = [request.BaselineUnits![0] with { Context = "changed" }] };
                tools = new AdaptationTools(request, state, workspace, Runtime(runner), store);
                break;
            case "revision":
                state.VerificationCache = state.VerificationCache! with { Revision = GeneratedAdapterVerifier.Revision - 1 };
                break;
            case "artifact":
                await File.WriteAllBytesAsync(Path.Combine(state.VerifiedPackage!.DirectoryPath, state.VerifiedPackage.EntryFile), [9, 9, 9]);
                break;
            case "missing-build":
                Directory.Delete(state.VerifiedPackage!.DirectoryPath, true);
                break;
        }
        using var next = JsonDocument.Parse(await tools.ExecuteAsync(new("again", "test_adapter", arguments), default));
        Assert.True(runs > initialRuns, "Changed inputs or build artifacts must execute a fresh check.");
        Assert.False(next.RootElement.GetProperty("result").GetProperty("reused").GetBoolean());
        if (change == "baseline") Assert.Null(state.VerifiedPackage);
    }

    [Theory]
    [InlineData("compilation")]
    [InlineData("discovery")]
    [InlineData("verification")]
    public async Task CompletedFailedChecksReuseDiagnosticsAndNeverGrantPublication(string stage)
    {
        var runs = 0;
        var runner = new ScriptedRunner { ObserveRequest = _ => runs++,
            CompilationError = stage == "compilation" ? "fixture compiler error" : null,
            CorruptUntouchedTrialText = stage == "verification" };
        var (request, state, workspace, store) = await VerificationCacheFixtureAsync(runner);
        if (stage == "discovery") state.Recognition = new(1, [new(["missing.dat"])], []);
        var tools = new AdaptationTools(request, state, workspace, Runtime(runner), store);
        var call = new AgentToolCall("test", "test_adapter", CacheTestArguments);
        string? error = null;
        if (stage == "compilation") error = (await Assert.ThrowsAsync<GeneratedAdapterException>(() => tools.ExecuteAsync(call, default))).Message;
        else await tools.ExecuteAsync(call, default);
        Assert.NotNull(state.VerificationCache);
        var initialRuns = runs;
        state = JsonSerializer.Deserialize<AdaptationSession>(JsonSerializer.Serialize(state, Json), Json)!;
        var resumed = new AdaptationTools(request, state, workspace, Runtime(runner), store);
        if (stage == "compilation")
            Assert.Equal(error, (await Assert.ThrowsAsync<GeneratedAdapterException>(() => resumed.ExecuteAsync(call, default))).Message);
        else
        {
            using var cached = JsonDocument.Parse(await resumed.ExecuteAsync(call, default));
            var result = cached.RootElement.GetProperty("result");
            Assert.False(result.GetProperty("passed").GetBoolean());
            Assert.True(result.GetProperty("reused").GetBoolean());
            Assert.Equal(stage, result.GetProperty("stage").GetString());
            Assert.NotEmpty(result.GetProperty("diagnostics").EnumerateArray());
        }
        Assert.Equal(initialRuns, runs);
        Assert.Null(state.VerifiedPackage);
        await Assert.ThrowsAsync<InvalidDataException>(() => resumed.ExecuteAsync(new("finish", "finish", """{"summary":"Cannot publish."}"""), default));
        Assert.Null(store.LoadActive(request.GameId));
    }

    [Fact]
    public async Task DiscoveryOnlyCacheCannotReplaceFullVerificationOrGrantPublication()
    {
        var runs = 0;
        var runner = new ScriptedRunner { ObserveRequest = _ => runs++ };
        var (request, state, workspace, store) = await VerificationCacheFixtureAsync(runner);
        var tools = new AdaptationTools(request, state, workspace, Runtime(runner), store);
        var discovery = CacheTestArguments[..^1] + ",\"discoveryOnly\":true}";
        await tools.ExecuteAsync(new("discovery", "test_adapter", discovery), default);
        var initialRuns = runs;
        using var cached = JsonDocument.Parse(await tools.ExecuteAsync(new("repeat", "test_adapter", discovery), default));
        Assert.True(cached.RootElement.GetProperty("result").GetProperty("reused").GetBoolean());
        Assert.Equal(initialRuns, runs);
        Assert.Null(state.VerifiedPackage);
        await tools.ExecuteAsync(new("full", "test_adapter", CacheTestArguments), default);
        Assert.True(runs > initialRuns);
        Assert.NotNull(state.VerifiedPackage);
        await tools.ExecuteAsync(new("discovery-again", "test_adapter", discovery), default);
        Assert.Null(state.VerifiedPackage);
        await Assert.ThrowsAsync<InvalidDataException>(() => tools.ExecuteAsync(new("finish", "finish", """{"summary":"Discovery only."}"""), default));
    }

    [Theory]
    [InlineData(true)]
    [InlineData(false)]
    public async Task ChangedOriginalOrStagedResourceCannotUsePreviousSuccess(bool original)
    {
        var runner = new ScriptedRunner();
        var (request, state, workspace, store) = await VerificationCacheFixtureAsync(runner);
        var tools = new AdaptationTools(request, state, workspace, Runtime(runner), store);
        await tools.ExecuteAsync(new("test", "test_adapter", CacheTestArguments), default);
        await File.AppendAllTextAsync(Path.Combine(original ? request.GameDirectory : Path.Combine(workspace, "resources"), "script.txt"), "changed");
        var error = await Assert.ThrowsAsync<InvalidDataException>(() => tools.ExecuteAsync(new("repeat", "test_adapter", CacheTestArguments), default));
        Assert.Contains("重新 stage_resources", error.Message);
        Assert.Null(state.VerifiedPackage);
    }

    [Fact]
    public async Task TemporaryIoFailureIsRetriedInsteadOfReplayingItsDiagnosticForever()
    {
        var failOnce = true;
        var runner = new ScriptedRunner { ObserveRequest = operation =>
        {
            if (failOnce && operation.StandardInput?.Contains("extract", StringComparison.Ordinal) == true)
            {
                failOnce = false;
                throw new IOException("fixture temporary file lock");
            }
        } };
        var (request, state, workspace, store) = await VerificationCacheFixtureAsync(runner);
        var tools = new AdaptationTools(request, state, workspace, Runtime(runner), store);
        await tools.ExecuteAsync(new("test", "test_adapter", CacheTestArguments), default);
        Assert.Contains(state.VerificationDiagnostics, text => text.Contains("temporary file lock"));
        Assert.Null(state.VerificationCache);
        Assert.Null(state.VerifiedPackage);
        await tools.ExecuteAsync(new("retry", "test_adapter", CacheTestArguments), default);
        Assert.NotNull(state.VerifiedPackage);
    }

    [Theory]
    [InlineData(true)]
    [InlineData(false)]
    public async Task InterruptedOrTimedOutVerificationNeverCreatesCacheEntry(bool cancelled)
    {
        var runner = new ScriptedRunner { ObserveRequest = operation =>
        {
            if (operation.StandardInput?.Contains("extract", StringComparison.Ordinal) == true)
                throw cancelled ? new OperationCanceledException() : new AdapterProcessLimitException("fixture timeout");
        } };
        var (request, state, workspace, store) = await VerificationCacheFixtureAsync(runner);
        var tools = new AdaptationTools(request, state, workspace, Runtime(runner), store);
        var error = await Record.ExceptionAsync(() => tools.ExecuteAsync(new("test", "test_adapter", CacheTestArguments), default));
        Assert.IsType(cancelled ? typeof(OperationCanceledException) : typeof(AdapterProcessLimitException), error);
        Assert.Null(state.VerificationCache);
        Assert.Null(state.VerifiedPackage);
    }
}
