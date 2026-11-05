using System.IO.Compression;
using System.Security.Cryptography;
using System.Text;
using System.Text.Json;
using GameL10n.Core.Adapters;
using System.Text.Json.Nodes;
using GameL10n.Core.Serialization;

namespace GameL10n.Core.Development;

/// <summary>The upstream wheel and small host bridge are bundled once; published adapters pin the combined archive.</summary>
public static class SiglusAssets
{
    public const string Version = "1.0.0";
    public static AdapterFamily Family { get; } = new("engine", "siglus", "Siglus / Scene.pck");
    private const string UpstreamHash = "709752e8048400d326824729dddb167b21005cd3a7878c5827b370dfae2df23e";
    private static readonly Lazy<byte[]> Archive = new(CreateLibrary);
    public static byte[] Library => Archive.Value;
    public static EngineReferencePin Pin { get; } = CreatePin();
    public const string EntryScript = "import sys\nfrom gamel10n_siglus import main\nsys.exit(main())\n";

    public static object Describe() => new JsonObject
    {
        ["id"] = Family.AdapterId,
        ["version"] = Version,
        ["name"] = Family.Name,
        ["upstream"] = "SiglusSceneScriptUtility 0.5.7 (Unlicense OR 0BSD)",
        ["capabilities"] = EngineJson.Node(new[] { "Scene.pck 解包及重封包", "内嵌源码查询", "字节码参数字符串引用", "变长 UTF-16 回填", "GDI 字体检查及 FKDS 图片字库补字", "独立中文启动入口" }, EngineJson.Web),
        ["templates"] = EngineJson.Node(new[] { "标准 CD_TEXT/CD_NAME", "FKDS 气泡对白" }, EngineJson.Web),
        ["sources"] = EngineJson.Node(new[] { "gamel10n_siglus.py", "siglus_binary.py", "siglus_font.py", "siglus_ssu/pck.py", "siglus_ssu/dat.py", "siglus_ssu/g00.py", "siglus_ssu/disam.py", "siglus_ssu/GEI.py" }, EngineJson.Web),
        ["usage"] = "inspect_siglus_game 直接分析当前游戏，返回结构、已识别文本和待处理调用；source/query 按需读取内嵌源码。"
            + "copy_adapter id=engine.siglus/version=1.0.0 生成固定依赖和 JSON 调用桥。规则放在 src/tools/siglus-rules.json，"
            + "commandRules:[{command:'精确函数名',argument:1,kind:'dialogue'}] 的 argument 从 0 开始。"
            + "仅选择源码或已验证语义确认的显示参数；不要按字符串内容猜测引用。test_adapter 仍须通过。",
        ["limits"] = "引擎版本、显示模板和 EXE 补丁特征分别检查；未识别的调用不自动翻译，游戏实际显示需试译确认。"
    };

    public static object ReadSource(string path, int offset, int length)
    {
        if (!path.EndsWith(".py", StringComparison.Ordinal) || path.Contains("..", StringComparison.Ordinal)
            || path.StartsWith('/') || path.Contains('\\')) throw new InvalidDataException("请从 Siglus 源码索引选择 Python 文件。");
        using var archive = new ZipArchive(new MemoryStream(Library));
        var entry = archive.GetEntry(path) ?? throw new FileNotFoundException("工具库中没有该文件：" + path);
        using var reader = new StreamReader(entry.Open(), Encoding.UTF8);
        var text = reader.ReadToEnd();
        offset = Math.Clamp(offset, 0, text.Length);
        var end = Math.Min(text.Length, offset + Math.Clamp(length, 1, 16000));
        return new JsonObject
        {
            ["path"] = path,
            ["offset"] = offset,
            ["nextOffset"] = end,
            ["more"] = end < text.Length,
            ["content"] = text[offset..end]
        };
    }

    private static EngineReferencePin CreatePin()
    {
        var hash = Convert.ToHexStringLower(SHA256.HashData(Library));
        return new(hash[..40], UpstreamHash, hash, "siglus");
    }

    private static byte[] Read(string name)
    {
        using var stream = typeof(SiglusAssets).Assembly.GetManifestResourceStream("GameL10n.Siglus." + name)
            ?? throw new InvalidDataException("程序缺少内置 Siglus 工具：" + name);
        using var bytes = new MemoryStream();
        stream.CopyTo(bytes);
        return bytes.ToArray();
    }

    private static byte[] CreateLibrary()
    {
        var wheel = Read("upstream.whl");
        if (Convert.ToHexStringLower(SHA256.HashData(wheel)) != UpstreamHash)
            throw new InvalidDataException("内置 Siglus 上游工具校验失败。");
        var patches = JsonSerializer.Deserialize<Dictionary<string, string[][]>>(Read("upstream-patches.json"), EngineJson.TypeInfo<Dictionary<string, string[][]>>(EngineJson.Default))!;
        using var input = new ZipArchive(new MemoryStream(wheel));
        using var output = new MemoryStream();
        using (var zip = new ZipArchive(output, ZipArchiveMode.Create, true))
        {
            foreach (var entry in input.Entries.OrderBy(entry => entry.FullName, StringComparer.Ordinal))
            {
                if (entry.FullName.EndsWith('/')) continue;
                using var source = entry.Open();
                using var bytes = new MemoryStream();
                source.CopyTo(bytes);
                var data = bytes.ToArray();
                if (patches.TryGetValue(entry.FullName, out var replacements))
                {
                    var text = Encoding.UTF8.GetString(data).Replace("\r\r\n", "\n").Replace("\r\n", "\n");
                    foreach (var replacement in replacements)
                    {
                        if (text.Split(replacement[0], StringSplitOptions.None).Length != 2)
                            throw new InvalidDataException("Siglus 上游接口发生变化，无法应用引用跟踪补充。");
                        text = text.Replace(replacement[0], replacement[1], StringComparison.Ordinal);
                    }
                    data = Encoding.UTF8.GetBytes(text);
                }
                Add(entry.FullName, data);
            }
            Add("gamel10n_siglus.py", Read("gamel10n_siglus.py"));
            Add("siglus_binary.py", Read("siglus_binary.py"));
            Add("siglus_font.py", Read("siglus_font.py"));
            void Add(string name, byte[] data)
            {
                var entry = zip.CreateEntry(name, CompressionLevel.Optimal);
                entry.LastWriteTime = new DateTimeOffset(2000, 1, 1, 0, 0, 0, TimeSpan.Zero);
                using var stream = entry.Open();
                stream.Write(data);
            }
        }
        return output.ToArray();
    }
}
