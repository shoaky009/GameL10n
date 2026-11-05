using GameL10n.Core.Adapters;
using GameL10n.Core.Development;

namespace GameL10n.Tests.Development;

public sealed partial class AutomaticAdaptationTests
{
    [Fact]
    public async Task AutomaticTemplatesRespectTaskIntentExistingSourcesAndBothDisabledFamilies()
    {
        var request = Request();
        await File.WriteAllTextAsync(Path.Combine(request.GameDirectory, "script.rpy"), "label start:\n    \"こんにちは。\"\n");
        var discovery = await BuiltinAdapterDiscovery.InspectAsync(request.GameDirectory);
        var store = new LocalAdapterPackageStore(Path.Combine(root, "store"));
        var workspace = Path.Combine(root, "workspace");
        var compiler = Runtime(new ScriptedRunner());
        async Task<AdaptationSession> Run(AdaptationRequest current, IReadOnlySet<string>? disabled = null)
        {
            var state = new AdaptationSession { Objective = AutomaticAdaptationService.ResolveObjective(current), BuiltinDiscovery = discovery };
            await new AdapterDevelopmentWorkflow(current, state, workspace, compiler, store)
                .RunAutomaticTemplateAsync(disabled ?? new HashSet<string>(), () => Task.CompletedTask, default);
            Assert.Null(store.LoadActive(request.GameId));
            Assert.Null(state.ReleaseFiles);
            return state;
        }
        foreach (var current in new[]
        {
            request with { UserInstruction = "保留已有启动方案，先调查字体。" },
            request with { Objective = AdaptationObjective.Repair },
            request with { Objective = AdaptationObjective.ExpandCoverage },
            request with { ImproveExisting = true },
            request with { BaselineUnits = [new("existing", "script.rpy", "line:1", "こんにちは。", "existing")] }
        })
            Assert.Null((await Run(current)).AutomaticTemplate);

        foreach (var family in new[] { "template.loose-renpy", VnTextPatchFormatCatalog.All.Single(format => format.Key == "Renpy").Family.AdapterId })
        {
            var state = await Run(request, new HashSet<string> { family });
            Assert.Contains(state.AutomaticTemplate!.Diagnostics, item => item.Contains("已禁用", StringComparison.Ordinal));
            Assert.False(Directory.Exists(Path.Combine(workspace, "src")));
        }
        Directory.CreateDirectory(Path.Combine(workspace, "src"));
        var source = Path.Combine(workspace, "src", "adapter.py");
        await File.WriteAllTextAsync(source, "existing implementation");
        Assert.Null((await Run(request)).AutomaticTemplate);
        Assert.Equal("existing implementation", await File.ReadAllTextAsync(source));
    }

    [Fact]
    public async Task LooseSamplesCannotHideCompiledScriptsArchivesOrKnownLaunchIssues()
    {
        var request = Request();
        await File.WriteAllTextAsync(Path.Combine(request.GameDirectory, "script.rpy"), "label start:\n    \"こんにちは。\"\n");
        var store = new LocalAdapterPackageStore(Path.Combine(root, "store"));
        var workspace = Path.Combine(root, "workspace");
        var compiler = Runtime(new ScriptedRunner());
        foreach (var blocker in new[] { "compiled.rpyc", "script_ren.py", "data.rpa", "data.xp3", "other.ks", "error.log" })
        {
            var path = Path.Combine(request.GameDirectory, blocker);
            await File.WriteAllTextAsync(path, "uninvestigated resource");
            var state = new AdaptationSession
            {
                Objective = AdaptationObjective.Develop,
                BuiltinDiscovery = await BuiltinAdapterDiscovery.InspectAsync(request.GameDirectory),
                LaunchInspection = await GameLaunchInspector.InspectAsync(request.GameDirectory)
            };
            await new AdapterDevelopmentWorkflow(request, state, workspace, compiler, store)
                .RunAutomaticTemplateAsync(new HashSet<string>(), () => Task.CompletedTask, default);
            Assert.Equal("incomplete", state.AutomaticTemplate!.Status);
            Assert.Contains(state.AutomaticTemplate.Diagnostics, item => item.Contains(blocker, StringComparison.Ordinal));
            Assert.False(Directory.Exists(Path.Combine(workspace, "src")));
            Assert.Empty(state.ResourceHashes);
            Assert.Null(store.LoadActive(request.GameId));
            File.Delete(path);
        }
    }
}
