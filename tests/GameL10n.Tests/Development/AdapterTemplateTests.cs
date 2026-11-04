using System.Text;
using System.Text.Json;
using GameL10n.Core.Adapters;
using GameL10n.Core.Development;

namespace GameL10n.Tests.Development;

public sealed class AdapterTemplateTests : IDisposable
{
    private readonly EngineArchiveFixture fixture = new();
    private static readonly JsonSerializerOptions Json = new(JsonSerializerDefaults.Web);

    [Theory]
    [InlineData("rpa3-renpy", "archive.rpa3", ".rpa", "script.rpy")]
    [InlineData("xp3-kirikiri-ks", "archive.xp3", ".xp3", "scenario/start.ks")]
    public async Task ComposedArchiveAdaptersPassIndependentFullAndPartialChineseRoundtrips(string id, string component, string extension, string member)
    {
        var game = Directory.CreateDirectory(Path.Combine(fixture.Root, "game")).FullName;
        var script = member.EndsWith(".rpy", StringComparison.Ordinal)
            ? "#" + new string('x', 70000) + "\nlabel start:\n    \"こんにちは。\"\n    \"次の台詞です。\"\n"
            : "\uFEFF;" + new string('x', 70000) + "\n*start\nこんにちは。[p]\n次の台詞です。[p]\n";
        var source = await fixture.CreateArchiveAsync(component, script: script, scriptName: member);
        File.Copy(source, Path.Combine(game, "first" + extension));
        File.Copy(source, Path.Combine(game, "second" + extension));
        var original = await File.ReadAllBytesAsync(source);
        var (tools, state, store, workspace, compiler, assets) = await ToolsAsync(game, archive: true);
        var composition = await ComposeAsync(tools, id);
        Assert.Null(state.VerifiedPackage);
        Assert.Equal(7, state.ReleaseFiles!.Count);
        Assert.Contains(state.ReleaseFiles, file => file.Path == EngineArchiveComponents.ScriptPath && file.Role == "runtime");
        Assert.Contains(state.ReleaseFiles, file => file.Path == BuiltinAdapterAssets.SharedDependencyPath);
        Assert.Empty(Directory.EnumerateFiles(Path.Combine(workspace, "src"), "*.dll", SearchOption.AllDirectories));
        Assert.False(Directory.Exists(Path.Combine(workspace, "src", "python")));
        await tools.ExecuteAsync(new("stage", "stage_resources", JsonSerializer.Serialize(new { files = new[] { "first" + extension, "second" + extension } })), default);
        using var tested = JsonDocument.Parse(await tools.ExecuteAsync(new("verify", "test_adapter", composition.GetProperty("testArguments").GetRawText()), default));
        var result = tested.RootElement.GetProperty("result");
        Assert.True(result.GetProperty("passed").GetBoolean(), tested.RootElement.ToString());
        Assert.NotNull(state.VerifiedPackage);
        Assert.False(result.GetProperty("runtimeTested").GetBoolean());
        Assert.Null(tools.PublishedAdapter);

        var toolkit = new EngineReferenceToolkit(Path.Combine(store.RootDirectory, "engine-tools"), parserAssets: assets);
        var adapter = new GeneratedGameAdapter(state.VerifiedPackage!, compiler.Toolchain, fixture.Runner, store.WorkDirectory, toolkit);
        var extracted = await adapter.ExtractAsync(game);
        Assert.Equal(extracted.Units.Count, extracted.Units.Select(unit => unit.Id).Distinct().Count());
        Assert.Contains(extracted.Units, unit => unit.ResourcePath == "first" + extension);
        Assert.Contains(extracted.Units, unit => unit.ResourcePath == "second" + extension);
        Assert.All(extracted.Units, unit => Assert.StartsWith("member:", unit.Locator));
        var selected = extracted.Units[0];
        var patch = Path.Combine(fixture.Root, "partial");
        await adapter.BuildPatchAsync(game, new Dictionary<string, string> { [selected.Id] = "这是单条中文修改。" }, patch);
        var after = await adapter.ExtractAsync(patch);
        Assert.Equal("这是单条中文修改。", after.Units.Single(unit => unit.Id == selected.Id).SourceText);
        Assert.All(after.Units.Where(unit => unit.Id != selected.Id), unit => Assert.Equal(extracted.Units.Single(before => before.Id == unit.Id), unit));
        Assert.Equal(original, await File.ReadAllBytesAsync(Path.Combine(game, "first" + extension)));
        var untouched = selected.ResourcePath == "first" + extension ? "second" + extension : "first" + extension;
        Assert.Equal(original, await File.ReadAllBytesAsync(Path.Combine(patch, untouched)));
        Assert.True((await adapter.ValidatePatchAsync(game, patch)).Passed);
        var unknown = await Assert.ThrowsAsync<GeneratedAdapterException>(() => adapter.BuildPatchAsync(game,
            new Dictionary<string, string> { ["unknown-unit"] = "错误映射" }, Path.Combine(fixture.Root, "unknown")));
        Assert.Contains("无法对应", unknown.Message);
        var tamperedMember = Path.Combine(fixture.Root, "tampered-logo");
        await File.WriteAllBytesAsync(tamperedMember, [3, 2, 1]);
        var tampered = await fixture.InvokeAsync(component, "rebuild", Path.Combine(patch, selected.ResourcePath),
            replacements: new Dictionary<string, string> { ["images/logo.bin"] = tamperedMember });
        Assert.True(tampered.GetProperty("success").GetBoolean(), tampered.ToString());
        File.Copy(Path.Combine(fixture.LastOutput, "rebuilt.bin"), Path.Combine(patch, selected.ResourcePath), true);
        var rejected = await Assert.ThrowsAsync<GeneratedAdapterException>(() => adapter.ValidatePatchAsync(game, patch));
        Assert.Contains("未选择的封包成员", rejected.Message);
        Assert.Empty(Directory.EnumerateDirectories(store.WorkDirectory));
    }

    [Theory]
    [InlineData("loose-renpy", ".rpy", "label start:\n    \"こんにちは。\"\n")]
    [InlineData("loose-kirikiri-ks", ".ks", "\uFEFFこんにちは。[p]\n")]
    [InlineData("rpa3-renpy", ".rpa", "label start:\n    \"こんにちは。\"\n")]
    public async Task FreshSupportedCombinationsVerifyAllResourcesAndPublishWithoutModelRequests(string id, string extension, string content)
    {
        var game = Directory.CreateDirectory(Path.Combine(fixture.Root, "game")).FullName;
        var archive = extension == ".rpa";
        foreach (var name in new[] { "first", "second" })
            if (archive) File.Copy(await fixture.CreateArchiveAsync("archive.rpa3", script: content), Path.Combine(game, name + extension));
            else await File.WriteAllTextAsync(Path.Combine(game, name + extension), content, new UTF8Encoding(false));
        var (_, _, store, _, compiler, assets) = await ToolsAsync(game, archive);
        var result = await new AutomaticAdaptationService(() => throw new InvalidOperationException("No analysis API configured"),
            compiler, store, builtinAssets: assets).AdaptAsync(new("game", game, []));
        Assert.Equal(AdaptationStatus.Activated, result.Status);
        Assert.Equal(0, result.Usage.TotalTokens);
        var workspace = Path.Combine(store.WorkDirectory, "current");
        var state = JsonSerializer.Deserialize<AdaptationSession>(await File.ReadAllTextAsync(Path.Combine(workspace, "session.json")), Json)!;
        Assert.Equal(0, state.TotalTurns);
        Assert.Equal("activated", state.AutomaticTemplate!.Status);
        Assert.Equal(id, state.AutomaticTemplate.TemplateId);
        Assert.Equal(archive ? 2 : 0, state.AutomaticTemplate.EvidenceFiles.Count);
        Assert.Equal(2, state.ResourceHashes.Count);
        Assert.Equal(archive ? 7 : 5, state.ReleaseFiles!.Count);
        Assert.NotNull(state.VerifiedPackage);
        Assert.Equal(state.VerifiedPackage.Version, store.LoadActive("game")!.Version);
        Assert.Contains(state.VerificationDiagnostics, item => item.Contains("byte for byte", StringComparison.Ordinal));
        Assert.Contains(state.VerificationDiagnostics, item => item.Contains("distinct Unicode translations", StringComparison.Ordinal));
        Assert.Contains(state.VerificationDiagnostics, item => item.Contains("Partial trial edits", StringComparison.Ordinal));
        Assert.Empty(state.TextCoverage.Samples); // The adapter's own output is never promoted to independent evidence.
        Assert.Contains("尚未评估", result.Summary);
        Assert.NotEmpty(result.PendingRuntimeChecks!);
        foreach (var path in state.ResourceHashes.Keys)
            Assert.Equal(await File.ReadAllBytesAsync(Path.Combine(game, path)), await File.ReadAllBytesAsync(Path.Combine(workspace, "resources", path)));
        if (!archive)
        {
            Assert.DoesNotContain(state.ReleaseFiles, file => file.Path == EngineReferenceToolkit.DependencyPath || file.Path.EndsWith(".py", StringComparison.Ordinal));
            Assert.False(Directory.Exists(Path.Combine(store.RootDirectory, "engine-tools", "python")));
            Assert.Null(state.EngineReference);
        }
    }

    [Fact]
    public async Task CompiledArchiveMemberAndVerificationFailureRetainEvidenceForTheModelAndResume()
    {
        var game = Directory.CreateDirectory(Path.Combine(fixture.Root, "game")).FullName;
        File.Copy(await fixture.CreateArchiveAsync("archive.rpa3", otherName: "scripts/compiled.rpyc"), Path.Combine(game, "scripts.rpa"));
        var (_, _, store, _, compiler, assets) = await ToolsAsync(game, archive: true);
        var model = new TemplateModel(context => Assert.Contains(context.Messages,
            message => message.Content.Contains("scripts/compiled.rpyc", StringComparison.Ordinal)));
        var service = new AutomaticAdaptationService(model, compiler, store, builtinAssets: assets);
        var request = new AdaptationRequest("game", game, []);
        Assert.Equal(AdaptationStatus.WaitingForUser, (await service.AdaptAsync(request)).Status);
        var workspace = Path.Combine(store.WorkDirectory, "current");
        var statePath = Path.Combine(workspace, "session.json");
        var state = JsonSerializer.Deserialize<AdaptationSession>(await File.ReadAllTextAsync(statePath), Json)!;
        Assert.Equal("archive-inspection", state.AutomaticTemplate!.Stage);
        Assert.Equal("incomplete", state.AutomaticTemplate.Status);
        Assert.True(File.Exists(Path.Combine(workspace, Assert.Single(state.AutomaticTemplate.EvidenceFiles))));
        Assert.True(File.Exists(Path.Combine(workspace, "resources", "scripts.rpa")));
        Assert.False(Directory.Exists(Path.Combine(workspace, "src")));
        Assert.Null(store.LoadActive("game"));
        var evidence = state.AutomaticTemplate;
        await service.AdaptAsync(request);
        state = JsonSerializer.Deserialize<AdaptationSession>(await File.ReadAllTextAsync(statePath), Json)!;
        Assert.Equal(evidence.EvidenceFiles, state.AutomaticTemplate!.EvidenceFiles);
        Assert.Equal(2, model.Calls);

        // A complete template that fails real extraction is kept for repair, with its exact failed stage.
        File.Delete(Path.Combine(game, "scripts.rpa"));
        await File.WriteAllTextAsync(Path.Combine(game, "script.rpy"), "# no translatable text\n");
        var secondStore = new LocalAdapterPackageStore(store.RootDirectory, Path.Combine(fixture.Root, "second-work"));
        model = new TemplateModel(context => Assert.Contains(context.Messages,
            message => message.Content.Contains("initial-extraction", StringComparison.Ordinal)));
        var failed = await new AutomaticAdaptationService(model, compiler, secondStore, builtinAssets: assets).AdaptAsync(request);
        Assert.Equal(AdaptationStatus.WaitingForUser, failed.Status);
        workspace = Path.Combine(secondStore.WorkDirectory, "current");
        state = JsonSerializer.Deserialize<AdaptationSession>(await File.ReadAllTextAsync(Path.Combine(workspace, "session.json")), Json)!;
        Assert.Equal("verification", state.AutomaticTemplate!.Stage);
        Assert.Equal("incomplete", state.AutomaticTemplate.Status);
        Assert.True(File.Exists(Path.Combine(workspace, "src", "Program.cs")));
        Assert.NotEmpty(state.ReleaseFiles!);
        Assert.NotEmpty(state.VerificationDiagnostics);
        Assert.Null(state.VerifiedPackage);
        Assert.Null(secondStore.LoadActive("game"));
    }

    [Fact]
    public async Task NewUnsupportedFilesDuringTheFinalProbePreventAutomaticPublication()
    {
        var game = Directory.CreateDirectory(Path.Combine(fixture.Root, "game")).FullName;
        await File.WriteAllTextAsync(Path.Combine(game, "script.rpy"), "label start:\n    \"こんにちは。\"\n");
        var (_, _, store, _, compiler, assets) = await ToolsAsync(game, archive: false);
        var workspace = Path.Combine(store.WorkDirectory, "current");
        var statePath = Path.Combine(workspace, "session.json");
        var added = false;
        var progress = new TemplateProgress(update =>
        {
            if (added || update.Stage != "识别引擎" || !File.Exists(statePath)) return;
            using var saved = JsonDocument.Parse(File.ReadAllText(statePath));
            if (saved.RootElement.GetProperty("automaticTemplate").GetProperty("stage").GetString() != "publish") return;
            File.WriteAllText(Path.Combine(game, "late.rpyc"), "new compiled script");
            added = true;
        });
        var model = new TemplateModel(context => Assert.Contains(context.Messages,
            message => message.Content.Contains("late.rpyc", StringComparison.Ordinal)));
        var result = await new AutomaticAdaptationService(model, compiler, store, builtinAssets: assets)
            .AdaptAsync(new("game", game, []), progress: progress);
        Assert.True(added);
        Assert.Equal(AdaptationStatus.WaitingForUser, result.Status);
        Assert.Equal(1, model.Calls);
        var state = JsonSerializer.Deserialize<AdaptationSession>(await File.ReadAllTextAsync(statePath), Json)!;
        Assert.Equal("scope-recheck", state.AutomaticTemplate!.Stage);
        Assert.Equal("incomplete", state.AutomaticTemplate.Status);
        Assert.Null(state.VerifiedPackage);
        Assert.Null(store.LoadActive("game"));
        Assert.True(File.Exists(Path.Combine(workspace, "src", "Program.cs")));
    }

    [Fact]
    public async Task CompositionPreflightsAllFilesPreservesEditsAndReusesIdenticalFiles()
    {
        var (tools, state, _, workspace, _, _) = await ToolsAsync(fixture.Root, archive: false);
        var source = Directory.CreateDirectory(Path.Combine(workspace, "src")).FullName;
        var program = Path.Combine(source, "Program.cs");
        await File.WriteAllTextAsync(program, "custom adapter source");
        await Assert.ThrowsAsync<IOException>(() => ComposeAsync(tools, "loose-renpy"));
        Assert.Equal("custom adapter source", await File.ReadAllTextAsync(program));
        Assert.Single(Directory.EnumerateFiles(source, "*", SearchOption.AllDirectories));
        Assert.Null(state.ReleaseFiles);
        File.Delete(program);
        await ComposeAsync(tools, "loose-renpy");
        var bytes = await File.ReadAllBytesAsync(program);
        var release = state.ReleaseFiles!.ToArray();
        await ComposeAsync(tools, "loose-renpy");
        Assert.Equal(bytes, await File.ReadAllBytesAsync(program));
        Assert.Equal(release.OrderBy(file => file.Path), state.ReleaseFiles!.OrderBy(file => file.Path));
        Assert.Equal(5, Directory.EnumerateFiles(source, "*", SearchOption.AllDirectories).Count());
        await Assert.ThrowsAsync<IOException>(() => ComposeAsync(tools, "loose-kirikiri-ks"));
        Assert.Equal(bytes, await File.ReadAllBytesAsync(program));
    }

    [Fact]
    public async Task InvalidPatternsAndUnconfirmedXp3FilterLeaveSourceAndReleaseSelectionUnchanged()
    {
        var (tools, state, _, workspace, _, _) = await ToolsAsync(fixture.Root, archive: false);
        await Assert.ThrowsAsync<InvalidDataException>(() => tools.ExecuteAsync(new("compose", "compose_adapter_template",
            """{"id":"xp3-kirikiri-ks"}"""), default));
        await Assert.ThrowsAsync<InvalidDataException>(() => tools.ExecuteAsync(new("compose", "compose_adapter_template",
            """{"id":"loose-renpy","resourcePatterns":["../*.rpy"]}"""), default));
        Assert.False(Directory.Exists(Path.Combine(workspace, "src")));
        Assert.Null(state.ReleaseFiles);
    }

    private static async Task<JsonElement> ComposeAsync(AdaptationTools tools, string id)
    {
        using var response = JsonDocument.Parse(await tools.ExecuteAsync(new("compose", "compose_adapter_template",
            JsonSerializer.Serialize(new { id, filter = id == "xp3-kirikiri-ks" ? "none" : null })), default));
        return response.RootElement.GetProperty("result").Clone();
    }

    private sealed class TemplateModel(Action<AgentRequest>? inspect = null) : IAdaptationAgentModel
    {
        public string Identity => "template-test";
        public int Calls { get; private set; }
        public Task<AgentTurn> NextAsync(AgentRequest request, CancellationToken cancellationToken)
        {
            Calls++;
            Assert.NotNull(inspect);
            inspect(request);
            return Task.FromResult(new AgentTurn(new("assistant", ToolCalls: [new("stop-" + Calls, "report_blocker",
                """{"kind":"needs_input","summary":"保留当前证据，等待反馈。","reason":"需要启动反馈。","nextAction":"提供游戏运行结果。"}""")]), new(10, 10)));
        }
    }

    private sealed class TemplateProgress(Action<AdaptationProgress> report) : IProgress<AdaptationProgress>
    {
        public void Report(AdaptationProgress value) => report(value);
    }

    private async Task<(AdaptationTools Tools, AdaptationSession State, LocalAdapterPackageStore Store, string Workspace,
        GeneratedAdapterCompiler Compiler, BuiltinAdapterAssets Assets)> ToolsAsync(string game, bool archive)
    {
        var assetsDirectory = Directory.CreateDirectory(Path.Combine(fixture.Root, "assets")).FullName;
        File.Copy(typeof(GameL10n.VnTextPatch.Worker).Assembly.Location, Path.Combine(assetsDirectory, "GameL10n.VnTextPatch.dll"));
        await File.WriteAllTextAsync(Path.Combine(assetsDirectory, "VNTextPatch.LICENSE"), "fixture MIT license");
        await File.WriteAllTextAsync(Path.Combine(assetsDirectory, "VNTextPatch.NOTICE.md"), "fixture attribution");
        var assets = new BuiltinAdapterAssets(assetsDirectory);
        var workspace = Directory.CreateDirectory(Path.Combine(fixture.Root, "workspace")).FullName;
        var store = new LocalAdapterPackageStore(Path.Combine(fixture.Root, "store"));
        if (archive) await fixture.PrepareSharedToolsAsync(store.RootDirectory);
        var repository = new DirectoryInfo(AppContext.BaseDirectory);
        while (repository is not null && !File.Exists(Path.Combine(repository.FullName, "global.json"))) repository = repository.Parent;
        Assert.NotNull(repository);
        var compiler = new GeneratedAdapterCompiler(new(Path.Combine(repository!.FullName, ".tools", "dotnet"), "10.0.402", "10.0.13"), fixture.Runner);
        var state = new AdaptationSession();
        var tools = new AdaptationTools(new("game", game, []), state, workspace, compiler, store, builtinAssets: assets);
        return (tools, state, store, workspace, compiler, assets);
    }
    public void Dispose() => fixture.Dispose();
}
