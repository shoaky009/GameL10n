using System.Text.Json.Serialization;

namespace AvgTranslator.Core.Development;

/// <summary>An optional user-facing observation, never an adapter verification or completion signal.</summary>
public sealed record AdaptationProgressNote(string Kind, string Message, DateTimeOffset UpdatedAt)
{
    private static readonly (string Kind, string Title)[] Kinds =
    [
        ("exploring", "仍在探索"), ("breakthrough", "关键突破"),
        ("implementing", "方案已确定"), ("correction", "进展更正")
    ];

    [JsonIgnore]
    public string Title => Kinds.FirstOrDefault(item => item.Kind == Kind).Title ?? "适配进展";

    public static AdaptationProgressNote? FromAssistantText(string text)
    {
        // Read only an explicitly labelled first line of assistant text. Tool output,
        // reasoning and arbitrary summaries must not become a user-facing progress claim.
        var firstLine = text.AsSpan().TrimStart();
        var end = firstLine.IndexOfAny('\r', '\n');
        if (end >= 0) firstLine = firstLine[..end];
        foreach (var (kind, title) in Kinds)
        {
            if (!firstLine.StartsWith(title, StringComparison.Ordinal)) continue;
            var rest = firstLine[title.Length..];
            if (rest.Length == 0 || rest[0] is not ('：' or ':')) continue;
            var message = rest[1..].Trim();
            if (message.Length is < 1 or > 400) return null;
            return new(kind, message.ToString(), DateTimeOffset.UtcNow);
        }
        return null;
    }
}
