using System.Text;
using System.Text.RegularExpressions;
using GameL10n.Core.Storage;

namespace GameL10n.Core.Adapters;

internal static partial class Sfa16Script
{
    public static Encoding ShiftJis { get; } = CreateEncoding(932);
    public static Encoding Chinese { get; } = CreateEncoding(936);
    private static readonly HashSet<string> BareCommands = new(StringComparer.Ordinal)
    {
        "bgoff", "wndoff", "cgoff", "pause", "savedisable", "btnenable", "loadenable",
        "seoff", "backtitle", "loaddisable", "bgmoff", "wbl", "saveenable", "exit"
    };

    private static Encoding CreateEncoding(int codePage)
    {
        Encoding.RegisterProvider(CodePagesEncodingProvider.Instance);
        return Encoding.GetEncoding(codePage, EncoderFallback.ExceptionFallback, DecoderFallback.ExceptionFallback);
    }

    public static IReadOnlyList<TextSpan> Extract(string resource, byte[] data, Encoding encoding)
    {
        var spans = new List<TextSpan>();
        var lineNumber = 0;
        for (var start = 0; start < data.Length;)
        {
            lineNumber++;
            var end = start;
            while (end < data.Length && data[end] != 10 && data[end] != 13) end++;
            ExtractLine(resource, data, start, end, lineNumber, encoding, spans);
            start = end;
            if (start < data.Length && data[start] == 13) start++;
            if (start < data.Length && data[start] == 10) start++;
        }
        // Line numbers are locators, not identity or cache context: inserting one line must
        // not invalidate the rest of a script. Duplicate anchors receive local occurrences.
        var occurrences = new Dictionary<string, int>(StringComparer.Ordinal);
        var narrative = spans.Where(s => s.Unit.Kind != "name").ToArray();
        var narrativeIndex = 0;
        for (var index = 0; index < spans.Count; index++)
        {
            var span = spans[index];
            var unit = span.Unit;
            var anchor = LocalFiles.HashText(unit.Kind + "\n" + unit.Speaker + "\n" + unit.SourceText);
            var occurrence = occurrences.GetValueOrDefault(anchor) + 1;
            occurrences[anchor] = occurrence;
            string context;
            if (unit.Kind == "name") context = "LiLiM SFA16; character-name";
            else
            {
                var previous = narrativeIndex > 0 ? narrative[narrativeIndex - 1].Unit.SourceText : "";
                var next = narrativeIndex + 1 < narrative.Length ? narrative[narrativeIndex + 1].Unit.SourceText : "";
                context = $"LiLiM SFA16; {resource}; previous: {previous[^Math.Min(previous.Length, 256)..]}; next: {next[..Math.Min(next.Length, 256)]}";
                narrativeIndex++;
            }
            spans[index] = span with { Unit = unit with { Id = $"{resource}:{unit.Kind}:{anchor[..24]}:{occurrence}", Context = context } };
        }
        return spans;
    }

    private static void ExtractLine(string resource, byte[] data, int start, int end, int line,
        Encoding encoding, List<TextSpan> spans)
    {
        while (start < end && data[start] is 9 or 32) start++;
        if (start == end || data[start] is (byte)'#' or (byte)'/' or (byte)':' or (byte)'^' or (byte)'%') return;
        while (end > start && data[end - 1] is 9 or 32) end--;

        void Add(int offset, int length, string kind, string? speaker = null)
        {
            if (length == 0) return;
            var text = encoding.GetString(data, offset, length);
            if (string.IsNullOrWhiteSpace(text)) return;
            var id = $"{resource}:L{line}:{kind}";
            var unit = new TranslationUnit(id, resource, $"line {line}, {kind}", text,
                $"LiLiM SFA16; {resource}; line {line}", speaker, kind);
            spans.Add(new TextSpan(offset, length, unit));
        }

        if (data[start] == '[')
        {
            var close = FindAscii(data, start + 1, end, (byte)']', encoding.CodePage);
            if (close < 0) throw new InvalidDataException($"Unterminated speaker label in {resource}, line {line}.");
            var speaker = encoding.GetString(data, start + 1, close - start - 1);
            Add(start + 1, close - start - 1, "name");
            Add(close + 1, end - close - 1, "dialogue", speaker);
            return;
        }
        if (IsLead(data[start], encoding.CodePage) || data[start] == '\\')
        {
            Add(start, end - start, "dialogue");
            return;
        }

        // Only explicit caption-bearing commands are supported; asset names and other
        // quoted parameters must never be sent to the translator.
        var asciiPrefix = Encoding.ASCII.GetString(data, start, Math.Min(end - start, 64));
        var firstWord = Regex.Match(asciiPrefix, @"^[A-Za-z_][A-Za-z0-9_]*", RegexOptions.CultureInvariant).Value;
        if (BareCommands.Contains(firstWord)) return;
        var isButton = Regex.IsMatch(asciiPrefix, @"^btnset\s*\(", RegexOptions.CultureInvariant);
        var isChoiceVariable = Regex.IsMatch(asciiPrefix, @"^\$(?:select|selsub)_text[0-9]+\s*=", RegexOptions.CultureInvariant);
        if (!isButton && !isChoiceVariable)
        {
            if (data[start] != '$' && !Regex.IsMatch(asciiPrefix,
                @"^(?:[A-Za-z_][A-Za-z0-9_]*\s*\(|(?:var|global|float)\b)", RegexOptions.CultureInvariant))
                Add(start, end - start, "dialogue");
            return;
        }
        var quotes = new List<(int Start, int Length)>();
        for (var cursor = start; cursor < end; cursor++)
        {
            if (data[cursor] == '#') break;
            if (data[cursor] != '"') continue;
            var quoteEnd = Array.IndexOf(data, (byte)'"', cursor + 1, end - cursor - 1);
            if (quoteEnd < 0) throw new InvalidDataException($"Unterminated command string in {resource}, line {line}.");
            quotes.Add((cursor + 1, quoteEnd - cursor - 1));
            cursor = quoteEnd;
        }
        var target = isButton ? 1 : 0;
        if (quotes.Count > target)
        {
            var caption = quotes[target];
            if (caption.Length != 0 && data[caption.Start] != '$') Add(caption.Start, caption.Length, "choice");
        }
    }

    public static byte[] Rebuild(byte[] original, IReadOnlyList<TextSpan> spans,
        IReadOnlyDictionary<string, string> translations, bool chinese)
    {
        using var output = new MemoryStream();
        var cursor = 0;
        foreach (var span in spans)
        {
            output.Write(original.AsSpan(cursor, span.Offset - cursor));
            var translated = translations.TryGetValue(span.Unit.Id, out var value) ? value : span.Unit.SourceText;
            if (chinese) translated = NormalizeChineseDisplayText(translated);
            ValidateText(span.Unit, translated);
            var encoding = chinese ? Chinese : ShiftJis;
            try { output.Write(encoding.GetBytes(translated)); }
            catch (EncoderFallbackException error)
            {
                throw new InvalidDataException($"Text cannot be represented by CP{encoding.CodePage}: {span.Unit.Id}. Remove emoji/unsupported characters.", error);
            }
            cursor = span.Offset + span.Length;
        }
        output.Write(original.AsSpan(cursor));
        return output.ToArray();
    }

    // CP936 lacks these two punctuation marks. Apply the same display substitutions
    // to translated and retained text; source snapshots and translation memory stay verbatim.
    internal static string NormalizeChineseDisplayText(string text) => text.Replace('・', '·').Replace('♪', '～');

    public static void ValidateText(TranslationUnit unit, string translated)
    {
        if (string.IsNullOrWhiteSpace(translated) || translated.Any(c => c is '\0' or '\r' or '\n'))
            throw new InvalidDataException($"Translation is empty or contains a script line break: {unit.Id}");
        if (unit.Kind is "choice" && translated.Contains('"'))
            throw new InvalidDataException($"Choice translation contains a command string delimiter: {unit.Id}");
        if (unit.Kind is "name" && translated.IndexOfAny(['[', ']']) >= 0)
            throw new InvalidDataException($"Speaker translation contains a label delimiter: {unit.Id}");
        if (unit.Kind is "dialogue" && translated.IndexOfAny(['[', ']']) >= 0)
            throw new InvalidDataException($"Dialogue translation contains an unsupported inline label: {unit.Id}");
        if (unit.Kind is "dialogue" && translated[0] is '#' or '/' or ':' or '^' or '$' or '%')
            throw new InvalidDataException($"Dialogue translation starts with a script instruction: {unit.Id}");
        if (unit.Kind == "dialogue" && unit.Speaker is null)
        {
            var firstWord = Regex.Match(translated, @"^[A-Za-z_][A-Za-z0-9_]*", RegexOptions.CultureInvariant).Value;
            if (BareCommands.Contains(firstWord) || Regex.IsMatch(translated,
                @"^(?:[A-Za-z_][A-Za-z0-9_]*\s*\(|(?:var|global|float)\b)", RegexOptions.CultureInvariant))
                throw new InvalidDataException($"Translation would be interpreted as a script command: {unit.Id}");
        }
        var sourceTokens = EscapeToken().Matches(unit.SourceText).Select(x => x.Value);
        var translatedTokens = EscapeToken().Matches(translated).Select(x => x.Value);
        if (!sourceTokens.SequenceEqual(translatedTokens))
            throw new InvalidDataException($"Translation changed an inline control token: {unit.Id}");
    }

    private static int FindAscii(byte[] data, int start, int end, byte value, int codePage)
    {
        for (var cursor = start; cursor < end; cursor++)
        {
            if (IsLead(data[cursor], codePage)) { cursor++; continue; }
            if (data[cursor] == value) return cursor;
        }
        return -1;
    }

    private static bool IsLead(byte value, int codePage) => codePage == 936
        ? value is >= 0x81 and <= 0xFE
        : value is >= 0x81 and <= 0x9F or >= 0xE0 and <= 0xFC;

    [GeneratedRegex(@"\\[A-Za-z0-9]+|@[A-Za-z0-9]", RegexOptions.CultureInvariant)]
    private static partial Regex EscapeToken();

    internal sealed record TextSpan(int Offset, int Length, TranslationUnit Unit);
}
