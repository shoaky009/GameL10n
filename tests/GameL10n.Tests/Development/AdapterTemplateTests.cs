using System.Text;
using System.Text.Json;
using GameL10n.Core.Adapters;
using GameL10n.Core.Development;

namespace GameL10n.Tests.Development;

public sealed class AdapterTemplateTests : IDisposable
{
    private readonly EngineArchiveFixture fixture = new("RenpyRpycReference.zip");
    private static readonly JsonSerializerOptions Json = new(JsonSerializerDefaults.Web);

    [Theory]
    [InlineData("rpa3-renpy", "archive.rpa3", ".rpa", "script.rpy")]
    [InlineData("rpa3-renpy-rpyc", "archive.rpa3", ".rpa", "scripts\\story.rpyc")]
    [InlineData("xp3-kirikiri-ks", "archive.xp3", ".xp3", "scenario/start.ks")]
    public async Task ComposedArchiveAdaptersPassIndependentFullAndPartialChineseRoundtrips(string id, string component, string extension, string member)
    {
        var game = Directory.CreateDirectory(Path.Combine(fixture.Root, "game")).FullName;
        var script = member.EndsWith(".rpy", StringComparison.Ordinal)
            ? "#" + new string('x', 70000) + "\nlabel start:\n    \"こんにちは。\"\n    \"次の台詞です。\"\n"
            : "\uFEFF;" + new string('x', 70000) + "\n*start\nこんにちは。[p]\n次の台詞です。[p]\n";
        var source = await fixture.CreateArchiveAsync(component, script: script, scriptName: member);
        File.Copy(source, Path.Combine(game, "first" + extension));
        var compiled = member.EndsWith(".rpyc", StringComparison.Ordinal);
        File.Copy(compiled ? await fixture.CreateArchiveAsync(component, scriptName: "second/" + member) : source,
            Path.Combine(game, "second" + extension));
        var secondOriginal = await File.ReadAllBytesAsync(Path.Combine(game, "second" + extension));
        var original = await File.ReadAllBytesAsync(source);
        var (tools, state, store, workspace, compiler, assets) = await ToolsAsync(game, archive: true);
        var composition = await ComposeAsync(tools, id);
        Assert.Null(state.VerifiedPackage);
        Assert.Equal(compiled ? 5 : 6, state.ReleaseFiles!.Count);
        Assert.Contains(state.ReleaseFiles, file => file.Path == EngineArchiveComponents.ScriptPath && file.Role == "runtime");
        Assert.Equal(!compiled, state.ReleaseFiles.Any(file => file.Path == BuiltinAdapterAssets.SharedDependencyPath));
        Assert.Empty(Directory.EnumerateFiles(Path.Combine(workspace, "src"), "*.dll", SearchOption.AllDirectories));
        Assert.False(Directory.Exists(Path.Combine(workspace, "src", "python")));
        await tools.ExecuteAsync(new("stage", "stage_resources", JsonSerializer.Serialize(new { files = new[] { "first" + extension, "second" + extension } })), default);
        var arguments = composition.GetProperty("testArguments").GetRawText();
        using var trial = JsonDocument.Parse(await tools.ExecuteAsync(new("trial", "test_adapter",
            arguments[..^1] + ",\"trialFiles\":[\"first" + extension + "\"]}"), default));
        Assert.True(trial.RootElement.GetProperty("result").GetProperty("passed").GetBoolean(), trial.RootElement.ToString());
        Assert.Equal("trial", trial.RootElement.GetProperty("result").GetProperty("stage").GetString());
        Assert.Null(state.VerifiedPackage);
        Assert.Null(state.VerificationCache);
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
        Assert.All(extracted.Units, unit => Assert.StartsWith(compiled ? "rpyc:" : "member:", unit.Locator));
        var selected = extracted.Units[0];
        var patch = Path.Combine(fixture.Root, "partial");
        var translation = "这是单条中文修改。" + string.Concat(selected.ProtectedTokens ?? []);
        await adapter.BuildPatchAsync(game, new Dictionary<string, string> { [selected.Id] = translation }, patch);
        var after = await adapter.ExtractAsync(patch);
        Assert.Equal(translation, after.Units.Single(unit => unit.Id == selected.Id).SourceText);
        Assert.All(after.Units.Where(unit => unit.Id != selected.Id), unit =>
        {
            var before = extracted.Units.Single(before => before.Id == unit.Id);
            Assert.Equal(before with { ProtectedTokens = null }, unit with { ProtectedTokens = null });
            Assert.Equal(before.ProtectedTokens, unit.ProtectedTokens);
        });
        Assert.Equal(original, await File.ReadAllBytesAsync(Path.Combine(game, "first" + extension)));
        var untouched = selected.ResourcePath == "first" + extension ? "second" + extension : "first" + extension;
        Assert.Equal(untouched.StartsWith("first", StringComparison.Ordinal) ? original : secondOriginal,
            await File.ReadAllBytesAsync(Path.Combine(patch, untouched)));
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
    [InlineData("loose-renpy-rpyc", ".rpyc", "")]
    [InlineData("rpa3-renpy-rpyc", ".rpa", "")]
    public async Task FreshSupportedCombinationsVerifyAllResourcesAndPublishWithoutModelRequests(string id, string extension, string content)
    {
        var game = Directory.CreateDirectory(Path.Combine(fixture.Root, "game")).FullName;
        var archive = extension == ".rpa";
        var compiled = id.EndsWith("-rpyc", StringComparison.Ordinal);
        foreach (var name in new[] { "first", "second" })
            if (archive) File.Copy(await fixture.CreateArchiveAsync("archive.rpa3", script: content,
                scriptName: compiled ? name + ".rpyc" : "script.rpy"), Path.Combine(game, name + extension));
            else if (compiled) await fixture.CreateCompiledScriptAsync(Path.Combine(game, name + extension));
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
        Assert.Equal(archive && !compiled ? 6 : 5, state.ReleaseFiles!.Count);
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
            Assert.Contains(state.ReleaseFiles, file => file.Path == EngineReferenceToolkit.DependencyPath);
            Assert.True(Directory.Exists(Path.Combine(store.RootDirectory, "engine-tools", "python")));
            Assert.NotNull(state.EngineReference);
        }
    }

    [Theory]
    [InlineData(true, false)]
    [InlineData(false, true)]
    [InlineData(true, true)]
    public async Task MixedRenpyResourcesPublishWithoutModelAndPreserveUntouchedContainers(bool loose, bool packed)
    {
        var game = Directory.CreateDirectory(Path.Combine(fixture.Root, "game")).FullName;
        var resources = Directory.CreateDirectory(Path.Combine(game, "game")).FullName;
        var source = Path.Combine(fixture.Root, "intro.rpy");
        await File.WriteAllTextAsync(source, "label intro:\n    \"こんにちは。\"\n", new UTF8Encoding(false));
        if (loose)
        {
            File.Copy(source, Path.Combine(resources, "intro.rpy"));
            await fixture.CreateCompiledScriptAsync(Path.Combine(resources, "second.rpyc"));
        }
        if (packed)
        {
            var scripts = await fixture.CreateArchiveAsync("archive.rpa3", otherName: "chapters/inside.rpy", scriptName: "compiled/fourth.rpyc");
            var rebuilt = await fixture.InvokeAsync("archive.rpa3", "rebuild", scripts,
                replacements: new Dictionary<string, string> { ["chapters/inside.rpy"] = source });
            Assert.True(rebuilt.GetProperty("success").GetBoolean(), rebuilt.ToString());
            File.Copy(Path.Combine(fixture.LastOutput, "rebuilt.bin"), Path.Combine(resources, "scripts.rpa"));
            File.Copy(await fixture.CreateArchiveAsync("archive.rpa3", scriptName: "images/background.bin", script: "media"),
                Path.Combine(resources, "media.rpa"));
        }
        var (_, _, store, _, runtime, assets) = await ToolsAsync(game, archive: packed);
        var result = await new AutomaticAdaptationService(() => throw new Xunit.Sdk.XunitException("Disjoint mixed resources need no model"),
            runtime, store, builtinAssets: assets).AdaptAsync(new("game", game, []));
        Assert.Equal(AdaptationStatus.Activated, result.Status);
        Assert.Equal(0, result.Usage.TotalTokens);
        var workspace = Path.Combine(store.WorkDirectory, "current");
        var state = JsonSerializer.Deserialize<AdaptationSession>(await File.ReadAllTextAsync(Path.Combine(workspace, "session.json")), Json)!;
        Assert.Equal("renpy-mixed", state.AutomaticTemplate!.TemplateId);
        Assert.Equal(loose ? 2 : 0, state.AutomaticTemplate.LooseScripts);
        Assert.Equal(packed ? 2 : 0, state.AutomaticTemplate.ArchivedScripts);
        Assert.Equal(packed ? 1 : 0, state.AutomaticTemplate.MediaOnlyArchives);
        Assert.Equal((loose ? 2 : 0) + (packed ? 2 : 0), state.ResourceHashes.Count);
        Assert.NotNull(state.VerifiedPackage);
        Assert.Contains(state.VerificationDiagnostics, diagnostic => diagnostic.Contains("Partial trial edits", StringComparison.Ordinal));
        var toolkit = new EngineReferenceToolkit(Path.Combine(store.RootDirectory, "engine-tools"), parserAssets: assets);
        var adapter = new GeneratedGameAdapter(state.VerifiedPackage!, runtime.Toolchain, fixture.Runner, store.WorkDirectory, toolkit);
        var before = await adapter.ExtractAsync(game);
        var translations = before.Units.Where(unit => unit.ResourcePath == "game/intro.rpy"
                || unit.ResourcePath == "game/scripts.rpa" && unit.Locator.Contains("compiled/fourth.rpyc", StringComparison.Ordinal))
            .ToDictionary(unit => unit.Id, unit => "这是局部中文修改。" + string.Concat(unit.ProtectedTokens ?? []));
        var patch = Path.Combine(fixture.Root, "mixed-patch");
        await adapter.BuildPatchAsync(game, translations, patch);
        var after = await adapter.ExtractAsync(patch);
        Assert.Equal(before.Units.Select(unit => unit.Id).Order(), after.Units.Select(unit => unit.Id).Order());
        foreach (var unit in after.Units)
            Assert.Equal(translations.GetValueOrDefault(unit.Id) ?? before.Units.Single(original => original.Id == unit.Id).SourceText, unit.SourceText);
        if (loose)
            Assert.Equal(await File.ReadAllBytesAsync(Path.Combine(resources, "second.rpyc")), await File.ReadAllBytesAsync(Path.Combine(patch, "game", "second.rpyc")));
        Assert.True((await adapter.ValidatePatchAsync(game, patch)).Passed);
        if (!packed) return;
        Assert.Equal(await File.ReadAllBytesAsync(Path.Combine(resources, "media.rpa")), await File.ReadAllBytesAsync(Path.Combine(patch, "game", "media.rpa")));
        var replacement = Path.Combine(fixture.Root, "tampered-media");
        await File.WriteAllTextAsync(replacement, "changed");
        var tampered = await fixture.InvokeAsync("archive.rpa3", "rebuild", Path.Combine(patch, "game", "media.rpa"),
            replacements: new Dictionary<string, string> { ["images/logo.bin"] = replacement });
        Assert.True(tampered.GetProperty("success").GetBoolean(), tampered.ToString());
        File.Copy(Path.Combine(fixture.LastOutput, "rebuilt.bin"), Path.Combine(patch, "game", "media.rpa"), overwrite: true);
        var rejected = await Assert.ThrowsAsync<GeneratedAdapterException>(() => adapter.ValidatePatchAsync(game, patch));
        Assert.Contains("未选择的封包成员", rejected.Message);
    }

    [Fact]
    public async Task LargeMediaArchivePublishesWithoutModelAndOnlyReadsScriptPayloadsBeforeVerification()
    {
        var game = Directory.CreateDirectory(Path.Combine(fixture.Root, "game")).FullName;
        var archive = await fixture.CreateArchiveAsync("archive.rpa3", mediaLength: 256L * 1024 * 1024 + 1);
        File.Copy(archive, Path.Combine(game, "scripts.rpa"));
        var (_, _, store, _, runtime, assets) = await ToolsAsync(game, archive: true);
        var result = await new AutomaticAdaptationService(() => throw new Xunit.Sdk.XunitException("This archive needs no model"),
            runtime, store, builtinAssets: assets).AdaptAsync(new("game", game, []));
        Assert.Equal(AdaptationStatus.Activated, result.Status);
        Assert.Equal(0, result.Usage.TotalTokens);
        var workspace = Path.Combine(store.WorkDirectory, "current");
        var state = JsonSerializer.Deserialize<AdaptationSession>(await File.ReadAllTextAsync(Path.Combine(workspace, "session.json")), Json)!;
        Assert.True(state.AutomaticTemplate!.ResourceBytes > 256L * 1024 * 1024);
        Assert.True(state.AutomaticTemplate.SelectedBytes < 1024);
        Assert.True(state.AutomaticTemplate.ArchiveReadBytes < 4096);
        Assert.Contains(state.VerificationDiagnostics, diagnostic => diagnostic.Contains("distinct Unicode translations", StringComparison.Ordinal));
        using var manifest = JsonDocument.Parse(await File.ReadAllTextAsync(Path.Combine(workspace, Assert.Single(state.AutomaticTemplate.EvidenceFiles))));
        Assert.False(manifest.RootElement.GetProperty("allContentsVerified").GetBoolean());

        // Normal extraction must also leave the media unread. Final validation
        // still hashes it: tampering in this member cannot escape acceptance.
        var extracted = await fixture.InvokeAsync("archive.rpa3", "extract", archive, extensions: [".rpy"]);
        Assert.True(extracted.GetProperty("success").GetBoolean(), extracted.ToString());
        Assert.True(extracted.GetProperty("readBytes").GetInt64() < 4096);
        var media = Assert.Single(extracted.GetProperty("entries").EnumerateArray(), entry => entry.GetProperty("name").GetString() == "images/logo.bin");
        Assert.Equal(JsonValueKind.Null, media.GetProperty("sha256").ValueKind);
        var toolkit = new EngineReferenceToolkit(Path.Combine(store.RootDirectory, "engine-tools"), parserAssets: assets);
        var adapter = new GeneratedGameAdapter(state.VerifiedPackage!, runtime.Toolchain, fixture.Runner, store.WorkDirectory, toolkit);
        var patch = Directory.CreateDirectory(Path.Combine(fixture.Root, "tampered")).FullName;
        var patched = Path.Combine(patch, "scripts.rpa");
        File.Copy(archive, patched);
        await using (var file = new FileStream(patched, FileMode.Open, FileAccess.Write))
        {
            file.Position = 34 + System.Text.Encoding.UTF8.GetByteCount("label start:\n    \"こんにちは。\"\n") + 128;
            file.WriteByte(42);
        }
        var error = await Assert.ThrowsAsync<GeneratedAdapterException>(() => adapter.ValidatePatchAsync(game, patch));
        Assert.Contains("未选择的封包成员", error.Message);
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
        Assert.Equal("verification", state.AutomaticTemplate!.Stage);
        Assert.Equal("incomplete", state.AutomaticTemplate.Status);
        Assert.True(File.Exists(Path.Combine(workspace, Assert.Single(state.AutomaticTemplate.EvidenceFiles))));
        Assert.True(File.Exists(Path.Combine(workspace, "resources", "scripts.rpa")));
        Assert.True(File.Exists(Path.Combine(workspace, "src", "adapter.py")));
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
        Assert.True(File.Exists(Path.Combine(workspace, "src", "adapter.py")));
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
        Assert.True(File.Exists(Path.Combine(workspace, "src", "adapter.py")));
    }

    [Fact]
    public async Task CompiledScriptProtocolRejectsNonTextChangesAmbiguousArchivesAndUnsafePickleExecution()
    {
        var (tools, _, _, workspace, _, _) = await ToolsAsync(fixture.Root, archive: false);
        await ComposeAsync(tools, "renpy-mixed");
        await fixture.CreateCompiledScriptAsync(Path.Combine(fixture.Root, "sample.rpyc"));
        var check = Path.Combine(fixture.Root, "check-rpyc.py");
        using (var source = typeof(AdapterTemplateTests).Assembly.GetManifestResourceStream("RenpyRpycAdapterCheck.py")!)
        await using (var target = File.Create(check)) await source.CopyToAsync(target);
        var result = await fixture.Runner.RunAsync(new(fixture.Paths.Python,
            EngineReferenceToolkit.PythonArguments(fixture.Paths, check, [Path.Combine(workspace, "src")]), fixture.Root), default);
        Assert.True(result.ExitCode == 0, result.StandardOutput + result.StandardError);
        Assert.Equal("rpyc-adapter-ok", result.StandardOutput.Trim());
    }

    [Fact]
    public async Task CompositionPreflightsAllFilesPreservesEditsAndReusesIdenticalFiles()
    {
        var (tools, state, _, workspace, _, _) = await ToolsAsync(fixture.Root, archive: false);
        var source = Directory.CreateDirectory(Path.Combine(workspace, "src")).FullName;
        var program = Path.Combine(source, "adapter.py");
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
        AdapterRuntime Compiler, BuiltinAdapterAssets Assets)> ToolsAsync(string game, bool archive)
    {
        var assets = ParserToolFixture.Create(Path.Combine(fixture.Root, "assets"));
        var workspace = Directory.CreateDirectory(Path.Combine(fixture.Root, "workspace")).FullName;
        var store = new LocalAdapterPackageStore(Path.Combine(fixture.Root, "store"));
        await fixture.PrepareSharedToolsAsync(store.RootDirectory);
        var repository = new DirectoryInfo(AppContext.BaseDirectory);
        while (repository is not null && !File.Exists(Path.Combine(repository.FullName, "global.json"))) repository = repository.Parent;
        Assert.NotNull(repository);
        var compiler = new AdapterRuntime(new(Path.Combine(repository!.FullName, ".tools", "dotnet"), "10.0.402", "10.0.13"), fixture.Runner);
        var state = new AdaptationSession();
        var tools = new AdaptationTools(new("game", game, []), state, workspace, compiler, store, builtinAssets: assets);
        return (tools, state, store, workspace, compiler, assets);
    }
    public void Dispose() => fixture.Dispose();
}
