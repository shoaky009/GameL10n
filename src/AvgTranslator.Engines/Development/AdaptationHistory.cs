using System.Text;
using System.Text.Json;

namespace AvgTranslator.Core.Development;

/// <summary>Bounds replayed tool output without altering the persisted history or provider reasoning.</summary>
internal static class AdaptationHistory
{
    public static (IReadOnlyList<AgentMessage> Messages, int OmittedResults) ForRequest(IReadOnlyList<AgentMessage> history)
    {
        // Keep the newest exchange intact. Older useful results compete for a byte budget,
        // rather than vanishing merely because three small model/tool turns have passed.
        var recentStart = 0;
        for (var i = history.Count - 1; i >= 0; i--)
            if (history[i].Role == "assistant") { recentStart = i; break; }

        var messages = history.ToArray();
        var omitted = 0;
        var remaining = 64 * 1024;
        var seen = new Dictionary<string, string>(StringComparer.Ordinal);
        var calls = FileCalls(history);
        var laterWrites = new Dictionary<string, string>(StringComparer.OrdinalIgnoreCase);
        for (var i = history.Count - 1; i >= 0; i--)
        {
            var message = history[i];
            if (message.Role != "tool" || message.ToolCallId is null) continue;
            var successful = IsSuccessful(message.Content);
            calls.TryGetValue(message.ToolCallId, out var call);
            if (successful && call is { Name: "write_file" or "replace_text" } && ChangedFile(message.Content))
                laterWrites.TryAdd(call.Path, message.ToolCallId);
            var bytes = Encoding.UTF8.GetByteCount(message.Content);
            var fingerprint = bytes > 2048 ? AdaptationTrace.Hash(message.Content) : null;
            string? repeated = null;
            if (fingerprint is not null) seen.TryGetValue(fingerprint, out repeated);
            string? supersededBy = null;
            if (successful && call is { Name: "read_file", Root: "workspace" })
                laterWrites.TryGetValue(call.Path, out supersededBy);
            // Never hide a new/distinct failure. Exact repeats can point to the latest full
            // diagnostic; old source reads are explicitly stale after a committed edit.
            if (i < recentStart && bytes > 2048 && (successful && bytes > remaining || repeated is not null || supersededBy is not null))
            {
                messages[i] = message with { Content = JsonSerializer.Serialize(new
                {
                    success = successful, previousResultOmitted = true, toolCallId = message.ToolCallId,
                    originalCharacters = message.Content.Length,
                    identicalTo = repeated,
                    supersededBy,
                    preview = message.Content[..Math.Min(384, message.Content.Length)],
                    readBack = supersededBy is not null
                        ? "This file was edited later. Read current file ranges for current code; read_tool_result retrieves this OLD snapshot only."
                        : repeated is not null
                            ? "Exact repeat of identicalTo; use that retained result. read_tool_result retrieves this original without rerunning the operation."
                            : "read_tool_result(toolCallId, offset, length) reads the retained original result. Omitted is not missing or failed."
                }) };
                omitted++;
            }
            else
            {
                if (successful) remaining -= bytes;
                if (fingerprint is not null) seen.TryAdd(fingerprint, message.ToolCallId);
            }
        }
        return (messages, omitted);
    }

    private sealed record FileCall(string Name, string Root, string Path);

    // Read only selectors, retaining provider output (including reasoning and call arguments)
    // verbatim. Both API protocols persist their original assistant response in ProviderData.
    private static Dictionary<string, FileCall> FileCalls(IReadOnlyList<AgentMessage> history)
    {
        var result = new Dictionary<string, FileCall>(StringComparer.Ordinal);
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
            if (id is null || arguments is null || name is not ("read_file" or "write_file" or "replace_text")) return;
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

    private static bool ChangedFile(string content)
    {
        try
        {
            using var document = JsonDocument.Parse(content);
            return document.RootElement.TryGetProperty("result", out var result) && result.ValueKind == JsonValueKind.Object
                && result.TryGetProperty("changed", out var changed) && changed.ValueKind == JsonValueKind.True;
        }
        catch (JsonException) { return false; }
    }

    private static bool IsSuccessful(string content)
    {
        try
        {
            using var document = JsonDocument.Parse(content);
            var root = document.RootElement;
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
        catch (JsonException) { return false; }
    }
}
