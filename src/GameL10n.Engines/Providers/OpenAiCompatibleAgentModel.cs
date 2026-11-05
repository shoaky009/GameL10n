using System.Text;
using System.Text.Json;
using GameL10n.Core.Development;
using GameL10n.Core.Translation;
using System.Text.Json.Nodes;
using GameL10n.Core.Serialization;

namespace GameL10n.Core.Providers;

public sealed class OpenAiCompatibleAgentModel : OpenAiCompatibleClient, IAdaptationAgentModel
{
    private static readonly JsonSerializerOptions AgentJson = new(Json) { TypeInfoResolver = EngineJson.Resolver };

    public OpenAiCompatibleAgentModel(HttpClient httpClient, ProviderSettings settings, string? diagnosticDirectory = null)
        : base(httpClient, settings, diagnosticDirectory) { }

    public string? AnalysisReasoningEffort => _settings.AnalysisReasoningEffort;
    public string? AnalysisModelName => _settings.Model;

    public async Task<AgentTurn> NextAsync(AgentRequest request, CancellationToken cancellationToken)
    {
        ValidateAgentRequest(request);
        byte[] payload = BuildAgentPayload(request);
        var toolNames = request.Tools.Select(tool => tool.Name).ToHashSet(StringComparer.Ordinal);
        if (request.KnownToolNames is not null) toolNames.UnionWith(request.KnownToolNames);
        object? responseDetails = null;
        try
        {
            return await SendAsync(payload, cancellationToken, (root, usage, requestId) =>
            {
                try
                {
                    return _settings.Protocol == ProviderApiProtocol.Responses
                        ? ReadResponsesTurn(root, usage, requestId, toolNames)
                        : ReadChatTurn(root, usage, requestId, toolNames);
                }
                catch (TranslationProviderException)
                {
                    responseDetails = DescribeAgentFailure(root);
                    throw;
                }
            }, requiresTools: true);
        }
        catch (TranslationProviderException error)
        {
            await WriteFailureLogAsync("adaptation", error, new JsonObject
            {
                ["maximumOutputTokens"] = request.MaximumOutputTokens,
                ["reasoningEffort"] = _settings.AnalysisReasoningEffort,
                ["response"] = EngineJson.Node(responseDetails, EngineJson.Web)
            });
            throw;
        }
    }

    private object DescribeAgentFailure(JsonElement root)
    {
        static JsonElement Property(JsonElement value, string name) => value.ValueKind == JsonValueKind.Object
            && value.TryGetProperty(name, out var property) ? property : default;
        static string? String(JsonElement value) => value.ValueKind == JsonValueKind.String ? value.GetString() : null;
        static IEnumerable<JsonElement> Items(JsonElement value) => value.ValueKind == JsonValueKind.Array
            ? value.EnumerateArray() : [];
        var choice = Items(Property(root, "choices")).FirstOrDefault();
        var output = Property(root, "output");
        var calls = _settings.Protocol == ProviderApiProtocol.Responses
            ? Items(output).Where(item => String(Property(item, "type")) == "function_call")
            : Items(Property(Property(choice, "message"), "tool_calls")).Select(item => Property(item, "function"));
        var usage = Property(root, "usage");
        var usageDetails = Property(usage, _settings.Protocol == ProviderApiProtocol.Responses
            ? "output_tokens_details" : "completion_tokens_details");
        return new JsonObject
        {
            ["status"] = String(Property(root, "status")),
            ["finishReason"] = String(Property(choice, "finish_reason")),
            ["incompleteReason"] = String(Property(Property(root, "incomplete_details"), "reason")),
            ["reasoningTokens"] = usageDetails.ValueKind == JsonValueKind.Object ? TokenCount(usageDetails, "reasoning_tokens") : null,
            ["toolCallCount"] = calls.Count(),
            // Metadata only: never retain generated code, arguments or opaque reasoning in failure diagnostics.
            ["tools"] = EngineJson.Array(calls.Take(8).Select(call => new JsonObject
            {
                ["name"] = RedactCredential(Limit(String(Property(call, "name")), 80)),
                ["status"] = String(Property(call, "status")),
                ["argumentBytes"] = Encoding.UTF8.GetByteCount(String(Property(call, "arguments")) ?? ""),
                ["argumentsComplete"] = ValidToolArguments(String(Property(call, "arguments")))
            }).ToArray())
        };
    }

    private void ValidateAgentRequest(AgentRequest request)
    {
        ArgumentNullException.ThrowIfNull(request);
        if (string.IsNullOrWhiteSpace(request.Instructions) || request.Messages is null || request.Tools is null ||
            request.Tools.Count is < 1 or > 128 || request.MaximumOutputTokens <= 0)
            throw new ArgumentException("Agent instructions, messages, function tools, and output budget are required.", nameof(request));
        var names = new HashSet<string>(StringComparer.Ordinal);
        foreach (var tool in request.Tools)
            if (tool is null || !ValidToolName(tool.Name) || !names.Add(tool.Name) || tool.Description is null ||
                tool.Parameters.ValueKind != JsonValueKind.Object)
                throw new ArgumentException("Function tools require unique valid names and JSON object parameter schemas.", nameof(request));
        if (request.KnownToolNames is { } known)
        {
            if (known.Any(name => !ValidToolName(name)))
                throw new ArgumentException("Known function names must be valid tool names.", nameof(request));
            names.UnionWith(known);
        }
        foreach (var message in request.Messages)
        {
            if (message is null || message.Role is not ("user" or "assistant" or "tool") || message.Content is null ||
                (message.Role == "tool" && !ValidToolId(message.ToolCallId)) ||
                (message.Role != "tool" && message.ToolCallId is not null) ||
                (message.Role != "assistant" && (message.ToolCalls is not null || message.ProviderData is not null)))
                throw new ArgumentException("An agent history message has an invalid role, content, or tool reference.", nameof(request));
            if (message.ProviderData is JsonElement data && data.ValueKind !=
                (_settings.Protocol == ProviderApiProtocol.Responses ? JsonValueKind.Array : JsonValueKind.Object))
                throw new ArgumentException("Assistant provider history does not match the selected API protocol.", nameof(request));
            if (message.ToolCalls is not null)
            {
                var ids = new HashSet<string>(StringComparer.Ordinal);
                foreach (var call in message.ToolCalls)
                    if (call is null || !ValidToolId(call.Id) || !ids.Add(call.Id) || !names.Contains(call.Name) ||
                        !ValidToolArguments(call.Arguments))
                        throw new ArgumentException("An assistant history message contains an invalid function call.", nameof(request));
            }
        }
    }

    private byte[] BuildAgentPayload(AgentRequest request)
    {
        var messages = new JsonArray();
        if (_settings.Protocol == ProviderApiProtocol.ChatCompletions)
            messages.Add((JsonNode)new JsonObject
            {
                ["role"] = "system",
                ["content"] = request.Instructions
            });
        foreach (var message in request.Messages)
        {
            if (message.Role == "assistant" && message.ProviderData is JsonElement original)
            {
                if (_settings.Protocol == ProviderApiProtocol.Responses)
                    foreach (var item in original.EnumerateArray()) messages.Add(JsonNode.Parse(item.GetRawText()));
                else messages.Add(JsonNode.Parse(original.GetRawText()));
            }
            else if (message.Role == "tool")
            {
                messages.Add(_settings.Protocol == ProviderApiProtocol.Responses
                    ? (JsonNode)new JsonObject
                    {
                        ["type"] = "function_call_output",
                        ["call_id"] = message.ToolCallId,
                        ["output"] = message.Content
                    }
                    : new JsonObject
                    {
                        ["role"] = "tool",
                        ["tool_call_id"] = message.ToolCallId,
                        ["content"] = message.Content
                    });
            }
            else if (message.Role == "assistant" && message.ToolCalls is { Count: > 0 })
            {
                if (_settings.Protocol == ProviderApiProtocol.Responses)
                {
                    if (!string.IsNullOrEmpty(message.Content)) messages.Add((JsonNode)new JsonObject
                    {
                        ["role"] = "assistant",
                        ["content"] = message.Content
                    });
                    foreach (var call in message.ToolCalls) messages.Add((JsonNode)new JsonObject
                    {
                        ["type"] = "function_call",
                        ["call_id"] = call.Id,
                        ["name"] = call.Name,
                        ["arguments"] = call.Arguments
                    });
                }
                else messages.Add((JsonNode)new JsonObject
                {
                    ["role"] = "assistant",
                    ["content"] = string.IsNullOrEmpty(message.Content) ? null : message.Content,
                    ["tool_calls"] = EngineJson.Array(message.ToolCalls.Select(call => new JsonObject
                    {
                        ["id"] = call.Id,
                        ["type"] = "function",
                        ["function"] = new JsonObject
                        {
                            ["name"] = call.Name,
                            ["arguments"] = call.Arguments
                        }
                    }))
                });
            }
            else messages.Add((JsonNode)new JsonObject
            {
                ["role"] = message.Role,
                ["content"] = message.Content
            });
        }
        var payload = new JsonObject
        {
            ["model"] = _settings.Model, ["tool_choice"] = "auto", ["parallel_tool_calls"] = true, ["stream"] = false
        };
        if (_settings.Protocol == ProviderApiProtocol.Responses)
        {
            payload["instructions"] = request.Instructions;
            payload["input"] = messages;
            payload["tools"] = EngineJson.Array(request.Tools.Select(tool => new JsonObject
            {
                ["type"] = "function",
                ["name"] = tool.Name,
                ["description"] = tool.Description,
                ["parameters"] = EngineJson.Node(tool.Parameters, EngineJson.Web),
                ["strict"] = false
            }));
            payload["max_output_tokens"] = request.MaximumOutputTokens;
            payload["store"] = false;
            payload["include"] = new JsonArray(JsonValue.Create("reasoning.encrypted_content"));
            if (_settings.AnalysisReasoningEffort is { } effort) payload["reasoning"] = new JsonObject
            {
                ["effort"] = effort
            };
        }
        else
        {
            payload["messages"] = messages;
            payload["tools"] = EngineJson.Array(request.Tools.Select(tool => new JsonObject
            {
                ["type"] = "function",
                ["function"] = new JsonObject
                {
                    ["name"] = tool.Name,
                    ["description"] = tool.Description,
                    ["parameters"] = EngineJson.Node(tool.Parameters, EngineJson.Web),
                    ["strict"] = false
                }
            }));
            payload["max_completion_tokens"] = request.MaximumOutputTokens;
            if (_settings.AnalysisReasoningEffort is { } effort) payload["reasoning_effort"] = effort;
        }
        return JsonSerializer.SerializeToUtf8Bytes(payload, EngineJson.TypeInfo<JsonObject>(AgentJson));
    }

    private AgentTurn ReadChatTurn(JsonElement root, TokenUsage usage, string? requestId, HashSet<string> toolNames)
    {
        if (!root.TryGetProperty("choices", out var choices) || choices.ValueKind != JsonValueKind.Array || choices.GetArrayLength() != 1)
            throw ResponseFailure("Expected exactly one Chat Completions choice.", "chat-choices", usage, requestId: requestId);
        var choice = choices[0];
        if (!choice.TryGetProperty("message", out var message) || message.ValueKind != JsonValueKind.Object ||
            !message.TryGetProperty("role", out var role) || role.GetString() != "assistant")
            throw AgentFailure("Chat Completions assistant message is missing or invalid.", usage, requestId);
        if (message.TryGetProperty("refusal", out var refusal) && refusal.ValueKind == JsonValueKind.String &&
            !string.IsNullOrWhiteSpace(refusal.GetString()))
            throw ResponseFailure("The model refused the request.", "model-refusal", usage, refusal.GetString(), requestId);
        string text = "";
        if (message.TryGetProperty("content", out var content) && content.ValueKind != JsonValueKind.Null)
        {
            if (content.ValueKind != JsonValueKind.String) throw AgentFailure("Assistant content is not text.", usage, requestId);
            text = content.GetString()!;
        }
        string? finish = choice.TryGetProperty("finish_reason", out var finishValue) ? finishValue.GetString() : null;
        if (finish is "length" or "content_filter")
            throw ResponseFailure(finish == "length" ? "The model reached its output token limit." : "The provider filtered the response.",
                finish == "length" ? "output-token-limit" : "model-refusal", usage, text, requestId);
        var calls = new List<AgentToolCall>();
        if (message.TryGetProperty("tool_calls", out var toolCalls) && toolCalls.ValueKind != JsonValueKind.Null)
        {
            if (toolCalls.ValueKind != JsonValueKind.Array) throw AgentFailure("Function calls must be an array.", usage, requestId);
            foreach (var item in toolCalls.EnumerateArray())
            {
                if (item.ValueKind != JsonValueKind.Object || !item.TryGetProperty("type", out var type) || type.GetString() != "function" ||
                    !item.TryGetProperty("function", out var function) || function.ValueKind != JsonValueKind.Object)
                    throw AgentFailure("The provider returned an unsupported tool call.", usage, requestId);
                calls.Add(ReadToolCall(item, "id", function, toolNames, usage, requestId));
            }
        }
        if ((calls.Count > 0 && finish != "tool_calls") || (calls.Count == 0 && finish != "stop"))
            throw ResponseFailure("The model did not finish an assistant turn.", "chat-incomplete", usage, requestId: requestId);
        return CompleteAgentTurn(text, calls, message, usage, requestId);
    }

    private AgentTurn ReadResponsesTurn(JsonElement root, TokenUsage usage, string? requestId, HashSet<string> toolNames)
    {
        if (!root.TryGetProperty("status", out var statusValue) || statusValue.ValueKind != JsonValueKind.String)
            throw ResponseFailure("Responses status is missing or invalid.", "responses-status", usage, requestId: requestId);
        string? status = statusValue.GetString();
        if (status != "completed" || (root.TryGetProperty("error", out var error) && error.ValueKind != JsonValueKind.Null))
        {
            string? reason = root.TryGetProperty("incomplete_details", out var details) && details.ValueKind == JsonValueKind.Object &&
                details.TryGetProperty("reason", out var reasonValue) && reasonValue.ValueKind == JsonValueKind.String ? reasonValue.GetString() : null;
            throw ResponseFailure("Responses did not return a completed result.",
                reason == "content_filter" ? "model-refusal" : reason == "max_output_tokens" ? "output-token-limit" : "responses-incomplete",
                usage, requestId: requestId, outcomeUnknown: status is "queued" or "in_progress");
        }
        if (!root.TryGetProperty("output", out var output) || output.ValueKind != JsonValueKind.Array)
            throw ResponseFailure("Responses output is missing or invalid.", "responses-output", usage, requestId: requestId);
        var text = new StringBuilder();
        var calls = new List<AgentToolCall>();
        foreach (var item in output.EnumerateArray())
        {
            if (item.ValueKind != JsonValueKind.Object || !item.TryGetProperty("type", out var type))
                throw AgentFailure("Responses contains an invalid output item.", usage, requestId);
            if (type.GetString() == "reasoning") continue;
            if (type.GetString() == "function_call")
            {
                if (item.TryGetProperty("status", out var callStatus) && callStatus.ValueKind != JsonValueKind.Null && callStatus.GetString() != "completed")
                    throw AgentFailure("Responses contains an incomplete function call.", usage, requestId);
                calls.Add(ReadToolCall(item, "call_id", item, toolNames, usage, requestId));
                continue;
            }
            if (type.GetString() != "message" || !item.TryGetProperty("role", out var role) || role.GetString() != "assistant" ||
                !item.TryGetProperty("status", out var messageStatus) || messageStatus.GetString() != "completed" ||
                !item.TryGetProperty("content", out var content) || content.ValueKind != JsonValueKind.Array)
                throw AgentFailure("Responses contains an unsupported or incomplete output item.", usage, requestId);
            foreach (var part in content.EnumerateArray())
            {
                if (part.ValueKind != JsonValueKind.Object || !part.TryGetProperty("type", out var partType))
                    throw AgentFailure("Responses contains invalid assistant content.", usage, requestId);
                if (partType.GetString() == "refusal")
                    throw ResponseFailure("The model refused the request.", "model-refusal", usage,
                        part.TryGetProperty("refusal", out var refusal) && refusal.ValueKind == JsonValueKind.String ? refusal.GetString() : null, requestId);
                if (partType.GetString() != "output_text" || !part.TryGetProperty("text", out var value) || value.ValueKind != JsonValueKind.String)
                    throw AgentFailure("Responses contains unsupported assistant content.", usage, requestId);
                text.Append(value.GetString());
            }
        }
        return CompleteAgentTurn(text.ToString(), calls, output, usage, requestId);
    }

    private AgentToolCall ReadToolCall(JsonElement item, string idProperty, JsonElement function,
        HashSet<string> toolNames, TokenUsage usage, string? requestId)
    {
        if (!item.TryGetProperty(idProperty, out var id) || id.ValueKind != JsonValueKind.String || !ValidToolId(id.GetString()) ||
            !function.TryGetProperty("name", out var name) || name.ValueKind != JsonValueKind.String || !toolNames.Contains(name.GetString()!) ||
            !function.TryGetProperty("arguments", out var arguments) || arguments.ValueKind != JsonValueKind.String || !ValidToolArguments(arguments.GetString()))
            throw AgentFailure("The provider returned a function call with an invalid ID, name, or JSON object argument string.", usage, requestId);
        return new AgentToolCall(id.GetString()!, name.GetString()!, arguments.GetString()!);
    }

    private AgentTurn CompleteAgentTurn(string text, List<AgentToolCall> calls, JsonElement original, TokenUsage usage, string? requestId)
    {
        if (Encoding.UTF8.GetByteCount(text) > MaximumContentBytes || calls.Count > 128)
            throw ResponseFailure("The assistant turn exceeds the output size limit.", "response-size-limit", usage, requestId: requestId);
        if (calls.Select(call => call.Id).Distinct(StringComparer.Ordinal).Count() != calls.Count)
            throw AgentFailure("The provider returned duplicate function-call IDs.", usage, requestId);
        if (calls.Count == 0 && string.IsNullOrWhiteSpace(text))
            throw ResponseFailure("The model returned neither function calls nor text.", "empty-content", usage, requestId: requestId);
        if (ModelRefusal.IsPlainText(text))
            throw ResponseFailure("The model refused the request.", "model-refusal", usage, text, requestId);
        return new AgentTurn(new AgentMessage("assistant", text, calls, ProviderData: original.Clone()), usage);
    }

    private TranslationProviderException AgentFailure(string detail, TokenUsage usage, string? requestId) =>
        ResponseFailure("The provider returned an invalid tool-calling response. Check function-tool support for the selected model and protocol.",
            "agent-tool-format", usage, requestId: requestId, detail: detail);

    private static bool ValidToolName(string? name) => name is { Length: > 0 and <= 64 } &&
        name.All(c => char.IsAsciiLetterOrDigit(c) || c is '_' or '-');
    private static bool ValidToolId(string? id) => id is { Length: > 0 and <= 512 } && !id.Any(c => char.IsControl(c) || char.IsWhiteSpace(c));

    private static bool ValidToolArguments(string? arguments)
    {
        if (string.IsNullOrWhiteSpace(arguments) || Encoding.UTF8.GetByteCount(arguments) > MaximumContentBytes) return false;
        try
        {
            using var document = JsonDocument.Parse(arguments, new JsonDocumentOptions { MaxDepth = 32 });
            return document.RootElement.ValueKind == JsonValueKind.Object && UniqueProperties(document.RootElement);
        }
        catch (JsonException) { return false; }

        static bool UniqueProperties(JsonElement value)
        {
            if (value.ValueKind == JsonValueKind.Object)
            {
                var names = new HashSet<string>(StringComparer.Ordinal);
                return value.EnumerateObject().All(property => names.Add(property.Name) && UniqueProperties(property.Value));
            }
            return value.ValueKind != JsonValueKind.Array || value.EnumerateArray().All(UniqueProperties);
        }
    }
}
