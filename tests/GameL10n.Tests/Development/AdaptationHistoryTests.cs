using System.Text.Json;
using GameL10n.Core.Development;

namespace GameL10n.Tests.Development;

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

    [Theory]
    [InlineData(true)]
    [InlineData(false)]
    public void NewestProcessFailureKeepsDiagnosticTailAndOriginalHistory(bool nested)
    {
        const string traceback = "Traceback (most recent call last):\n  File probe.py, line 42\nValueError: decisive failure";
        var stdout = "stdout head\n" + new string('x', 5000) + "PRIVATE_MIDDLE_OUTPUT" + new string('x', 5000) + "\nstdout tail";
        var stderr = "stderr head\n" + new string('y', 9000) + "\n" + traceback;
        var body = nested
            ? JsonSerializer.Serialize(new { success = true, result = new { exitCode = 1, stdout, stderr, durationMs = 123 } })
            : JsonSerializer.Serialize(new { success = false, phase = "run", timedOut = true, stdout, stderr });
        var provider = JsonSerializer.SerializeToElement(new object[] {
            new { type = "reasoning", encrypted_content = "opaque", summary = new[] { new { text = "run the probe" } } },
            new { type = "function_call", call_id = "probe", name = "run_engine_python", arguments = "{ \"script\": \"work/probe.py\" }" }
        });
        var assistant = new AgentMessage("assistant", ProviderData: provider);
        var messages = new AgentMessage[] { assistant, new("tool", body, ToolCallId: "probe") };

        var replay = AdaptationHistory.ForRequest(messages);

        Assert.Equal(1, replay.OmittedResults);
        Assert.Same(assistant, replay.Messages[0]);
        Assert.Equal(provider.GetRawText(), replay.Messages[0].ProviderData!.Value.GetRawText());
        using var preview = JsonDocument.Parse(replay.Messages[1].Content);
        var process = nested ? preview.RootElement.GetProperty("result") : preview.RootElement;
        Assert.StartsWith("stdout head", process.GetProperty("stdout").GetString());
        Assert.EndsWith("stdout tail", process.GetProperty("stdout").GetString());
        Assert.StartsWith("stderr head", process.GetProperty("stderr").GetString());
        Assert.EndsWith(traceback, process.GetProperty("stderr").GetString());
        Assert.Contains("characters omitted", process.GetProperty("stderr").GetString());
        Assert.DoesNotContain("PRIVATE_MIDDLE_OUTPUT", replay.Messages[1].Content);
        Assert.Equal(nested, preview.RootElement.GetProperty("success").GetBoolean());
        if (nested)
        {
            Assert.Equal(1, process.GetProperty("exitCode").GetInt32());
            Assert.Equal(123, process.GetProperty("durationMs").GetInt32());
        }
        else
        {
            Assert.True(preview.RootElement.GetProperty("timedOut").GetBoolean());
            Assert.Equal("run", preview.RootElement.GetProperty("phase").GetString());
        }
        var metadata = preview.RootElement.GetProperty("processOutputPreview");
        Assert.True(metadata.GetProperty("truncated").GetBoolean());
        Assert.Equal("probe", metadata.GetProperty("toolCallId").GetString());
        Assert.Equal(body.Length, metadata.GetProperty("originalCharacters").GetInt32());
        Assert.Contains("read_tool_result", metadata.GetProperty("readBack").GetString());
        var streams = metadata.GetProperty("streams").EnumerateArray().ToArray();
        Assert.Equal(stdout.Length - 4000, streams[0].GetProperty("omittedCharacters").GetInt32());
        Assert.Equal(stderr.Length, streams[1].GetProperty("originalCharacters").GetInt32());
        Assert.Equal(stderr.Length - 3000, streams[1].GetProperty("omittedCharacters").GetInt32());
        // read_tool_result retrieves Content from the original history by toolCallId.
        Assert.Equal(body, messages.Single(message => message.ToolCallId == "probe").Content);
    }

    [Fact]
    public void SuccessfulProcessPreviewsPreserveMetadataAndBudgetOlderVisibleResults()
    {
        var messages = new List<AgentMessage>();
        foreach (var (id, size) in new[] { ("old", 100_000), ("new", 80_000) })
        {
            messages.Add(new("assistant", ToolCalls: [new(id, "run_engine_python", "{}") ]));
            messages.Add(new("tool", JsonSerializer.Serialize(new { success = true,
                result = new { exitCode = 0, stdout = new string(id[0], size), stderr = "", module = "python.engines.probe", durationMs = 99 } }), ToolCallId: id));
        }

        var replay = AdaptationHistory.ForRequest(messages);

        Assert.Equal(2, replay.OmittedResults);
        Assert.True(AutomaticAdaptationService.VisibleHistoryBytes(replay.Messages) < 12_000);
        foreach (var index in new[] { 1, 3 })
        {
            using var preview = JsonDocument.Parse(replay.Messages[index].Content);
            Assert.True(preview.RootElement.GetProperty("success").GetBoolean());
            var result = preview.RootElement.GetProperty("result");
            Assert.Equal(0, result.GetProperty("exitCode").GetInt32());
            Assert.Equal("python.engines.probe", result.GetProperty("module").GetString());
            Assert.Equal(99, result.GetProperty("durationMs").GetInt32());
            Assert.Equal("", result.GetProperty("stderr").GetString());
            Assert.True(preview.RootElement.GetProperty("processOutputPreview").GetProperty("truncated").GetBoolean());
            Assert.DoesNotContain("previousResultOmitted", replay.Messages[index].Content);
            Assert.True(messages[index].Content.Length > 80_000);
        }
    }

    [Fact]
    public void SmallStreamsAndLargeNonProcessDiagnosticsAndRetrievedChunksStayUnchanged()
    {
        var diagnostic = new string('x', 10_000);
        var messages = new AgentMessage[] {
            new("assistant", ToolCalls: [new("small", "run_engine_python", "{}"), new("validation", "test_adapter", "{}"),
                new("read", "read_file", "{}"), new("recovered", "read_tool_result", "{\"toolCallId\":\"old\"}")]),
            new("tool", "{ \"success\":true, \"result\":{\"exitCode\":0,\"stdout\":\"small\",\"stderr\":\"\"} }", ToolCallId: "small"),
            new("tool", JsonSerializer.Serialize(new { success = false, result = new { diagnostics = diagnostic } }), ToolCallId: "validation"),
            new("tool", JsonSerializer.Serialize(new { success = true, result = new { content = diagnostic } }), ToolCallId: "read"),
            new("tool", JsonSerializer.Serialize(new { success = true, result = new { content = diagnostic, totalCharacters = 100_000 } }), ToolCallId: "recovered")
        };

        var replay = AdaptationHistory.ForRequest(messages);

        Assert.Equal(0, replay.OmittedResults);
        Assert.Equal(messages, replay.Messages);
    }

    [Fact]
    public void OlderLargeResultsStillCompeteForTheReplayBudget()
    {
        var messages = new List<AgentMessage>();
        foreach (var id in new[] { "old", "middle", "new" })
        {
            messages.Add(new("assistant", ToolCalls: [new(id, "read_file", "{}") ]));
            messages.Add(new("tool", JsonSerializer.Serialize(new { success = true, result = new { content = id + new string('x', 40_000) } }), ToolCallId: id));
        }

        var replay = AdaptationHistory.ForRequest(messages);

        Assert.Equal(2, replay.OmittedResults);
        Assert.Contains("previousResultOmitted", replay.Messages[1].Content);
        Assert.Contains("previousResultOmitted", replay.Messages[3].Content);
        Assert.Equal(messages[5], replay.Messages[5]);
        Assert.DoesNotContain("previousResultOmitted", messages[1].Content);
    }
}
