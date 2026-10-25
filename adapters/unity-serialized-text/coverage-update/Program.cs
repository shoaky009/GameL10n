using System;
using System.IO;
using System.Linq;
using System.Text.Json;
using System.Text.Json.Nodes;
using System.Collections.Generic;
using System.Buffers.Binary;

public sealed record ResourceUnit(string id, string resourcePath, string locator, string sourceText, string context,
    string? speaker, string kind,
    [property:System.Text.Json.Serialization.JsonIgnore(Condition=System.Text.Json.Serialization.JsonIgnoreCondition.WhenWritingNull)]
    string[]? protectedTokens = null);

public static class Program
{
    const string Key = "unity-textasset-dialogue-json", Adapter = "engine." + Key;
    public static readonly string[] CoverageWarnings = {
        "已支持对白、物品与职业、效果和状态说明、静态界面，以及已确认的教程、篝火交谈、套装、角色属性和房间提示。",
        "部分武器分类、技能标签、按键名和共用界面词仍未覆盖；提取完成不代表全部游戏文字均已汉化。",
        "游戏内显示、换行及存档兼容性尚待实机确认。"
    };
    static string Str(JsonNode j, string k) => j[k]?.GetValue<string>() ?? "";
    static string[] Files(JsonNode r) => r["resourceFiles"]!.AsArray().Select(n => n!.GetValue<string>()).ToArray();
    static object Reply(string name, object? value) => new Dictionary<string, object?> { { "protocolVersion", 1 }, { "success", true }, { name, value } };
    static Il2CppText Metadata(string root, string path) => new(File.ReadAllBytes(Path.Combine(root, path)));
    static string[] AssetFiles(string[] paths) => paths.Where(p => !Il2CppText.IsPath(p)).ToArray();

    public static object Probe(JsonNode r)
    {
        string directory = Str(r, "sourceDirectory");
        var files = new List<string>();
        var warnings = CoverageWarnings.ToList();
        var reads = new List<object>();
        foreach (string path in Files(r))
        {
            if (Il2CppText.IsPath(path))
            {
                // Probe receives bounded headers, not the full metadata. Match an immutable
                // type-definition range; extract/build additionally verify every non-literal table.
                byte[] header = File.ReadAllBytes(Path.Combine(directory, path));
                int I(int at) => BinaryPrimitives.ReadInt32LittleEndian(header.AsSpan(at, 4));
                if (header.Length < 256 || I(0) != unchecked((int)0xFAB11BAF) || I(4) != 31 || I(12) != 17876 * 8)
                { warnings.Add("此版本的程序内置文案尚未适配；元数据文件未纳入补丁。"); continue; }
                int offset = I(160);
                long length = r["probeFiles"]?.AsArray().FirstOrDefault(p => Str(p!, "path") == path)?["length"]?.GetValue<long>() ?? header.Length;
                if (offset < 256 || offset + 65536L > length) throw new InvalidDataException("IL2CPP 类型表范围无效。");
                byte[]? sample = offset + 65536L <= header.Length ? header.AsSpan(offset, 65536).ToArray() : null;
                var range = r["probeRanges"]?.AsArray().FirstOrDefault(p => Str(p!, "path") == path && p!["offset"]!.GetValue<long>() == offset
                    && p["length"]!.GetValue<int>() == 65536);
                if (sample == null && range != null) sample = File.ReadAllBytes(Str(range, "dataPath"));
                if (sample == null) { reads.Add(new { path, offset, length = 65536 }); continue; }
                if (Convert.ToHexStringLower(System.Security.Cryptography.SHA256.HashData(sample)) == "19a16edf8bf85629aaf0b35ed67345d80983386caafe55d00a81ecdb23205b20")
                    files.Add(path);
                else warnings.Add("此版本的程序内置文案尚未适配；元数据文件未纳入补丁。");
                continue;
            }
            using var f = File.OpenRead(Path.Combine(directory, path));
            byte[] b = new byte[64];
            if (f.Read(b, 0, b.Length) == 64 && BinaryPrimitives.ReadInt32BigEndian(b.AsSpan(8, 4)) == 22
                && b[16] == 0 && System.Text.Encoding.ASCII.GetString(b, 48, 12).StartsWith("2022.")) files.Add(path);
        }
        if (reads.Count > 0) return Reply("probeReads", reads);
        return Reply("probe", !files.Any(p => !Il2CppText.IsPath(p)) ? null : new {
            engineId = Key, adapterId = Adapter, adapterVersion = "1.5.0",
            description = "Unity 2022 / 已验证的序列化文字与 IL2CPP 显示文案；保留资源引用和程序标识",
            requiredFiles = files, warnings, originalExecutable = (string?)null
        });
    }

    public static List<ResourceUnit> ReadUnits(UnitySet set)
    {
        var units = new List<ResourceUnit>();
        foreach (var a in set.Assets) foreach (dynamic inf in a.Value.Infos)
        {
            if ((int)inf.TypeId == 49)
            {
                DialogueAsset? d = DialogueAsset.Parse((byte[])a.Value.Raw(inf), (long)inf.PathId);
                if (d != null) foreach (var t in d.Tokens)
                    units.Add(new(DialogueAsset.Id(a.Key, d.PathId, t.Key), a.Key, $"TextAsset:{d.PathId}/{d.Name}/{t.Key}",
                        t.Text, d.Name + " / " + t.Key, null, "dialogue"));
            }
            else if ((int)inf.TypeId == 114)
            {
                SerializedText? s = SerializedText.Parse(set, a.Key, inf);
                if (s != null) foreach (var t in s.Tokens)
                    units.Add(new(SerializedText.Id(a.Key, s.PathId, t.Field), a.Key, $"MonoBehaviour:{s.PathId}/{s.Script}/{t.Field}",
                        t.Text, s.Script + " / " + s.Name + " / " + t.Field, null, t.Kind, SerializedText.ProtectedTokens(s.Script, t.Field, t.Text)));
            }
        }
        return units;
    }

    public static object Extract(JsonNode r)
    {
        var files = Files(r); string directory = Str(r, "sourceDirectory");
        using var set = new UnitySet(directory, AssetFiles(files));
        var units = ReadUnits(set).Concat(files.Where(Il2CppText.IsPath).SelectMany(p => Metadata(directory, p).Units(p)))
            .Where(t => !string.IsNullOrWhiteSpace(t.sourceText)).ToArray();
        if (units.Length == 0) throw new InvalidDataException("未发现受支持的可见文字字段");
        if (units.Select(t => t.id).Distinct(StringComparer.Ordinal).Count() != units.Length) throw new InvalidDataException("文本 ID 冲突");
        return Reply("extraction", new { units, warnings = CoverageWarnings });
    }

    static Dictionary<string, string> Translations(JsonNode r)
    {
        var path = Str(r, "translationsFile");
        var n = path.Length > 0 ? JsonNode.Parse(File.ReadAllText(path)) : r["translations"];
        return n?.Deserialize<Dictionary<string, string>>() ?? new();
    }

    static Dictionary<(string, long), byte[]> Plan(UnitySet set, Dictionary<string, Il2CppText> metadata, Dictionary<string, string> translations)
    {
        var metadataIds = metadata.SelectMany(m => m.Value.Units(m.Key)).Select(t => t.id).ToHashSet(StringComparer.Ordinal);
        return TmpFallback.Plan(set, translations.Where(t => !metadataIds.Contains(t.Key)).ToDictionary(t => t.Key, t => t.Value),
            metadata.SelectMany(m => m.Value.Changed(m.Key, translations)));
    }

    public static object Build(JsonNode r)
    {
        var translations = Translations(r); string directory = Str(r, "sourceDirectory"), output = Str(r, "outputDirectory");
        var files = Files(r);
        using var set = new UnitySet(directory, AssetFiles(files));
        var metadata = files.Where(Il2CppText.IsPath).ToDictionary(p => p, p => Metadata(directory, p));
        var plan = Plan(set, metadata, translations);
        foreach (var p in files)
        {
            string destination = Path.Combine(output, p); Directory.CreateDirectory(Path.GetDirectoryName(destination)!);
            if (metadata.TryGetValue(p, out var text)) { File.WriteAllBytes(destination, text.Rebuild(p, translations)); continue; }
            var asset = set.Assets[p]; bool changed = false;
            foreach (dynamic info in asset.Infos)
                if (plan.TryGetValue((p, (long)info.PathId), out byte[]? b) && !b.AsSpan().SequenceEqual((byte[])asset.Raw(info)))
                { info.SetNewData(b); changed = true; }
            if (changed) asset.Save(destination); else File.Copy(Path.Combine(directory, p), destination, true);
        }
        return Reply("build", new { files, warnings = CoverageWarnings, launchExecutable = (string?)null });
    }

    public static object Validate(JsonNode r)
    {
        var issues = new List<object>();
        try
        {
            string source = Str(r, "sourceDirectory"), output = Str(r, "outputDirectory");
            var files = Files(r); if (output.Length == 0) output = source;
            using var original = new UnitySet(source, AssetFiles(files));
            using var built = new UnitySet(output, AssetFiles(files));
            var metadata = files.Where(Il2CppText.IsPath).ToDictionary(p => p, p => Metadata(source, p));
            var current = ReadUnits(built).Concat(metadata.Keys.SelectMany(p => Metadata(output, p).Units(p)))
                .ToDictionary(t => t.id, t => t.sourceText, StringComparer.Ordinal);
            foreach (var t in Translations(r)) current[t.Key] = t.Value;
            var plan = Plan(original, metadata, current);
            foreach (var p in files)
            {
                if (metadata.TryGetValue(p, out var text))
                {
                    if (!text.Rebuild(p, current).AsSpan().SequenceEqual(File.ReadAllBytes(Path.Combine(output, p))))
                        throw new InvalidDataException("IL2CPP 元数据与预期回填不符：" + p);
                    continue;
                }
                var src = original.Assets[p]; var dst = built.Assets[p];
                var infos = dst.Infos.ToDictionary(i => (long)i.PathId);
                if (infos.Count != src.Infos.Count()) throw new InvalidDataException("Unity 对象数量变化：" + p);
                foreach (dynamic i in src.Infos)
                {
                    long id = i.PathId;
                    if (!infos.TryGetValue(id, out dynamic? j) || (int)j.TypeId != (int)i.TypeId) throw new InvalidDataException("Unity 对象标识变化");
                    byte[] expected = plan.TryGetValue((p, id), out byte[]? patch) ? patch : src.Raw(i);
                    if (!expected.AsSpan().SequenceEqual((byte[])dst.Raw(j))) throw new InvalidDataException("资源对象与预期回填不符：" + p + ":" + id);
                }
            }
        }
        catch (Exception e) { issues.Add(new { code = "resource-mismatch", message = e.Message, unitId = (string?)null }); }
        return Reply("validation", new { passed = issues.Count == 0, issues, runtimeStatus = "not-tested" });
    }

    public static void Main()
    {
        Console.InputEncoding = Console.OutputEncoding = new System.Text.UTF8Encoding(false);
        JsonNode? r = null;
        try
        {
            r = JsonNode.Parse(Console.In.ReadToEnd())!;
            object result = Str(r, "operation") switch {
                "probe" => Probe(r), "extract" => Extract(r), "build" => Build(r), "validate" => Validate(r), _ => throw new Exception("未知操作")
            };
            WriteReply(r, result);
        }
        catch (Exception e) { WriteReply(r, new { protocolVersion = 1, success = false, error = "Unity 资源处理失败：" + e.Message }); }
    }
    static void WriteReply(JsonNode? request, object result)
    {
        string json = JsonSerializer.Serialize(result), path = request == null ? "" : Str(request, "responseFile");
        if (path.Length > 0) File.WriteAllText(path, json); else Console.WriteLine(json);
    }
}
