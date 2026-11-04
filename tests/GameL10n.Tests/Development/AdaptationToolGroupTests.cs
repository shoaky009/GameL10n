using System.Text.Json;
using GameL10n.Core.Development;

namespace GameL10n.Tests.Development;

public sealed class AdaptationToolGroupTests
{
    [Fact]
    public void ReplacingAllCanUnloadSchemasWithoutLosingBasicToolsOrRestoringArchivedGroups()
    {
        var state = new AdaptationSession
        {
            Messages = [new("assistant", ToolCalls: [new("old", "inspect_pe_code", "{}")])]
        };
        Load(state, """{"groups":["all"]}""");
        Assert.Equal(AdaptationTools.Definitions.Count, AdaptationToolGroups.ForRequest(state).Count);
        Load(state, """{"groups":["development"],"mode":"replace"}""");
        AdaptationToolGroups.Restore(state);
        Assert.Contains(AdaptationToolGroups.ForRequest(state), tool => tool.Name == "test_adapter");
        Assert.DoesNotContain(AdaptationToolGroups.ForRequest(state), tool => tool.Name == "inspect_pe_code");

        var selected = state.EnabledToolGroups.ToArray();
        Assert.Throws<InvalidDataException>(() => Load(state, """{"groups":["unknown"],"mode":"replace"}"""));
        Assert.Equal(selected, state.EnabledToolGroups);
        Load(state, """{"groups":[],"mode":"replace"}""");
        Assert.Empty(state.EnabledToolGroups);
        Assert.Contains(AdaptationToolGroups.ForRequest(state), tool => tool.Name == "load_tool_groups");
        Assert.Contains(AdaptationToolGroups.ForRequest(state), tool => tool.Name == "read_tool_result");
        Assert.DoesNotContain(AdaptationToolGroups.ForRequest(state), tool => tool.Name == "test_adapter");
        Load(state, """{"groups":["native"]}""");
        Assert.Contains(AdaptationToolGroups.ForRequest(state), tool => tool.Name == "inspect_pe_code");
    }

    [Theory]
    [InlineData(true)]
    [InlineData(false)]
    public void ContextSelectionUsesOnlyRecentCallsAndPreservesPendingTools(bool responses)
    {
        using var provider = JsonDocument.Parse(responses
            ? """[{"type":"function_call","call_id":"recent","name":"inspect_pe_code","arguments":"{}"}]"""
            : """{"tool_calls":[{"id":"recent","function":{"name":"inspect_pe_code","arguments":"{}"}}]}""");
        var state = new AdaptationSession
        {
            EnabledToolGroups = ["all"],
            PendingCalls = [new("pending", "run_engine_python", "{}")],
            Messages =
            [
                new("assistant", ToolCalls: [new("archived", "inspect_unity_schema", "{}")]),
                new("assistant", ToolCalls: [new("read1", "read_file", "{}")]),
                new("assistant", ToolCalls: [new("read2", "read_file", "{}")]),
                new("assistant", ToolCalls: [new("read3", "read_file", "{}")]),
                new("assistant", ProviderData: provider.RootElement.Clone())
            ]
        };
        AdaptationToolGroups.Restore(state);
        AdaptationToolGroups.SelectForContext(state, ["development"], state.Messages);
        Assert.Equal(new[] { "development", "native", "research" }, state.EnabledToolGroups);
        Assert.DoesNotContain(AdaptationToolGroups.ForRequest(state), tool => tool.Name == "inspect_unity_schema");
        Assert.Contains(AdaptationToolGroups.ForRequest(state), tool => tool.Name == "run_engine_python");
    }

    private static void Load(AdaptationSession state, string arguments)
    {
        using var document = JsonDocument.Parse(arguments);
        AdaptationToolGroups.Load(state, document.RootElement);
    }
}
