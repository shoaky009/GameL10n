using System.Text.Json;
using AvgTranslator.Core.Development;

namespace AvgTranslator.Tests.Development;

public sealed class AdaptationHistoryTests
{
    [Theory]
    [InlineData(true)]
    [InlineData(false)]
    public void SupersededReadUsesCommittedWriteEvidenceAndPreservesProviderData(bool changed)
    {
        var body = JsonSerializer.Serialize(new { success = true, result = new { content = new string('x', 12000) } });
        var raw = JsonSerializer.SerializeToElement(new object[] {
            new { type = "reasoning", encrypted_content = "opaque" },
            new { type = "function_call", call_id = "write", name = "replace_text",
                arguments = "{\"path\":\"src/Parser.cs\",\"oldText\":\"old\",\"newText\":\"new\"}" }
        });
        var messages = new AgentMessage[] {
            new("assistant", ToolCalls: [new("read", "read_file", """{"root":"workspace","path":"./src/Parser.cs"}""")]),
            new("tool", body, ToolCallId: "read"),
            new("assistant", ProviderData: raw),
            new("tool", JsonSerializer.Serialize(new { success = true, result = new { changed } }), ToolCallId: "write")
        };
        var replay = AdaptationHistory.ForRequest(messages);
        Assert.Equal(changed ? 1 : 0, replay.OmittedResults);
        if (changed) Assert.Contains("supersededBy", replay.Messages[1].Content);
        else Assert.Equal(body, replay.Messages[1].Content);
        Assert.Equal(body, messages[1].Content);
        Assert.Equal(raw.GetRawText(), replay.Messages[2].ProviderData!.Value.GetRawText());
    }

    [Fact]
    public void RepeatedFailureRetainsLatestFullDiagnosticAndDistinctFailures()
    {
        var error = JsonSerializer.Serialize(new { success = false, error = new string('x', 4096) });
        var distinct = JsonSerializer.Serialize(new { success = false, error = new string('y', 4096) });
        var messages = new List<AgentMessage>();
        foreach (var (id, result) in new[] { ("first", error), ("other", distinct), ("latest", error) })
        {
            messages.Add(new("assistant", ToolCalls: [new(id, "test_adapter", "{}")]));
            messages.Add(new("tool", result, ToolCallId: id));
        }
        var replay = AdaptationHistory.ForRequest(messages);
        Assert.Equal(1, replay.OmittedResults);
        using var omitted = JsonDocument.Parse(replay.Messages[1].Content);
        Assert.False(omitted.RootElement.GetProperty("success").GetBoolean());
        Assert.Equal("latest", omitted.RootElement.GetProperty("identicalTo").GetString());
        Assert.Equal(distinct, replay.Messages[3].Content);
        Assert.Equal(error, replay.Messages[5].Content);
        Assert.Equal(error, messages[1].Content);
    }
}
