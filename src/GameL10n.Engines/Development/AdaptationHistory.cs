using System.Text;
using System.Text.Encodings.Web;
using System.Text.Json;
using System.Text.Json.Nodes;
using GameL10n.Core.Serialization;

namespace GameL10n.Core.Development;

/// <summary>Bounds replayed tool output without altering the persisted history or provider reasoning.</summary>
internal static class AdaptationHistory
{
    private static readonly JsonSerializerOptions Json = new(EngineJson.Web)
        { Encoder = JavaScriptEncoder.UnsafeRelaxedJsonEscaping };

    internal static AgentMessage[] RecentExchanges(IReadOnlyList<AgentMessage> history, int byteBudget)
    {
        // Retain complete assistant/tool groups, including the newest one. Never split a batch
        // or edit provider reasoning/call arguments to fit the budget.
        var start = history.Count;
        for (var i = history.Count - 1; i >= 0; i--)
        {
            if (history[i].Role == "user" && start < history.Count) break;
            if (history[i].Role != "assistant") continue;
            if (AutomaticAdaptationService.VisibleHistoryBytes(history.Skip(i)) > byteBudget) break;
            start = i;
        }
        return history.Skip(start).ToArray();
    }

    internal static string ArchiveIndex(IReadOnlyList<AgentMessage> history, int start) => JsonSerializer.Serialize(
        history.Select((message, index) => (message, index)).Skip(start)
            .Where(item => item.message.Role != "user").TakeLast(16).Select(item => new JsonObject
            {
                ["messageIndex"] = item.index,
                ["role"] = item.message.Role,
                ["toolCallId"] = item.message.ToolCallId,
                ["preview"] = Preview(ReadableMessage(item.message))
            }), EngineJson.TypeInfo<IEnumerable<JsonObject>>(Json));

    internal static string ReadableMessage(AgentMessage message)
    {
        var text = new StringBuilder(message.Content);
        if (message.ToolCalls is { Count: > 0 }) text.AppendLine().Append(JsonSerializer.Serialize(message.ToolCalls, EngineJson.TypeInfo<IReadOnlyList<AgentToolCall>>(Json)));
        if (message.ProviderData is { } data) Append(data);
        return text.ToString();

        void Append(JsonElement value)
        {
            switch (value.ValueKind)
            {
                case JsonValueKind.Object:
                    foreach (var property in value.EnumerateObject())
                    {
                        if (property.Name == "encrypted_content") continue;
                        text.AppendLine().Append(property.Name).Append(':');
                        Append(property.Value);
                    }
                    break;
                case JsonValueKind.Array:
                    foreach (var item in value.EnumerateArray()) Append(item);
                    break;
                case JsonValueKind.String:
                    text.AppendLine().Append(value.GetString());
                    break;
                default:
                    text.Append(value.GetRawText());
                    break;
            }
        }
    }

    private static string Preview(string text) => text[..Math.Min(512, text.Length)];

    public static (IReadOnlyList<AgentMessage> Messages, int OmittedResults, long ProcessOutputBytesSaved) ForRequest(IReadOnlyList<AgentMessage> history)
    {
        // Keep the newest exchange, with bounded process streams. Older useful results compete
        // for a byte budget rather than vanishing merely because small turns have passed.
        var recentStart = 0;
        for (var i = history.Count - 1; i >= 0; i--)
            if (history[i].Role == "assistant") { recentStart = i; break; }

        var messages = history.ToArray();
        var omitted = 0;
        long processOutputBytesSaved = 0;
        var remaining = 64 * 1024;
        var seen = new Dictionary<string, string>(StringComparer.Ordinal);
        var calls = HistoryCalls(history);
        var laterWrites = new Dictionary<string, string>(StringComparer.OrdinalIgnoreCase);
        for (var i = history.Count - 1; i >= 0; i--)
        {
            var message = history[i];
            if (message.Role != "tool" || message.ToolCallId is null) continue;
            using var document = ParseResult(message.Content);
            var root = document?.RootElement ?? default;
            var successful = IsSuccessful(root);
            calls.TryGetValue(message.ToolCallId, out var call);
            if (successful && call is { Name: "write_file" or "replace_text" } && ChangedFile(root))
                laterWrites.TryAdd(call.Path, message.ToolCallId);
            var bytes = Encoding.UTF8.GetByteCount(message.Content);
            var content = call?.Name == "read_tool_result" ? message.Content : ProcessPreview(root, message);
            var previewed = !ReferenceEquals(content, message.Content);
            var visibleBytes = previewed ? Encoding.UTF8.GetByteCount(content) : bytes;
            processOutputBytesSaved += Math.Max(0, bytes - visibleBytes);
            var fingerprint = bytes > 2048 ? AdaptationTrace.Hash(message.Content) : null;
            string? repeated = null;
            if (fingerprint is not null) seen.TryGetValue(fingerprint, out repeated);
            string? supersededBy = null;
            if (successful && call is { Name: "read_file", Root: "workspace" })
                laterWrites.TryGetValue(call.Path, out supersededBy);
            // Never hide a new/distinct failure. Exact repeats can point to the latest full
            // diagnostic; old source reads are explicitly stale after a committed edit.
            if (i < recentStart && bytes > 2048 && (successful && visibleBytes > remaining || repeated is not null || supersededBy is not null))
            {
                messages[i] = message with { Content = JsonSerializer.Serialize(new JsonObject
                {
                    ["success"] = successful,
                    ["previousResultOmitted"] = true,
                    ["toolCallId"] = message.ToolCallId,
                    ["originalCharacters"] = message.Content.Length,
                    ["identicalTo"] = repeated,
                    ["supersededBy"] = supersededBy,
                    ["preview"] = message.Content[..Math.Min(384, message.Content.Length)],
                    ["readBack"] = supersededBy is not null
                        ? "This file was edited later. Read current file ranges for current code; read_tool_result retrieves this OLD snapshot only."
                        : repeated is not null
                            ? "Exact repeat of identicalTo; use that retained result. read_tool_result retrieves this original without rerunning the operation."
                            : "read_tool_result(toolCallId, offset, length) reads the retained original result. Omitted is not missing or failed."
                }, EngineJson.TypeInfo<JsonObject>(EngineJson.Default)) };
                omitted++;
            }
            else
            {
                if (previewed)
                {
                    messages[i] = message with { Content = content };
                    omitted++;
                }
                if (successful) remaining -= visibleBytes;
                if (fingerprint is not null) seen.TryAdd(fingerprint, message.ToolCallId);
            }
        }
        return (messages, omitted, processOutputBytesSaved);
    }

    private sealed record HistoryCall(string Name, string Root, string Path);

    // Read only selectors, retaining provider output (including reasoning and call arguments)
    // verbatim. Both API protocols persist their original assistant response in ProviderData.
    private static Dictionary<string, HistoryCall> HistoryCalls(IReadOnlyList<AgentMessage> history)
    {
        var result = new Dictionary<string, HistoryCall>(StringComparer.Ordinal);
        foreach (var message in history.Where(message => message.Role == "assistant"))
        {
            foreach (var call in message.ToolCalls ?? []) Add(call.Id, call.Name, call.Arguments);
            if (message.ProviderData is not { } data) continue;
            if (data.ValueKind == JsonValueKind.Array)
            {
                foreach (var item in data.EnumerateArray())
                    if (String(item, "type") == "function_call")
                        Add(String(item, "call_id"), String(item, "name"), String(item, "arguments"));
            }
            else if (data.ValueKind == JsonValueKind.Object && data.TryGetProperty("tool_calls", out var calls)
                && calls.ValueKind == JsonValueKind.Array)
                foreach (var call in calls.EnumerateArray())
                    if (call.ValueKind == JsonValueKind.Object && call.TryGetProperty("function", out var function))
                        Add(String(call, "id"), String(function, "name"), String(function, "arguments"));
        }
        return result;

        void Add(string? id, string? name, string? arguments)
        {
            if (id is null) return;
            if (name == "read_tool_result")
            {
                result[id] = new(name, "", "");
                return;
            }
            if (arguments is null || name is not ("read_file" or "write_file" or "replace_text")) return;
            try
            {
                using var document = JsonDocument.Parse(arguments);
                var path = String(document.RootElement, "path")?.Replace('\\', '/');
                if (string.IsNullOrWhiteSpace(path)) return;
                var parts = path.Split('/').Where(part => part is not ("" or ".")).ToArray();
                if (parts.Contains("..")) return;
                result[id] = new(name, String(document.RootElement, "root") ?? "workspace", string.Join('/', parts));
            }
            catch (JsonException) { /* Malformed history is not evidence that a file changed. */ }
        }
    }

    private static string? String(JsonElement element, string name) => element.ValueKind == JsonValueKind.Object
        && element.TryGetProperty(name, out var value) && value.ValueKind == JsonValueKind.String ? value.GetString() : null;

    private static JsonDocument? ParseResult(string content)
    {
        try { return JsonDocument.Parse(content); }
        catch (JsonException) { return null; }
    }

    private static bool ChangedFile(JsonElement root) => root.ValueKind == JsonValueKind.Object
        && root.TryGetProperty("result", out var result) && result.ValueKind == JsonValueKind.Object
        && result.TryGetProperty("changed", out var changed) && changed.ValueKind == JsonValueKind.True;

    private static bool IsSuccessful(JsonElement root)
    {
        if (root.ValueKind != JsonValueKind.Object) return false;
        if (!root.TryGetProperty("success", out var success) || success.ValueKind != JsonValueKind.True) return false;
        if (root.TryGetProperty("result", out var result) && result.ValueKind == JsonValueKind.Object)
        {
            if (result.TryGetProperty("exitCode", out var exit) && exit.ValueKind == JsonValueKind.Number && exit.GetInt32() != 0) return false;
            if (result.TryGetProperty("passed", out var passed) && passed.ValueKind == JsonValueKind.False) return false;
            if (result.TryGetProperty("compiled", out var compiled) && compiled.ValueKind == JsonValueKind.False) return false;
            if (result.TryGetProperty("status", out var status) && status.ValueKind == JsonValueKind.String
                && status.GetString() is "failed" or "rejected" or "no-text" or "limit-exceeded") return false;
            if (result.TryGetProperty("error", out var error) && error.ValueKind is not (JsonValueKind.Null or JsonValueKind.Undefined)) return false;
        }
        return true;
    }

    private sealed record StreamPreview(string Name, bool InResult, string Content, int OriginalCharacters, int OmittedCharacters);

    private static string ProcessPreview(JsonElement root, AgentMessage message)
    {
        if (root.ValueKind != JsonValueKind.Object) return message.Content;
        var streams = new List<StreamPreview>();
        AddStreams(root, inResult: false);
        if (root.TryGetProperty("result", out var result) && result.ValueKind == JsonValueKind.Object)
            AddStreams(result, inResult: true);
        if (streams.Count == 0) return message.Content;

        // Write directly from the parsed result: cloning a JsonNode would parse large output twice.
        using var buffer = new MemoryStream();
        using (var writer = new Utf8JsonWriter(buffer, new JsonWriterOptions { Encoder = Json.Encoder }))
        {
            writer.WriteStartObject();
            foreach (var property in root.EnumerateObject())
            {
                if (property.Name == "result" && property.Value.ValueKind == JsonValueKind.Object)
                {
                    writer.WritePropertyName(property.Name);
                    writer.WriteStartObject();
                    foreach (var item in property.Value.EnumerateObject()) WriteProperty(writer, item, inResult: true);
                    writer.WriteEndObject();
                }
                else WriteProperty(writer, property, inResult: false);
            }
            writer.WritePropertyName("processOutputPreview");
            JsonSerializer.Serialize(writer, new JsonObject
            {
                ["truncated"] = true,
                ["toolCallId"] = message.ToolCallId,
                ["originalCharacters"] = message.Content.Length,
                ["streams"] = new JsonArray(streams.Select(stream => (JsonNode)new JsonObject
                {
                    ["path"] = (stream.InResult ? "result." : "") + stream.Name,
                    ["originalCharacters"] = stream.OriginalCharacters,
                    ["omittedCharacters"] = stream.OmittedCharacters
                }).ToArray()),
                ["readBack"] = "read_tool_result(toolCallId, offset, length) reads the retained original JSON, including omitted output; query can locate a diagnostic without rerunning the process."
            }, EngineJson.TypeInfo<JsonObject>(Json));
            writer.WriteEndObject();
        }
        return Encoding.UTF8.GetString(buffer.GetBuffer(), 0, (int)buffer.Length);

        void AddStreams(JsonElement value, bool inResult)
        {
            foreach (var (name, limit, head) in new[] { ("stdout", 4000, 2000), ("stderr", 3000, 1000) })
            {
                if (!value.TryGetProperty(name, out var field) || field.ValueKind != JsonValueKind.String) continue;
                var text = field.GetString()!;
                if (text.Length <= limit) continue;
                var omitted = text.Length - limit;
                streams.Add(new(name, inResult,
                    text[..head] + $"\n[... {omitted} characters omitted ...]\n" + text[^(limit - head)..], text.Length, omitted));
            }
        }

        void WriteProperty(Utf8JsonWriter writer, JsonProperty property, bool inResult)
        {
            var preview = streams.FirstOrDefault(stream => stream.Name == property.Name && stream.InResult == inResult);
            if (preview is null) property.WriteTo(writer);
            else writer.WriteString(property.Name, preview.Content);
        }
    }
}
