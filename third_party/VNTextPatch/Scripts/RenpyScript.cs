using System.Text;
using System.Text.RegularExpressions;

namespace VNTextPatch.Shared.Scripts;

// Ren'Py say/menu statements are UTF-8. Quoted Python, image, audio and screen expressions are not dialogue.
public sealed class RenpyScript : PlainTextScript
{
    private static readonly Regex Say = new("^(?:(?<speaker>[A-Za-z_][A-Za-z_0-9.]*)[ \\t]+)?(?<quote>['\"])(?<text>(?:\\\\.|(?!\\k<quote>)[^\\\\])*)\\k<quote>[ \\t]*(?<menu>:)?[ \\t]*(?:#.*)?$", RegexOptions.Compiled);
    private static readonly HashSet<string> Statements = new(StringComparer.Ordinal)
    {
        "image", "play", "queue", "stop", "scene", "show", "hide", "jump", "call", "return", "pause",
        "define", "default", "init", "python", "screen", "transform", "style", "label", "translate",
        "old", "new", "text", "textbutton", "button", "add", "use", "on", "action", "voice", "window",
        "with", "if", "elif", "else", "while", "for", "menu", "nvl", "camera", "at", "config"
    };
    private bool _bom;
    public override string Extension => ".rpy";

    protected override Encoding GetReadEncoding(ArraySegment<byte> data)
    {
        _bom = data.AsSpan().StartsWith(new byte[] { 0xEF, 0xBB, 0xBF });
        return new UTF8Encoding(_bom, true);
    }

    protected override Encoding GetWriteEncoding() => new UTF8Encoding(_bom, true);

    protected override IEnumerable<Range> GetRanges(string script)
    {
        int offset = 0;
        int? excludedIndent = null;
        using var reader = new StringReader(script);
        while (reader.ReadLine() is { } line)
        {
            int lineOffset = offset;
            offset += line.Length;
            if (offset < script.Length && script[offset] == '\r') offset++;
            if (offset < script.Length && script[offset] == '\n') offset++;
            int indent = line.Length - line.TrimStart(' ', '\t').Length;
            string statement = line[indent..];
            if (string.IsNullOrWhiteSpace(statement) || statement.StartsWith('#')) continue;
            if (excludedIndent.HasValue)
            {
                if (indent > excludedIndent.Value) continue;
                excludedIndent = null;
            }
            if (Regex.IsMatch(statement, @"^(?:(?:init(?:\s+-?\d+)?\s+)?python\b|screen\b|transform\b|style\b|image\b|init\b)"))
            {
                if (statement.Split('#')[0].TrimEnd().EndsWith(':')) excludedIndent = indent;
                continue;
            }
            Match match = Say.Match(statement);
            if (!match.Success || Statements.Contains(match.Groups["speaker"].Value)) continue;
            Group text = match.Groups["text"];
            if (text.Length > 0) yield return new Range(lineOffset + indent + text.Index, text.Length, ScriptStringType.Message);
        }
    }

    protected override string GetTextForRead(Range range)
    {
        string text = base.GetTextForRead(range);
        return Regex.Replace(text, @"\\([\\'""nrt])", m => m.Groups[1].Value switch
        {
            "n" => "\n", "r" => "\r", "t" => "\t", _ => m.Groups[1].Value
        });
    }

    protected override string GetTextForWrite(Range range, ScriptString str)
    {
        string quote = GetScriptSubstring(range.Offset - 1, 1);
        return str.Text.Replace("\\", "\\\\").Replace(quote, "\\" + quote)
            .Replace("\r", "\\r").Replace("\n", "\\n").Replace("\t", "\\t");
    }
}
