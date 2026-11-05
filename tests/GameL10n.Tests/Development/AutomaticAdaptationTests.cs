using System.Security.Cryptography;
using System.Text;
using System.Text.Json;
using GameL10n.Core;
using GameL10n.Core.Development;
using GameL10n.Core.Isolation;
using GameL10n.Core.Adapters;
using GameL10n.Core.Pipeline;
using GameL10n.Core.Storage;

namespace GameL10n.Tests.Development;

public sealed partial class AutomaticAdaptationTests : IDisposable
{
    private readonly string root = Path.Combine(Path.GetTempPath(), "gamel10n-adaptation-test-" + Guid.NewGuid().ToString("N"));
    private static readonly JsonSerializerOptions Json = new(JsonSerializerDefaults.Web);

    public AutomaticAdaptationTests()
    {
        Directory.CreateDirectory(root);
        using var fixture = new EngineArchiveFixture();
        fixture.PrepareSharedToolsAsync(Path.Combine(root, "store")).GetAwaiter().GetResult();
    }

    [Fact]
    public async Task RecognitionDeclarationIsVerifiedPersistedAndVersionedIndependentlyOfTheCli()
    {
        var workspace = Path.Combine(root, "workspace");
        var state = new AdaptationSession();
        var store = new LocalAdapterPackageStore(Path.Combine(root, "store"));
        var compiler = Runtime(new ScriptedRunner());
        var tools = new AdaptationTools(Request(), state, workspace, compiler, store);
        await tools.ExecuteAsync(new("source", "write_file", """{"path":"src/adapter.py","content":"# fixture"}"""), default);
        await tools.ExecuteAsync(new("stage", "stage_resources", """{"files":["script.txt"]}"""), default);
        var initial = await tools.ExecuteAsync(new("test", "test_adapter", """
            {"kind":"template","key":"line-script","name":"Line script","resourcePatterns":["*.txt"],
             "recognition":{"schemaVersion":1,"required":[{"patterns":["script.txt"],"signatures":[{"offset":0,"hex":"FFFF"}]}],"hints":[]}}
            """), default);
        Assert.Contains("识别声明未在真实游戏上确认", initial);
        Assert.Null(state.VerifiedPackage);
        await tools.ExecuteAsync(new("test", "test_adapter", """
            {"kind":"template","key":"line-script","name":"Line script","resourcePatterns":["*.txt"],
             "recognition":{"schemaVersion":1,"required":[{"patterns":["*.txt"],"signatures":[{"offset":0,"hex":"4669727374"}]}],"hints":[]}}
            """), default);
        var package = state.VerifiedPackage!;
        Assert.NotNull(package);
        Assert.Equal(1, package.ProtocolVersion);
        Assert.Equal(4, package.ManifestVersion);
        var restored = JsonSerializer.Deserialize<AdaptationSession>(JsonSerializer.Serialize(state, Json), Json)!;
        Assert.NotNull(restored.Recognition);
        var resumed = new AdaptationTools(Request(), restored, workspace, compiler, store);
        await resumed.ExecuteAsync(new("retest", "test_adapter", """
            {"kind":"template","key":"line-script","name":"Line script","resourcePatterns":["*.txt"]}
            """), default);
        Assert.Equal(package.Version, restored.VerifiedPackage!.Version);
        await resumed.ExecuteAsync(new("finish", "finish", """{"summary":"通过"}"""), default);
        var loaded = store.LoadActive(Request().GameId)!;
        Assert.NotNull(loaded.Recognition);
        Assert.NotNull(Assert.Single(await store.ReadDiscoveryCandidatesAsync()).Descriptor!.Recognition);
        var python = await new EngineReferenceToolkit(Path.Combine(store.RootDirectory, "engine-tools")).ResolvePackageAsync(
            package.DirectoryPath, default);
        var without = await new GeneratedAdapterBuilder(compiler.Runner).BuildWorkspaceAsync(Path.Combine(workspace, "src"), Path.Combine(root, "plain"),
            ["*.txt"], restored.ReleaseFiles!, new(python!.Python!, python.EngineLibrary!), family: loaded.Family);
        Assert.NotEqual(loaded.Version, without.Version);
    }

    [Fact]
    public async Task SharedProbeHeadersRemainIndependentWhenOneAdapterModifiesItsInput()
    {
        var request = Request();
        var source = Path.Combine(root, "source");
        Directory.CreateDirectory(source);
        await File.WriteAllTextAsync(Path.Combine(source, "Program.cs"), "// fixture");
        var compiler = LegacyCompiler(new ScriptedRunner());
        var package = await compiler.CompileWorkspaceAsync(source, Path.Combine(root, "compiled"), ["*.txt"],
            [new("src/Program.cs", "source", "Entry")]);
        var inventory = GameDiscoverySession.Scan(request.GameDirectory);
        var corrupting = new ScriptedRunner { ObserveRequest = call =>
        {
            using var message = JsonDocument.Parse(call.StandardInput);
            var path = message.RootElement.GetProperty("sourceDirectory").GetString()!;
            File.WriteAllText(Path.Combine(path, "script.txt"), "corrupted");
        } };
        var bad = new GeneratedGameAdapter(package, compiler.Toolchain, corrupting, Path.Combine(root, "bad"), discovery: inventory);
        await Assert.ThrowsAsync<GeneratedAdapterException>(() => bad.ProbeAsync(request.GameDirectory));
        var good = new GeneratedGameAdapter(package, compiler.Toolchain, new ScriptedRunner(), Path.Combine(root, "good"), discovery: inventory);
        Assert.NotNull(await good.ProbeAsync(request.GameDirectory));
        Assert.Equal(1, inventory.RangeReads);
        Assert.Equal("First line\nSecond line", await File.ReadAllTextAsync(Path.Combine(request.GameDirectory, "script.txt")));
    }


    [Theory]
    [InlineData("same", true)]
    [InlineData("missing", false)]
    [InlineData("extra", false)]
    [InlineData("changed", false)]
    public async Task PublishedPackageVerificationUsesRelocatedFilesAndRejectsBaselineChanges(string change, bool passed)
    {
        var (package, project, source) = await PackageVerificationFixtureAsync();
        var operations = new List<AdapterProcessRequest>();
        var runner = new ScriptedRunner { ObserveRequest = operations.Add, RewriteExtraction = units => change switch
        {
            "missing" => units.Take(1).ToArray(),
            "extra" => [.. units, units[0] with { Id = "extra", Locator = "extra" }],
            "changed" => units.Select(unit => unit with { SourceText = unit.SourceText + " changed" }).ToArray(),
            _ => units
        } };
        var originalHash = Hash(Path.Combine(package.DirectoryPath, "adapter.dll"));
        var result = await new PublishedPackageVerifier(Toolchain(), runner).VerifyAsync(package, project, Path.Combine(root, "verification"));
        Assert.Equal(passed, result.Passed);
        Assert.NotEmpty(operations);
        Assert.All(operations, operation => Assert.NotEqual(Path.Combine(package.DirectoryPath, "adapter.dll"), operation.Arguments[0]));
        Assert.All(operations, operation => Assert.DoesNotContain(source, operation.StandardInput));
        Assert.False(result.RuntimeTested);
        Assert.NotNull(result.PackageFingerprint);
        Assert.NotNull(result.SourceFingerprint);
        Assert.NotNull(result.BaselineFingerprint);
        Assert.Equal(originalHash, Hash(Path.Combine(package.DirectoryPath, "adapter.dll")));
        Assert.Empty(Directory.GetFileSystemEntries(Path.Combine(root, "verification")));
        var saved = await LocalFiles.ReadJsonAsync<PublishedPackageVerification>(Path.Combine(project, "package-verification.json"));
        Assert.Equal(result.Status, saved!.Status);
    }

    [Fact]
    public async Task PublishedPackageVerificationRefusesChangedSourcesAndMissingBaselineWithoutRegeneratingIt()
    {
        var (package, project, source) = await PackageVerificationFixtureAsync();
        var operations = new List<AdapterProcessRequest>();
        var verifier = new PublishedPackageVerifier(Toolchain(), new ScriptedRunner { ObserveRequest = operations.Add });
        await File.AppendAllTextAsync(Path.Combine(source, "script.txt"), "changed");
        var result = await verifier.VerifyAsync(package, project, Path.Combine(root, "verification"));
        Assert.False(result.Passed);
        Assert.Contains(result.Diagnostics, text => text.Contains("原文快照内容已变化"));
        Assert.Empty(operations);
        // Remove only the fixture cache, not the saved source record.
        var manifest = Directory.GetFiles(project, "manifest.sha256", SearchOption.AllDirectories).Single();
        File.Delete(manifest);
        result = await verifier.VerifyAsync(package, project, Path.Combine(root, "verification"));
        Assert.False(result.Passed);
        Assert.Contains(result.Diagnostics, text => text.Contains("已有提取记录缺失或损坏"));
        Assert.False(File.Exists(manifest));
        Assert.Empty(operations);
    }

    [Fact]
    public async Task PublishedPackageCancellationAndProcessFailureReplacePreviousSuccessReport()
    {
        var (package, project, _) = await PackageVerificationFixtureAsync();
        var runner = new ScriptedRunner();
        var verifier = new PublishedPackageVerifier(Toolchain(), runner);
        Assert.True((await verifier.VerifyAsync(package, project, Path.Combine(root, "verification"))).Passed);
        using var cancel = new CancellationTokenSource();
        cancel.Cancel();
        var result = await verifier.VerifyAsync(package, project, Path.Combine(root, "verification"), cancel.Token);
        Assert.Equal("cancelled", result.Status);
        var failing = new ScriptedRunner { ObserveRequest = _ => throw new AdapterProcessLimitException("fixture timeout") };
        result = await new PublishedPackageVerifier(Toolchain(), failing).VerifyAsync(package, project, Path.Combine(root, "verification"));
        Assert.False(result.Passed);
        Assert.Contains("fixture timeout", result.Diagnostics);
        Assert.Empty(Directory.GetFileSystemEntries(Path.Combine(root, "verification")));
        Assert.False((await LocalFiles.ReadJsonAsync<PublishedPackageVerification>(Path.Combine(project, "package-verification.json")))!.Passed);
    }

    private async Task<(GeneratedAdapterPackage Package, string Project, string Source)> PackageVerificationFixtureAsync()
    {
        var package = CreatePackage();
        var project = Directory.CreateDirectory(Path.Combine(root, "project")).FullName;
        var game = Request().GameDirectory;
        SourceFile[] files = [new("script.txt", Hash(Path.Combine(game, "script.txt")))];
        var fingerprint = LocalFiles.HashText(JsonSerializer.Serialize(files.OrderBy(file => file.RelativePath)));
        var source = Directory.CreateDirectory(Path.Combine(project, "sources", fingerprint)).FullName;
        File.Copy(Path.Combine(game, "script.txt"), Path.Combine(source, "script.txt"));
        await LocalFiles.WriteJsonAsync(Path.Combine(project, "source.json"),
            new SourceSnapshot(Path.GetRelativePath(project, source), package.Id, package.Version, files));
        var baseline = await new GeneratedGameAdapter(package, Toolchain(), new ScriptedRunner(), Path.Combine(root, "baseline-run")).ExtractAsync(source);
        await new ExtractionStore(project).StoreAsync(fingerprint, package.Id, package.Version, baseline);
        return (package, project, source);
    }

    [Fact]
    public async Task RegisteredReleaseSurvivesFailedVerificationReloadAndSourceEdits()
    {
        var workspace = Path.Combine(root, "workspace");
        var state = new AdaptationSession();
        var request = Request();
        var store = new LocalAdapterPackageStore(Path.Combine(root, "store"));
        var tools = new AdaptationTools(request, state, workspace,
            Runtime(new ScriptedRunner { ValidationResponse = new(false, [new("fixture", "fixture failure")], "failed") }), store);
        await tools.ExecuteAsync(new("source", "write_file", """{"path":"src/adapter.py","content":"# fixture"}"""), default);
        await tools.ExecuteAsync(new("dependency", "write_file", """
            {"path":"src/tools/parser.py","content":"# fixture","releaseRole":"runtime","purpose":"Parser"}
            """), default);
        await tools.ExecuteAsync(new("probe", "write_file", """{"path":"work/Probe.py","content":"// analysis only"}"""), default);
        await tools.ExecuteAsync(new("stage", "stage_resources", """{"files":["script.txt"]}"""), default);
        const string verify = """{"kind":"template","key":"line-script","name":"Line script","resourcePatterns":["*.txt"]}""";
        await tools.ExecuteAsync(new("test", "test_adapter", verify), default);
        Assert.Null(state.VerifiedPackage);
        Assert.Equal(3, state.ReleaseFiles!.Count);

        // Checkpoint/restart must not depend on the original tool-call arguments surviving in history.
        state.Messages.Clear();
        state = JsonSerializer.Deserialize<AdaptationSession>(JsonSerializer.Serialize(state, Json), Json)!;
        tools = new AdaptationTools(request, state, workspace, Runtime(new ScriptedRunner()), store);
        await tools.ExecuteAsync(new("edit", "write_file", """{"path":"src/adapter.py","content":"// repaired"}"""), default);
        await tools.ExecuteAsync(new("test", "test_adapter", verify), default);
        Assert.NotNull(state.VerifiedPackage);
        await tools.ExecuteAsync(new("edit", "write_file", """{"path":"src/adapter.py","content":"// repaired again"}"""), default);
        Assert.Null(state.VerifiedPackage);
        Assert.Equal(3, state.ReleaseFiles!.Count);
        await tools.ExecuteAsync(new("test", "test_adapter", verify), default);
        await tools.ExecuteAsync(new("finish", "finish", """{"summary":"Verified release."}"""), default);
        Assert.Equal(store.LoadActive(request.GameId)!.DirectoryPath, state.VerifiedPackage!.DirectoryPath);
    }

    [Fact]
    public async Task RenamedGameCopyCannotBePublishedAndExclusionDoesNotDeleteIt()
    {
        var workspace = Path.Combine(root, "workspace");
        var state = new AdaptationSession();
        var store = new LocalAdapterPackageStore(Path.Combine(root, "store"));
        var tools = new AdaptationTools(Request(), state, workspace, Runtime(new ScriptedRunner()), store);
        await tools.ExecuteAsync(new("source", "write_file", """{"path":"src/adapter.py","content":"# fixture"}"""), default);
        await tools.ExecuteAsync(new("copy", "write_file", """
            {"path":"src/tools/renamed.dat","content":"First line\nSecond line","releaseRole":"asset","purpose":"Mistaken game copy"}
            """), default);
        await tools.ExecuteAsync(new("stage", "stage_resources", """{"files":["script.txt"]}"""), default);
        const string verify = """{"kind":"template","key":"line-script","name":"Line script","resourcePatterns":["*.txt"]}""";
        var error = await Assert.ThrowsAsync<InvalidDataException>(() => tools.ExecuteAsync(new("test", "test_adapter", verify), default));
        Assert.Contains("原文件副本", error.Message);
        Assert.Null(state.VerifiedPackage);
        Assert.Null(store.LoadActive("game"));
        await tools.ExecuteAsync(new("exclude", "update_release_files", """{"remove":["tools/renamed.dat"]}"""), default);
        Assert.True(File.Exists(Path.Combine(workspace, "src/tools/renamed.dat")));
        await tools.ExecuteAsync(new("test", "test_adapter", verify), default);
        Assert.NotNull(state.VerifiedPackage);
        Assert.False(File.Exists(Path.Combine(state.VerifiedPackage!.DirectoryPath, "tools/renamed.dat")));
    }

    [Fact]
    public async Task ReleaseSelectionExcludesUnselectedFilesAndKeepsTheirEditsOutOfPublication()
    {
        var workspace = Path.Combine(root, "workspace");
        Directory.CreateDirectory(Path.Combine(workspace, "src/tools"));
        await File.WriteAllTextAsync(Path.Combine(workspace, "src/adapter.py"), "# fixture");
        await File.WriteAllTextAsync(Path.Combine(workspace, "src/Probe.py"), "not compilable; analysis only");
        await File.WriteAllTextAsync(Path.Combine(workspace, "src/tools/unused.zip"), "download");
        await File.WriteAllTextAsync(Path.Combine(workspace, "src/tools/parser.py"), "# fixture dependency");
        var state = new AdaptationSession();
        var store = new LocalAdapterPackageStore(Path.Combine(root, "store"));
        var tools = new AdaptationTools(Request(), state, workspace, Runtime(new ScriptedRunner()), store);
        await tools.ExecuteAsync(new("stage", "stage_resources", """{"files":["script.txt"]}"""), default);
        await tools.ExecuteAsync(new("verify", "test_adapter", """
            {"kind":"template","key":"line-script","name":"Line script","resourcePatterns":["*.txt"],
             "releaseFiles":[{"path":"src/adapter.py","role":"source","purpose":"Parser entry"},
                             {"path":"tools/parser.py","role":"runtime","purpose":"Script parser"}]}
            """), default);
        var verified = state.VerifiedPackage!;
        Assert.NotNull(verified);
        Assert.Equal(4, verified.ManifestVersion);
        Assert.Equal(3, verified.PackageFiles!.Count);
        Assert.False(File.Exists(Path.Combine(verified.DirectoryPath, "src/Probe.py")));
        Assert.False(File.Exists(Path.Combine(verified.DirectoryPath, "tools/unused.zip")));
        await tools.ExecuteAsync(new("edit", "write_file", """{"path":"src/Probe.py","content":"new analysis probe"}"""), default);
        Assert.Same(verified, state.VerifiedPackage);
        await tools.ExecuteAsync(new("finish", "finish", """{"summary":"Selected release verified."}"""), default);
        var published = store.LoadActive(Request().GameId)!;
        Assert.Equal(verified.ReleaseFiles, published.ReleaseFiles);
        Assert.Equal(verified.Version, published.Version);
        Assert.False(File.Exists(Path.Combine(published.DirectoryPath, "tools/unused.zip")));
    }

    [Theory]
    [InlineData("work/Probe.py", "source")]
    [InlineData("src/tools/parser.py", "runtime")]
    [InlineData("tools/../parser.py", "runtime")]
    [InlineData("tools/*", "runtime")]
    [InlineData("src/Extra.py", "analysis")]
    public async Task InvalidReleaseSelectionIsRejectedBeforeCompilation(string path, string role)
    {
        var source = Directory.CreateDirectory(Path.Combine(root, "src")).FullName;
        await File.WriteAllTextAsync(Path.Combine(source, "Program.cs"), "// fixture");
        var compiler = LegacyCompiler(new ScriptedRunner());
        var error = await Record.ExceptionAsync(() => compiler.CompileWorkspaceAsync(source, Path.Combine(root, "build"), ["*.txt"],
            releaseFiles: [new("src/Program.cs", "source", "Entry"), new(path, role, "Invalid input")]));
        Assert.True(error is GeneratedAdapterException or InvalidDataException, error?.ToString());
        Assert.False(Directory.Exists(Path.Combine(root, "build")));
    }


    [Fact]
    public async Task MissingReleaseSelectionIsRejectedAndLegacyPackagesRemainLoadable()
    {
        var source = Directory.CreateDirectory(Path.Combine(root, "src")).FullName;
        await File.WriteAllTextAsync(Path.Combine(source, "Program.cs"), "// fixture");
        var compiler = LegacyCompiler(new ScriptedRunner());
        await Assert.ThrowsAsync<GeneratedAdapterException>(() => compiler.CompileWorkspaceAsync(source, Path.Combine(root, "build"), ["*.txt"], null!));
        var package = await compiler.CompileWorkspaceAsync(source, Path.Combine(root, "build"), ["*.txt"],
            [new("src/Program.cs", "source", "Entry")], family: new("template", "line-script", "Line script"));
        var store = new LocalAdapterPackageStore(Path.Combine(root, "store"));
        await store.PublishAndActivateAsync("legacy", package with { ManifestVersion = 2, ReleaseFiles = null }, []);
        var loaded = store.LoadActive("legacy")!;
        Assert.Equal(2, loaded.ManifestVersion);
        Assert.Null(loaded.ReleaseFiles);
    }


    [Fact]
    public async Task ExplicitSampleCorrectionSurvivesCheckpointAndRequiresFreshVerification()
    {
        var original = new TextCoverageSample("text.json", "items/0/title", "Wrong title");
        var replacement = new TextCoverageSample("text.json", "dialogues/80/text", "Actual original dialogue");
        var state = new AdaptationSession { TextCoverage = new([original, replacement], ["Known omission."])
            { RuntimeChecks = ["Font not tested."] }, VerifiedPackage = CreatePackage(), VerifiedCoverageFingerprint = "old" };
        var tools = new AdaptationTools(Request(), state, Path.Combine(root, "workspace"), Runtime(new ScriptedRunner()),
            new LocalAdapterPackageStore(Path.Combine(root, "store")));
        var correction = new TextCoverageSampleCorrection(original, replacement, "The original title came from the manual.",
            "The items field is an internal key; dialogues/80/text independently contains the original dialogue.");
        await tools.ExecuteAsync(new("correct", "checkpoint", JsonSerializer.Serialize(new { summary = "Continue full tests.",
            coverageSampleCorrections = new[] { correction } }, Json)), default);
        Assert.Null(state.VerifiedPackage);
        Assert.Null(state.VerifiedCoverageFingerprint);
        Assert.Equal(replacement, Assert.Single(state.TextCoverage.Samples));
        await tools.ExecuteAsync(new("save", "checkpoint", """{"summary":"Original sample has been corrected; next step is full test_adapter."}"""), default);
        Assert.Single(state.TextCoverage.Corrections);
        Assert.Single(state.TextCoverage.Pending);
        Assert.Single(state.TextCoverage.RuntimeChecks);
    }

    [Fact]
    public async Task SuccessfulProbeArgumentsSurviveCheckpointAndRestartWithoutAutomaticRerun()
    {
        var store = new LocalAdapterPackageStore(Path.Combine(root, "store"));
        using var fixture = new EngineArchiveFixture();
        await fixture.PrepareSharedToolsAsync(store.RootDirectory);
        var workspace = Path.Combine(store.WorkDirectory, "current");
        Directory.CreateDirectory(Path.Combine(workspace, "work"));
        await File.WriteAllTextAsync(Path.Combine(workspace, "work/probe.py"), "print(42)");
        const string arguments = """{"script":"work/probe.py","arguments":["--known-boundary"]}""";
        var runs = 0;
        var runner = new ScriptedRunner { LocalProbe = () => runs++ };
        var round = 0;
        var model = new FakeModel(context =>
        {
            switch (round++)
            {
                case 0:
                    Assert.Contains(context.Tools, tool => tool.Name == "load_tool_groups");
                    Assert.DoesNotContain(context.Tools, tool => tool.Name == "run_engine_python");
                    Assert.Contains("run_engine_python", context.KnownToolNames!);
                    Assert.DoesNotContain("run_csharp", context.KnownToolNames!);
                    Assert.Contains("development:", context.Instructions);
                    return new(new("assistant", ToolCalls: [new("load", "load_tool_groups", """{"groups":["development"]}""")]), new(100, 20));
                case 1:
                    Assert.Contains(context.Tools, tool => tool.Name == "run_engine_python");
                    Assert.DoesNotContain(context.Tools, tool => tool.Name == "inspect_pe_code");
                    Assert.Contains("\"success\":true", Assert.Single(context.Messages, message => message.ToolCallId == "load").Content);
                    return new(new("assistant", ToolCalls: [new("probe", "run_engine_python", arguments)]), new(100, 20));
                case 2:
                    return new(new("assistant", ToolCalls: [new("checkpoint", "checkpoint", """{"summary":"已确认布局，下一步接入适配入口。"}""")]), new(100, 20));
                default: return CheckContext(context);
            }
        });
        AgentTurn CheckContext(AgentRequest context)
        {
            Assert.Contains(context.Tools, tool => tool.Name == "run_engine_python");
            Assert.DoesNotContain(context.Tools, tool => tool.Name == "inspect_pe_code");
            var user = context.Messages.Last(message => message.Role == "user").Content;
            Assert.Contains("work/probe.py", user);
            Assert.Contains("--known-boundary", user);
            Assert.DoesNotContain("run_csharp", user);
            Assert.Contains("历史执行成功不代表", user);
            return Blocked("needs_input", "需要显示反馈。", "提供画面后继续。");
        }
        var request = Request();
        await new AutomaticAdaptationService(model, Runtime(runner), store).AdaptAsync(request);
        Assert.Equal(1, runs);
        var state = await GameL10n.Core.Storage.LocalFiles.ReadJsonAsync<AdaptationSession>(Path.Combine(workspace, "session.json"));
        Assert.Equal(arguments, Assert.Single(state!.SuccessfulProbes).Arguments);
        Assert.Contains("development", state.EnabledToolGroups);
        state.SuccessfulProbes.Add(new("run_csharp", "{\"sourceFiles\":[\"work/legacy.cs\"]}", 0));
        await GameL10n.Core.Storage.LocalFiles.WriteJsonAsync(Path.Combine(workspace, "session.json"), state);
        await new AutomaticAdaptationService(new FakeModel(CheckContext), Runtime(runner), store).AdaptAsync(request);
        Assert.Equal(1, runs);
    }

    [Fact]
    public async Task TruncationRetriesWithSmallerOperationAndKeepsBudgetUsageAndOutputLimitOnResume()
    {
        var store = new LocalAdapterPackageStore(Path.Combine(root, "store"));
        var request = Request();
        var model = new FakeModel(context =>
        {
            if (context.MaximumOutputTokens == 10_000)
                throw new GameL10n.Core.Providers.TranslationProviderException("truncated", usage: new(500, 10_000),
                    diagnostic: new("output-token-limit"));
            Assert.Equal(20_000, context.MaximumOutputTokens);
            Assert.Contains(context.Messages, message => message.Content.Contains("整次响应未接受", StringComparison.Ordinal));
            return Blocked("needs_input", "需要一个资源样本。", "提供样本后继续。");
        });
        var result = await new AutomaticAdaptationService(model, Runtime(new ScriptedRunner()), store).AdaptAsync(request);
        Assert.Equal(2, model.Calls);
        Assert.Equal(10_620, result.Usage.TotalTokens);
        var restored = new FakeModel(context =>
        {
            Assert.Equal(20_000, context.MaximumOutputTokens);
            return Blocked("needs_input", "仍缺样本。", "提供样本后继续。");
        });
        await new AutomaticAdaptationService(restored, Runtime(new ScriptedRunner()), store).AdaptAsync(request);
        Assert.Equal(1, restored.Calls);
    }

    [Fact]
    public async Task RepeatedTruncationStopsWithoutExecutingPartialToolsOrRetryingForever()
    {
        var model = new FakeModel(_ => throw new GameL10n.Core.Providers.TranslationProviderException("truncated",
            usage: new(100, 10_000), diagnostic: new("output-token-limit")));
        var store = new LocalAdapterPackageStore(Path.Combine(root, "store"));
        var result = await new AutomaticAdaptationService(model, Runtime(new ScriptedRunner()), store).AdaptAsync(Request());
        Assert.Equal(2, model.Calls);
        Assert.Equal(AdaptationStatus.Failed, result.Status);
        Assert.Equal(20_200, result.Usage.TotalTokens);
        var state = await GameL10n.Core.Storage.LocalFiles.ReadJsonAsync<AdaptationSession>(Path.Combine(store.WorkDirectory, "current/session.json"));
        Assert.Empty(state!.PendingCalls);
        Assert.DoesNotContain(state.Messages, message => message.Role == "assistant");
    }

    [Theory]
    [InlineData("network-error")]
    [InlineData("agent-tool-format")]
    public async Task OutputRecoveryDoesNotRetryOtherFailures(string code)
    {
        var model = new FakeModel(_ => throw new GameL10n.Core.Providers.TranslationProviderException("failure",
            usage: new(100, 20), diagnostic: new(code)));
        var result = await new AutomaticAdaptationService(model, Runtime(new ScriptedRunner()),
            new LocalAdapterPackageStore(Path.Combine(root, "store"))).AdaptAsync(Request());
        Assert.Equal(1, model.Calls);
        Assert.Equal(120, result.Usage.TotalTokens);
    }

    [Fact]
    public async Task TruncationRecoveryHonorsModelCallLimit()
    {
        var model = new FakeModel(_ => throw new GameL10n.Core.Providers.TranslationProviderException("truncated",
            usage: new(100, 10_000), diagnostic: new("output-token-limit")));
        var result = await new AutomaticAdaptationService(model, Runtime(new ScriptedRunner()),
            new LocalAdapterPackageStore(Path.Combine(root, "store"))).AdaptAsync(Request() with { Limits = new(MaximumTurns: 1) });
        Assert.Equal(1, model.Calls);
        Assert.Equal(AdaptationStatus.BudgetExhausted, result.Status);
        Assert.Equal(10_100, result.Usage.TotalTokens);
    }

    [Fact]
    public async Task CoverageSamplesAndPendingWorkSurviveSummaryOnlyCheckpoints()
    {
        var state = new AdaptationSession { Objective = AdaptationObjective.Repair };
        var tools = new AdaptationTools(Request(), state, Path.Combine(root, "workspace"), Runtime(new ScriptedRunner()),
            new LocalAdapterPackageStore(Path.Combine(root, "store")));
        await tools.ExecuteAsync(new("a", "checkpoint", """{"summary":"Known fields.","coverageSamples":[{"resourcePath":"resources.assets","locator":"Item/description","sourceText":"Unlock the class."}],"pendingCoverage":["Trace item effects."],"runtimeChecks":["Check the selected font in game."]}"""), default);
        await tools.ExecuteAsync(new("b", "checkpoint", """{"summary":"Test the adapter next."}"""), default);
        var restored = JsonSerializer.Deserialize<AdaptationSession>(JsonSerializer.Serialize(state, Json), Json)!;
        Assert.Single(restored.TextCoverage.Samples);
        Assert.Equal("Trace item effects.", Assert.Single(restored.TextCoverage.Pending));
        Assert.Equal("Check the selected font in game.", Assert.Single(restored.TextCoverage.RuntimeChecks));
        Assert.Equal(AdaptationObjective.Repair, restored.Objective);
        await tools.ExecuteAsync(new("c", "checkpoint", """{"summary":"Runtime checks were reclassified from recorded evidence.","runtimeChecks":[]}"""), default);
        Assert.Empty(state.TextCoverage.RuntimeChecks);
        Assert.Single(state.TextCoverage.Pending);
    }

    [Theory]
    [InlineData(AdaptationObjective.Repair)]
    [InlineData(AdaptationObjective.ExpandCoverage)]
    public async Task InferredObjectiveSurvivesCheckpointAndResumeWithoutRepeatingTheInstruction(AdaptationObjective objective)
    {
        var store = new LocalAdapterPackageStore(Path.Combine(root, "store"));
        var request = Request() with { ImproveExisting = true,
            UserInstruction = objective == AdaptationObjective.Repair ? "修复中文编码；保留现有文本身份。" : null };
        var turns = 0;
        var model = new FakeModel(context =>
        {
            Assert.Contains("（" + objective + "）", context.Instructions);
            Assert.Contains(context.Messages, message => message.Role == "user"
                && message.Content.Contains("当前任务目标", StringComparison.Ordinal)
                && message.Content.Contains("（" + objective + "）", StringComparison.Ordinal));
            return turns++ == 0
                ? new(new("assistant", ToolCalls: [new("save", "checkpoint", """{"summary":"现有实现保留；下一步验证修正后的资源。"}""")]), new(100, 20))
                : Blocked("needs_input", "需要一个缺失的资源包。", "补齐资源包后继续。");
        });
        await new AutomaticAdaptationService(model, Runtime(new ScriptedRunner()), store).AdaptAsync(request);
        Assert.Equal(2, model.Calls);
        var continuation = new FakeModel(context =>
        {
            Assert.Contains("（" + objective + "）", context.Instructions);
            Assert.Contains("（" + objective + "）", context.Messages.Last(message => message.Role == "user").Content);
            return Blocked("needs_input", "仍缺资源包。", "补齐资源包后继续。");
        });
        await new AutomaticAdaptationService(continuation, Runtime(new ScriptedRunner()), store)
            .AdaptAsync(request with { ImproveExisting = false, UserInstruction = null });
        var saved = await GameL10n.Core.Storage.LocalFiles.ReadJsonAsync<AdaptationSession>(
            Path.Combine(store.WorkDirectory, "current", "session.json"));
        Assert.Equal(objective, saved!.Objective);
        Assert.Equal(1, continuation.Calls);
    }

    [Theory]
    [InlineData(false)]
    [InlineData(true)]
    public async Task RepairCanPublishWithPendingRuntimeChecksWhileKnownMissingCoverageRemainsExplicit(bool missingCoverage)
    {
        var request = Request() with { ImproveExisting = true, Objective = AdaptationObjective.Repair,
            BaselineUnits = [new("line-0", "script.txt", "line:0", "First line", "fixture"),
                new("line-1", "script.txt", "line:1", "Second line", "fixture")] };
        var workspace = Path.Combine(root, "workspace");
        Directory.CreateDirectory(Path.Combine(workspace, "src"));
        await File.WriteAllTextAsync(Path.Combine(workspace, "src/adapter.py"), "# fixture");
        var state = new AdaptationSession { Objective = AdaptationObjective.Repair };
        var store = new LocalAdapterPackageStore(Path.Combine(root, "store"));
        var tools = new AdaptationTools(request, state, workspace, Runtime(new ScriptedRunner()), store);
        await tools.ExecuteAsync(new("stage", "stage_resources", """{"files":["script.txt"]}"""), default);
        var testOutput = await tools.ExecuteAsync(new("test", "test_adapter", JsonSerializer.Serialize(new
        {
            kind = "template", key = "line-script", name = "Line script", resourcePatterns = new[] { "*.txt" },
            releaseFiles = new[] { new AdapterReleaseFile("src/adapter.py", "source", "Fixture parser") },
            pendingCoverage = missingCoverage ? new[] { "Options menu is not extracted." } : [],
            runtimeChecks = new[] { "Confirm original font selection in game." }
        })), default);
        using var test = JsonDocument.Parse(testOutput);
        Assert.True(test.RootElement.GetProperty("result").GetProperty("passed").GetBoolean());
        Assert.Equal("finish-ready", test.RootElement.GetProperty("result").GetProperty("stage").GetString());
        Assert.Contains("不要自动开始补全其他文字类别", test.RootElement.GetProperty("result").GetProperty("nextAction").GetString());
        if (missingCoverage)
            await Assert.ThrowsAsync<InvalidDataException>(() => tools.ExecuteAsync(new("unacknowledged", "finish",
                """{"summary":"中文编码修复完成。"}"""), default));
        var finishOutput = await tools.ExecuteAsync(new("finish", "finish", JsonSerializer.Serialize(new
            { summary = "中文编码修复已通过资源验收；运行效果仍需确认。", allowPartial = missingCoverage })), default);
        Assert.NotNull(store.LoadActive(request.GameId));
        using var finish = JsonDocument.Parse(finishOutput);
        Assert.Equal(missingCoverage ? "partial" : "not-assessed", finish.RootElement.GetProperty("result").GetProperty("coverageStatus").GetString());
        Assert.Equal(missingCoverage ? 1 : 0, state.TextCoverage.Pending.Count);
        Assert.Contains("运行效果待验证", state.Summary);
        if (missingCoverage) Assert.Contains("Options menu is not extracted.", state.Summary);
    }

    [Theory]
    [InlineData(AdaptationObjective.ExpandCoverage)]
    [InlineData(AdaptationObjective.Repair)]
    public async Task RepairRequiresRetestingAVerificationThatDidNotCheckItsExistingTextBaseline(AdaptationObjective previousObjective)
    {
        var request = Request() with { Objective = AdaptationObjective.Repair,
            BaselineUnits = [new("line-0", "script.txt", "line:0", "First line", "fixture")] };
        var store = new LocalAdapterPackageStore(Path.Combine(root, "store"));
        var state = new AdaptationSession { Objective = AdaptationObjective.Repair, VerifiedPackage = CreatePackage(),
            VerificationRevision = GeneratedAdapterVerifier.Revision, VerifiedObjective = previousObjective,
            VerifiedBaselineFingerprint = "previous-baseline" };
        var tools = new AdaptationTools(request, state, Path.Combine(root, "workspace"), Runtime(new ScriptedRunner()), store);
        var error = await Assert.ThrowsAsync<InvalidDataException>(() => tools.ExecuteAsync(
            new("finish", "finish", """{"summary":"Repair complete."}"""), default));
        Assert.Contains("原有文本基线", error.Message);
        Assert.Null(store.LoadActive(request.GameId));
    }

    [Fact]
    public async Task RepairWithoutExistingTextEvidenceStillRequiresAnIndependentSample()
    {
        var request = Request() with { ImproveExisting = true, Objective = AdaptationObjective.Repair };
        var store = new LocalAdapterPackageStore(Path.Combine(root, "store"));
        var state = new AdaptationSession { Objective = AdaptationObjective.Repair };
        var tools = new AdaptationTools(request, state, Path.Combine(root, "workspace"), Runtime(new ScriptedRunner()), store);
        var error = await Assert.ThrowsAsync<InvalidDataException>(() => tools.ExecuteAsync(
            new("finish", "finish", """{"summary":"Repair complete."}"""), default));
        Assert.Contains("coverageSamples", error.Message);
        Assert.Null(store.LoadActive(request.GameId));
    }

    [Theory]
    [InlineData("First line", true)]
    [InlineData("Missing item description", false)]
    public async Task PublicationChecksIndependentSamplesAndRequiresExplicitPartialSupport(string expected, bool valid)
    {
        var request = Request() with { ImproveExisting = true };
        var workspace = Path.Combine(root, "workspace");
        Directory.CreateDirectory(Path.Combine(workspace, "src"));
        await File.WriteAllTextAsync(Path.Combine(workspace, "src/adapter.py"), "# fixture");
        var state = new AdaptationSession();
        var store = new LocalAdapterPackageStore(Path.Combine(root, "store"));
        var tools = new AdaptationTools(request, state, workspace, Runtime(new ScriptedRunner()), store);
        await tools.ExecuteAsync(new("stage", "stage_resources", """{"files":["script.txt"]}"""), default);
        await tools.ExecuteAsync(new("test", "test_adapter", JsonSerializer.Serialize(new
        {
            kind = "template", key = "line-script", name = "Line script", resourcePatterns = new[] { "*.txt" },
            releaseFiles = new[] { new AdapterReleaseFile("src/adapter.py", "source", "Fixture parser") },
            coverageSamples = new[] { new { resourcePath = "script.txt", locator = "line:0", sourceText = expected } },
            pendingCoverage = new[] { "Input labels need a display-only mapping." }
        })), default);
        Assert.Equal(valid, state.VerifiedPackage is not null);
        await Assert.ThrowsAsync<InvalidDataException>(() => tools.ExecuteAsync(new("finish", "finish", """{"summary":"Done"}"""), default));
        Assert.Null(store.LoadActive(request.GameId));
        if (valid)
        {
            await tools.ExecuteAsync(new("partial", "finish", """{"summary":"Input labels use internal lookup keys; display layer is still unresolved.","allowPartial":true}"""), default);
            Assert.NotNull(store.LoadActive(request.GameId));
            Assert.StartsWith("已发布部分文字支持", state.Summary);
            Assert.Single(state.TextCoverage.Pending);
        }
        else Assert.Contains(state.VerificationDiagnostics, message => message.Contains("提取结果与保留样本不符"));
    }

    [Fact]
    public async Task EngineReferenceLookupSeparatesRelatedFormatsAndReusesIndexWithoutWorkspaceFiles()
    {
        Directory.Delete(Path.Combine(root, "store", "engine-tools"), recursive: true);
        var requests = 0;
        using var client = new HttpClient(new ResearchHandler(_ =>
        {
            requests++;
            return new(System.Net.HttpStatusCode.OK) { Content = new StringContent("""
                {"engines":[
                  {"id":"qlie","title":"QLIE PACK3","aliases":["QLIE","ImoScripter"],"document":"engines/qlie.md",
                   "python_references":["python/engines/qlie_imos.py"],"scope":"PACK3 only","evidence":"synthetic tests"},
                  {"id":"bishop","title":"Bishop GSC","aliases":["Bishop"],"document":"engines/bishop.md",
                   "python_references":["python/engines/bishop.py"],"scope":"GSC only"},
                  {"id":"alicesoft","title":"AliceSoft AIN","aliases":["AIN"],"document":"engines/alicesoft.md"}
                ]}
                """) };
        }));
        var workspace = Path.Combine(root, "workspace");
        var tools = new AdaptationTools(Request(), new(), workspace, Runtime(new ScriptedRunner()),
            new LocalAdapterPackageStore(Path.Combine(root, "store")), client);
        using var qlie = JsonDocument.Parse(await tools.ExecuteAsync(new("ref", "search_engine_references", """{"query":"ImoScripter"}"""), default));
        var result = qlie.RootElement.GetProperty("result");
        var match = Assert.Single(result.GetProperty("matches").EnumerateArray());
        Assert.Equal("qlie", match.GetProperty("id").GetString());
        Assert.Equal("PACK3 only", match.GetProperty("scope").GetString());
        Assert.EndsWith("/python/engines/qlie_imos.py", match.GetProperty("sourceUrls")[0].GetString());
        Assert.Equal(64, result.GetProperty("contentSha256").GetString()!.Length);
        using var bishop = JsonDocument.Parse(await tools.ExecuteAsync(new("other", "search_engine_references", """{"query":"BSScript Rev.6"}"""), default));
        Assert.Empty(bishop.RootElement.GetProperty("result").GetProperty("matches").EnumerateArray());
        Assert.Contains("GSC", bishop.RootElement.GetProperty("result").GetProperty("guidance").ToString());
        using var system4 = JsonDocument.Parse(await tools.ExecuteAsync(new("system4", "search_engine_references", """{"query":"System4"}"""), default));
        Assert.Equal("alicesoft", Assert.Single(system4.RootElement.GetProperty("result").GetProperty("matches").EnumerateArray()).GetProperty("id").GetString());
        Assert.Equal(1, requests);
        Assert.False(Directory.Exists(workspace));
        Assert.True(AdaptationTools.CanBatch("search_engine_references"));
    }

    [Fact]
    public async Task UnavailableReferenceIndexIsReportedAsUnavailableAndCanBeRetried()
    {
        Directory.Delete(Path.Combine(root, "store", "engine-tools"), recursive: true);
        var calls = 0;
        using var client = new HttpClient(new ResearchHandler(_ => ++calls == 1
            ? new(System.Net.HttpStatusCode.TooManyRequests)
            : new(System.Net.HttpStatusCode.OK) { Content = new StringContent("""{"engines":[]}""") }));
        var tools = new AdaptationTools(Request(), new(), Path.Combine(root, "workspace"), Runtime(new ScriptedRunner()),
            new LocalAdapterPackageStore(Path.Combine(root, "store")), client);
        using var first = JsonDocument.Parse(await tools.ExecuteAsync(new("ref", "search_engine_references", """{"query":"cp932"}"""), default));
        var result = first.RootElement.GetProperty("result");
        Assert.Equal("unavailable", result.GetProperty("status").GetString());
        Assert.Contains("429", result.GetProperty("error").GetString());
        Assert.Contains("jis-substitution.md", result.GetProperty("guidance").ToString());
        using var retry = JsonDocument.Parse(await tools.ExecuteAsync(new("retry", "search_engine_references", """{"query":"cp932"}"""), default));
        Assert.Equal("queried", retry.RootElement.GetProperty("result").GetProperty("status").GetString());
        Assert.Equal(2, calls);
    }

    [Fact]
    public async Task ReferenceSearchFindsLateMatchesAndPaginatesWithoutRefetching()
    {
        var content = new string('x', 600_000) + " BSScript Rev.6 first " + new string('y', 30_000) + " BSScript Rev.6 second ";
        var requests = 0;
        var unavailable = false;
        using var client = new HttpClient(new ResearchHandler(_ =>
        {
            requests++;
            return unavailable ? new(System.Net.HttpStatusCode.ServiceUnavailable)
                : new(System.Net.HttpStatusCode.OK) { Content = new StringContent(content) };
        }));
        var store = new LocalAdapterPackageStore(Path.Combine(root, "store"));
        var tools = new AdaptationTools(Request(), new(), Path.Combine(root, "workspace"), Runtime(new ScriptedRunner()),
            store, client);
        using var first = JsonDocument.Parse(await tools.ExecuteAsync(new("read", "fetch_url",
            """{"url":"https://example.com/formats.json","query":"BSScript Rev.6","length":100}"""), default));
        var result = first.RootElement.GetProperty("result");
        var match = Assert.Single(result.GetProperty("matches").EnumerateArray());
        Assert.Contains("first", match.GetProperty("content").GetString());
        Assert.True(result.GetProperty("more").GetBoolean());
        var offset = result.GetProperty("nextOffset").GetInt32();
        using var second = JsonDocument.Parse(await tools.ExecuteAsync(new("read2", "fetch_url", JsonSerializer.Serialize(new
            { url = "https://example.com/formats.json", query = "BSScript Rev.6", offset, length = 100 })), default));
        Assert.Contains("second", second.RootElement.GetProperty("result").GetProperty("matches")[0].GetProperty("content").GetString());
        Assert.False(second.RootElement.GetProperty("result").GetProperty("more").GetBoolean());
        using var slice = JsonDocument.Parse(await tools.ExecuteAsync(new("slice", "fetch_url", JsonSerializer.Serialize(new
            { url = "https://example.com/formats.json", offset = 600_000, length = 21 })), default));
        Assert.Equal(content.Substring(600_000, 21), slice.RootElement.GetProperty("result").GetProperty("content").GetString());
        Assert.Equal(1, requests);

        // A different project/tool instance reuses the persisted document and its actual fetch time.
        var resumed = new AdaptationTools(Request(), new(), Path.Combine(root, "other-workspace"), Runtime(new ScriptedRunner()), store, client);
        using var persisted = JsonDocument.Parse(await resumed.ExecuteAsync(new("persisted", "fetch_url",
            """{"url":"https://example.com/formats.json","length":21}"""), default));
        Assert.True(persisted.RootElement.GetProperty("result").GetProperty("reused").GetBoolean());
        Assert.Equal(result.GetProperty("retrievedAt").GetString(), persisted.RootElement.GetProperty("result").GetProperty("retrievedAt").GetString());
        Assert.Equal(1, requests);

        var receiptPath = Assert.Single(Directory.GetFiles(Path.Combine(store.RootDirectory, "downloads"), "source.json", SearchOption.AllDirectories));
        var receipt = System.Text.Json.Nodes.JsonNode.Parse(await File.ReadAllTextAsync(receiptPath))!;
        var expiredAt = DateTimeOffset.UtcNow.AddDays(-8);
        receipt["retrievedAt"] = expiredAt;
        await File.WriteAllTextAsync(receiptPath, receipt.ToJsonString());
        unavailable = true;
        resumed = new(Request(), new(), Path.Combine(root, "other-workspace"), Runtime(new ScriptedRunner()), store, client);
        using var stale = JsonDocument.Parse(await resumed.ExecuteAsync(new("offline", "fetch_url",
            """{"url":"https://example.com/formats.json","length":21}"""), default));
        Assert.True(stale.RootElement.GetProperty("result").GetProperty("stale").GetBoolean());
        Assert.Equal(expiredAt, stale.RootElement.GetProperty("result").GetProperty("retrievedAt").GetDateTimeOffset());
        Assert.Equal(2, requests);
        await Assert.ThrowsAsync<HttpRequestException>(() => resumed.ExecuteAsync(new("refresh-failed", "fetch_url",
            """{"url":"https://example.com/formats.json","refresh":true}"""), default));
        Assert.Equal(3, requests);
        unavailable = false;
        content = "updated source";
        using var fresh = JsonDocument.Parse(await resumed.ExecuteAsync(new("refresh", "fetch_url",
            """{"url":"https://example.com/formats.json","refresh":true}"""), default));
        Assert.Equal("updated source", fresh.RootElement.GetProperty("result").GetProperty("content").GetString());
        Assert.False(fresh.RootElement.GetProperty("result").GetProperty("reused").GetBoolean());
        Assert.False(fresh.RootElement.GetProperty("result").GetProperty("stale").GetBoolean());
        Assert.Equal(4, requests);
        Assert.False(Directory.Exists(Path.Combine(root, "workspace")));
        Assert.False(Directory.Exists(Path.Combine(root, "other-workspace")));
    }

    [Fact]
    public async Task ToolDownloadsShareVerifiedCopiesWithoutChangingExistingProjectPins()
    {
        var requests = 0;
        var body = "tool version one";
        var unavailable = false;
        using var client = new HttpClient(new ResearchHandler(_ =>
        {
            requests++;
            return unavailable ? new(System.Net.HttpStatusCode.ServiceUnavailable)
                : new(System.Net.HttpStatusCode.OK) { Content = new StringContent(body) };
        }));
        var store = new LocalAdapterPackageStore(Path.Combine(root, "store"));
        var firstWorkspace = Path.Combine(root, "first-workspace");
        var secondWorkspace = Path.Combine(root, "second-workspace");
        var firstState = new AdaptationSession();
        var first = new AdaptationTools(Request(), firstState, firstWorkspace, Runtime(new ScriptedRunner()), store, client);
        var second = new AdaptationTools(Request(), new(), secondWorkspace, Runtime(new ScriptedRunner()), store, client);
        var download = new AgentToolCall("download", "download_tool", """
            {"url":"https://example.com/tool.bin","path":"src/tools/tool.bin","releaseRole":"runtime","purpose":"Fixture parser"}
            """);
        var copies = await Task.WhenAll(first.ExecuteAsync(download, default), second.ExecuteAsync(download, default));
        Assert.Equal(1, requests);
        using var firstCopy = JsonDocument.Parse(copies[0]);
        using var secondCopy = JsonDocument.Parse(copies[1]);
        Assert.NotEqual(firstCopy.RootElement.GetProperty("result").GetProperty("reused").GetBoolean(),
            secondCopy.RootElement.GetProperty("result").GetProperty("reused").GetBoolean());
        var originalHash = LocalFiles.HashText(body);
        var firstPath = Path.Combine(firstWorkspace, "src", "tools", "tool.bin");
        var secondPath = Path.Combine(secondWorkspace, "src", "tools", "tool.bin");
        Assert.Equal(body, await File.ReadAllTextAsync(firstPath));
        Assert.Contains(firstState.ReleaseFiles!, file => file.Path == "tools/tool.bin" && file.Role == "runtime");
        using var provenance = JsonDocument.Parse(await File.ReadAllTextAsync(Path.Combine(firstWorkspace, "src", "tools", "sources.json")));
        Assert.Equal(originalHash, provenance.RootElement.GetProperty("tool.bin").GetProperty("sha256").GetString());

        // Updating another project does not update an existing project's fixed tool.
        body = "tool version two";
        var refresh = download with { Arguments = """{"url":"https://example.com/tool.bin","path":"src/tools/tool.bin","refresh":true}""" };
        await second.ExecuteAsync(refresh, default);
        await first.ExecuteAsync(download, default);
        Assert.Equal(2, requests);
        Assert.Equal("tool version one", await File.ReadAllTextAsync(firstPath));
        Assert.Equal("tool version two", await File.ReadAllTextAsync(secondPath));
        File.Delete(firstPath);
        await first.ExecuteAsync(download, default);
        Assert.Equal("tool version one", await File.ReadAllTextAsync(firstPath));
        Assert.Equal(2, requests);

        // Failed refreshes leave the usable file and the last completed cache receipt intact.
        unavailable = true;
        await Assert.ThrowsAsync<HttpRequestException>(() => second.ExecuteAsync(refresh, default));
        await second.ExecuteAsync(download, default);
        Assert.Equal(3, requests);
        Assert.Equal("tool version two", await File.ReadAllTextAsync(secondPath));
        unavailable = false;

        // Cache corruption is repaired from upstream, never copied to a new project.
        var cacheRoot = Path.Combine(store.RootDirectory, "downloads");
        var currentBlob = Assert.Single(Directory.GetFiles(cacheRoot, LocalFiles.HashText(body) + ".bin", SearchOption.AllDirectories));
        await File.WriteAllTextAsync(currentBlob, new string('x', body.Length));
        var thirdWorkspace = Path.Combine(root, "third-workspace");
        var third = new AdaptationTools(Request(), new(), thirdWorkspace, Runtime(new ScriptedRunner()), store, client);
        await third.ExecuteAsync(download, default);
        Assert.Equal(4, requests);
        Assert.Equal(body, await File.ReadAllTextAsync(Path.Combine(thirdWorkspace, "src", "tools", "tool.bin")));
        await third.ExecuteAsync(new("analysis-copy", "download_tool", """{"url":"https://example.com/tool.bin","path":"work/a"}"""), default);
        Assert.Equal(body, await File.ReadAllTextAsync(Path.Combine(thirdWorkspace, "work", "a")));
        Assert.Equal(4, requests);

        await File.WriteAllTextAsync(firstPath, "local edit");
        await Assert.ThrowsAsync<InvalidDataException>(() => first.ExecuteAsync(download, default));
        Assert.Equal("local edit", await File.ReadAllTextAsync(firstPath));
        Assert.Equal(4, requests);
        await first.ExecuteAsync(refresh, default);
        Assert.Equal(body, await File.ReadAllTextAsync(firstPath));
        Assert.Equal(5, requests);
        Assert.Empty(Directory.GetFiles(cacheRoot, "*.partial", SearchOption.AllDirectories));
    }

    [Fact]
    public async Task GitHubSearchReportsUpstreamTruncationEvenWhenNoKeywordMatches()
    {
        using var client = new HttpClient(new ResearchHandler(_ => new(System.Net.HttpStatusCode.OK)
            { Content = new StringContent("""{"tree":[],"truncated":true}""") }));
        var tools = new AdaptationTools(Request(), new(), Path.Combine(root, "workspace"), Runtime(new ScriptedRunner()),
            new LocalAdapterPackageStore(Path.Combine(root, "store")), client);
        using var read = JsonDocument.Parse(await tools.ExecuteAsync(new("tree", "fetch_url",
            """{"url":"https://api.github.com/repos/example/repo/git/trees/main?recursive=1","query":"Bishop"}"""), default));
        Assert.True(read.RootElement.GetProperty("result").GetProperty("upstreamTruncated").GetBoolean());
        Assert.Empty(read.RootElement.GetProperty("result").GetProperty("matches").EnumerateArray());
    }

    [Fact]
    public async Task OversizedReferenceIsRejectedRatherThanSearchedAsACompleteDocument()
    {
        using var client = new HttpClient(new ResearchHandler(_ => new(System.Net.HttpStatusCode.OK)
            { Content = new StringContent(new string('x', 8 * 1024 * 1024 + 1)) }));
        var tools = new AdaptationTools(Request(), new(), Path.Combine(root, "workspace"), Runtime(new ScriptedRunner()),
            new LocalAdapterPackageStore(Path.Combine(root, "store")), client);
        var error = await Assert.ThrowsAsync<InvalidDataException>(() => tools.ExecuteAsync(new("large", "fetch_url",
            """{"url":"https://example.com/large","query":"Bishop"}"""), default));
        Assert.Contains("未将截断内容当作完整资料", error.Message);
    }

    private sealed class ResearchHandler(Func<HttpRequestMessage, HttpResponseMessage> respond) : HttpMessageHandler
    {
        protected override Task<HttpResponseMessage> SendAsync(HttpRequestMessage request, CancellationToken cancellationToken)
            => Task.FromResult(respond(request));
    }

    [Fact]
    public async Task BuiltinCandidatesAndRegularProbeSurviveCheckpointAndResumeWithoutExtraModelCalls()
    {
        var request = Request() with
        {
            BuiltinProbes = [new("Renpy", "script-probe-passed", ["script.rpy"], null, DateTimeOffset.UtcNow)]
        };
        await File.WriteAllTextAsync(Path.Combine(request.GameDirectory, "script.rpy"), "label start:\n    \"こんにちは。\"\n");
        var store = new LocalAdapterPackageStore(Path.Combine(root, "store"));
        var trialCalls = 0;
        var runner = new ScriptedRunner { ObserveRequest = call =>
        {
            if (call.StandardInput.Contains("\"format\":\"Renpy\"", StringComparison.Ordinal)) trialCalls++;
        } };
        // Protocol fixtures keep parser behavior real while avoiding a Windows apphost dependency.
        var executable = Path.Combine(root, "worker.exe");
        var assembly = Path.Combine(root, "worker.dll");
        await File.WriteAllTextAsync(executable, "fixture apphost");
        await File.WriteAllTextAsync(assembly, "fixture parser version");
        var trial = new BuiltinAdapterTrial(runner, executable, assembly);
        var turn = 0;
        var model = new FakeModel(context =>
        {
            Assert.Contains(context.Messages, message => message.Content.Contains("Renpy", StringComparison.Ordinal)
                && message.Content.Contains("script-probe-passed", StringComparison.Ordinal));
            Assert.Contains(context.Tools, tool => tool.Name == "try_builtin_adapter");
            Assert.Contains(context.Tools, tool => tool.Name == "inspect_builtin_adapter");
            Assert.Equal(2, trialCalls); // Extraction and original-byte build precede the first model request.
            Assert.Contains(context.Messages, message => message.Content.Contains("こんにちは。", StringComparison.Ordinal));
            return turn++ == 0
                ? new(new("assistant", ToolCalls: [new("save", "checkpoint", """{"summary":"Reuse the Renpy parser; verify the font next."}""")]), new(100, 20))
                : Blocked("needs_input", "需要启动反馈。", "提供游戏运行报错。");
        });
        await new AutomaticAdaptationService(model, Runtime(runner), store, builtinTrial: trial).AdaptAsync(request);
        Assert.Equal(2, model.Calls);
        var statePath = Path.Combine(store.WorkDirectory, "current", "session.json");
        var first = JsonSerializer.Deserialize<AdaptationSession>(await File.ReadAllTextAsync(statePath), Json)!;
        Assert.Contains(first.BuiltinDiscovery!.Candidates, candidate => candidate.Format == "Renpy");
        var observation = Assert.Single(first.BuiltinTrials);
        Assert.Equal("passed", observation.Status);
        Assert.Equal("passed", observation.RoundtripStatus);
        Assert.NotNull(observation.Fingerprint);
        Assert.NotEmpty(observation.Samples!);
        Assert.Equal("completed", first.AutomaticTrials!.Status);
        Assert.Equal(await File.ReadAllBytesAsync(Path.Combine(request.GameDirectory, "script.rpy")),
            await File.ReadAllBytesAsync(Path.Combine(store.WorkDirectory, "current", "resources", "script.rpy")));
        Assert.False(Directory.Exists(Path.Combine(store.WorkDirectory, "current", "src")));
        Assert.Null(store.LoadActive(request.GameId));

        await new AutomaticAdaptationService(model, Runtime(runner), store, builtinTrial: trial)
            .AdaptAsync(request with { BuiltinProbes = null });
        Assert.Equal(3, model.Calls);
        var resumed = JsonSerializer.Deserialize<AdaptationSession>(await File.ReadAllTextAsync(statePath), Json)!;
        Assert.Equal(first.BuiltinDiscoveryObservedAt, resumed.BuiltinDiscoveryObservedAt);
        Assert.Equal("script-probe-passed", Assert.Single(resumed.BuiltinProbes).Status);
        Assert.Equal(observation.Fingerprint, Assert.Single(resumed.BuiltinTrials).Fingerprint);
        Assert.Equal(2, trialCalls);
    }

    [Fact]
    public async Task AdapterListExposesBundledParsersAndCanRefreshSavedCandidates()
    {
        var request = Request();
        var workspace = Path.Combine(root, "workspace");
        var state = new AdaptationSession();
        var tools = new AdaptationTools(request, state, workspace, Runtime(new ScriptedRunner()),
            new LocalAdapterPackageStore(Path.Combine(root, "store")));
        using var first = JsonDocument.Parse(await tools.ExecuteAsync(new("list", "list_adapters", """{"query":"Renpy"}"""), default));
        var builtin = Assert.Single(first.RootElement.GetProperty("result").GetProperty("builtIns").EnumerateArray());
        Assert.Equal("Renpy", builtin.GetProperty("format").GetString());
        Assert.Empty(state.BuiltinDiscovery!.Candidates);
        await File.WriteAllTextAsync(Path.Combine(request.GameDirectory, "script.rpy"), "label start:\n    \"こんにちは。\"\n");
        await tools.ExecuteAsync(new("refresh", "list_adapters", """{"query":"Renpy","refreshCandidates":true}"""), default);
        Assert.Contains(state.BuiltinDiscovery!.Candidates, candidate => candidate.Format == "Renpy");
        Assert.False(Directory.Exists(workspace));
    }

    [Fact]
    public async Task AgentCanInspectAndCopyComponentsWithoutPublishingThem()
    {
        var request = Request();
        var workspace = Path.Combine(root, "workspace");
        var state = new AdaptationSession { VerifiedPackage = CreatePackage() };
        var tools = new AdaptationTools(request, state, workspace, Runtime(new ScriptedRunner()),
            new LocalAdapterPackageStore(Path.Combine(root, "store")));
        using var list = JsonDocument.Parse(await tools.ExecuteAsync(new("list", "list_adapters", """{"query":"cp932"}"""), default));
        Assert.Contains(list.RootElement.GetProperty("result").GetProperty("components").EnumerateArray(),
            item => item.GetProperty("id").GetString() == "cp932-glyph-map");
        using var description = JsonDocument.Parse(await tools.ExecuteAsync(new("inspect", "inspect_adapter_component", """{"id":"cp932-glyph-map"}"""), default));
        Assert.Contains("allowedSlots", description.RootElement.GetProperty("result").GetProperty("api").GetString());
        Assert.NotNull(state.VerifiedPackage);
        Assert.False(Directory.Exists(workspace));
        await tools.ExecuteAsync(new("copy", "copy_adapter_components", """{"ids":["cp932-glyph-map"]}"""), default);
        Assert.Null(state.VerifiedPackage);
        Assert.Null(tools.PublishedAdapter);
        Assert.True(File.Exists(Path.Combine(workspace, "src/tools/gamel10n_host.py")));
        Assert.True(AdaptationTools.CanBatch("inspect_adapter_component"));
        Assert.False(AdaptationTools.CanBatch("copy_adapter_components"));
    }

    [Fact]
    public async Task LaunchEvidenceSurvivesCheckpointAndResumeWithoutExtraModelCalls()
    {
        var request = Request();
        await File.WriteAllTextAsync(Path.Combine(request.GameDirectory, "game.exe"), "fixture");
        await File.WriteAllTextAsync(Path.Combine(request.GameDirectory, "game.exe.le.config"),
            "<LEConfig><Profiles><Profile><Location>ja-JP</Location></Profile></Profiles></LEConfig>");
        var store = new LocalAdapterPackageStore(Path.Combine(root, "store"));
        var turn = 0;
        var model = new FakeModel(context =>
        {
            Assert.Contains(context.Messages, message => message.Content.Contains("ja-JP", StringComparison.Ordinal)
                && message.Content.Contains("game.exe.le.config", StringComparison.Ordinal));
            return turn++ == 0
                ? new(new("assistant", ToolCalls: [new("save", "checkpoint", """{"summary":"Parser work is pending."}""")]), new(100, 20))
                : Blocked("needs_input", "需要启动反馈。", "提供游戏运行报错。");
        });
        await new AutomaticAdaptationService(model, Runtime(new ScriptedRunner()), store).AdaptAsync(request);
        Assert.Equal(2, model.Calls);
        await new AutomaticAdaptationService(model, Runtime(new ScriptedRunner()), store).AdaptAsync(request);
        Assert.Equal(3, model.Calls);
    }

    [Fact]
    public async Task ResumedToolBatchReceivesLaunchEvidenceAfterItsToolResults()
    {
        var request = Request();
        await File.WriteAllTextAsync(Path.Combine(request.GameDirectory, "start.cmd"), "legacy.exe");
        var pending = new AgentToolCall("files", "list_files", """{"root":"game"}""");
        var store = new LocalAdapterPackageStore(Path.Combine(root, "store"));
        await GameL10n.Core.Storage.LocalFiles.WriteJsonAsync(Path.Combine(store.WorkDirectory, "current", "session.json"),
            new AdaptationSession { GameId = request.GameId, GameDirectory = request.GameDirectory, ModelIdentity = "fake",
                Messages = [new("user", "develop"), new("assistant", ToolCalls: [pending])], PendingCalls = [pending] });
        var model = new FakeModel(context =>
        {
            var messages = context.Messages.ToList();
            var toolIndex = messages.FindIndex(message => message.Role == "tool" && message.ToolCallId == "files");
            var evidenceIndex = messages.FindIndex(message => message.Role == "user" && message.Content.Contains("legacy.exe", StringComparison.Ordinal));
            Assert.True(toolIndex >= 0 && evidenceIndex > toolIndex);
            return Blocked("needs_input", "需要启动反馈。", "提供游戏运行报错。");
        });
        await new AutomaticAdaptationService(model, Runtime(new ScriptedRunner()), store).AdaptAsync(request);
        Assert.Equal(1, model.Calls);
    }

    [Fact]
    public async Task HostRejectsUndeclaredLauncherEvenWhenTextRoundtripWorks()
    {
        var request = Request();
        var compiler = LegacyCompiler(new ScriptedRunner { UndeclaredLaunchScript = true });
        var package = await compiler.CompileAsync("// fixture", Path.Combine(root, "compile"), ["script.txt"],
            family: new("template", "launcher-test", "Launcher fixture"));
        var adapter = new GeneratedGameAdapter(package, compiler.Toolchain, compiler.Runner, Path.Combine(root, "run"));
        var result = await new GeneratedAdapterVerifier().VerifyAsync(adapter, request.GameDirectory, Path.Combine(root, "verification"));
        Assert.False(result.Passed);
        Assert.Contains(result.Diagnostics, diagnostic => diagnostic.Contains("未声明启动入口", StringComparison.Ordinal));
        Assert.False(File.Exists(Path.Combine(request.GameDirectory, "start.cmd")));
    }


    [Fact]
    public async Task TestingRequiresLocaleProfileAlongsideItsSelectedExecutable()
    {
        var request = Request();
        await File.WriteAllTextAsync(Path.Combine(request.GameDirectory, "game.exe"), "fixture");
        await File.WriteAllTextAsync(Path.Combine(request.GameDirectory, "game.exe.le.config"),
            "<LEConfig><Profiles><Profile><Location>ja-JP</Location></Profile></Profiles></LEConfig>");
        var state = new AdaptationSession { LaunchInspection = await GameLaunchInspector.InspectAsync(request.GameDirectory),
            ResourceHashes = new() { ["game.exe"] = "fixture" } };
        var store = new LocalAdapterPackageStore(Path.Combine(root, "store"));
        var tools = new AdaptationTools(request, state, Path.Combine(root, "workspace"), Runtime(new ScriptedRunner()), store);
        var error = await Assert.ThrowsAsync<InvalidDataException>(() => tools.ExecuteAsync(new("test", "test_adapter",
            """{"kind":"template","key":"locale-test","name":"Locale fixture","resourcePatterns":["*.exe"]}"""), default));
        Assert.Contains("game.exe.le.config", error.Message);
        Assert.Null(state.VerifiedPackage);
    }

    [Fact]
    public async Task ToolOnlyProgressCanAccompanyCheckpointAndSurvivesRestart()
    {
        var store = new LocalAdapterPackageStore(Path.Combine(root, "store"));
        var updates = new ProgressRecorder();
        var turn = 0;
        var model = new FakeModel(_ => turn++ == 0
            ? new(new("assistant", ToolCalls: [
                new("progress", "report_progress", """{"kind":"breakthrough","message":"已确认索引结构，正在实现回填。"}"""),
                new("save", "checkpoint", """{"summary":"Index understood; implement writer."}""")]), new(100, 20))
            : new(new("assistant", ToolCalls: [new("list", "list_files", """{"root":"workspace"}""")]), new(100, 20)));
        var result = await new AutomaticAdaptationService(model, Runtime(new ScriptedRunner()), store)
            .AdaptAsync(Request() with { Limits = new(MaximumTurns: 2) }, progress: updates);
        Assert.Equal("breakthrough", result.ProgressNote!.Kind);
        Assert.Equal(result.ProgressNote, result.LastBreakthrough);
        Assert.Contains(updates.Updates, update => update.ProgressNote == result.ProgressNote);
        Assert.Equal(2, model.Calls);
        var saved = await GameL10n.Core.Storage.LocalFiles.ReadJsonAsync<AdaptationSession>(Path.Combine(store.WorkDirectory, "current/session.json"));
        Assert.Equal(result.ProgressNote, saved!.ProgressNote);
        Assert.Contains("Index understood", saved.Notes);
        Assert.Null(saved.VerifiedPackage);
    }

    [Fact]
    public async Task ProgressDoesNotAllowMixedReadAndMutationOrClaimHostVerification()
    {
        var store = new LocalAdapterPackageStore(Path.Combine(root, "store"));
        var turn = 0;
        var model = new FakeModel(_ => turn++ == 0
            ? new(new("assistant", ToolCalls: [
                new("progress", "report_progress", """{"kind":"verified","message":"已通过所有检查。"}"""),
                new("write", "write_file", """{"path":"src/unwanted.cs","content":"fixture"}"""),
                new("list", "list_files", """{"root":"workspace"}""")]), new(100, 20))
            : new(new("assistant", ToolCalls: [new("progress-alone", "report_progress", """{"kind":"verified","message":"已通过所有检查。"}""")]), new(100, 20)));
        var result = await new AutomaticAdaptationService(model, Runtime(new ScriptedRunner()), store)
            .AdaptAsync(Request() with { Limits = new(MaximumTurns: 2) });
        Assert.Null(result.ProgressNote);
        Assert.False(File.Exists(Path.Combine(store.WorkDirectory, "current/src/unwanted.cs")));
        Assert.Null(result.Adapter);
    }

    [Fact]
    public async Task OptionalProgressSurvivesToolsCheckpointAndRestartWithoutExtraModelCalls()
    {
        const string breakthrough = "关键突破：代表性汉字的字体探针已通过，接下来整合资源回填。";
        const string implementation = "方案已确定：正在整合字符映射与回填，尚未验证游戏内显示。";
        var store = new LocalAdapterPackageStore(Path.Combine(root, "store"));
        var updates = new ProgressRecorder();
        var round = 0;
        var model = new FakeModel(context =>
        {
            if (round++ == 0)
                return new(new("assistant", breakthrough, [new("save", "checkpoint", """{"summary":"Font probe passed; implement resource rebuild next."}""")]), new(100, 20));
            Assert.Contains("最近向用户说明的进展", Assert.Single(context.Messages).Content);
            return new(new("assistant", implementation, [new("files", "list_files", """{"root":"workspace"}""")]), new(100, 20));
        });
        var request = Request() with { Limits = new(MaximumTurns: 2) };
        var result = await new AutomaticAdaptationService(model, Runtime(new ScriptedRunner()), store).AdaptAsync(request, progress: updates);
        Assert.Equal(2, model.Calls);
        Assert.Equal(AdaptationStatus.BudgetExhausted, result.Status);
        Assert.Null(result.Adapter);
        Assert.Equal("implementing", result.ProgressNote!.Kind);
        Assert.Equal("breakthrough", result.LastBreakthrough!.Kind);
        Assert.Contains(updates.Updates, update => update.ProgressNote?.Kind == "breakthrough");
        Assert.Equal(result.ProgressNote, updates.Updates.Last().ProgressNote);
        var saved = await GameL10n.Core.Storage.LocalFiles.ReadJsonAsync<AdaptationSession>(Path.Combine(store.WorkDirectory, "current", "session.json"));
        Assert.Equal(result.ProgressNote, saved!.ProgressNote);
        Assert.Equal(result.LastBreakthrough, saved.LastBreakthrough);

        var continued = new FakeModel(context =>
        {
            Assert.Contains(context.Messages, message => message.Content.Contains("此前突破", StringComparison.Ordinal)
                && message.Content.Contains(result.ProgressNote.Message, StringComparison.Ordinal));
            return new(new("assistant", ToolCalls: [new("files-again", "list_files", """{"root":"workspace"}""")]), new(100, 20));
        });
        var resumed = await new AutomaticAdaptationService(continued, Runtime(new ScriptedRunner()), store)
            .AdaptAsync(request with { Limits = new(MaximumTurns: 1) });
        Assert.Equal(1, continued.Calls);
        Assert.Equal(result.ProgressNote, resumed.ProgressNote);
        Assert.Equal(result.LastBreakthrough, resumed.LastBreakthrough);
    }

    [Fact]
    public async Task UnchangedOrInvalidNotesKeepTheirTimestampAndCorrectionRetractsBreakthrough()
    {
        var store = new LocalAdapterPackageStore(Path.Combine(root, "store"));
        var request = Request() with { Limits = new(MaximumTurns: 1) };
        const string firstText = "关键突破：字符映射探针通过。";
        async Task<AdaptationResult> Run(string text) => await new AutomaticAdaptationService(
            new FakeModel(_ => new(new("assistant", text, [new(Guid.NewGuid().ToString("N"), "list_files", """{"root":"workspace"}""")]), new(100, 20))),
            Runtime(new ScriptedRunner()), store).AdaptAsync(request);

        var first = await Run(firstText);
        Assert.Equal(first.ProgressNote, (await Run(firstText)).ProgressNote);
        Assert.Equal(first.ProgressNote, (await Run("普通技术总结。\n关键突破：这不是首行进展说明。")).ProgressNote);
        Assert.Equal(first.ProgressNote, (await Run("方案已确定：" + new string('字', 401))).ProgressNote);
        var corrected = await Run("进展更正：补测发现边界字符仍有错位，原方案需要继续调查。");
        Assert.Equal("correction", corrected.ProgressNote!.Kind);
        Assert.Null(corrected.LastBreakthrough);
        Assert.Null(corrected.Adapter);
        var saved = await GameL10n.Core.Storage.LocalFiles.ReadJsonAsync<AdaptationSession>(Path.Combine(store.WorkDirectory, "current", "session.json"));
        Assert.Null(saved!.LastBreakthrough);
        Assert.Equal(corrected.ProgressNote, saved.ProgressNote);
    }

    private sealed class ProgressRecorder : IProgress<AdaptationProgress>
    {
        public List<AdaptationProgress> Updates { get; } = [];
        public void Report(AdaptationProgress value) => Updates.Add(value);
    }

    [Fact]
    public async Task ExistingAdapterIsReusedWithoutSpendingModelTokens()
    {
        var model = new FakeModel(_ => throw new InvalidOperationException("The model should not be called."));
        var adapter = new ExistingAdapter();
        var service = new AutomaticAdaptationService(model, Runtime(new ScriptedRunner()), new LocalAdapterPackageStore(Path.Combine(root, "store")));
        var result = await service.AdaptAsync(Request([adapter]));
        Assert.Equal(AdaptationStatus.Reused, result.Status);
        Assert.Same(adapter, result.Adapter);
        Assert.Equal(0, model.Calls);
        Assert.Equal(Path.Combine(root, "game"), adapter.ProbedDirectory);
    }

    [Fact]
    public async Task MissingToolchainDoesNotCallModelOrLeaveAWorkspace()
    {
        var model = new FakeModel(_ => throw new Exception("No API expected"));
        var compiler = Runtime(new ScriptedRunner());
        File.Delete(compiler.Toolchain.ExecutablePath);
        var store = new LocalAdapterPackageStore(Path.Combine(root, "store"));
        var result = await new AutomaticAdaptationService(model, compiler, store).AdaptAsync(Request());
        Assert.Equal(AdaptationStatus.Unavailable, result.Status);
        Assert.Equal(0, model.Calls);
        Assert.False(Directory.Exists(store.WorkDirectory));
    }

    [Fact]
    public async Task CompletedToolResultsAndSourceSurviveRestart()
    {
        var model = new FakeModel(_ => new(new("assistant", ToolCalls:
            [new("write-1", "write_file", "{\"path\":\"src/adapter.py\",\"content\":\"// saved source\"}")]), new(100, 40)));
        var store = new LocalAdapterPackageStore(Path.Combine(root, "store"));
        var request = Request() with { Limits = new(MaximumTurns: 1) };
        var first = await new AutomaticAdaptationService(model, Runtime(new ScriptedRunner()), store).AdaptAsync(request);
        Assert.Equal(AdaptationStatus.BudgetExhausted, first.Status);
        Assert.Equal("// saved source", await File.ReadAllTextAsync(Path.Combine(store.WorkDirectory, "current", "src", "adapter.py")));
        var resumedModel = new FakeModel(context =>
        {
            Assert.Contains(context.Messages, message => message.Role == "tool" && message.ToolCallId == "write-1");
            return new(new("assistant", "需要继续验证。"), new(100, 20));
        });
        var resumed = await new AutomaticAdaptationService(resumedModel, Runtime(new ScriptedRunner()), store).AdaptAsync(request);
        Assert.Equal(AdaptationStatus.BudgetExhausted, resumed.Status);
        Assert.Equal(1, resumedModel.Calls);
        Assert.Equal(260, resumed.Usage.TotalTokens);
    }

    [Fact]
    public async Task ToolFailureIsReturnedToTheSameConversation()
    {
        var calls = 0;
        var model = new FakeModel(context =>
        {
            if (calls++ == 0) return new(new("assistant", ToolCalls:
                [new("bad-path", "write_file", "{\"path\":\"../escape.cs\",\"content\":\"invalid\"}")]), new(50, 20));
            var result = Assert.Single(context.Messages, message => message.Role == "tool");
            Assert.Equal("bad-path", result.ToolCallId);
            Assert.Contains("false", result.Content);
            return new(new("assistant", "已收到路径错误。"), new(60, 10));
        });
        var result = await new AutomaticAdaptationService(model, Runtime(new ScriptedRunner()),
            new LocalAdapterPackageStore(Path.Combine(root, "store"))).AdaptAsync(Request());
        Assert.Equal(AdaptationStatus.Incomplete, result.Status);
        Assert.Equal(3, model.Calls);
        Assert.False(File.Exists(Path.Combine(root, "store", "work", "escape.cs")));
    }

    [Fact]
    public async Task ProviderUsageOverBudgetCannotExecuteReturnedTools()
    {
        var model = new FakeModel(_ => new(new("assistant", ToolCalls:
            [new("write-1", "write_file", "{\"path\":\"src/adapter.py\",\"content\":\"not executed\"}")]), new(100_000, 100_000)));
        var store = new LocalAdapterPackageStore(Path.Combine(root, "store"));
        var result = await new AutomaticAdaptationService(model, Runtime(new ScriptedRunner()), store)
            .AdaptAsync(Request() with { Limits = new(MaximumTokens: 100_000) });
        Assert.Equal(AdaptationStatus.BudgetExhausted, result.Status);
        Assert.Equal(1, model.Calls);
        Assert.False(Directory.Exists(Path.Combine(store.WorkDirectory, "current", "src")));
    }

    [Fact]
    public async Task InterimSummaryContinuesWithToolsAndReportsAnActionableBlocker()
    {
        var store = new LocalAdapterPackageStore(Path.Combine(root, "store"));
        var turn = 0;
        var model = new FakeModel(context =>
        {
            switch (turn++)
            {
                case 0:
                    Assert.True(context.Tools.Count < AdaptationTools.Definitions.Count);
                    return new(new("assistant", "CP932 无法写入中文，还需要调查字体。"), new(100, 20));
                case 1:
                    Assert.Equal(AdaptationTools.Definitions.Select(tool => tool.Name), context.Tools.Select(tool => tool.Name));
                    Assert.Contains(context.Messages, item => item.Content.Contains("主程序已补齐全部工具定义", StringComparison.Ordinal));
                    Assert.Contains(context.Messages, item => item.Role == "user" && item.Content.Contains("请执行下一项工具操作", StringComparison.Ordinal));
                    return new(new("assistant", ToolCalls: [new("note", "write_file", """{"path":"work/next.txt","content":"继续调查字体入口"}""")]), new(100, 20));
                default:
                    Assert.Contains(context.Messages, item => item.ToolCallId == "note");
                    return Blocked("needs_input", "当前设置的字体名无法从资源中确认。", "请在游戏设置中查看当前字体名，填写到补充要求后继续。");
            }
        });
        var result = await new AutomaticAdaptationService(model, Runtime(new ScriptedRunner()), store).AdaptAsync(Request());
        Assert.Equal(AdaptationStatus.WaitingForUser, result.Status);
        Assert.Equal(3, model.Calls);
        Assert.Contains("当前字体名", result.NextAction);
        Assert.True(result.Blocker!.RequiresUserInput);
        Assert.Null(result.Adapter);
        Assert.Null(store.LoadActive("game"));
        Assert.True(File.Exists(Path.Combine(store.WorkDirectory, "current", "work", "next.txt")));
        var saved = await GameL10n.Core.Storage.LocalFiles.ReadJsonAsync<AdaptationSession>(Path.Combine(store.WorkDirectory, "current", "session.json"));
        Assert.Equal(result.Status, saved!.LastStatus);
        Assert.Equal(result.Blocker, saved.Blocker);
        Assert.Equal(result.NextAction, saved.NextAction);
    }

    [Fact]
    public async Task UnfinishedEncodingWorkContinuesInsteadOfDeclaringUnsupported()
    {
        var store = new LocalAdapterPackageStore(Path.Combine(root, "store"));
        var turn = 0;
        var model = new FakeModel(context =>
        {
            switch (turn++)
            {
                case 0:
                    return new(new("assistant", ToolCalls: [new("early-stop", "report_blocker", """
                        {"kind":"unsupported","summary":"字符串池往返通过。","reason":"CP932 无法编码中文，字体路径未验证。",
                         "nextAction":"检查真实字体导入与调用位置。","canContinueLocally":true,"evidence":"原字节往返通过，尚未尝试字体映射。"}
                        """)]), new(100, 20));
                case 1:
                    var response = Assert.Single(context.Messages, item => item.ToolCallId == "early-stop");
                    using (var json = JsonDocument.Parse(response.Content))
                        Assert.False(json.RootElement.GetProperty("result").GetProperty("stopped").GetBoolean());
                    return new(new("assistant", ToolCalls: [new("work", "write_file", """{"path":"work/next.md","content":"已继续调查实际字体调用。"}""")]), new(100, 20));
                default: return Blocked("needs_input", "已生成可检查的字体证据，但需要确认原版在本机的显示。", "请描述原版相同位置的字体显示情况。");
            }
        });
        var result = await new AutomaticAdaptationService(model, Runtime(new ScriptedRunner()), store).AdaptAsync(Request());
        Assert.Equal(AdaptationStatus.WaitingForUser, result.Status);
        Assert.Equal(3, model.Calls);
        Assert.True(File.Exists(Path.Combine(store.WorkDirectory, "current", "work", "next.md")));
        Assert.Null(result.Adapter);
    }

    [Fact]
    public async Task RepeatedUnsupportedWithoutAssessmentPausesAsIncompleteWithoutBurningBudget()
    {
        var model = new FakeModel(_ => new(new("assistant", ToolCalls: [new("stop", "report_blocker", """
            {"kind":"unsupported","summary":"尚未完成。","reason":"还需要分析编码。","nextAction":"继续分析字符遍历。"}
            """)]), new(100, 20)));
        var result = await new AutomaticAdaptationService(model, Runtime(new ScriptedRunner()),
            new LocalAdapterPackageStore(Path.Combine(root, "store"))).AdaptAsync(Request());
        Assert.Equal(AdaptationStatus.Incomplete, result.Status);
        Assert.True(result.Blocker!.WorkRemaining);
        Assert.Equal("继续分析字符遍历。", result.NextAction);
        Assert.Equal(2, model.Calls);
        Assert.Null(result.Adapter);
    }

    [Fact]
    public async Task RepeatedPlainSummariesStopWithoutClaimingSuccessOrSpendingIndefinitely()
    {
        var model = new FakeModel(_ => new(new("assistant", "适配完成了。"), new(100, 20)));
        var result = await new AutomaticAdaptationService(model, Runtime(new ScriptedRunner()),
            new LocalAdapterPackageStore(Path.Combine(root, "store"))).AdaptAsync(Request());
        Assert.Equal(AdaptationStatus.Incomplete, result.Status);
        Assert.Equal(2, model.Calls);
        Assert.Null(result.Adapter);
        Assert.Contains("未继续执行", result.Summary);
        Assert.Contains("继续适配", result.NextAction);
    }

    [Fact]
    public async Task UserFeedbackAndPreviousBlockerSurviveResumeAndCheckpoint()
    {
        var store = new LocalAdapterPackageStore(Path.Combine(root, "store"));
        var request = Request();
        await new AutomaticAdaptationService(new FakeModel(_ => Blocked("needs_input", "需要确认字体名称。", "请查看字体设置。")),
            Runtime(new ScriptedRunner()), store).AdaptAsync(request);
        const string feedback = "字体名称为 SimSun；保留原启动程序。";
        var turn = 0;
        var model = new FakeModel(context =>
        {
            Assert.Contains(context.Messages, item => item.Role == "user" && item.Content.Contains(feedback, StringComparison.Ordinal));
            if (turn++ == 0)
            {
                Assert.Contains(context.Messages, item => item.Role == "user" && item.Content.Contains("需要确认字体名称", StringComparison.Ordinal));
                return new(new("assistant", ToolCalls: [new("checkpoint", "checkpoint", """{"summary":"已确认字体配置，正在调查解析入口。"}""")]), new(100, 20));
            }
            return Blocked("unsupported", "工具尚不支持此字库版本。", "提供支持此版本的字库工具链接后可以继续。");
        });
        var result = await new AutomaticAdaptationService(model, Runtime(new ScriptedRunner()), store)
            .AdaptAsync(request with { UserInstruction = feedback });
        Assert.Equal(AdaptationStatus.Unsupported, result.Status);
        Assert.False(result.Blocker!.RequiresUserInput);
        var saved = await GameL10n.Core.Storage.LocalFiles.ReadJsonAsync<AdaptationSession>(Path.Combine(store.WorkDirectory, "current", "session.json"));
        Assert.Equal(feedback, saved!.UserInstruction);
    }

    [Theory]
    [InlineData(true, null)]
    [InlineData(false, null)]
    [InlineData(false, AdaptationObjective.Repair)]
    [InlineData(false, AdaptationObjective.ExpandCoverage)]
    public async Task RequestedImprovementDoesNotStopAtAnExistingAdapterMatch(bool explicitImprovement, AdaptationObjective? objective)
    {
        var adapter = new ExistingAdapter();
        var model = new FakeModel(_ => Blocked("needs_input", "需要缺失的语言资源包。", "补齐资源包后继续。"));
        var result = await new AutomaticAdaptationService(model, Runtime(new ScriptedRunner()),
            new LocalAdapterPackageStore(Path.Combine(root, "store"))).AdaptAsync(Request([adapter]) with
            {
                ImproveExisting = explicitImprovement,
                UserInstruction = objective.HasValue ? null : explicitImprovement ? "" : "对话已支持，请补充遗漏的菜单文字。",
                Objective = objective
            });
        Assert.Equal(1, model.Calls);
        Assert.Equal(AdaptationStatus.WaitingForUser, result.Status);
        Assert.Null(result.Adapter);
        Assert.Null(adapter.ProbedDirectory);
    }

    [Fact]
    public async Task CompletedAdaptationResumesCoverageWorkWithItsExistingSourceAndFindings()
    {
        var store = new LocalAdapterPackageStore(Path.Combine(root, "store"));
        var request = Request();
        var current = Path.Combine(store.WorkDirectory, "current");
        var source = Path.Combine(current, "src", "ExistingParser.cs");
        Directory.CreateDirectory(Path.GetDirectoryName(source)!);
        await File.WriteAllTextAsync(source, "// confirmed parser");
        await GameL10n.Core.Storage.LocalFiles.WriteJsonAsync(Path.Combine(current, "session.json"),
            new AdaptationSession
            {
                GameId = request.GameId, GameDirectory = request.GameDirectory, ModelIdentity = "fake",
                LastStatus = AdaptationStatus.Activated, Summary = "对话表已通过验证。", Notes = "物品文字位于序列化对象字段。",
                Messages = [new("user", "implement adapter"), new("assistant", "原有对话表已完成。")]
            });
        var model = new FakeModel(context =>
        {
            var continuation = context.Messages.Last(item => item.Role == "user").Content;
            Assert.Contains("检查遗漏文字", continuation);
            Assert.Contains("物品文字位于序列化对象字段", continuation);
            Assert.Contains("ExistingParser.cs", continuation);
            Assert.Contains("保留已有文本的 ID、上下文和类型", continuation);
            return Blocked("needs_input", "尚缺一个资源包。", "补齐资源包后继续。" );
        });
        var result = await new AutomaticAdaptationService(model, Runtime(new ScriptedRunner()), store).AdaptAsync(request);
        Assert.Equal(AdaptationStatus.WaitingForUser, result.Status);
        Assert.Equal(1, model.Calls);
        Assert.Equal("// confirmed parser", await File.ReadAllTextAsync(source));
    }

    [Fact]
    public async Task NewInstructionsPreventPreviouslyQueuedToolsFromRunning()
    {
        var store = new LocalAdapterPackageStore(Path.Combine(root, "store"));
        var request = Request();
        var queued = new AgentToolCall("old-write", "write_file", """{"path":"src/obsolete.cs","content":"obsolete"}""");
        await GameL10n.Core.Storage.LocalFiles.WriteJsonAsync(Path.Combine(store.WorkDirectory, "current", "session.json"),
            new AdaptationSession { GameId = request.GameId, GameDirectory = request.GameDirectory, ModelIdentity = "fake",
                Messages = [new("user", "develop"), new("assistant", ToolCalls: [queued])], PendingCalls = [queued] });
        var model = new FakeModel(context =>
        {
            var skipped = Assert.Single(context.Messages, item => item.Role == "tool" && item.ToolCallId == queued.Id);
            Assert.Contains("尚未执行", skipped.Content);
            Assert.Contains(context.Messages, item => item.Role == "user" && item.Content.Contains("仅调查", StringComparison.Ordinal));
            return Blocked("unsupported", "目前缺少兼容工具。", "可补充工具来源后继续调查。");
        });
        var result = await new AutomaticAdaptationService(model, Runtime(new ScriptedRunner()), store)
            .AdaptAsync(request with { UserInstruction = "仅调查，不修改已有源码。" });
        Assert.Equal(AdaptationStatus.Unsupported, result.Status);
        Assert.False(File.Exists(Path.Combine(store.WorkDirectory, "current", "src", "obsolete.cs")));
    }

    private static AgentTurn Blocked(string kind, string reason, string nextAction) => new(new("assistant", ToolCalls:
        [new("blocker", "report_blocker", JsonSerializer.Serialize(new { kind, summary = "适配尚未完成。", reason, nextAction,
            canContinueLocally = false, evidence = "测试提供的障碍及已尝试结果：" + reason }))]), new(100, 20));

    [Theory]
    [InlineData("fake")]
    [InlineData("previous-model")]
    public async Task ContinuationUsesSavedFilesAndVerificationWithoutRepeatingDiscovery(string previousModel)
    {
        var store = new LocalAdapterPackageStore(Path.Combine(root, "store"));
        var localAdapter = new ExistingAdapter();
        var request = Request([localAdapter]);
        var workspace = Path.Combine(store.WorkDirectory, "current");
        Directory.CreateDirectory(Path.Combine(workspace, "src"));
        Directory.CreateDirectory(Path.Combine(workspace, "work"));
        await File.WriteAllTextAsync(Path.Combine(workspace, "src", "Parser.cs"), "// existing parser");
        await File.WriteAllTextAsync(Path.Combine(workspace, "work", "status.md"), "Investigate FNL font table next.");
        await GameL10n.Core.Storage.LocalFiles.WriteJsonAsync(Path.Combine(workspace, "session.json"), new AdaptationSession
        {
            GameId = request.GameId, GameDirectory = request.GameDirectory, ModelIdentity = previousModel,
            Messages = [new("user", "initial task"), new("assistant", ProviderData: JsonSerializer.SerializeToElement(new[]
                { new { type = "function_call", call_id = "legacy-code", name = "inspect_pe_code", arguments = "{}" } })),
                new("tool", "Legacy code evidence.", ToolCallId: "legacy-code")],
            HistoryStart = 3,
            Summary = "Parsing complete; fonts unresolved.", ResourceHashes = new() { ["script.txt"] = "existing-hash" },
            VerificationDiagnostics = ["Byte roundtrip passed", "CP932 cannot encode Chinese"]
        });
        var model = new FakeModel(context =>
        {
            var restored = Assert.Single(context.Messages, item => item.Role == "user" && item.Content.Contains("恢复快照", StringComparison.Ordinal));
            Assert.DoesNotContain(context.Tools, tool => tool.Name == "inspect_pe_code");
            Assert.Contains("inspect_pe_code", context.KnownToolNames!);
            Assert.Contains(context.Tools, tool => tool.Name == "test_adapter");
            Assert.DoesNotContain(context.Tools, tool => tool.Name == "inspect_unity_schema");
            Assert.Contains("Parser.cs", restored.Content);
            Assert.Contains("script.txt", restored.Content);
            Assert.Contains("Byte roundtrip passed", restored.Content);
            Assert.Contains("CP932 cannot encode Chinese", restored.Content);
            Assert.Contains("Investigate FNL font table next", restored.Content);
            return Blocked("needs_input", "需要确认显示设置。", "提供字体设置名称后继续。");
        });
        var result = await new AutomaticAdaptationService(model, Runtime(new ScriptedRunner()), store).AdaptAsync(request);
        Assert.Equal(AdaptationStatus.WaitingForUser, result.Status);
        Assert.Null(localAdapter.ProbedDirectory);
        Assert.Equal("// existing parser", await File.ReadAllTextAsync(Path.Combine(workspace, "src", "Parser.cs")));
    }

    [Fact]
    public async Task LongConversationRequestsCheckpointAndKeepsConfirmedProgress()
    {
        var store = new LocalAdapterPackageStore(Path.Combine(root, "store"));
        var request = Request();
        await GameL10n.Core.Storage.LocalFiles.WriteJsonAsync(Path.Combine(store.WorkDirectory, "current", "session.json"), new AdaptationSession
        {
            GameId = request.GameId, GameDirectory = request.GameDirectory, ModelIdentity = "fake",
            Messages = [new("user", "develop"), new("assistant", new string('x', 210_000))]
        });
        var turn = 0;
        var model = new FakeModel(context =>
        {
            if (turn++ == 0)
            {
                Assert.Contains(context.Messages, item => item.Role == "user" && item.Content.Contains("下一步先调用 checkpoint", StringComparison.Ordinal));
                return new(new("assistant", ToolCalls: [new("save", "checkpoint", """{"summary":"Engine identified; parser in src/Parser.py; byte roundtrip passed; font support pending."}""")]), new(100, 30));
            }
            var compacted = Assert.Single(context.Messages);
            Assert.Contains("byte roundtrip passed", compacted.Content);
            Assert.DoesNotContain(new string('x', 1000), compacted.Content);
            return Blocked("needs_input", "需要运行反馈。", "提供游戏字体设置名称后继续。");
        });
        var result = await new AutomaticAdaptationService(model, Runtime(new ScriptedRunner()), store).AdaptAsync(request);
        Assert.Equal(2, model.Calls);
        Assert.Equal(AdaptationStatus.WaitingForUser, result.Status);
        var saved = await GameL10n.Core.Storage.LocalFiles.ReadJsonAsync<AdaptationSession>(Path.Combine(store.WorkDirectory, "current", "session.json"));
        Assert.Contains("font support pending", saved!.Notes);
    }

    [Theory]
    [InlineData(false)]
    [InlineData(true)]
    public async Task IgnoredCheckpointCompactsRequestsAndPreservesEvidenceAcrossCheckpointAndRestart(bool responses)
    {
        var store = new LocalAdapterPackageStore(Path.Combine(root, "store"));
        var request = Request() with { Objective = AdaptationObjective.Repair };
        var workspace = Path.Combine(store.WorkDirectory, "current");
        var path = Path.Combine(workspace, "session.json");
        var original = "Exact field order: flags,length,text; stride=12; rejected UTF16 hypothesis.";
        var oldResult = JsonSerializer.Serialize(new { success = false, error = "Partial write mismatch at unit 7." });
        await LocalFiles.WriteJsonAsync(path, new AdaptationSession
        {
            GameId = request.GameId, GameDirectory = request.GameDirectory, ModelIdentity = "fake",
            Notes = "Confirmed byte roundtrip; implement partial writer in src/Parser.py.",
            VerificationDiagnostics = ["[partial-translation-re-extraction] Partial write mismatch at unit 7."],
            Messages = [new("user", "repair"), new("assistant", original + new string('x', 210_000)),
                new("assistant", ToolCalls: [new("old-failure", "test_adapter", "{}")]),
                new("tool", oldResult, ToolCallId: "old-failure")]
        });
        AgentToolCall[] calls = [new("recent-a", "read_file", """{"root":"game","path":"script.txt","length":6,"encoding":"utf8"}"""),
            new("recent-b", "list_files", """{"root":"game"}""")];
        var provider = responses
            ? JsonSerializer.SerializeToElement(calls.Select(call => new { type = "function_call", call_id = call.Id, name = call.Name, arguments = call.Arguments }))
            : JsonSerializer.SerializeToElement(new { role = "assistant", content = "", tool_calls = calls.Select(call =>
                new { id = call.Id, type = "function", function = new { name = call.Name, arguments = call.Arguments } }) });
        var round = 0;
        var model = new FakeModel(context =>
        {
            switch (round++)
            {
                case 0:
                    Assert.Contains(context.Messages, message => message.Content.StartsWith("当前会话较长。下一步先调用 checkpoint"));
                    return new(new("assistant", ToolCalls: calls, ProviderData: provider), new(100, 20));
                case 1:
                    Assert.True(AutomaticAdaptationService.VisibleHistoryBytes(context.Messages) < 100 * 1024);
                    Assert.Contains(context.Messages, message => message.Content.Contains("主程序已自动缩短请求历史"));
                    Assert.Contains(context.Messages, message => message.Content.Contains("Confirmed byte roundtrip")
                        && message.Content.Contains("Partial write mismatch") && message.Content.Contains("修复现有支持（Repair）"));
                    Assert.Equal(provider.GetRawText(), Assert.Single(context.Messages, message => message.ProviderData.HasValue).ProviderData!.Value.GetRawText());
                    Assert.Single(context.Messages, message => message.ToolCallId == "recent-a");
                    Assert.Single(context.Messages, message => message.ToolCallId == "recent-b");
                    return new(new("assistant", ToolCalls: [new("recover", "read_session_history", """{"query":"Exact field order","length":256}""")]), new(100, 20));
                case 2:
                    Assert.Contains("stride=12", Assert.Single(context.Messages, message => message.ToolCallId == "recover").Content);
                    return new(new("assistant", ToolCalls: [new("save", "checkpoint", """{"summary":"Repair partial writer using confirmed stride=12."}""")]), new(100, 20));
                default:
                    Assert.DoesNotContain(context.Messages, message => message.Content.Contains(new string('x', 1000)));
                    return Blocked("needs_input", "需要实际显示反馈。", "提供反馈后继续。");
            }
        });
        var result = await new AutomaticAdaptationService(model, Runtime(new ScriptedRunner()), store).AdaptAsync(request);
        Assert.True(result.Status == AdaptationStatus.WaitingForUser, string.Join("\n", result.Diagnostics));
        Assert.Equal(4, model.Calls);
        var saved = (await LocalFiles.ReadJsonAsync<AdaptationSession>(path))!;
        Assert.True(saved.HistoryStart > 0);
        Assert.StartsWith(original, saved.Messages[1].Content);
        Assert.Equal(oldResult, Assert.Single(saved.Messages, message => message.ToolCallId == "old-failure").Content);
        var resumed = new FakeModel(context =>
        {
            Assert.True(AutomaticAdaptationService.VisibleHistoryBytes(context.Messages) < 100 * 1024);
            var recovered = context.Messages.LastOrDefault(message => message.ToolCallId == "recover-old");
            if (recovered is null)
                return new(new("assistant", ToolCalls: [new("recover-old", "read_tool_result", """{"toolCallId":"old-failure"}""")]), new(100, 20));
            Assert.Contains("Partial write mismatch at unit 7.", recovered.Content);
            return Blocked("needs_input", "需要实际显示反馈。", "提供反馈后继续。");
        });
        Assert.Equal(AdaptationStatus.WaitingForUser,
            (await new AutomaticAdaptationService(resumed, Runtime(new ScriptedRunner()), store).AdaptAsync(request)).Status);
        Assert.Equal(2, resumed.Calls);
    }

    [Fact]
    public async Task BatchedReadsCompleteTogetherAndSurviveCheckpoint()
    {
        var request = Request();
        var store = new LocalAdapterPackageStore(Path.Combine(root, "store"));
        var round = 0;
        var model = new FakeModel(context =>
        {
            if (round++ == 0) return new(new("assistant", ToolCalls: [
                new("read", "read_file", """{"root":"game","path":"script.txt","encoding":"utf8"}"""),
                new("adapters", "list_adapters", """{"query":"Yuris"}"""),
                new("list", "list_files", """{"root":"game"}""")]), new(100, 20));
            Assert.Contains(context.Messages, message => message.ToolCallId == "read" && message.Content.Contains("First line"));
            Assert.Contains(context.Messages, message => message.ToolCallId == "list" && message.Content.Contains("script.txt"));
            Assert.Contains(context.Messages, message => message.ToolCallId == "adapters" && message.Content.Contains("Yuris"));
            return round == 2
                ? new(new("assistant", ToolCalls: [new("save", "checkpoint", """{"summary":"Known sample; implement parser."}""")]), new(100, 20))
                : Blocked("needs_input", "需要显示反馈。", "提供字体设置名称。");
        });
        var result = await new AutomaticAdaptationService(model, Runtime(new ScriptedRunner()), store).AdaptAsync(request);
        Assert.Equal(3, model.Calls);
        Assert.Equal(AdaptationStatus.WaitingForUser, result.Status);
    }

    [Fact]
    public async Task WritesAndLocalRunShareOneModelTurnAndExecuteInOrder()
    {
        var request = Request();
        var store = new LocalAdapterPackageStore(Path.Combine(root, "store"));
        var workspace = Path.Combine(store.WorkDirectory, "current");
        var runs = 0;
        var runner = new ProbeRunner(_ =>
        {
            Assert.Equal("first", File.ReadAllText(Path.Combine(workspace, "work/one.txt")));
            Assert.Equal("second", File.ReadAllText(Path.Combine(workspace, "work/two.txt")));
            runs++;
        });
        var model = new FakeModel(context => context.Messages.Any(message => message.ToolCallId == "run")
            ? Blocked("needs_input", "需要运行反馈。", "提供画面。")
            : new(new("assistant", ToolCalls: [
                new("one", "write_file", """{"path":"work/one.txt","content":"first"}"""),
                new("two", "write_file", """{"path":"work/two.txt","content":"second"}"""),
                new("run", "run_tool", """{"executable":"dotnet","arguments":["--probe"]}""")]), new(100, 20)));
        var result = await new AutomaticAdaptationService(model, Runtime(runner), store).AdaptAsync(request);
        Assert.Equal(AdaptationStatus.WaitingForUser, result.Status);
        Assert.Equal(2, model.Calls);
        Assert.Equal(1, runs);
        var saved = await GameL10n.Core.Storage.LocalFiles.ReadJsonAsync<AdaptationSession>(Path.Combine(workspace, "session.json"));
        Assert.False(saved!.PendingCallsAreSequential);
        Assert.Equal(new[] { "one", "two", "run" }, saved.Messages.Where(message => message.Role == "tool").Take(3).Select(message => message.ToolCallId));
    }

    [Fact]
    public async Task FailedWriteSkipsRemainingWritesAndDependentRun()
    {
        var request = Request();
        var store = new LocalAdapterPackageStore(Path.Combine(root, "store"));
        var model = new FakeModel(context =>
        {
            if (!context.Messages.Any(message => message.Role == "tool"))
                return new(new("assistant", ToolCalls: [
                    new("bad", "write_file", """{"path":"../outside.cs","content":"bad"}"""),
                    new("skip", "write_file", """{"path":"src/should-not-run.cs","content":"bad"}"""),
                    new("run", "run_tool", """{"executable":"dotnet","arguments":["--probe"]}""")]), new(100, 20));
            Assert.Contains("本步骤未执行", Assert.Single(context.Messages, message => message.ToolCallId == "skip").Content);
            Assert.Contains("本步骤未执行", Assert.Single(context.Messages, message => message.ToolCallId == "run").Content);
            return Blocked("needs_input", "需要信息。", "提供反馈。");
        });
        await new AutomaticAdaptationService(model, Runtime(new ProbeRunner(_ => Assert.Fail("Dependent run executed"))), store).AdaptAsync(request);
        Assert.False(File.Exists(Path.Combine(store.WorkDirectory, "current/src/should-not-run.cs")));
        Assert.Equal(2, model.Calls);
    }

    [Theory]
    [InlineData(true)]
    [InlineData(false)]
    public async Task OrderedBatchResumesOnlyAfterACommittedPrerequisite(bool interrupted)
    {
        var request = Request();
        var store = new LocalAdapterPackageStore(Path.Combine(root, "store"));
        AgentToolCall write = new("write", "write_file", """{"path":"work/probe.txt","content":"fixture"}""");
        AgentToolCall run = new("run", "run_tool", """{"executable":"dotnet","arguments":["--probe"]}""");
        var state = new AdaptationSession
        {
            GameId = request.GameId, GameDirectory = request.GameDirectory, ModelIdentity = "fake",
            Messages = [new("user", "continue"), new("assistant", ToolCalls: [write, run])],
            PendingCalls = interrupted ? [write, run] : [run], PendingCallsAreSequential = true,
            RunningToolId = interrupted ? write.Id : null
        };
        if (!interrupted) state.Messages.Add(new("tool", """{"success":true,"result":{"changed":true}}""", ToolCallId: write.Id));
        var path = Path.Combine(store.WorkDirectory, "current/session.json");
        await GameL10n.Core.Storage.LocalFiles.WriteJsonAsync(path, state);
        var runs = 0;
        var model = new FakeModel(context =>
        {
            var output = Assert.Single(context.Messages, message => message.ToolCallId == run.Id).Content;
            Assert.Contains(interrupted ? "本步骤未执行" : "probe-ok", output);
            Assert.Equal(2, context.Messages.Count(message => message.Role == "tool"));
            return Blocked("needs_input", "需要信息。", "提供反馈。");
        });
        await new AutomaticAdaptationService(model, Runtime(new ProbeRunner(_ => runs++)), store).AdaptAsync(request);
        Assert.Equal(interrupted ? 0 : 1, runs);
        var saved = await GameL10n.Core.Storage.LocalFiles.ReadJsonAsync<AdaptationSession>(path);
        Assert.False(saved!.PendingCallsAreSequential);
        Assert.Empty(saved.PendingCalls);
    }

    [Fact]
    public async Task ResearchReviewSurvivesCheckpointAndDoesNotStopTheTask()
    {
        var request = Request();
        var store = new LocalAdapterPackageStore(Path.Combine(root, "store"));
        var turn = 0;
        var model = new FakeModel(context =>
        {
            if (turn++ == 24)
            {
                Assert.Contains(context.Messages, message => message.Content.StartsWith("收敛检查："));
                return new(new("assistant", ToolCalls: [new("implement", "write_file", """{"path":"src/Parser.py","content":"// implementation"}""")]), new(100, 20));
            }
            if (turn > 25) return Blocked("needs_input", "需要反馈。", "提供画面。");
            return new(new("assistant", ToolCalls: [turn % 6 == 0
                ? new("checkpoint" + turn, "checkpoint", """{"summary":"Read sample; implement parser next."}""")
                : new("read" + turn, "read_file", """{"root":"game","path":"script.txt","encoding":"utf8"}""")]), new(100, 20));
        });
        var result = await new AutomaticAdaptationService(model, Runtime(new ScriptedRunner()), store).AdaptAsync(request);
        Assert.Equal(AdaptationStatus.WaitingForUser, result.Status);
        var saved = await GameL10n.Core.Storage.LocalFiles.ReadJsonAsync<AdaptationSession>(Path.Combine(store.WorkDirectory, "current/session.json"));
        Assert.Equal(24, saved!.LastEfficiencyReviewTurn);
        Assert.Equal(25, saved.LastImplementationTurn);
    }

    [Fact]
    public async Task UnchangedSourceDoesNotResetImplementationProgressOrInvalidateVerification()
    {
        var state = new AdaptationSession { TotalTurns = 8 };
        var tools = new AdaptationTools(Request(), state, Path.Combine(root, "workspace"), Runtime(new ScriptedRunner()),
            new LocalAdapterPackageStore(Path.Combine(root, "store")));
        AgentToolCall call = new("write", "write_file", """{"path":"src/Parser.py","content":"// same bytes"}""");
        await tools.ExecuteAsync(call, default);
        var verified = CreatePackage();
        state.VerifiedPackage = verified;
        state.TotalTurns = 30;
        using var result = JsonDocument.Parse(await tools.ExecuteAsync(call, default));
        Assert.False(result.RootElement.GetProperty("result").GetProperty("changed").GetBoolean());
        Assert.Equal(8, state.LastImplementationTurn);
        Assert.Same(verified, state.VerifiedPackage);
        var error = await Assert.ThrowsAsync<InvalidDataException>(() => tools.ExecuteAsync(new("read", "read_file",
            """{"root":"workspace","path":"src/Parser.py","length":50000}"""), default));
        Assert.Contains("32768", error.Message);
        Assert.Contains("50000", error.Message);
    }

    [Theory]
    [InlineData(false)]
    [InlineData(true)]
    public async Task LocalTextEditPreservesOtherBytesAndRejectsAmbiguousOrStaleMatches(bool bom)
    {
        var workspace = Path.Combine(root, "workspace");
        Directory.CreateDirectory(Path.Combine(workspace, "src"));
        var file = Path.Combine(workspace, "src/Parser.py");
        const string original = "// 保留\r\nkeep keep\r\nreturn 1;\r\n";
        await File.WriteAllTextAsync(file, original, new UTF8Encoding(bom));
        var state = new AdaptationSession { VerifiedPackage = CreatePackage(), TotalTurns = 9 };
        var tools = new AdaptationTools(Request(), state, workspace, Runtime(new ScriptedRunner()),
            new LocalAdapterPackageStore(Path.Combine(root, "store")));
        async Task<JsonDocument> Replace(string oldText, string newText) => JsonDocument.Parse(await tools.ExecuteAsync(
            new("edit", "replace_text", JsonSerializer.Serialize(new { path = "src/Parser.py", oldText, newText })), default));
        using var changed = await Replace("return 1;", "return 2;");
        Assert.True(changed.RootElement.GetProperty("success").GetBoolean());
        Assert.Null(state.VerifiedPackage);
        Assert.Equal(9, state.LastImplementationTurn);
        var expected = (bom ? Encoding.UTF8.GetPreamble() : []).Concat(Encoding.UTF8.GetBytes(original.Replace("return 1;", "return 2;"))).ToArray();
        Assert.Equal(expected, await File.ReadAllBytesAsync(file));
        state.VerifiedPackage = CreatePackage();
        foreach (var oldText in new[] { "return 1;", "keep", "" })
        {
            await Assert.ThrowsAsync<InvalidDataException>(() => Replace(oldText, "unsafe"));
            Assert.NotNull(state.VerifiedPackage);
            Assert.Equal(expected, await File.ReadAllBytesAsync(file));
        }
    }

    [Fact]
    public async Task DirectorySearchPagesThroughMatchesAndSkipsBinaryAndCredentialFiles()
    {
        var workspace = Path.Combine(root, "workspace");
        Directory.CreateDirectory(Path.Combine(workspace, "src/nested"));
        var content = string.Join('\n', Enumerable.Range(0, 25).Select(index => $"needle 中文 {index}"));
        await File.WriteAllTextAsync(Path.Combine(workspace, "src/nested/Parser.cs"), content, new UTF8Encoding(false));
        await File.WriteAllTextAsync(Path.Combine(workspace, "src/.env.json"), "needle secret");
        await File.WriteAllTextAsync(Path.Combine(workspace, "src/game.ypf"), "needle binary");
        var tools = new AdaptationTools(Request(), new(), workspace, Runtime(new ScriptedRunner()),
            new LocalAdapterPackageStore(Path.Combine(root, "store")));
        var found = new List<long>();
        var offset = 0;
        long byteOffset = 0;
        for (var page = 0; page < 3; page++)
        {
            using var response = JsonDocument.Parse(await tools.ExecuteAsync(new("search", "search_text",
                JsonSerializer.Serialize(new { root = "workspace", path = "src", query = "needle", offset, fileByteOffset = byteOffset })), default));
            Assert.True(response.RootElement.GetProperty("success").GetBoolean(), response.RootElement.GetRawText());
            var result = response.RootElement.GetProperty("result");
            foreach (var match in result.GetProperty("matches").EnumerateArray())
            {
                Assert.Equal("src/nested/Parser.cs", match.GetProperty("path").GetString());
                Assert.Equal("utf8", match.GetProperty("encoding").GetString());
                found.Add(match.GetProperty("byteOffset").GetInt64());
            }
            if (!result.GetProperty("more").GetBoolean()) break;
            offset = result.GetProperty("nextOffset").GetInt32();
            byteOffset = result.GetProperty("nextFileByteOffset").GetInt64();
            Assert.True(page < 2, "Search did not advance.");
        }
        Assert.Equal(25, found.Count);
        Assert.Equal(25, found.Distinct().Count());
        var bytes = Encoding.UTF8.GetBytes(content);
        Assert.All(found, offset => Assert.Equal("needle", Encoding.UTF8.GetString(bytes, (int)offset, 6)));
    }

    [Fact]
    public async Task WorkspaceReportsDefaultToUtf8AndSearchLocatesExactBytes()
    {
        var request = Request();
        var state = new AdaptationSession { VerifiedPackage = CreatePackage() };
        var tools = new AdaptationTools(request, state, Path.Combine(root, "workspace"), Runtime(new ScriptedRunner()),
            new LocalAdapterPackageStore(Path.Combine(root, "store")));
        const string report = "中文字段布局\nWeightEffect: Action, Metric; stride=8";
        await tools.ExecuteAsync(new("write", "write_file", JsonSerializer.Serialize(new { path = "work/layout.txt", content = report })), default);
        Assert.NotNull(state.VerifiedPackage);
        using var read = JsonDocument.Parse(await tools.ExecuteAsync(new("read", "read_file",
            """{"root":"workspace","path":"work/layout.txt"}"""), default));
        Assert.Equal("utf8", read.RootElement.GetProperty("result").GetProperty("encoding").GetString());
        Assert.Equal(report, read.RootElement.GetProperty("result").GetProperty("content").GetString());
        using var search = JsonDocument.Parse(await tools.ExecuteAsync(new("search", "search_text",
            """{"root":"workspace","path":"work/layout.txt","query":"WeightEffect"}"""), default));
        Assert.Equal(Encoding.UTF8.GetByteCount("中文字段布局\n"), search.RootElement.GetProperty("result").GetProperty("byteOffsets")[0].GetInt32());
        using var game = JsonDocument.Parse(await tools.ExecuteAsync(new("game", "read_file",
            """{"root":"game","path":"script.txt"}"""), default));
        Assert.Equal("hex", game.RootElement.GetProperty("result").GetProperty("encoding").GetString());
    }

    [Theory]
    [InlineData("report", true)]
    [InlineData("source", false)]
    [InlineData("dependency", false)]
    [InlineData("resource", false)]
    [InlineData("added-source", true)]
    [InlineData("removed-source", false)]
    public async Task PublicationRechecksActualInputsAfterLocalTools(string change, bool publishable)
    {
        var request = Request();
        var workspace = Path.Combine(root, "workspace");
        Directory.CreateDirectory(Path.Combine(workspace, "src/tools"));
        await File.WriteAllTextAsync(Path.Combine(workspace, "src/adapter.py"), "# fixture");
        await File.WriteAllTextAsync(Path.Combine(workspace, "src/tools/data.txt"), "dependency");
        var runner = new ScriptedRunner
        {
            LocalProbe = () =>
            {
                var relative = change switch
                {
                    "source" or "removed-source" => "src/adapter.py", "dependency" => "src/tools/data.txt",
                    "resource" => "resources/script.txt", "added-source" => "src/Extra.py", _ => "work/report.txt"
                };
                var path = Path.Combine(workspace, relative);
                Directory.CreateDirectory(Path.GetDirectoryName(path)!);
                if (change == "removed-source") File.Delete(path);
                else File.WriteAllText(path, "updated");
            }
        };
        var store = new LocalAdapterPackageStore(Path.Combine(root, "store"));
        var state = new AdaptationSession();
        var tools = new AdaptationTools(request, state, workspace, Runtime(runner), store);
        await tools.ExecuteAsync(new("stage", "stage_resources", """{"files":["script.txt"]}"""), default);
        await tools.ExecuteAsync(new("verify", "test_adapter",
            """{"kind":"template","key":"line-script","name":"Line script","resourcePatterns":["*.txt"],"releaseFiles":[{"path":"src/adapter.py","role":"source","purpose":"Fixture parser"},{"path":"tools/data.txt","role":"asset","purpose":"Fixture dependency"}]}"""), default);
        Assert.NotNull(state.VerifiedPackage);
        await tools.ExecuteAsync(new("run", "run_tool", """{"executable":"dotnet","arguments":["--probe"]}"""), default);
        var publish = new AgentToolCall("finish", "finish", """{"summary":"verified fixture"}""");
        if (publishable)
        {
            await tools.ExecuteAsync(publish, default);
            Assert.NotNull(store.LoadActive(request.GameId));
        }
        else
        {
            await Assert.ThrowsAsync<InvalidDataException>(() => tools.ExecuteAsync(publish, default));
            Assert.Null(store.LoadActive(request.GameId));
        }
    }

    [Fact]
    public async Task ReadProtocolSurvivesCheckpointWithoutAnotherLookup()
    {
        var round = 0;
        var model = new FakeModel(context =>
        {
            if (round++ == 0) return new(new("assistant", ToolCalls: [new("protocol", "adapter_protocol", "{}")]), new(100, 20));
            if (round == 2) return new(new("assistant", ToolCalls: [new("save", "checkpoint", """{"summary":"Known layout: two enums per element, stride 8. Implement next."}""")]), new(100, 20));
            Assert.Contains(context.Messages, message => message.Role == "user" && message.Content.Contains("无需再次调用 adapter_protocol"));
            Assert.Contains(context.Messages, message => message.Content.Contains("stride 8"));
            return Blocked("needs_input", "需要显示反馈。", "提供画面。");
        });
        await new AutomaticAdaptationService(model, Runtime(new ScriptedRunner()), new(Path.Combine(root, "store"))).AdaptAsync(Request());
        Assert.Equal(3, model.Calls);
    }

    [Fact]
    public async Task IdenticalLargeReadResultsOnlyReplayTheLatestCopy()
    {
        var request = Request();
        var store = new LocalAdapterPackageStore(Path.Combine(root, "store"));
        var payload = JsonSerializer.Serialize(new { success = true, result = new { content = new string('x', 12000) } });
        var messages = new List<AgentMessage> { new("user", "continue") };
        for (var i = 0; i < 3; i++)
        {
            messages.Add(new("assistant", ToolCalls: [new("read" + i, "read_file", """{"root":"game","path":"script.txt"}""")]));
            messages.Add(new("tool", payload, ToolCallId: "read" + i));
        }
        await GameL10n.Core.Storage.LocalFiles.WriteJsonAsync(Path.Combine(store.WorkDirectory, "current/session.json"),
            new AdaptationSession { GameId = request.GameId, GameDirectory = request.GameDirectory, ModelIdentity = "fake", Messages = messages });
        var model = new FakeModel(context =>
        {
            Assert.Single(context.Messages, message => message.Content == payload);
            Assert.Contains(context.Messages, message => message.ToolCallId == "read0" && message.Content.Contains("identicalTo"));
            return Blocked("needs_input", "需要显示反馈。", "提供画面。");
        });
        await new AutomaticAdaptationService(model, Runtime(new ScriptedRunner()), store).AdaptAsync(request);
    }

    [Fact]
    public async Task MixedReadAndMutationBatchDoesNotRunAnyCall()
    {
        var request = Request();
        var store = new LocalAdapterPackageStore(Path.Combine(root, "store"));
        var round = 0;
        var model = new FakeModel(context =>
        {
            if (round++ == 0) return new(new("assistant", ToolCalls: [
                new("write", "write_file", """{"path":"src/unwanted.cs","content":"fixture"}"""),
                new("list", "list_files", """{"root":"game"}""")]), new(100, 20));
            var results = context.Messages.Where(message => message.Role == "tool").ToArray();
            Assert.Equal(2, results.Length);
            Assert.All(results, message => Assert.Contains("本批未执行", message.Content));
            return Blocked("needs_input", "需要显示反馈。", "提供字体设置名称。");
        });
        await new AutomaticAdaptationService(model, Runtime(new ScriptedRunner()), store).AdaptAsync(request);
        Assert.False(File.Exists(Path.Combine(store.WorkDirectory, "current", "src", "unwanted.cs")));
    }

    [Fact]
    public async Task ManySmallMessagesAndOrdinarySourceReadsDoNotForceCheckpoint()
    {
        var store = new LocalAdapterPackageStore(Path.Combine(root, "store"));
        var request = Request();
        var messages = Enumerable.Range(0, 70).Select(i => new AgentMessage("user", "context " + i)).ToList();
        messages.Add(new("assistant", new string('x', 70_000)));
        await GameL10n.Core.Storage.LocalFiles.WriteJsonAsync(Path.Combine(store.WorkDirectory, "current", "session.json"),
            new AdaptationSession { GameId = request.GameId, GameDirectory = request.GameDirectory, ModelIdentity = "fake", Messages = messages });
        var model = new FakeModel(context =>
        {
            Assert.DoesNotContain(context.Messages, message => message.Content.StartsWith("当前会话较长。下一步先调用 checkpoint"));
            return Blocked("needs_input", "需要显示反馈。", "提供字体设置名称。");
        });
        await new AutomaticAdaptationService(model, Runtime(new ScriptedRunner()), store).AdaptAsync(request);
        Assert.Equal(1, model.Calls);
    }

    [Fact]
    public async Task OpaqueReasoningAndJsonEscapingDoNotForceACheckpointOrLoseReplayData()
    {
        var store = new LocalAdapterPackageStore(Path.Combine(root, "store"));
        var request = Request();
        var encrypted = new string('x', 90_000);
        var original = JsonSerializer.SerializeToElement(new object[]
        {
            new { type = "reasoning", encrypted_content = encrypted, summary = Array.Empty<object>() },
            new { type = "message", role = "assistant", content = new[] { new { type = "output_text", text = new string('中', 12_000) } } }
        });
        await GameL10n.Core.Storage.LocalFiles.WriteJsonAsync(Path.Combine(store.WorkDirectory, "current", "session.json"), new AdaptationSession
        {
            GameId = request.GameId, GameDirectory = request.GameDirectory, ModelIdentity = "fake",
            Messages = [new("user", "continue existing parser"), new("assistant", ProviderData: original)]
        });
        var model = new FakeModel(context =>
        {
            Assert.DoesNotContain(context.Messages, item => item.Content.StartsWith("当前会话较长。下一步先调用 checkpoint", StringComparison.Ordinal));
            var replay = Assert.Single(context.Messages, item => item.ProviderData.HasValue).ProviderData!.Value;
            Assert.Equal(encrypted, replay[0].GetProperty("encrypted_content").GetString());
            Assert.Equal(new string('中', 12_000), replay[1].GetProperty("content")[0].GetProperty("text").GetString());
            return Blocked("needs_input", "需要实际运行反馈。", "提供试译画面的显示情况后继续。");
        });
        var result = await new AutomaticAdaptationService(model, Runtime(new ScriptedRunner()), store).AdaptAsync(request);
        Assert.Equal(AdaptationStatus.WaitingForUser, result.Status);
        Assert.Equal(1, model.Calls);
    }

    [Fact]
    public async Task OlderSuccessfulResultsUseBoundedReplayAndCanBeReadBackWithoutRerunningTools()
    {
        var store = new LocalAdapterPackageStore(Path.Combine(root, "store"));
        var request = Request();
        var messages = new List<AgentMessage> { new("user", "Continue the known parser.") };
        for (var i = 0; i < 7; i++)
        {
            messages.Add(new("assistant", ToolCalls: [new("read" + i, "read_file", """{"root":"game","path":"script.txt"}""")]));
            messages.Add(new("tool", JsonSerializer.Serialize(new { success = i != 1,
                result = new { content = new string((char)('a' + i), 12000) + "decisive-finding-" + i } }), ToolCallId: "read" + i));
        }
        var original = messages[2].Content;
        var path = Path.Combine(store.WorkDirectory, "current", "session.json");
        await GameL10n.Core.Storage.LocalFiles.WriteJsonAsync(path, new AdaptationSession
        {
            GameId = request.GameId, GameDirectory = request.GameDirectory, ModelIdentity = "fake", Messages = messages
        });
        var round = 0;
        var model = new FakeModel(context =>
        {
            if (round++ == 0)
            {
                var omitted = Assert.Single(context.Messages, message => message.ToolCallId == "read0");
                Assert.Contains("previousResultOmitted", omitted.Content);
                Assert.True(omitted.Content.Length < 1500);
                Assert.Equal(messages[4].Content, Assert.Single(context.Messages, message => message.ToolCallId == "read1").Content);
                Assert.Contains(new string('g', 12000), Assert.Single(context.Messages, message => message.ToolCallId == "read6").Content);
                return new(new("assistant", ToolCalls: [new("recover", "read_tool_result",
                    """{"toolCallId":"read0","query":"decisive-finding","length":512}""")]), new(100, 20));
            }
            Assert.Contains("decisive-finding-0", Assert.Single(context.Messages, message => message.ToolCallId == "recover").Content);
            return Blocked("needs_input", "需要显示反馈。", "提供试译画面。");
        });
        var result = await new AutomaticAdaptationService(model, Runtime(new ScriptedRunner()), store).AdaptAsync(request);
        Assert.Equal(AdaptationStatus.WaitingForUser, result.Status);
        Assert.Equal(2, model.Calls);
        var saved = await GameL10n.Core.Storage.LocalFiles.ReadJsonAsync<AdaptationSession>(path);
        Assert.Equal(original, Assert.Single(saved!.Messages, message => message.ToolCallId == "read0").Content);
    }

    [Fact]
    public async Task TimingLogRecordsToolFailuresWithoutDuplicatingSuccessfulPayloads()
    {
        var store = new LocalAdapterPackageStore(Path.Combine(root, "store"));
        const string sourceText = "successful-source-must-not-appear-in-timing-log";
        var turn = 0;
        var model = new FakeModel(_ => turn++ switch
        {
            0 => new(new("assistant", ToolCalls: [new("write", "write_file", JsonSerializer.Serialize(new { path = "work/probe.py", content = sourceText }))]), new(100, 30)),
            1 => new(new("assistant", ToolCalls: [new("fail", "read_file", """{"root":"workspace","path":"work/missing.cs"}""")]), new(100, 30)),
            _ => Blocked("needs_input", "缺少用于核对的文件。", "提供对应文件来源后继续。")
        });
        var result = await new AutomaticAdaptationService(model, Runtime(new ScriptedRunner()), store).AdaptAsync(Request());
        Assert.Equal(AdaptationStatus.WaitingForUser, result.Status);
        var log = Assert.Single(Directory.GetFiles(Path.Combine(store.RootDirectory, "logs"), "adaptation-*.jsonl"));
        var lines = await File.ReadAllLinesAsync(log);
        var entries = lines.Select(line => JsonSerializer.Deserialize<JsonElement>(line).GetProperty("entry")).ToArray();
        Assert.DoesNotContain(sourceText, string.Join('\n', lines));
        Assert.Contains(entries, entry => entry.GetProperty("operation").GetString() == "model"
            && entry.GetProperty("reasoningEffort").GetString() == "provider-default"
            && entry.GetProperty("elapsedMs").GetInt64() >= 0 && entry.GetProperty("apiMs").GetDouble() >= 0);
        Assert.Contains(entries, entry => entry.GetProperty("operation").GetString() == "tool"
            && entry.GetProperty("tool").GetString() == "read_file" && entry.GetProperty("status").GetString() == "failed");
    }

    [Fact]
    public async Task CancellationStopsAWaitingModelWithoutActivation()
    {
        using var cancellation = new CancellationTokenSource();
        var model = new WaitingModel(cancellation);
        var result = await new AutomaticAdaptationService(model, Runtime(new ScriptedRunner()), new LocalAdapterPackageStore(Path.Combine(root, "store")))
            .AdaptAsync(Request(), cancellation.Token);
        Assert.Equal(AdaptationStatus.Cancelled, result.Status);
        Assert.Null(result.Adapter);
    }

    [Theory]
    [InlineData("fake")]
    [InlineData("previous-model")]
    public async Task InterruptedToolIsReportedInsteadOfBlindlyExecutedAgain(string previousModel)
    {
        var request = Request();
        var store = new LocalAdapterPackageStore(Path.Combine(root, "store"));
        var call = new AgentToolCall("interrupted", "write_file", """{"path":"src/should-not-run.cs","content":"duplicate"}""");
        await GameL10n.Core.Storage.LocalFiles.WriteJsonAsync(Path.Combine(store.WorkDirectory, "current", "session.json"),
            new AdaptationSession { GameId = request.GameId, GameDirectory = request.GameDirectory, ModelIdentity = previousModel,
                Messages = [new("user", "develop adapter"), new("assistant", ToolCalls: [call])],
                PendingCalls = [call], RunningToolId = call.Id, VerifiedPackage = CreatePackage() });
        var model = new FakeModel(context =>
        {
            if (previousModel == "fake")
            {
                var toolResult = Assert.Single(context.Messages, item => item.Role == "tool" && item.ToolCallId == call.Id);
                using var payload = JsonDocument.Parse(toolResult.Content);
                Assert.Contains("中断", payload.RootElement.GetProperty("error").GetString());
            }
            else Assert.Contains(context.Messages, item => item.Role == "user" && item.Content.Contains("中断", StringComparison.Ordinal));
            return new(new("assistant", "需要检查已有结果。"), new(40, 20));
        });
        var result = await new AutomaticAdaptationService(model, Runtime(new ScriptedRunner()), store).AdaptAsync(request);
        Assert.Equal(AdaptationStatus.Incomplete, result.Status);
        Assert.False(File.Exists(Path.Combine(store.WorkDirectory, "current", "src", "should-not-run.cs")));
        var saved = await GameL10n.Core.Storage.LocalFiles.ReadJsonAsync<AdaptationSession>(Path.Combine(store.WorkDirectory, "current", "session.json"));
        Assert.Null(saved!.VerifiedPackage);
        Assert.Null(saved.RunningToolId);
    }

    [Fact]
    public async Task LargeGameResourceCanBeInspectedByOffsetWithoutCopyingOrSendingEntireFile()
    {
        var request = Request();
        var file = Path.Combine(request.GameDirectory, "scenario.ain");
        var contents = new byte[1024 * 1024];
        Encoding.UTF8.GetBytes("SCENARIO").CopyTo(contents, contents.Length - 8);
        await File.WriteAllBytesAsync(file, contents);
        var workspace = Path.Combine(root, "workspace");
        var tools = new AdaptationTools(request, new(), workspace, Runtime(new ScriptedRunner()),
            new LocalAdapterPackageStore(Path.Combine(root, "store")));
        var result = await tools.ExecuteAsync(new("read", "read_file",
            """{"root":"game","path":"scenario.ain","offset":1048568,"length":8,"encoding":"utf8"}"""), CancellationToken.None);
        using var parsed = JsonDocument.Parse(result);
        Assert.Equal("SCENARIO", parsed.RootElement.GetProperty("result").GetProperty("content").GetString());
        Assert.Equal(1024 * 1024, parsed.RootElement.GetProperty("result").GetProperty("bytes").GetInt32());
        Assert.False(Directory.Exists(workspace));
        await Assert.ThrowsAsync<InvalidDataException>(() => tools.ExecuteAsync(
            new("finish", "finish", """{"summary":"untested"}"""), CancellationToken.None));
    }
    [Fact]
    public async Task HostVerificationRejectsUnitDropsAndPreservesPreviousActivation()
    {
        var runner = new ScriptedRunner { DropTranslatedUnit = true };
        var package = CreatePackage();
        var store = new LocalAdapterPackageStore(Path.Combine(root, "store"));
        await store.PublishAndActivateAsync("game", package, ["known prior version"]);
        var adapter = new GeneratedGameAdapter(package, Toolchain(), runner, Path.Combine(root, "runs"));
        var request = Request();
        var result = await new GeneratedAdapterVerifier().VerifyAsync(adapter, request.GameDirectory, Path.Combine(root, "verification"));
        Assert.False(result.Passed);
        var failure = Assert.Single(result.Diagnostics, diagnostic => diagnostic.Contains("missing=1", StringComparison.Ordinal));
        Assert.True(result.OriginalRoundtripPassed);
        Assert.Contains("[full-translation-re-extraction]", failure);
        Assert.Contains("expected=2, actual=1", failure);
        Assert.Contains("line-1", failure);
        Assert.Contains("script.txt", failure);
        Assert.Equal(package.Version, store.LoadActive("game")!.Version);
    }

    [Fact]
    public async Task HostVerificationKeepsNameSyntaxInsteadOfForcingTheParserToAcceptInvalidNames()
    {
        var request = Request();
        await File.WriteAllTextAsync(Path.Combine(request.GameDirectory, "script.txt"), "【？？？】\n【Alice】\nHello");
        var runner = new ScriptedRunner { RewriteExtraction = units => units.Select(unit => unit with
        {
            Kind = unit.SourceText.StartsWith('【') && unit.SourceText.EndsWith('】') ? "name" : "dialogue",
            ProtectedTokens = unit.SourceText.StartsWith('【') ? ["【", "】"] : []
        }).ToArray() };
        var adapter = new GeneratedGameAdapter(CreatePackage(), Toolchain(), runner, Path.Combine(root, "runs"));
        var result = await new GeneratedAdapterVerifier().VerifyAsync(adapter, request.GameDirectory, Path.Combine(root, "verification"));
        Assert.True(result.Passed, string.Join("\n", result.Diagnostics));
        Assert.True(result.OriginalRoundtripPassed);
    }

    [Fact]
    public async Task HostVerificationRejectsAnExtractionWithNoMutableText()
    {
        var request = Request();
        await File.WriteAllTextAsync(Path.Combine(request.GameDirectory, "script.txt"), "{player}\n[wait]");
        var adapter = new GeneratedGameAdapter(CreatePackage(), Toolchain(), new ScriptedRunner(), Path.Combine(root, "runs"));
        var result = await new GeneratedAdapterVerifier().VerifyAsync(adapter, request.GameDirectory, Path.Combine(root, "verification"));
        Assert.False(result.Passed);
        Assert.Contains(result.Diagnostics, item => item.Contains("无法生成有效测试译文"));
        Assert.True(result.OriginalRoundtripPassed);
    }

    [Theory]
    [InlineData(false)]
    [InlineData(true)]
    public async Task HostMilestonesAreAvailableWithoutModelNotesEvenWhenChineseVerificationFails(bool fails)
    {
        var state = new AdaptationSession();
        var store = new LocalAdapterPackageStore(Path.Combine(root, "store"));
        var updates = new List<AdaptationProgressNote>();
        var tools = new AdaptationTools(Request(), state, Path.Combine(root, "workspace"),
            Runtime(new ScriptedRunner { DropTranslatedUnit = fails }), store,
            reportProgress: (_, _) => { if (state.ProgressNote is { } note) updates.Add(note); });
        await tools.ExecuteAsync(new("write", "write_file", """{"path":"src/adapter.py","content":"# fixture"}"""), default);
        await tools.ExecuteAsync(new("stage", "stage_resources", """{"files":["script.txt"]}"""), default);
        await tools.ExecuteAsync(new("test", "test_adapter", """{"kind":"template","key":"line-script","name":"Line script","resourcePatterns":["*.txt"]}"""), default);
        Assert.Equal("verified", state.ProgressNote!.Kind);
        Assert.Contains(state.ProgressNote, updates);
        Assert.Contains(fails ? "尚未通过" : "仍需实际验证", state.ProgressNote.Message);
        Assert.Equal(!fails, state.VerifiedPackage is not null);
    }

    [Fact]
    public async Task HostVerificationChecksByteRoundtripAndDistinctUnicodeTranslations()
    {
        var package = CreatePackage();
        var runner = new ScriptedRunner();
        var adapter = new GeneratedGameAdapter(package, Toolchain(), runner, Path.Combine(root, "runs"));
        var request = Request();
        var result = await new GeneratedAdapterVerifier().VerifyAsync(adapter, request.GameDirectory, Path.Combine(root, "verification"));
        Assert.True(result.Passed, string.Join("\n", result.Diagnostics));
        Assert.True(runner.SawUnicodeTranslations);
        Assert.False(Directory.EnumerateDirectories(Path.Combine(root, "runs")).Any());
    }

    [Fact]
    public async Task HostVerificationRejectsCorruptionOfUntranslatedTrialText()
    {
        var request = Request();
        var runner = new ScriptedRunner { CorruptUntouchedTrialText = true };
        var adapter = new GeneratedGameAdapter(CreatePackage(), Toolchain(), runner, Path.Combine(root, "worker"));
        var result = await new GeneratedAdapterVerifier().VerifyAsync(adapter, request.GameDirectory, Path.Combine(root, "verification"));
        Assert.False(result.Passed);
        var failure = Assert.Single(result.Diagnostics, diagnostic => diagnostic.Contains("line-1", StringComparison.Ordinal));
        Assert.Contains("[partial-translation-re-extraction]", failure);
        Assert.Contains("expected=\"Second line\", actual=\"corrupted original text\"", failure);
        Assert.True(runner.SawUnicodeTranslations);
    }

    [Fact]
    public async Task HostVerificationReportsFirstShiftedUnitAsWellAsAddedTailWithBoundedSamples()
    {
        var request = Request();
        File.WriteAllText(Path.Combine(request.GameDirectory, "script.txt"),
            string.Join('\n', Enumerable.Range(0, 100).Select(index => $"Text {index} " + new string('a', 2000))));
        var runner = new ScriptedRunner
        {
            RewriteExtraction = units => units[0].SourceText.StartsWith("AVG_TRANSLATION_", StringComparison.Ordinal)
                ? units.Prepend(units[0] with { SourceText = "newly split name", Kind = "name", Speaker = "new speaker" })
                    .Select((unit, index) => unit with { Id = "line-" + index, Locator = "line:" + index }).ToArray()
                : units
        };
        var adapter = new GeneratedGameAdapter(CreatePackage(), Toolchain(), runner, Path.Combine(root, "runs"));
        var result = await new GeneratedAdapterVerifier().VerifyAsync(adapter, request.GameDirectory, Path.Combine(root, "verification"));
        Assert.False(result.Passed);
        var failure = Assert.Single(result.Diagnostics, diagnostic => diagnostic.Contains("Re-extraction mismatch", StringComparison.Ordinal));
        Assert.Contains("expected=100, actual=101; missing=0, added=1, changed=100", failure);
        Assert.Contains("Changed id=\"line-0\"", failure);
        Assert.Contains("Added id=\"line-100\"", failure);
        Assert.Contains("locator=\"line:0\"", failure);
        Assert.Contains("sourceText: expected=\"AVG_TRANSLATION_0_翻译 Text 0", failure);
        Assert.Contains("actual=\"newly split name\"", failure);
        Assert.Contains("kind: expected=\"dialogue\", actual=\"name\"", failure);
        Assert.Contains("speaker: expected=null, actual=\"new speaker\"", failure);
        Assert.DoesNotContain("Changed id=\"line-2\"", failure);
        Assert.True(failure.Length < 8192);
    }

    [Theory]
    [InlineData("script.txt", true)]
    [InlineData("./script.txt", false)]
    [InlineData("../script.txt", false)]
    [InlineData("undiscovered.txt", false)]
    public async Task RepeatedResourcePathsDoNotHideAnInvalidPathInALaterUnit(string lastPath, bool valid)
    {
        var request = Request();
        var runner = new ScriptedRunner
        {
            RewriteExtraction = units => Enumerable.Range(0, 512).Select(index => units[0] with { Id = "repeated-" + index })
                .Append(units[1] with { ResourcePath = lastPath }).ToArray()
        };
        var adapter = new GeneratedGameAdapter(CreatePackage(), Toolchain(), runner, Path.Combine(root, "runs"));
        if (valid)
            Assert.Equal(513, (await adapter.ExtractAsync(request.GameDirectory)).Units.Count);
        else
        {
            var error = await Record.ExceptionAsync(() => adapter.ExtractAsync(request.GameDirectory));
            Assert.True(error is InvalidDataException or GeneratedAdapterException, error?.ToString());
        }
    }

    [Theory]
    [InlineData(true)]
    [InlineData(false)]
    public async Task ValidationIssuesBlockPublicationWithActionableDiagnostics(bool claimsPassed)
    {
        var request = Request();
        var runner = new ScriptedRunner
        {
            ValidationResponse = new(claimsPassed, [new("fixture-check", "校验说明")])
        };
        var adapter = new GeneratedGameAdapter(CreatePackage(), Toolchain(), runner, Path.Combine(root, "runs"));
        var result = await new GeneratedAdapterVerifier().VerifyAsync(adapter, request.GameDirectory, Path.Combine(root, "verification"));
        Assert.False(result.Passed);
        var failure = Assert.Single(result.Diagnostics, diagnostic => diagnostic.Contains("[full-translation-validation]", StringComparison.Ordinal));
        Assert.Contains("校验说明", failure);
        if (claimsPassed)
        {
            Assert.Contains("passed=true", failure);
            Assert.Contains("probe/extraction/build.warnings", failure);
            Assert.Contains("fixture-check", failure);
        }
    }

    [Fact]
    public async Task SavedVerificationFromEarlierRulesRequiresRetestBeforePublishing()
    {
        var store = new LocalAdapterPackageStore(Path.Combine(root, "store"));
        var state = new AdaptationSession { VerifiedPackage = CreatePackage() };
        var tools = new AdaptationTools(Request(), state, Path.Combine(root, "workspace"), Runtime(new ScriptedRunner()), store);
        var error = await Assert.ThrowsAsync<InvalidDataException>(() => tools.ExecuteAsync(
            new("publish", "finish", """{"summary":"old verification"}"""), default));
        Assert.Contains("重新 test_adapter", error.Message);
        Assert.Null(store.LoadActive("game"));
        Assert.NotNull(state.VerifiedPackage);
    }

    [Fact]
    public async Task PathEscapingBuildOutputIsRejectedBeforeAnythingIsCopied()
    {
        var package = CreatePackage();
        var runner = new ScriptedRunner { EscapeOutput = true };
        var adapter = new GeneratedGameAdapter(package, Toolchain(), runner, Path.Combine(root, "runs"));
        var request = Request();
        var output = Path.Combine(root, "patch");
        Directory.CreateDirectory(output);
        await Assert.ThrowsAsync<GeneratedAdapterException>(() => adapter.BuildPatchAsync(request.GameDirectory, new Dictionary<string, string>(), output));
        Assert.Empty(Directory.EnumerateFileSystemEntries(output));
    }

    [Fact]
    public async Task DisabledFamilyIsNotProbedDuringAutomaticReuse()
    {
        var store = new LocalAdapterPackageStore(Path.Combine(root, "store"));
        var adapter = new ExistingAdapter("engine.fixture");
        await store.SetFamilyEnabledAsync(adapter.Id, false);
        var model = new FakeModel(_ => Blocked("needs_input", "需要格式样本。", "提供样本后继续。"));
        var result = await new AutomaticAdaptationService(model, Runtime(new ScriptedRunner()), store)
            .AdaptAsync(Request([adapter]));
        Assert.Null(adapter.ProbedDirectory);
        Assert.NotEqual(AdaptationStatus.Reused, result.Status);
        Assert.Equal(1, model.Calls);
    }

    [Fact]
    public async Task FailedReuseEvidenceReachesModelAndSurvivesCheckpointAndRestart()
    {
        var store = new LocalAdapterPackageStore(Path.Combine(root, "store"));
        var published = await store.PublishAndActivateAsync("first-game", CreatePackage(), ["previously passed"]);
        var request = Request();
        var runs = 0;
        var builds = 0;
        var trace = new List<JsonElement>();
        var runner = new ScriptedRunner
        {
            CorruptUntouchedTrialText = true,
            ObserveRequest = execution =>
            {
                runs++;
                if (execution.StandardInput is not { } input) return;
                using var document = JsonDocument.Parse(input);
                if (document.RootElement.TryGetProperty("operation", out var operation) && operation.GetString() == "probe")
                    Assert.Contains(trace, entry => entry.GetProperty("operation").GetString() == "candidate-phase"
                        && entry.GetProperty("adapterId").GetString() == published.Id
                        && entry.GetProperty("phase").GetString() == "probe" && entry.GetProperty("status").GetString() == "started");
                if (operation.ValueKind == JsonValueKind.String && operation.GetString() == "build") builds++;
            }
        };
        foreach (var format in VnTextPatchFormatCatalog.All.Where(format => format.Extensions.Contains(".txt")))
            await store.SetFamilyEnabledAsync(format.Family.AdapterId, false);
        var compiler = Runtime(runner);
        var discovery = new AdapterDiscoveryService(store, () => compiler.Toolchain, runner,
            new EngineReferenceToolkit(Path.Combine(store.RootDirectory, "engine-tools")));
        var analysis = await new GameAnalysisService(new TranslationPipeline(Path.Combine(root, "data")), discovery)
            .AnalyzeAsync(new(request.GameId, request.GameDirectory, request.ExistingAdapters), trace: async entry =>
            {
                await Task.Yield();
                trace.Add(JsonSerializer.SerializeToElement(entry, Json));
            });
        Assert.Null(analysis.Session);
        Assert.NotNull(Assert.Single(analysis.Discovery.Failures).InputFingerprint);
        Assert.Null(store.LoadActive(request.GameId));
        Assert.True(builds > 0);
        var start = Assert.Single(trace, entry => entry.GetProperty("operation").GetString() == "candidate-start"
            && entry.GetProperty("adapterId").GetString() == published.Id);
        Assert.Equal(new FileInfo(Path.Combine(request.GameDirectory, "script.txt")).Length,
            start.GetProperty("selection").GetProperty("resources").GetProperty("totalBytes").GetInt64());
        Assert.Contains(trace, entry => entry.GetProperty("operation").GetString() == "verification-result"
            && !entry.GetProperty("passed").GetBoolean());
        var previousBuilds = builds;
        request = request with { ReuseFailures = analysis.Discovery.Failures };
        void CheckEvidence(AgentRequest context)
        {
            Assert.Contains(context.Messages, message => message.Role == "user"
                && message.Content.Contains("\"id\":\"template.line-script\"", StringComparison.Ordinal)
                && message.Content.Contains("\"version\":\"one\"", StringComparison.Ordinal)
                && message.Content.Contains("\"failedStage\":\"partial-translation-re-extraction\"", StringComparison.Ordinal)
                && message.Content.Contains("Re-extraction mismatch", StringComparison.Ordinal)
                && message.Content.Contains("\"originalRoundtripPassed\":true", StringComparison.Ordinal)
                && message.Content.Contains("\"sourceDirectory\":" + JsonSerializer.Serialize(published.DirectoryPath, Json), StringComparison.Ordinal)
                && message.Content.Contains("copy_adapter", StringComparison.Ordinal));
        }
        var model = new FakeModel(context =>
        {
            CheckEvidence(context);
            return context.Messages.Any(message => message.Content.Contains("Continue repairing the partial writer.", StringComparison.Ordinal))
                ? Blocked("needs_input", "需要实际运行反馈。", "提供画面后继续。")
                : new(new("assistant", ToolCalls: [new("save", "checkpoint", """{"summary":"Continue repairing the partial writer."}""")]), new(100, 20));
        });
        var result = await new AutomaticAdaptationService(model, Runtime(runner), store).AdaptAsync(request);
        Assert.Equal(AdaptationStatus.WaitingForUser, result.Status);
        Assert.Equal(2, model.Calls);
        Assert.Equal(previousBuilds, builds);
        var saved = await LocalFiles.ReadJsonAsync<AdaptationSession>(Path.Combine(store.WorkDirectory, "current", "session.json"));
        var failure = Assert.Single(saved!.ReuseFailures);
        Assert.Equal("partial-translation-re-extraction", failure.FailedStage);
        Assert.True(failure.OriginalRoundtripPassed);
        Assert.Equal(published.DirectoryPath, failure.SourceDirectory);
        Assert.All(failure.Evidence, item => Assert.InRange(item.Length, 1, 1500));
        var previousRuns = runs;
        var resumedModel = new FakeModel(context =>
        {
            CheckEvidence(context);
            return Blocked("needs_input", "需要实际运行反馈。", "提供画面后继续。");
        });
        await new AutomaticAdaptationService(resumedModel, Runtime(runner), store).AdaptAsync(request);
        Assert.Equal(previousRuns, runs);
        Assert.Equal(published.SourceSha256, Hash(Path.Combine(published.DirectoryPath, "Program.cs")));

        // Equal length and timestamp must not hide a changed resource from failure reuse.
        var script = Path.Combine(request.GameDirectory, "script.txt");
        var timestamp = File.GetLastWriteTimeUtc(script);
        await File.WriteAllTextAsync(script, "Third line\nSecond line", new UTF8Encoding(false));
        File.SetLastWriteTimeUtc(script, timestamp);
        var changed = await discovery.FindAsync(new(request.GameId, request.GameDirectory, request.ExistingAdapters,
            PreviousFailures: analysis.Discovery.Failures));
        Assert.True(builds > previousBuilds);
        Assert.NotEqual(failure.InputFingerprint, Assert.Single(changed.Failures).InputFingerprint);
    }

    [Fact]
    public async Task PublishedTemplateIsVerifiedAndReusedForAnotherGameWithoutModelCalls()
    {
        var store = new LocalAdapterPackageStore(Path.Combine(root, "store"));
        var package = CreatePackage();
        await store.PublishAndActivateAsync("first-game", package, ["passed"]);
        var interruptedVerification = Path.Combine(store.WorkDirectory, "reuse-check", "roundtrip");
        Directory.CreateDirectory(interruptedVerification);
        File.WriteAllText(Path.Combine(interruptedVerification, "incomplete.txt"), "interrupted verification output");
        var model = new FakeModel(_ => throw new InvalidOperationException("No model call should be needed."));
        var result = await new AutomaticAdaptationService(model, Runtime(new ScriptedRunner()), store).AdaptAsync(Request());
        Assert.Equal(AdaptationStatus.Reused, result.Status);
        Assert.Equal(0, model.Calls);
        Assert.False(Directory.Exists(Path.Combine(store.WorkDirectory, "reuse-check")));
        var reloaded = new LocalAdapterPackageStore(store.RootDirectory);
        Assert.Equal(package.Id, reloaded.LoadActive("game")!.Id);
        var catalog = Assert.Single(reloaded.ReadCatalog([], "game"));
        Assert.Equal("template.line-script", catalog.Family.AdapterId);
        var compatible = Assert.Single(catalog.Games);
        Assert.Equal("resource-roundtrip-passed", compatible.StructureStatus);
        Assert.Equal("not-tested", compatible.RuntimeStatus);
        Assert.NotNull(compatible.UpdatedAt);
        Assert.Single(reloaded.ReadCatalog([], "Line script"));
    }

    [Fact]
    public async Task PackagesCanBeReactivatedFromDiskAndRolledBack()
    {
        var package = CreatePackage();
        var store = new LocalAdapterPackageStore(Path.Combine(root, "store"));
        var first = await store.PublishAndActivateAsync("game", package, ["passed"]);
        var second = await store.PublishAndActivateAsync("game", package with { Version = "second" }, ["passed"]);
        await store.PublishAndActivateAsync("game", second, ["repeat activation"]);
        Assert.Equal(second.Version, new LocalAdapterPackageStore(store.RootDirectory).LoadActive("game")!.Version);
        Assert.True(await store.RollbackAsync("game"));
        Assert.Equal(first.Version, store.LoadActive("game")!.Version);
        Assert.True(Directory.Exists(second.DirectoryPath));
        var reusable = Assert.Single(new LocalAdapterPackageStore(store.RootDirectory).ListReusablePackages());
        Assert.Equal(first.Version, reusable.Version);
        Assert.Equal("template.line-script", reusable.Id);
    }

    private AdaptationRequest Request(IReadOnlyList<IGameAdapter>? adapters = null)
    {
        var game = Path.Combine(root, "game");
        Directory.CreateDirectory(game);
        File.WriteAllText(Path.Combine(game, "script.txt"), "First line\nSecond line", new UTF8Encoding(false));
        return new AdaptationRequest("game", game, adapters ?? []);
    }

    private PinnedDotNetToolchain Toolchain()
    {
        var runtime = Path.Combine(root, "runtime");
        Directory.CreateDirectory(Path.Combine(runtime, "sdk", "10.0.402", "Roslyn", "bincore"));
        Directory.CreateDirectory(Path.Combine(runtime, "packs", "Microsoft.NETCore.App.Ref", "10.0.13", "ref", "net10.0"));
        File.WriteAllText(Path.Combine(runtime, "dotnet.exe"), "test-only-runtime");
        File.WriteAllText(Path.Combine(runtime, "sdk", "10.0.402", "Roslyn", "bincore", "csc.dll"), "test-only-compiler");
        File.WriteAllText(Path.Combine(runtime, "packs", "Microsoft.NETCore.App.Ref", "10.0.13", "ref", "net10.0", "System.Runtime.dll"), "test-only-reference");
        return new PinnedDotNetToolchain(runtime, "10.0.402", "10.0.13");
    }

    private AdapterRuntime Runtime(IAdapterProcessRunner runner) => new(Toolchain(), runner);
    private GeneratedAdapterCompiler LegacyCompiler(IAdapterProcessRunner runner) => new(Toolchain(), runner);

    private GeneratedAdapterPackage CreatePackage()
    {
        var path = Path.Combine(root, "package");
        Directory.CreateDirectory(path);
        File.WriteAllText(Path.Combine(path, "Program.cs"), "// protocol fixture, never executed", new UTF8Encoding(false));
        File.WriteAllBytes(Path.Combine(path, "adapter.dll"), [1, 2, 3]);
        File.WriteAllText(Path.Combine(path, "adapter.runtimeconfig.json"), "{}");
        return new GeneratedAdapterPackage("template.line-script", "one", path, ["script.txt"], Hash(Path.Combine(path, "Program.cs")),
            Hash(Path.Combine(path, "adapter.dll")), new AdapterFamily("template", "line-script", "Line script"));
    }


    private static string Hash(string path) => Convert.ToHexStringLower(SHA256.HashData(File.ReadAllBytes(path)));
    public void Dispose() { if (Directory.Exists(root)) Directory.Delete(root, true); }

    private sealed class FakeModel(Func<AgentRequest, AgentTurn> generate) : IAdaptationAgentModel
    {
        public string Identity => "fake";
        public int Calls { get; private set; }
        public Task<AgentTurn> NextAsync(AgentRequest context, CancellationToken cancellationToken)
        {
            Calls++;
            return Task.FromResult(generate(context));
        }
    }

    private sealed class ProbeRunner(Action<AdapterProcessRequest> onProbe) : IAdapterProcessRunner
    {
        public Task<AdapterProcessResult> RunAsync(AdapterProcessRequest request, CancellationToken cancellationToken)
        {
            if (request.Arguments.Contains("--probe")) onProbe(request);
            return Task.FromResult(new AdapterProcessResult(0, "probe-ok", ""));
        }
    }

    private sealed class WaitingModel(CancellationTokenSource cancellation) : IAdaptationAgentModel
    {
        public string Identity => "fake";
        public async Task<AgentTurn> NextAsync(AgentRequest context, CancellationToken cancellationToken)
        {
            cancellation.Cancel();
            await Task.Delay(Timeout.Infinite, cancellationToken);
            throw new Exception("unreachable");
        }
    }
    private sealed class ExistingAdapter(string id = "existing") : IGameAdapter
    {
        public string Id => id;
        public string Version => "1";
        public string? ProbedDirectory { get; private set; }
        public Task<GameProbe?> ProbeAsync(string gameDirectory, CancellationToken cancellationToken = default)
        {
            ProbedDirectory = gameDirectory;
            return Task.FromResult<GameProbe?>(new GameProbe("test", Id, Version, "test", ["script.txt"], []));
        }
        public Task<ExtractionResult> ExtractAsync(string sourceDirectory, CancellationToken cancellationToken = default) => Task.FromResult(new ExtractionResult([new("line-0", "script.txt", "line:0", "First line", "fixture")], []));
        public Task<AdapterBuildResult> BuildPatchAsync(string sourceDirectory, IReadOnlyDictionary<string, string> translations, string patchDirectory, CancellationToken cancellationToken = default) => throw new NotSupportedException();
        public Task<ValidationResult> ValidatePatchAsync(string sourceDirectory, string patchDirectory, CancellationToken cancellationToken = default) => throw new NotSupportedException();
    }

    // Protocol fixtures test the host's independent checks, not OS containment or actual C# execution.
    private sealed class ScriptedRunner : IAdapterProcessRunner
    {
        public Action? LocalProbe { get; init; }
        public Action<AdapterProcessRequest>? ObserveRequest { get; init; }
        public bool UndeclaredLaunchScript { get; init; }
        public bool DropTranslatedUnit { get; init; }
        public bool CorruptUntouchedTrialText { get; init; }
        public bool EscapeOutput { get; init; }
        public string? CompilationError { get; init; }
        public Func<TranslationUnit[], TranslationUnit[]>? RewriteExtraction { get; init; }
        public ValidationResult ValidationResponse { get; init; } = new(true, [], "passed");
        public bool SawUnicodeTranslations { get; private set; }
        public Task<AdapterProcessResult> RunAsync(AdapterProcessRequest request, CancellationToken cancellationToken)
        {
            ObserveRequest?.Invoke(request);
            if (request.Arguments.Contains("--probe"))
            {
                LocalProbe?.Invoke();
                return Task.FromResult(new AdapterProcessResult(0, "probe complete", ""));
            }
            if (request.Arguments[0] == "--info") return Task.FromResult(new AdapterProcessResult(0, "trusted toolchain", ""));
            if (request.Arguments.Any(argument => argument.StartsWith("import ast,pathlib,sys;", StringComparison.Ordinal)))
                return Task.FromResult(new AdapterProcessResult(CompilationError is null ? 0 : 1, "", CompilationError ?? ""));
            if (Path.GetFileName(request.ExecutablePath) == "python.exe" && string.IsNullOrEmpty(request.StandardInput))
            {
                LocalProbe?.Invoke();
                return Task.FromResult(new AdapterProcessResult(0, "confirmed layout", ""));
            }
            if (request.Arguments[0].EndsWith("csc.dll", StringComparison.OrdinalIgnoreCase))
            {
                if (CompilationError is not null) return Task.FromResult(new AdapterProcessResult(1, "", CompilationError));
                var compilerArguments = File.ReadAllLines(request.Arguments.Last(argument => argument.StartsWith('@'))[1..]);
                var outputPath = compilerArguments.Single(line => line.StartsWith("/out:", StringComparison.Ordinal))[5..].Trim('"');
                File.WriteAllBytes(outputPath, [1, 2, 3]);
                return Task.FromResult(new AdapterProcessResult(0, "", ""));
            }
            using var document = JsonDocument.Parse(request.StandardInput);
            var input = document.RootElement;
            if (input.TryGetProperty("format", out _))
            {
                var parsed = GameL10n.VnTextPatch.Worker.ExecuteJson(request.StandardInput);
                File.WriteAllText(input.GetProperty("responseFile").GetString()!, parsed);
                return Task.FromResult(new AdapterProcessResult(0, "", ""));
            }
            var source = input.GetProperty("sourceDirectory").GetString()!;
            var output = input.GetProperty("outputDirectory").GetString()!;
            var lines = File.ReadAllText(Path.Combine(source, "script.txt")).Split('\n');
            var operation = input.GetProperty("operation").GetString();
            object response;
            if (operation == "probe") response = new { protocolVersion = 1, success = true, probe = new GameProbe("fixture", "ignored", "ignored", "fixture", ["script.txt"], []) };
            else if (operation == "extract")
            {
                var units = lines.Select((line, index) => new TranslationUnit("line-" + index, "script.txt", "line:" + index, line, "fixture")).ToArray();
                if (DropTranslatedUnit && lines[0].Contains("AVG_TRANSLATION_", StringComparison.Ordinal)) units = units.Take(1).ToArray();
                if (RewriteExtraction is not null) units = RewriteExtraction(units);
                response = new { protocolVersion = 1, success = true, extraction = new ExtractionResult(units, []) };
            }
            else if (operation == "build")
            {
                var translations = input.GetProperty("translations");
                if (translations.ValueKind == JsonValueKind.Null)
                    translations = JsonSerializer.Deserialize<JsonElement>(File.ReadAllText(input.GetProperty("translationsFile").GetString()!));
                for (var index = 0; index < lines.Length; index++)
                    if (translations.TryGetProperty("line-" + index, out var value)) lines[index] = value.GetString()!;
                if (CorruptUntouchedTrialText && translations.EnumerateObject().Count() < lines.Length)
                    lines[^1] = "corrupted original text";
                SawUnicodeTranslations |= lines.Any(line => line.Contains("翻译", StringComparison.Ordinal));
                File.WriteAllText(Path.Combine(output, "script.txt"), string.Join('\n', lines), new UTF8Encoding(false));
                if (UndeclaredLaunchScript && SawUnicodeTranslations) File.WriteAllText(Path.Combine(output, "start.cmd"), "game.exe");
                response = new { protocolVersion = 1, success = true, build = new AdapterBuildResult(EscapeOutput ? ["../escape.txt"]
                    : UndeclaredLaunchScript && SawUnicodeTranslations ? ["script.txt", "start.cmd"] : ["script.txt"], []) };
            }
            else response = new { protocolVersion = 1, success = true, validation = ValidationResponse };
            return Task.FromResult(new AdapterProcessResult(0, JsonSerializer.Serialize(response, Json), ""));
        }
    }
}
