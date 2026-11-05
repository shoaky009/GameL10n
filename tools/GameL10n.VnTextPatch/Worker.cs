using System.Security.Cryptography;
using System.Text;
using System.Text.Json;
using System.Text.RegularExpressions;
using VNTextPatch.Shared;

namespace GameL10n.VnTextPatch;

public static class Worker
{
    internal static readonly JsonSerializerOptions Json = new(VnTextPatchJson.Web);
    private static readonly object Gate = new();
    private const string RuntimeWarning = "脚本已由内置解析器处理；游戏启动、字体及中文显示尚未实机验证。";
    public static IReadOnlyList<string> SupportedFormats { get; } = Array.AsReadOnly(Format.All.Select(f => f.Key).ToArray());

    public static string ExecuteJson(string requestJson)
    {
        lock (Gate)
        {
            try
            {
                var request = JsonSerializer.Deserialize<Request>(requestJson, VnTextPatchJson.TypeInfo<Request>(Json)) ?? throw new InvalidDataException("请求内容为空。");
                return JsonSerializer.Serialize(Execute(request), VnTextPatchJson.TypeInfo<Response>(Json));
            }
            catch (Exception error)
            {
                return JsonSerializer.Serialize(Failure(error), VnTextPatchJson.TypeInfo<Response>(Json));
            }
        }
    }

    internal static Response Failure(Exception error)
    {
        Console.Error.WriteLine(error);
        string message = error is EncoderFallbackException || error.InnerException is EncoderFallbackException
            ? "该脚本的原始编码无法保存部分中文字符。请使用智能适配配置并验证游戏运行时的中文编码与字体支持；已有译文可继续复用。"
            : error is ScriptException ? error.Message
            : "内置脚本工具未能完成操作：" + error.Message;
        return new Response(Success: false, Error: message, Diagnostic: Describe(error));
    }

    private static Diagnostic Describe(Exception error, int depth = 0)
    {
        static string Limit(string value, int maximum) => value.Length <= maximum ? value : value[..maximum] + "…";
        string? path = error is ScriptException script ? script.ResourcePath
            : error is FileNotFoundException missing ? missing.FileName : error.Data["resourcePath"] as string;
        long? offset = long.TryParse(error.Data["offset"]?.ToString(), out var position) ? position : null;
        return new Diagnostic(error.GetType().FullName ?? error.GetType().Name, Limit(error.Message, 2000),
            path is null ? null : Limit(path, 1024), offset,
            error.StackTrace is null ? null : Limit(error.StackTrace, 6000),
            depth < 5 && error.InnerException is { } inner ? Describe(inner, depth + 1) : null);
    }

    internal static Response Execute(Request request)
    {
        if (request.ProtocolVersion != 1) throw new InvalidDataException("不支持该通信协议版本。");
        GlobalVariables.NoWrap = true;
        GlobalVariables.ReadUtf8 = false;
        GlobalVariables.WriteUtf8 = false;
        GlobalVariables.jisStrings.Clear();
        Format format = Format.Get(request.Format);
        string source = Path.GetFullPath(request.SourceDirectory);
        if (!Directory.Exists(source)) throw new DirectoryNotFoundException("找不到源脚本目录。");
        string[] files = (request.ResourceFiles ?? Directory.EnumerateFiles(source, "*", SearchOption.AllDirectories)
            .Select(p => Path.GetRelativePath(source, p)).ToArray()).Select(Normalize).Distinct(StringComparer.OrdinalIgnoreCase)
            .Order(StringComparer.Ordinal).ToArray();
        foreach (string file in files)
        {
            if (!File.Exists(Resolve(source, file))) throw new FileNotFoundException($"找不到源文件：{file}");
        }
        string[] scripts = files.Where(format.Matches).Where(p => !IsCompanionOnly(format, p)).ToArray();
        return request.Operation switch
        {
            "probe" => Probe(format, source, files, scripts),
            "extract" => new Response(Extraction: new Extraction(Extract(format, source, scripts), Warnings(format))),
            "build" => Build(format, source, files, scripts, request),
            "validate" => Validate(format, source, files, scripts, request),
            _ => throw new InvalidDataException("不支持该脚本操作。")
        };
    }

    private static Response Probe(Format format, string source, string[] files, string[] scripts)
    {
        var selected = new HashSet<string>(StringComparer.OrdinalIgnoreCase);
        bool foundText = false;
        int rejected = 0;
        foreach (string script in scripts)
        {
            try
            {
                if (!HasProbeEvidence(format, source, script)) continue;
                var loaded = Load(format, source, script);
                if (format.Key == "TmrHiroAdvSystemText" && !loaded.Strings.Any(s => LooksLikeNarrative(s.Text))) continue;
                string[] companions = Companions(format, files, script).ToArray();
                if (format.Key == "Softpal" && companions.Length != 2) continue;
                foundText |= loaded.Strings.Any(s => IsText(format, s));
                selected.Add(script);
                selected.UnionWith(companions);
            }
            catch (Exception error) when (error is not OutOfMemoryException)
            {
                rejected++;
                Console.Error.WriteLine($"{script}: {error.GetBaseException().Message}");
            }
        }
        if (!foundText) return new Response();
        var warnings = Warnings(format).ToList();
        if (rejected > 0) warnings.Add($"有 {rejected} 个候选文件未通过该格式解析，未纳入补丁；如缺少剧情，请使用智能适配检查资源版本。" );
        string id = "vntextpatch-" + format.Key.ToLowerInvariant();
        return new Response(Probe: new Probe(id, id, "1.0.1", $"VNTextPatch · {format.Key}", selected.Order(StringComparer.Ordinal).ToArray(), warnings));
    }

    private static List<Unit> Extract(Format format, string source, string[] scripts)
    {
        var units = new List<Unit>();
        foreach (string script in scripts)
        {
            var loaded = Load(format, source, script);
            for (int i = 0; i < loaded.Strings.Length; i++)
            {
                ScriptString value = loaded.Strings[i];
                if (!IsText(format, value)) continue;
                units.Add(new Unit(UnitId(format, script, i), script, $"string:{i}", value.Text,
                    $"{format.Key} · {script}", Kind: value.Type == ScriptStringType.CharacterName ? "name" : "dialogue"));
            }
        }
        return units;
    }

    private static Response Build(Format format, string source, string[] files, string[] scripts, Request request)
    {
        string output = Output(source, request.OutputDirectory);
        Dictionary<string, string> translations;
        if (request.TranslationsFile is { } translationsFile)
        {
            using var input = File.OpenRead(translationsFile);
            translations = JsonSerializer.Deserialize<Dictionary<string, string>>(input, VnTextPatchJson.TypeInfo<Dictionary<string, string>>(Json)) ?? [];
        }
        else translations = request.Translations ?? [];
        var unseen = new HashSet<string>(translations.Keys, StringComparer.Ordinal);
        Directory.CreateDirectory(output);
        foreach (string file in files)
        {
            string destination = Resolve(output, file);
            Directory.CreateDirectory(Path.GetDirectoryName(destination)!);
            File.Copy(Resolve(source, file), destination, true);
        }
        var expectedByFile = new Dictionary<string, ScriptString[]>(StringComparer.OrdinalIgnoreCase);
        foreach (string script in scripts)
        {
            var loaded = Load(format, source, script);
            var expected = loaded.Strings.ToArray();
            bool changed = false;
            for (int i = 0; i < expected.Length; i++)
            {
                if (!IsText(format, expected[i])) continue;
                string id = UnitId(format, script, i);
                if (!translations.TryGetValue(id, out string? text)) continue;
                unseen.Remove(id);
                if (text is null || text.Contains('\0')) throw new InvalidDataException($"译文包含无效内容：{script}，第 {i + 1} 项。");
                changed |= text != expected[i].Text;
                expected[i] = new ScriptString(text, expected[i].Type);
            }
            expectedByFile.Add(script, expected);
            if (!changed) continue;
            try
            {
                loaded.Parser.WritePatched(expected, ScriptLocation.FromFilePath(Resolve(output, script), format.Key));
            }
            catch (Exception error)
            {
                throw new ScriptException($"无法写回脚本 {script}。请使用智能适配检查该引擎的编码和脚本版本。", error, script);
            }
        }
        if (unseen.Count != 0) throw new InvalidDataException("部分译文无法对应当前脚本，请重新提取后再构建补丁。");
        // Verify every selected script, including resources sharing a companion text pool.
        foreach ((string script, ScriptString[] expected) in expectedByFile)
        {
            ScriptString[] actual = Load(format, output, script).Strings;
            if (!EqualStrings(expected, actual))
                throw new ScriptException($"脚本 {script} 写回后的文本或结构不一致，已拒绝该补丁。请使用智能适配检查脚本版本。" );
        }
        return new Response(Build: new Build(files, Warnings(format)));
    }

    private static Response Validate(Format format, string source, string[] files, string[] scripts, Request request)
    {
        string output = Output(source, request.OutputDirectory);
        var issues = new List<Issue>();
        foreach (string file in files)
        {
            if (!File.Exists(Resolve(output, file))) issues.Add(new Issue("missing-resource", $"补丁缺少源资源：{file}"));
        }
        if (issues.Count > 0) return new Response(Validation: new Validation(false, issues));
        foreach (string script in scripts)
        {
            try
            {
                ScriptString[] original = Load(format, source, script).Strings;
                ScriptString[] patched = Load(format, output, script).Strings;
                if (original.Length != patched.Length || original.Where((s, i) => s.Type != patched[i].Type).Any())
                    issues.Add(new Issue("script-structure", $"补丁改变了脚本文本结构：{script}"));
            }
            catch (Exception error)
            {
                Console.Error.WriteLine(error);
                issues.Add(new Issue("script-parse", $"补丁脚本无法重新解析：{script}"));
            }
        }
        return new Response(Validation: new Validation(issues.Count == 0, issues));
    }

    private static (IScript Parser, ScriptString[] Strings) Load(Format format, string root, string script)
    {
        try
        {
            IScript parser = format.Create();
            parser.Load(ScriptLocation.FromFilePath(Resolve(root, script), format.Key));
            return (parser, parser.GetStrings().ToArray());
        }
        catch (Exception error)
        {
            throw new ScriptException($"无法解析脚本 {script}。请确认资源已解包且属于支持的脚本版本。", error, script);
        }
    }

    private static bool IsText(Format format, ScriptString value)
    {
        if (value.Type == ScriptStringType.Internal || string.IsNullOrWhiteSpace(value.Text)) return false;
        if (format.Key is not ("KirikiriTjs" or "Mware")) return true;
        return !Regex.IsMatch(value.Text, @"^(?:https?://|[A-Za-z]:[/\\])|[/\\]|\.(?:png|jpe?g|bmp|webp|ogg|wav|mp3|mp4|avi|mpg|tjs|ks|ttf|otf|dll|exe|dat|bin)$", RegexOptions.IgnoreCase);
    }
    private static bool EqualStrings(ScriptString[] expected, ScriptString[] actual) => expected.Length == actual.Length &&
        expected.Where((s, i) => s.Type != actual[i].Type || s.Text != actual[i].Text).Any() == false;
    private static string UnitId(Format format, string script, int index) => "vntp:" + Convert.ToHexStringLower(
        SHA256.HashData(Encoding.UTF8.GetBytes(format.Key + "\n" + script.ToLowerInvariant() + "\n" + index)));
    private static string[] Warnings(Format format) => format.Key switch
    {
        "Renpy" => [RuntimeWarning, "Ren’Py 仅提取单行对白、旁白和菜单文本；Python、界面及动态表达式需专用适配。"],
        "TmrHiroAdvSystemText" => [RuntimeWarning, "此文本表没有引擎签名，当前仅确认长度记录与文本结构兼容；请验证游戏确实使用并加载该脚本。"],
        "KirikiriTjs" or "Mware" => [RuntimeWarning, "此编译脚本使用上游文本识别规则；已排除常见资源路径，试译前请复核文本列表中的变量或动态表达式。"],
        _ => [RuntimeWarning]
    };
    private static bool LooksLikeNarrative(string text) => text.Length >= 4 &&
        !Regex.IsMatch(text, @"(?:[/\\]|\.[A-Za-z0-9]{1,5}$|^[A-Za-z_][A-Za-z_0-9]*$)") &&
        (text.Count(c => c >= 0x3000 && char.IsLetter(c)) >= 3 || text.Any(char.IsWhiteSpace) && text.Count(char.IsLetter) >= 3);
    private static bool IsCompanionOnly(Format format, string path) => format.Key == "Yuris" && Path.GetFileName(path).Equals("ysc.ybn", StringComparison.OrdinalIgnoreCase);
    private static bool HasProbeEvidence(Format format, string source, string script)
    {
        // A generic text extension alone does not distinguish these unrelated script languages.
        if (format.Key is not ("ArtemisTxt" or "KirikiriKs" or "Whale")) return true;
        if (format.Key != "Whale" && !Path.GetExtension(script).Equals(".txt", StringComparison.OrdinalIgnoreCase)) return true;
        byte[] bytes = File.ReadAllBytes(Resolve(source, script));
        string text;
        if (bytes.AsSpan().StartsWith(new byte[] { 0xFF, 0xFE })) text = Encoding.Unicode.GetString(bytes);
        else if (bytes.AsSpan().StartsWith(new byte[] { 0xEF, 0xBB, 0xBF })) text = new UTF8Encoding(false, true).GetString(bytes);
        else text = VNTextPatch.Shared.Util.StringUtil.SjisEncoding.GetString(bytes);
        return format.Key switch
        {
            "ArtemisTxt" => Regex.IsMatch(text, @"(?m)^\s*\[(?:name|sel_text)(?:\s|\])"),
            "KirikiriKs" => Regex.IsMatch(text, @"(?m)(?:^\s*@(?:iscript|jump|s|p|l|r|cm|eval|font)\b|\[(?:iscript|jump|p|l|r|cm|eval|font)(?:\s|\]))"),
            "Whale" => Regex.IsMatch(text, @"(?m)^(?:SELECT |CS |MS\.HS |【[^\r\n]+】)"),
            _ => false
        };
    }
    private static IEnumerable<string> Companions(Format format, string[] files, string script)
    {
        string? directory = Path.GetDirectoryName(script);
        foreach (string name in format.Companions)
        {
            string relative = Normalize(Path.Combine(directory ?? "", name));
            string? found = files.FirstOrDefault(p => p.Equals(relative, StringComparison.OrdinalIgnoreCase));
            if (found is not null) yield return found;
        }
    }
    private static string Normalize(string relative)
    {
        string path = relative.Replace('\\', '/');
        if (string.IsNullOrWhiteSpace(path) || Path.IsPathRooted(path) || path.Contains(':') ||
            path.Split('/').Any(part => part is "" or "." or "..")) throw new InvalidDataException("资源路径必须位于脚本目录内。");
        return path;
    }
    private static string Resolve(string root, string relative)
    {
        string path = Path.GetFullPath(Path.Combine(root, Normalize(relative)));
        for (string? current = path; current is not null && current.Length >= root.Length; current = Path.GetDirectoryName(current))
        {
            if ((File.Exists(current) || Directory.Exists(current)) && (File.GetAttributes(current) & FileAttributes.ReparsePoint) != 0)
                throw new InvalidDataException("脚本路径不能经过符号链接或目录联接。");
        }
        return path;
    }
    private static string Output(string source, string? outputDirectory)
    {
        if (string.IsNullOrWhiteSpace(outputDirectory)) throw new InvalidDataException("缺少补丁输出目录。");
        string output = Path.GetFullPath(outputDirectory);
        string a = source.TrimEnd(Path.DirectorySeparatorChar) + Path.DirectorySeparatorChar;
        string b = output.TrimEnd(Path.DirectorySeparatorChar) + Path.DirectorySeparatorChar;
        if (a.StartsWith(b, StringComparison.OrdinalIgnoreCase) || b.StartsWith(a, StringComparison.OrdinalIgnoreCase))
            throw new InvalidDataException("补丁输出目录必须与源目录分开。");
        return output;
    }

    private sealed class ScriptException(string message, Exception? inner = null, string? resourcePath = null) : Exception(message, inner)
    {
        public string? ResourcePath { get; } = resourcePath;
    }
}
