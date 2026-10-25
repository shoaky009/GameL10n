using System.Text.RegularExpressions;

namespace AvgTranslator.Core.Translation;

public static partial class TextValidation
{
    [GeneratedRegex(@"\{[^{}\r\n]+\}|\[[^\[\]\r\n]+\]|%(?:\d+\$)?[-+0 #]*\d*(?:\.\d+)?[sdifuxX]|\\[nrt]", RegexOptions.CultureInvariant)]
    private static partial Regex Markers();

    public static IReadOnlyList<ValidationIssue> Validate(TranslationUnit unit, string translated)
    {
        var issues = new List<ValidationIssue>();
        if (string.IsNullOrWhiteSpace(translated))
            issues.Add(new("empty-translation", "译文为空。", unit.Id));
        if (translated.Contains('\0')) issues.Add(new("nul-character", "译文含有空字符。", unit.Id));
        if (!unit.SourceText.Contains('\n') && translated.Contains('\n'))
            issues.Add(new("unexpected-newline", "译文增加了脚本中不存在的换行。", unit.Id));
        var original = Markers().Matches(unit.SourceText).Select(m => m.Value).Order(StringComparer.Ordinal);
        var changed = Markers().Matches(translated).Select(m => m.Value).Order(StringComparer.Ordinal);
        if (!original.SequenceEqual(changed))
            issues.Add(new("placeholder-mismatch", "占位符或控制标记与原文不一致。", unit.Id));
        return issues;
    }
}
