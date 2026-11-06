using System.Text.RegularExpressions;

namespace VNTextPatch.Shared.Util;

// The translator preserves explicit line breaks. Layout changes require engine/runtime evidence.
internal class WordWrapper
{
    public string Wrap(string text, Regex? controlCodePattern = null, string lineBreak = "\r\n") => text;
    public IEnumerable<int> GetWrapPositions(string text, Regex? controlCodePattern = null)
    {
        yield return text.Length;
    }
}

internal sealed class MonospaceWordWrapper : WordWrapper
{
    public static readonly MonospaceWordWrapper Default = new();
}

internal static class ProportionalWordWrapper
{
    public static readonly WordWrapper Default = new();
    public static readonly WordWrapper Secondary = new();
}
