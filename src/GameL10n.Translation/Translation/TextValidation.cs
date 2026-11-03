using System.Text.RegularExpressions;

namespace GameL10n.Core.Translation;

public static partial class TextValidation
{
    [GeneratedRegex("""\((?:\*|#?[+-]?xx?\d+(?:%?x\d+)?%?)\)|\{[^{}\r\n]+\}|</?[A-Za-z][A-Za-z0-9_-]*(?:[^<>"'\r\n]|"[^"\r\n]*"|'[^'\r\n]*')*>|%%|%(?:\d+\$)?[-+0 #]*\d*(?:\.\d+)?[sdifuxX]|\\[nrt]""", RegexOptions.CultureInvariant)]
    private static partial Regex Markers();

    [GeneratedRegex(@"\[\[[^\[\]\r\n]+\]\]|\[[^\[\]\r\n]+\]", RegexOptions.CultureInvariant)]
    private static partial Regex SquareGroups();

    [GeneratedRegex(@"\[+|\]+", RegexOptions.CultureInvariant)]
    private static partial Regex SquareDelimiters();

    [GeneratedRegex(@"<[^<>\r\n]*>", RegexOptions.CultureInvariant)]
    private static partial Regex Tags();

    [GeneratedRegex("""[=(){}]|\s[+*/%<>!&|?:-]\s|^(?:not|if)\s|\s(?:if|else)\s|^[\w.]+(?:\s+(?:and|or|in|is)\s+[\w.]+)+$|^[/@#]?[A-Za-z_][\w.-]*(?:\s+(?:[-+]?\d+(?:\.\d+)?|"[^"]*"|'[^']*'))+$""", RegexOptions.CultureInvariant)]
    private static partial Regex SquareExpression();

    // Double-bracket emphasis can contain a single visible word without a paired ((...)) span.
    // Match the complete group so protected expressions never lose their outer brackets.
    // Single-bracket identifiers, interpolation expressions and script commands remain protected.
    // Classification uses the source: translated Chinese prose need not contain spaces.
    private static bool IsBracketProse(string group)
    {
        var doubled = group.StartsWith("[[", StringComparison.Ordinal) && group.EndsWith("]]", StringComparison.Ordinal);
        var width = doubled ? 2 : 1;
        var content = Tags().Replace(group[width..^width], "").Trim();
        return !SquareExpression().IsMatch(content)
            && (content.Any(char.IsWhiteSpace) || doubled && content.Length > 0 && content.All(char.IsLetter));
    }

    private static IEnumerable<Match> CommonMarkerMatches(string text) => Markers().Matches(text)
        // "25% increased" is prose, while "% i" alone can be a real printf argument.
        .Where(match => !(match.Value.StartsWith("% ", StringComparison.Ordinal)
            && (match.Index > 0 && char.IsDigit(text[match.Index - 1])
                || match.Index + match.Length < text.Length && char.IsAsciiLetter(text[match.Index + match.Length]))));

    private static IEnumerable<string> CommonMarkers(string text) => CommonMarkerMatches(text).Select(match => match.Value);

    // Prefer the outer script expression when it contains tags/variables. For visible bracketed
    // prose only protect its delimiters, leaving its words available to the translation model.
    internal static IReadOnlyList<(int Start, int Length)> ProtectedSpans(string source, IReadOnlyList<string>? protectedTokens = null)
    {
        var spans = CommonMarkerMatches(source).Select(m => (Start: m.Index, Length: m.Length)).ToList();
        spans.AddRange(DeclaredSpans(source, protectedTokens));
        foreach (Match group in SquareGroups().Matches(source))
        {
            if (!IsBracketProse(group.Value)) spans.Add((group.Index, group.Length));
            else
            {
                var width = group.Value.StartsWith("[[", StringComparison.Ordinal) ? 2 : 1;
                spans.Add((group.Index, width));
                spans.Add((group.Index + group.Length - width, width));
            }
        }
        var result = new List<(int Start, int Length)>();
        int end = 0;
        foreach (var span in spans.OrderBy(s => s.Start).ThenByDescending(s => s.Length))
            if (span.Start >= end) { result.Add(span); end = span.Start + span.Length; }
        return result;
    }

    private static IEnumerable<(int Start, int Length)> DeclaredSpans(string source, IReadOnlyList<string>? tokens)
    {
        foreach (var token in tokens?.Distinct(StringComparer.Ordinal) ?? [])
        {
            if (string.IsNullOrEmpty(token)) throw new ArgumentException("适配器声明的保护标记不能为空。");
            int offset = 0;
            while ((offset = source.IndexOf(token, offset, StringComparison.Ordinal)) >= 0)
            {
                yield return (offset, token.Length);
                offset += token.Length;
            }
        }
    }

    internal static string ProtectionSignature(TranslationUnit unit) => unit.ProtectedTokens is not { Count: > 0 } ? ""
        : "\nprotected:" + System.Text.Json.JsonSerializer.Serialize(unit.ProtectedTokens.Distinct(StringComparer.Ordinal).Order(StringComparer.Ordinal));

    public static IReadOnlyList<string> ProtectedMarkers(string source) => CommonMarkers(source)
        .Concat(SquareGroups().Matches(source).Select(group => group.Value).Where(group => !IsBracketProse(group)))
        .ToArray();

    public static IReadOnlyList<string> TranslatableBracketGroups(string source) => SquareGroups().Matches(source)
        .Select(group => group.Value).Where(IsBracketProse).ToArray();

    public static IReadOnlyList<ValidationIssue> Validate(TranslationUnit unit, string translated)
    {
        var issues = new List<ValidationIssue>();
        if (string.IsNullOrWhiteSpace(translated))
            issues.Add(new("empty-translation", "译文为空。", unit.Id));
        if (translated.Contains('\0')) issues.Add(new("nul-character", "译文含有空字符。", unit.Id));
        if (!unit.SourceText.Contains('\n') && translated.Contains('\n'))
            issues.Add(new("unexpected-newline", "译文增加了脚本中不存在的换行。", unit.Id));
        var sourceGroups = SquareGroups().Matches(unit.SourceText).Select(group => group.Value).ToArray();
        var protectedSourceGroups = sourceGroups.Where(group => !IsBracketProse(group)).ToArray();
        var protectedGroups = protectedSourceGroups.ToHashSet(StringComparer.Ordinal);
        var hasBracketProse = sourceGroups.Length != protectedSourceGroups.Length;
        var original = CommonMarkers(unit.SourceText).Concat(protectedSourceGroups)
            .Concat(DeclaredSpans(unit.SourceText, unit.ProtectedTokens).Select(span => unit.SourceText.Substring(span.Start, span.Length))).ToArray();
        var changed = CommonMarkers(translated).Concat(SquareGroups().Matches(translated)
            .Select(match => match.Value).Where(group => !hasBracketProse || protectedGroups.Contains(group)))
            .Concat(DeclaredSpans(translated, unit.ProtectedTokens).Select(span => translated.Substring(span.Start, span.Length))).ToArray();
        var missing = Difference(original, changed);
        var added = Difference(changed, original);
        if (missing is not null || added is not null)
        {
            var detail = string.Join("；", new[]
            {
                missing is null ? null : $"缺少或减少了“{Preview(missing)}”（原文 {original.Count(marker => marker == missing)} 次，译文 {changed.Count(marker => marker == missing)} 次）",
                added is null ? null : $"新增或改动了“{Preview(added)}”（原文 {original.Count(marker => marker == added)} 次，译文 {changed.Count(marker => marker == added)} 次）"
            }.Where(value => value is not null));
            issues.Add(new("placeholder-mismatch", "占位符或控制标记与原文不一致：" + detail + "。", unit.Id));
        }
        else if (!SquareDelimiters().Matches(unit.SourceText).Select(match => match.Value).Order(StringComparer.Ordinal)
            .SequenceEqual(SquareDelimiters().Matches(translated).Select(match => match.Value).Order(StringComparer.Ordinal)))
            issues.Add(new("placeholder-mismatch", "方括号结构与原文不一致：原文 " + BracketSummary(unit.SourceText)
                + "；译文 " + BracketSummary(translated) + "。保留每组括号的单双层数，只翻译其中的可见文字。", unit.Id));
        return issues;
    }

    private static string BracketSummary(string text)
    {
        var groups = SquareDelimiters().Matches(text).Select(match => match.Value).GroupBy(value => value, StringComparer.Ordinal);
        var parts = groups.Select(group => $"“{Preview(group.Key)}” {group.Count()} 次").ToArray();
        return parts.Length == 0 ? "无方括号" : string.Join("、", parts);
    }

    private static string? Difference(IEnumerable<string> expected, IEnumerable<string> actual)
    {
        var counts = actual.GroupBy(value => value, StringComparer.Ordinal)
            .ToDictionary(group => group.Key, group => group.Count(), StringComparer.Ordinal);
        return expected.GroupBy(value => value, StringComparer.Ordinal)
            .FirstOrDefault(group => group.Count() > counts.GetValueOrDefault(group.Key))?.Key;
    }

    private static string Preview(string marker) => marker.Length <= 60 ? marker : marker[..57] + "…";
}
