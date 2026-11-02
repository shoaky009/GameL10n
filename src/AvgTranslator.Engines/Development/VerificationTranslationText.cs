using System.Globalization;
using AvgTranslator.Core.Translation;

namespace AvgTranslator.Core.Development;

internal static class VerificationTranslationText
{
    internal static string Create(TranslationUnit unit, int index, bool trial = false)
    {
        var source = unit.SourceText;
        var spans = TextValidation.ProtectedSpans(source, unit.ProtectedTokens);
        var position = 0;
        foreach (var span in spans)
        {
            var changed = Mutate(position, span.Start);
            if (changed is not null) return changed;
            position = span.Start + span.Length;
        }
        return Mutate(position, source.Length) ?? source;

        string? Mutate(int start, int end)
        {
            // Keep leading tags, whitespace and visible wrappers in their original order.
            // In particular, moving text before 【...】 can turn a name into dialogue.
            while (start < end && (char.IsWhiteSpace(source[start]) || IsOpening(source[start]))) start++;
            while (end > start && (char.IsWhiteSpace(source[end - 1]) || IsClosing(source[end - 1]))) end--;
            if (start == end) return null;
            var isName = unit.Kind.Equals("name", StringComparison.OrdinalIgnoreCase);
            if (!isName && spans.Count == 0 && start == 0 && end == source.Length)
                return (trial ? "AVG_TRIAL_" : "AVG_TRANSLATION_") + index + (trial ? "_译文 " : "_翻译 ") + source;

            var marker = (trial ? "试" : "译") + ChineseIndex(index);
            if (!isName) return source.Insert(start, marker + " ");
            // A synthetic name must stay short enough to exercise normal name parsing.
            if (source.AsSpan(start, end - start).SequenceEqual(marker)) marker = "测" + ChineseIndex(index);
            return source[..start] + marker + source[end..];
        }
    }

    private static bool IsOpening(char value) => value is '\'' or '"'
        || char.GetUnicodeCategory(value) is UnicodeCategory.OpenPunctuation or UnicodeCategory.InitialQuotePunctuation;

    private static bool IsClosing(char value) => value is '\'' or '"'
        || char.GetUnicodeCategory(value) is UnicodeCategory.ClosePunctuation or UnicodeCategory.FinalQuotePunctuation;

    private static string ChineseIndex(int index)
    {
        const string digits = "甲乙丙丁戊己庚辛壬癸";
        var result = "";
        do { result = digits[index % digits.Length] + result; index = index / digits.Length - 1; } while (index >= 0);
        return result;
    }
}
