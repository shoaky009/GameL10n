using System.Text;
using System.Text.RegularExpressions;

namespace AvgTranslator.Core.Translation;

/// <summary>Request-local placeholders. Resource syntax and original text never enter the model's editing surface.</summary>
internal sealed class ProtectedTranslationText
{
    private readonly Dictionary<string, string> originals = new(StringComparer.Ordinal);
    private readonly Dictionary<string, string> tokens = new(StringComparer.Ordinal);
    private readonly string prefix;
    public string Text { get; }
    public IReadOnlyDictionary<string, int> Counts { get; }

    public ProtectedTranslationText(string source, string batchPrefix, int unitIndex, IReadOnlyList<string>? protectedTokens = null)
    {
        prefix = batchPrefix;
        if (source.Contains(prefix, StringComparison.Ordinal)) throw new ArgumentException("Placeholder prefix collides with source text.");
        var text = new StringBuilder(source.Length);
        var counts = new Dictionary<string, int>(StringComparer.Ordinal);
        int position = 0;
        foreach (var span in TextValidation.ProtectedSpans(source, protectedTokens))
        {
            text.Append(source, position, span.Start - position);
            var original = source.Substring(span.Start, span.Length);
            if (!tokens.TryGetValue(original, out var token))
            {
                token = prefix + unitIndex + "_" + tokens.Count + "}";
                tokens.Add(original, token);
                originals.Add(token, original);
            }
            text.Append(token);
            counts[token] = counts.GetValueOrDefault(token) + 1;
            position = span.Start + span.Length;
        }
        text.Append(source, position, source.Length - position);
        Text = text.ToString();
        Counts = counts;
    }

    public string EncodeFeedback(string feedback)
    {
        // Feedback refers to the source markers. Present the same simple names as this request.
        foreach (var pair in tokens.OrderByDescending(p => p.Key.Length)) feedback = feedback.Replace(pair.Key, pair.Value, StringComparison.Ordinal);
        return feedback;
    }

    public string Restore(string translated)
    {
        // One pass: restored game text must never be interpreted as another temporary token.
        // Unknown/omitted tokens are not guessed. The pipeline's original-source checks reject
        // missing or additional markers and preserve valid responses from other units.
        return Regex.Replace(translated, Regex.Escape(prefix) + @"\d+_\d+\}",
            match => originals.GetValueOrDefault(match.Value, match.Value), RegexOptions.CultureInvariant);
    }
}
