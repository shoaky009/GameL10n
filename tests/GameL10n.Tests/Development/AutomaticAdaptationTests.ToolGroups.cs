using GameL10n.Core.Development;
using GameL10n.Core.Storage;

namespace GameL10n.Tests.Development;

public sealed partial class AutomaticAdaptationTests
{
    [Fact]
    public async Task BoundReferencePreloadsExecutionWithoutKeepingResearchSchemas()
    {
        var request = Request();
        var store = new LocalAdapterPackageStore(Path.Combine(root, "store"));
        await LocalFiles.WriteJsonAsync(Path.Combine(store.WorkDirectory, "current/session.json"), new AdaptationSession
        {
            GameId = request.GameId, GameDirectory = request.GameDirectory, ModelIdentity = "fake",
            EngineReference = new(new string('a', 40), new string('b', 64), new string('c', 64)),
            EnabledToolGroups = ["research"], Messages = [new("user", "Continue the implementation using the pinned library.")]
        }, FixtureJson.TypeInfo<AdaptationSession>(FixtureJson.Storage));
        var round = 0;
        var model = new FakeModel(context =>
        {
            if (round++ == 0)
            {
                Assert.Contains(context.Tools, tool => tool.Name == "run_engine_python");
                Assert.DoesNotContain(context.Tools, tool => tool.Name == "inspect_engine_reference");
                return new(new("assistant", ToolCalls: [new("research", "load_tool_groups", """{"groups":["research"]}""")]), new(100, 20));
            }
            if (round == 2)
            {
                Assert.Contains(context.Tools, tool => tool.Name == "inspect_engine_reference");
                return new(new("assistant", ToolCalls: [new("development", "load_tool_groups", """{"groups":["development"],"mode":"replace"}""")]), new(100, 20));
            }
            Assert.Contains(context.Tools, tool => tool.Name == "run_engine_python");
            Assert.DoesNotContain(context.Tools, tool => tool.Name == "inspect_engine_reference");
            return Blocked("needs_input", "需要显示反馈。", "提供画面。");
        });
        var result = await new AutomaticAdaptationService(model, Runtime(new ScriptedRunner()), store).AdaptAsync(request);
        Assert.Equal(AdaptationStatus.WaitingForUser, result.Status);
        Assert.Equal(3, model.Calls);
    }

    [Fact]
    public async Task ExplicitToolReplacementIsNotUndoneByHintsAndCheckpointTrimsUnusedGroups()
    {
        var request = Request();
        var store = new LocalAdapterPackageStore(Path.Combine(root, "store"));
        Directory.CreateDirectory(Path.Combine(store.WorkDirectory, "current/src"));
        var round = 0;
        var model = new FakeModel(context =>
        {
            AgentToolCall call;
            switch (round++)
            {
                case 0:
                    Assert.Contains(context.Tools, tool => tool.Name == "test_adapter");
                    call = new("all", "load_tool_groups", """{"groups":["all"]}""");
                    break;
                case 1:
                    Assert.Equal(AdaptationTools.Definitions.Count, context.Tools.Count);
                    call = new("replace", "load_tool_groups", """{"groups":["research"],"mode":"replace"}""");
                    break;
                case 2:
                    Assert.DoesNotContain(context.Tools, tool => tool.Name == "test_adapter");
                    Assert.DoesNotContain(context.Tools, tool => tool.Name == "inspect_pe_code");
                    Assert.Contains(context.Tools, tool => tool.Name == "inspect_engine_reference");
                    Assert.DoesNotContain(context.Tools, tool => tool.Name == "run_engine_python");
                    call = new("checkpoint", "checkpoint", """{"summary":"Keep working from existing source."}""");
                    break;
                default:
                    Assert.Contains(context.Tools, tool => tool.Name == "test_adapter");
                    Assert.DoesNotContain(context.Tools, tool => tool.Name == "inspect_pe_code");
                    return Blocked("needs_input", "需要显示反馈。", "提供画面。");
            }
            return new(new("assistant", ToolCalls: [call]), new(100, 20));
        });
        await new AutomaticAdaptationService(model, Runtime(new ScriptedRunner()), store).AdaptAsync(request);
        var saved = await LocalFiles.ReadJsonAsync<AdaptationSession>(Path.Combine(store.WorkDirectory, "current/session.json"), FixtureJson.TypeInfo<global::GameL10n.Core.Development.AdaptationSession>(FixtureJson.Storage));
        Assert.DoesNotContain("native", saved!.EnabledToolGroups);
        // A historical all choice must not return when the same session is reopened with another model.
        saved.ModelIdentity = "previous-model";
        saved.EnabledToolGroups = ["all"];
        await LocalFiles.WriteJsonAsync(Path.Combine(store.WorkDirectory, "current/session.json"), saved, FixtureJson.TypeInfo<object>(FixtureJson.Storage));
        var resumed = new FakeModel(context =>
        {
            Assert.Contains(context.Tools, tool => tool.Name == "test_adapter");
            Assert.DoesNotContain(context.Tools, tool => tool.Name == "inspect_pe_code");
            Assert.DoesNotContain(context.Tools, tool => tool.Name == "inspect_unity_schema");
            return Blocked("needs_input", "需要显示反馈。", "提供画面。");
        });
        await new AutomaticAdaptationService(resumed, Runtime(new ScriptedRunner()), store).AdaptAsync(request);
        Assert.Equal(1, resumed.Calls);
    }
}
