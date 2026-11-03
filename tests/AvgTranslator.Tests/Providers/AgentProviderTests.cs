using System.Net;
using System.Text;
using System.Text.Json;
using AvgTranslator.Core;
using AvgTranslator.Core.Development;
using AvgTranslator.Core.Providers;

namespace AvgTranslator.Tests.Providers;

public sealed class AgentProviderTests
{
    private static readonly AgentToolDefinition[] Tools =
    [
        new("read_file", "Read one workspace file", JsonSerializer.SerializeToElement(new
        {
            type = "object", properties = new { path = new { type = "string" } }, required = new[] { "path" }, additionalProperties = false
        })),
        new("write_file", "Write one workspace file", JsonSerializer.SerializeToElement(new
        {
            type = "object", properties = new { path = new { type = "string" }, content = new { type = "string" } },
            required = new[] { "path", "content" }, additionalProperties = false
        }))
    ];

    [Theory]
    [InlineData(ProviderApiProtocol.ChatCompletions)]
    [InlineData(ProviderApiProtocol.Responses)]
    public async Task ContinuesTwoToolRoundsWithOriginalAssistantReasoningAndToolResults(ProviderApiProtocol protocol)
    {
        var history = new List<AgentMessage> { new("user", "Inspect and adapt the sample.") };
        JsonElement[] originalOutputs = new JsonElement[2];
        int call = 0;
        var handler = new Handler(async request =>
        {
            int round = call++;
            Assert.EndsWith(protocol == ProviderApiProtocol.Responses ? "/responses" : "/chat/completions", request.RequestUri!.AbsoluteUri);
            using var body = JsonDocument.Parse(await request.Content!.ReadAsStringAsync());
            var root = body.RootElement;
            Assert.Equal("chosen-model", root.GetProperty("model").GetString());
            Assert.True(root.GetProperty("parallel_tool_calls").GetBoolean());
            Assert.Equal(round < 2 ? 1 : 2, root.GetProperty("tools").GetArrayLength());
            Assert.Equal("function", root.GetProperty("tools")[0].GetProperty("type").GetString());
            var definition = protocol == ProviderApiProtocol.Responses
                ? root.GetProperty("tools")[0] : root.GetProperty("tools")[0].GetProperty("function");
            Assert.Equal("read_file", definition.GetProperty("name").GetString());
            Assert.False(definition.GetProperty("strict").GetBoolean());
            Assert.True(JsonElement.DeepEquals(Tools[0].Parameters, definition.GetProperty("parameters")));
            if (protocol == ProviderApiProtocol.Responses)
            {
                Assert.False(root.GetProperty("store").GetBoolean());
                Assert.Equal("reasoning.encrypted_content", root.GetProperty("include")[0].GetString());
                var input = root.GetProperty("input");
                Assert.Equal("user", input[0].GetProperty("role").GetString());
                for (int previous = 0; previous < round; previous++)
                {
                    int index = 1 + previous * 3;
                    Assert.True(JsonElement.DeepEquals(originalOutputs[previous][0], input[index]));
                    Assert.True(JsonElement.DeepEquals(originalOutputs[previous][1], input[index + 1]));
                    Assert.Equal("function_call_output", input[index + 2].GetProperty("type").GetString());
                    Assert.Equal("call-" + previous, input[index + 2].GetProperty("call_id").GetString());
                    Assert.Equal("result-" + previous, input[index + 2].GetProperty("output").GetString());
                }
            }
            else
            {
                var messages = root.GetProperty("messages");
                Assert.Equal("system", messages[0].GetProperty("role").GetString());
                for (int previous = 0; previous < round; previous++)
                {
                    int index = 2 + previous * 2;
                    Assert.True(JsonElement.DeepEquals(originalOutputs[previous], messages[index]));
                    Assert.Equal("tool", messages[index + 1].GetProperty("role").GetString());
                    Assert.Equal("call-" + previous, messages[index + 1].GetProperty("tool_call_id").GetString());
                    Assert.Equal("result-" + previous, messages[index + 1].GetProperty("content").GetString());
                }
            }
            object envelope = round < 2 ? ToolEnvelope(protocol, "call-" + round,
                round == 0 ? "read_file" : "write_file", round == 0 ? "{\"path\":\"sample.bin\"}" : "{\"path\":\"adapter.cs\",\"content\":\"class Adapter {}\"}")
                : TextEnvelope(protocol, "Adapter validated.");
            if (round < 2)
            {
                var document = JsonSerializer.SerializeToElement(envelope);
                originalOutputs[round] = protocol == ProviderApiProtocol.Responses ? document.GetProperty("output") : document.GetProperty("choices")[0].GetProperty("message");
            }
            return Response(envelope);
        });
        using var client = new HttpClient(handler);
        var provider = Provider(client, protocol);
        var knownTools = Tools.Select(tool => tool.Name).ToHashSet(StringComparer.Ordinal);
        for (int round = 0; round < 2; round++)
        {
            // The second response names a registered tool whose schema is omitted from this request.
            var turn = await provider.NextAsync(new("Use workspace tools.", history, [Tools[0]], 2048, knownTools), CancellationToken.None);
            var tool = Assert.Single(turn.Message.ToolCalls!);
            Assert.Equal("call-" + round, tool.Id);
            Assert.Equal(new TokenUsage(11, 7), turn.Usage);
            Assert.NotNull(turn.Message.ProviderData);
            history.Add(turn.Message);
            history.Add(new("tool", "result-" + round, ToolCallId: tool.Id));
        }
        var final = await provider.NextAsync(new("Use workspace tools.", history, Tools, 2048), CancellationToken.None);
        Assert.Equal("Adapter validated.", final.Message.Content);
        Assert.Empty(final.Message.ToolCalls!);
        Assert.Equal(3, handler.Calls);
    }

    [Theory]
    [InlineData(ProviderApiProtocol.ChatCompletions)]
    [InlineData(ProviderApiProtocol.Responses)]
    public async Task MultipleCallsReplayBothResultsAndOriginalReasoning(ProviderApiProtocol protocol)
    {
        var envelope = System.Text.Json.Nodes.JsonNode.Parse(JsonSerializer.Serialize(ToolEnvelope(protocol)))!;
        var calls = protocol == ProviderApiProtocol.Responses ? envelope["output"]!.AsArray()
            : envelope["choices"]![0]!["message"]!["tool_calls"]!.AsArray();
        var second = calls.Last()!.DeepClone();
        second[protocol == ProviderApiProtocol.Responses ? "call_id" : "id"] = "call-second";
        if (protocol == ProviderApiProtocol.Responses) second["id"] = "function-second";
        calls.Add(second);
        var round = 0;
        using var client = new HttpClient(new Handler(async request =>
        {
            if (round++ == 0) return Response(envelope);
            using var body = JsonDocument.Parse(await request.Content!.ReadAsStringAsync());
            var items = body.RootElement.GetProperty(protocol == ProviderApiProtocol.Responses ? "input" : "messages");
            var results = items.EnumerateArray().Where(item => protocol == ProviderApiProtocol.Responses
                ? item.TryGetProperty("type", out var type) && type.GetString() == "function_call_output"
                : item.GetProperty("role").GetString() == "tool").ToArray();
            Assert.Equal(2, results.Length);
            Assert.Equal("call-second", results[1].GetProperty(protocol == ProviderApiProtocol.Responses ? "call_id" : "tool_call_id").GetString());
            Assert.Contains("opaque-", items.GetRawText());
            return Response(TextEnvelope(protocol, "continue"));
        }));
        var provider = Provider(client, protocol);
        var turn = await provider.NextAsync(Request, default);
        Assert.Equal(2, turn.Message.ToolCalls!.Count);
        var history = new List<AgentMessage> { new("user", "read"), turn.Message };
        history.AddRange(turn.Message.ToolCalls.Select(call => new AgentMessage("tool", "result", ToolCallId: call.Id)));
        await provider.NextAsync(new("Read samples.", history, Tools, 2048), default);
        Assert.Equal(2, round);
    }

    [Theory]
    [InlineData(ProviderApiProtocol.ChatCompletions)]
    [InlineData(ProviderApiProtocol.Responses)]
    public async Task RejectsMalformedFunctionCallsWithoutGuessingFromTextAndRetainsUsage(ProviderApiProtocol protocol)
    {
        foreach (var (id, name, arguments) in new (string?, string?, string?)[]
        {
            (null, "read_file", "{}"), ("", "read_file", "{}"), ("invalid id", "read_file", "{}"),
            ("call-0", null, "{}"), ("call-0", "unknown_tool", "{}"), ("call-0", "read_file", null),
            ("call-0", "read_file", "{\"path\":"), ("call-0", "read_file", "[]"),
            ("call-0", "read_file", "{\"path\":\"a\",\"path\":\"b\"}")
        })
        {
            var handler = new Handler(_ => Task.FromResult(Response(ToolEnvelope(protocol, id, name, arguments))));
            using var client = new HttpClient(handler);
            var error = await Assert.ThrowsAsync<TranslationProviderException>(() => Provider(client, protocol).NextAsync(Request, CancellationToken.None));
            Assert.Equal("agent-tool-format", error.Diagnostic!.Code);
            Assert.Equal(new TokenUsage(11, 7), error.Usage);
            Assert.Null(error.Diagnostic.ResponsePreview);
            Assert.Equal(1, handler.Calls);
        }
        using var textClient = new HttpClient(new Handler(_ => Task.FromResult(Response(TextEnvelope(protocol,
            "{\"tool\":\"read_file\",\"arguments\":{\"path\":\"a\"}}")))));
        var turn = await Provider(textClient, protocol).NextAsync(Request, CancellationToken.None);
        Assert.Empty(turn.Message.ToolCalls!);
        Assert.Contains("read_file", turn.Message.Content);
    }

    [Theory]
    [InlineData(ProviderApiProtocol.ChatCompletions)]
    [InlineData(ProviderApiProtocol.Responses)]
    public async Task RejectsIncompleteToolOutputAndPreservesKnownUsage(ProviderApiProtocol protocol)
    {
        object body = protocol == ProviderApiProtocol.Responses
            ? new { status = "incomplete", incomplete_details = new { reason = "max_output_tokens" }, output = Array.Empty<object>(), usage = new { input_tokens = 11, output_tokens = 7 } }
            : new { choices = new[] { new { finish_reason = "length", message = new { role = "assistant", content = "{\"path\":" } } }, usage = new { prompt_tokens = 11, completion_tokens = 7 } };
        using var client = new HttpClient(new Handler(_ => Task.FromResult(Response(body))));
        var error = await Assert.ThrowsAsync<TranslationProviderException>(() => Provider(client, protocol).NextAsync(Request, CancellationToken.None));
        Assert.Equal("output-token-limit", error.Diagnostic!.Code);
        Assert.Equal(new TokenUsage(11, 7), error.Usage);
    }

    [Theory]
    [InlineData(ProviderApiProtocol.ChatCompletions)]
    [InlineData(ProviderApiProtocol.Responses)]
    public async Task TruncationDiagnosticsDescribeBudgetAndToolWithoutRetainingArguments(ProviderApiProtocol protocol)
    {
        string directory = Path.Combine(Path.GetTempPath(), "avg-agent-diagnostic-" + Guid.NewGuid().ToString("N"));
        try
        {
            var envelope = JsonSerializer.SerializeToNode(ToolEnvelope(protocol, name: "write_file",
                arguments: "{\"content\":\"private-key unfinished-source"))!;
            if (protocol == ProviderApiProtocol.Responses)
            {
                envelope["status"] = "incomplete";
                envelope["incomplete_details"] = JsonSerializer.SerializeToNode(new { reason = "max_output_tokens" });
                envelope["usage"]!["output_tokens_details"] = JsonSerializer.SerializeToNode(new { reasoning_tokens = 5 });
            }
            else
            {
                envelope["choices"]![0]!["finish_reason"] = "length";
                envelope["usage"]!["completion_tokens_details"] = JsonSerializer.SerializeToNode(new { reasoning_tokens = 5 });
            }
            using var client = new HttpClient(new Handler(_ => Task.FromResult(Response(envelope))));
            var error = await Assert.ThrowsAsync<TranslationProviderException>(() =>
                Provider(client, protocol, directory).NextAsync(Request, default));
            Assert.Contains("单次输出上限", error.AnalysisFailureSummary);
            Assert.DoesNotContain("每批文本", error.AnalysisFailureSummary);
            using var log = JsonDocument.Parse(await File.ReadAllTextAsync(Assert.Single(Directory.GetFiles(directory))));
            var details = log.RootElement.GetProperty("requestDetails");
            Assert.Equal(2048, details.GetProperty("maximumOutputTokens").GetInt32());
            var response = details.GetProperty("response");
            Assert.Equal(5, response.GetProperty("reasoningTokens").GetInt32());
            var tool = Assert.Single(response.GetProperty("tools").EnumerateArray());
            Assert.Equal("write_file", tool.GetProperty("name").GetString());
            Assert.True(tool.GetProperty("argumentBytes").GetInt32() > 0);
            Assert.False(tool.GetProperty("argumentsComplete").GetBoolean());
            Assert.DoesNotContain("unfinished-source", log.RootElement.GetRawText());
            Assert.DoesNotContain("private-key", log.RootElement.GetRawText());
            Assert.DoesNotContain("opaque-", log.RootElement.GetRawText());
        }
        finally { if (Directory.Exists(directory)) Directory.Delete(directory, true); }
    }

    [Fact]
    public async Task ExplainsToolRequestRejectionAndDoesNotLogSuccessfulReasoning()
    {
        string directory = Path.Combine(Path.GetTempPath(), "avg-agent-provider-" + Guid.NewGuid().ToString("N"));
        try
        {
            using var client = new HttpClient(new Handler(_ => Task.FromResult(Response(ToolEnvelope(ProviderApiProtocol.Responses)))));
            await Provider(client, ProviderApiProtocol.Responses, directory).NextAsync(Request, CancellationToken.None);
            Assert.False(Directory.Exists(directory));
            using var rejectedClient = new HttpClient(new Handler(_ => Task.FromResult(new HttpResponseMessage(HttpStatusCode.BadRequest)
                { Content = new StringContent("private-key provider details") })));
            var error = await Assert.ThrowsAsync<TranslationProviderException>(() => Provider(rejectedClient, ProviderApiProtocol.Responses).NextAsync(Request, CancellationToken.None));
            Assert.Contains("support function tools", error.Message);
            Assert.DoesNotContain("private-key", error.ToString());
        }
        finally { if (Directory.Exists(directory)) Directory.Delete(directory, true); }
    }

    [Fact]
    public void IdentitySeparatesModelsAndProtocolsAndExcludesApiKey()
    {
        using var client = new HttpClient(new Handler(_ => throw new InvalidOperationException()));
        var one = Provider(client, ProviderApiProtocol.Responses);
        var changedKey = new OpenAiCompatibleAgentModel(client, new("https://example.invalid/v1", "another-key", "chosen-model", ProviderApiProtocol.Responses));
        var changedModel = new OpenAiCompatibleAgentModel(client, new("https://example.invalid/v1", "private-key", "another-model", ProviderApiProtocol.Responses));
        Assert.Equal(one.Identity, changedKey.Identity);
        Assert.NotEqual(one.Identity, changedModel.Identity);
        Assert.NotEqual(one.Identity, Provider(client, ProviderApiProtocol.ChatCompletions).Identity);
        Assert.DoesNotContain("private-key", one.Identity);
    }

    [Theory]
    [InlineData(ProviderApiProtocol.ChatCompletions, null)]
    [InlineData(ProviderApiProtocol.Responses, null)]
    [InlineData(ProviderApiProtocol.ChatCompletions, "high")]
    [InlineData(ProviderApiProtocol.Responses, "xhigh")]
    public async Task AnalysisEffortUsesProtocolSpecificFieldWithoutChangingSessionIdentity(ProviderApiProtocol protocol, string? effort)
    {
        using var client = new HttpClient(new Handler(async request =>
        {
            using var body = JsonDocument.Parse(await request.Content!.ReadAsStringAsync());
            var root = body.RootElement;
            Assert.Equal(effort is not null && protocol == ProviderApiProtocol.Responses, root.TryGetProperty("reasoning", out var reasoning));
            Assert.Equal(effort is not null && protocol == ProviderApiProtocol.ChatCompletions, root.TryGetProperty("reasoning_effort", out var chatEffort));
            if (effort is not null)
                Assert.Equal(effort, protocol == ProviderApiProtocol.Responses ? reasoning.GetProperty("effort").GetString() : chatEffort.GetString());
            return Response(TextEnvelope(protocol, "Continue investigation."));
        }));
        var provider = new OpenAiCompatibleAgentModel(client, new("https://example.invalid/v1", "private-key", "chosen-model", protocol, effort));
        Assert.Equal(Provider(client, protocol).Identity, provider.Identity);
        await provider.NextAsync(Request, CancellationToken.None);
    }

    [Theory]
    [InlineData(ProviderApiProtocol.ChatCompletions)]
    [InlineData(ProviderApiProtocol.Responses)]
    public async Task ProgressTextAndToolCallArriveTogetherAndProviderHistoryIsUnchanged(ProviderApiProtocol protocol)
    {
        const string progress = "关键突破：已验证字体映射，接下来整合回填。";
        var envelope = JsonSerializer.SerializeToNode(ToolEnvelope(protocol))!;
        if (protocol == ProviderApiProtocol.Responses)
            envelope["output"]!.AsArray().Add(JsonSerializer.SerializeToNode(new
            {
                type = "message", role = "assistant", status = "completed",
                content = new[] { new { type = "output_text", text = progress } }
            }));
        else envelope["choices"]![0]!["message"]!["content"] = progress;
        var handler = new Handler(_ => Task.FromResult(Response(envelope)));
        using var client = new HttpClient(handler);
        var turn = await Provider(client, protocol).NextAsync(Request, CancellationToken.None);
        Assert.Equal(progress, turn.Message.Content);
        Assert.Single(turn.Message.ToolCalls!);
        Assert.Equal(1, handler.Calls);
        var original = JsonSerializer.SerializeToElement(envelope);
        Assert.True(JsonElement.DeepEquals(protocol == ProviderApiProtocol.Responses ? original.GetProperty("output")
            : original.GetProperty("choices")[0].GetProperty("message"), turn.Message.ProviderData!.Value));
    }

    private static readonly AgentRequest Request = new("Use workspace tools.", [new("user", "Inspect the sample.")], Tools, 2048);
    private static OpenAiCompatibleAgentModel Provider(HttpClient client, ProviderApiProtocol protocol, string? directory = null) =>
        new(client, new("https://example.invalid/v1", "private-key", "chosen-model", protocol), directory);

    private static object ToolEnvelope(ProviderApiProtocol protocol, string? id = "call-0", string? name = "read_file", string? arguments = "{\"path\":\"sample.bin\"}") =>
        protocol == ProviderApiProtocol.Responses ? new
        {
            status = "completed", output = new object[]
            {
                new { id = "reasoning-id", type = "reasoning", summary = Array.Empty<object>(), encrypted_content = "opaque-encrypted-reasoning" },
                new { id = "function-id", type = "function_call", status = "completed", call_id = id, name, arguments }
            }, usage = new { input_tokens = 11, output_tokens = 7 }
        } : new
        {
            choices = new[] { new { finish_reason = "tool_calls", message = new
            {
                role = "assistant", content = (string?)null, reasoning_content = "opaque-provider-reasoning",
                tool_calls = new[] { new { id, type = "function", function = new { name, arguments } } }
            } } }, usage = new { prompt_tokens = 11, completion_tokens = 7 }
        };
    private static object TextEnvelope(ProviderApiProtocol protocol, string content) => protocol == ProviderApiProtocol.Responses
        ? new { status = "completed", output = new[] { new { type = "message", role = "assistant", status = "completed", content = new[] { new { type = "output_text", text = content } } } }, usage = new { input_tokens = 11, output_tokens = 7 } }
        : new { choices = new[] { new { finish_reason = "stop", message = new { role = "assistant", content } } }, usage = new { prompt_tokens = 11, completion_tokens = 7 } };
    private static HttpResponseMessage Response(object body) => new(HttpStatusCode.OK)
        { Content = new StringContent(JsonSerializer.Serialize(body), Encoding.UTF8, "application/json") };
    private sealed class Handler(Func<HttpRequestMessage, Task<HttpResponseMessage>> handle) : HttpMessageHandler
    {
        public int Calls { get; private set; }
        protected override Task<HttpResponseMessage> SendAsync(HttpRequestMessage request, CancellationToken cancellationToken)
        { Calls++; return handle(request); }
    }
}
