using System.Text.Json;
using AvgTranslator.Core.Development;

namespace AvgTranslator.Tests.Development;

public sealed class AdaptationHistoryTests
{
    [Fact]
    public void AutomaticCompactionKeepsWholeRecentBatchesAndOriginalProviderData()
    {
        var provider = JsonSerializer.SerializeToElement(new object[] {
            new { type = "reasoning", encrypted_content = "opaque", summary = new[] { new { text = "known stride 8" } } },
            new { type = "function_call", call_id = "first", name = "read_file", arguments = "{\"path\":\"src/A.cs\"}" },
            new { type = "function_call", call_id = "second", name = "read_file", arguments = "{\"path\":\"src/B.cs\"}" }
        });
        var messages = new AgentMessage[] {
            new("user", "task"), new("assistant", new string('x', 70_000)),
            new("assistant", ProviderData: provider),
            new("tool", "first result", ToolCallId: "first"), new("tool", "second result", ToolCallId: "second")
        };
        var recent = AdaptationHistory.RecentExchanges(messages, 64 * 1024);
        Assert.Equal(3, recent.Length);
        Assert.Equal(provider.GetRawText(), recent[0].ProviderData!.Value.GetRawText());
        Assert.Equal(new[] { "first", "second" }, recent.Skip(1).Select(message => message.ToolCallId));
        Assert.Empty(AdaptationHistory.RecentExchanges(messages, 10));
        Assert.Contains("known stride 8", AdaptationHistory.ReadableMessage(messages[2]));
        Assert.DoesNotContain("opaque", AdaptationHistory.ReadableMessage(messages[2]));
        Assert.Equal(70_000, messages[1].Content.Length);
    }

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
