using System.Security.Cryptography;
using System.Text;
using System.Text.Json;

namespace GameL10n.Core.Development;

/// <summary>Small selectors and fingerprints for investigation, without retaining tool bodies or provider payloads.</summary>
internal static class AdaptationTrace
{
    internal static object Call(AgentToolCall call)
    {
        try { return CallFields(call); }
        catch (Exception error) when (error is JsonException or InvalidOperationException)
        { return new { call.Id, argumentFingerprint = Hash(call.Arguments), malformedArguments = true }; }
    }

    private static object CallFields(AgentToolCall call)
    {
        var fields = new Dictionary<string, object?>();
        using var document = JsonDocument.Parse(call.Arguments);
        var args = document.RootElement;
        foreach (var name in new[] { "root", "path", "script", "directory", "format", "engine", "id", "version", "key", "kind", "executable", "toolCallId", "addressKind", "encoding", "releaseRole", "purpose" })
            if (args.TryGetProperty(name, out var value) && value.ValueKind == JsonValueKind.String)
                fields[name] = Limit(value.GetString()!, 240);
        foreach (var name in new[] { "offset", "length", "limit", "instructionLimit", "timeoutSeconds", "discoveryOnly", "checkRoundtrip", "refreshCandidates", "refresh", "canContinueLocally" })
            if (args.TryGetProperty(name, out var value) && value.ValueKind is JsonValueKind.Number or JsonValueKind.True or JsonValueKind.False)
                fields[name] = value.Clone();
        if (args.TryGetProperty("url", out var url) && url.ValueKind == JsonValueKind.String
            && Uri.TryCreate(url.GetString(), UriKind.Absolute, out var uri) && uri.Scheme is "http" or "https")
            fields["url"] = Limit(new UriBuilder(uri) { UserName = "", Password = "", Query = "", Fragment = "" }.Uri.AbsoluteUri, 500);
        foreach (var name in new[] { "query", "content", "oldText", "newText", "stdin", "evidence" })
            if (args.TryGetProperty(name, out var value) && value.ValueKind == JsonValueKind.String)
                fields[name + "Bytes"] = Encoding.UTF8.GetByteCount(value.GetString()!);
        if (call.Name == "inspect_unity_schema" && args.TryGetProperty("query", out var typeQuery) && typeQuery.ValueKind == JsonValueKind.String)
            fields["typeQuery"] = Limit(typeQuery.GetString()!, 320);
        foreach (var name in new[] { "files", "sourceFiles", "resourcePatterns" })
            if (args.TryGetProperty(name, out var value) && value.ValueKind == JsonValueKind.Array)
                fields[name + "Count"] = value.GetArrayLength();
        if (args.TryGetProperty("resourcePatterns", out var patterns) && patterns.ValueKind == JsonValueKind.Array)
            fields["resourcePatterns"] = patterns.EnumerateArray().Where(item => item.ValueKind == JsonValueKind.String)
                .Take(8).Select(item => Limit(item.GetString()!, 160)).ToArray();
        if (args.TryGetProperty("ranges", out var ranges) && ranges.ValueKind == JsonValueKind.Array)
            fields["ranges"] = ranges.EnumerateArray().Where(item => item.ValueKind == JsonValueKind.Object).Take(8)
                .Select(item => new { address = item.TryGetProperty("address", out var address) && address.ValueKind == JsonValueKind.String
                    ? Limit(address.GetString()!, 32) : null,
                    length = item.TryGetProperty("length", out var length) && length.TryGetInt32(out var count) ? count : (int?)null }).ToArray();
        return new { call.Id, argumentFingerprint = Hash(call.Arguments), selectors = fields };
    }

    internal static object Result(string output)
    {
        var fields = new Dictionary<string, object?>();
        using var document = JsonDocument.Parse(output);
        foreach (var name in new[] { "phase", "timedOut" })
            if (document.RootElement.TryGetProperty(name, out var value) && value.ValueKind is JsonValueKind.String or JsonValueKind.True or JsonValueKind.False)
                fields[name] = value.ValueKind == JsonValueKind.String ? Limit(value.GetString()!, 160) : value.Clone();
        if (document.RootElement.TryGetProperty("result", out var result) && result.ValueKind == JsonValueKind.Object)
        {
            foreach (var name in new[] { "passed", "stage", "compiled", "changed", "reused", "stale", "unitCount", "status", "stopped", "more", "nextOffset", "totalCharacters", "queryFound", "totalMatches", "failedRanges", "compileMs", "discoveryMs", "verificationMs", "encoding", "bytes", "sha256", "releaseRegistered" })
                if (result.TryGetProperty(name, out var value) && value.ValueKind is JsonValueKind.True or JsonValueKind.False or JsonValueKind.Number or JsonValueKind.String)
                    fields[name] = value.ValueKind == JsonValueKind.String ? Limit(value.GetString()!, 160) : value.Clone();
            if (result.TryGetProperty("matches", out var matches) && matches.ValueKind == JsonValueKind.Array)
                fields["matches"] = matches.GetArrayLength();
        }
        return fields;
    }

    internal static string Hash(string text) => Convert.ToHexStringLower(SHA256.HashData(Encoding.UTF8.GetBytes(text)))[..24];
    private static string Limit(string text, int length) => text[..Math.Min(text.Length, length)];
}
