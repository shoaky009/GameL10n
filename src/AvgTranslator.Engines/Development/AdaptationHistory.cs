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
        for (var i = history.Count - 1; i >= 0; i--)
        {
            var message = history[i];
            if (message.Role != "tool" || message.ToolCallId is null || !IsSuccessful(message.Content)) continue;
            var bytes = Encoding.UTF8.GetByteCount(message.Content);
            var fingerprint = bytes > 2048 ? AdaptationTrace.Hash(message.Content) : null;
            string? repeated = null;
            if (fingerprint is not null) seen.TryGetValue(fingerprint, out repeated);
            if (i < recentStart && bytes > 2048 && (bytes > remaining || repeated is not null))
            {
                messages[i] = message with { Content = JsonSerializer.Serialize(new
                {
                    success = true, previousResultOmitted = true, toolCallId = message.ToolCallId,
                    originalCharacters = message.Content.Length,
                    identicalTo = repeated,
                    preview = message.Content[..Math.Min(384, message.Content.Length)],
                    readBack = "read_tool_result(toolCallId, offset, length) reads the retained original result. Omitted is not missing or failed."
                }) };
                omitted++;
            }
            else
            {
                remaining -= bytes;
                if (fingerprint is not null) seen.TryAdd(fingerprint, message.ToolCallId);
            }
        }
        return (messages, omitted);
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
