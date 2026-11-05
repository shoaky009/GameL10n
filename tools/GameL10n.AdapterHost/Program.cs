using System.Text;
using System.Text.Json;
using System.Text.Json.Nodes;
using GameL10n.AdapterComponents;

namespace GameL10n.AdapterHost;

/// <summary>Fixed parser/template/font operations used by Python adapters; never compiles adapter sources.</summary>
public static class Program
{
    private static readonly JsonSerializerOptions Json = new(AdapterHostJson.Web);

    public static int Main(string[] args)
    {
        Console.InputEncoding = new UTF8Encoding(false);
        Console.OutputEncoding = new UTF8Encoding(false);
        var output = Console.Out;
        Console.SetOut(Console.Error);
        JsonObject response;
        try
        {
            var input = Console.In.ReadToEnd();
            var request = JsonNode.Parse(input)!.AsObject();
            if (request["protocolVersion"]?.GetValue<int>() != 1 || args.Length != 1)
                throw new InvalidDataException("不支持的组件调用协议。");
            if (args[0] == "component")
                response = new() { ["protocolVersion"] = 1, ["success"] = true, ["result"] = Component(request) };
            else
            {
                var directory = request["adapterDirectory"]?.GetValue<string>() ?? Directory.GetCurrentDirectory();
                var format = args[0] == "template"
                    ? JsonNode.Parse(File.ReadAllText(Path.Combine(directory, "tools", "adapter-template.json")))!["format"]!.GetValue<string>()
                    : request["format"]?.GetValue<string>() ?? throw new InvalidDataException("缺少脚本格式。");
                BuiltinScriptParser.Configure(request, format);
                var result = args[0] switch
                {
                    "parser" => BuiltinScriptParser.ExecuteJson(input),
                    "template" => ScriptComponentAdapter.ExecuteJson(input, directory),
                    _ => throw new InvalidDataException("未知组件操作。")
                };
                response = JsonNode.Parse(result)!.AsObject();
            }
        }
        catch (Exception error)
        {
            Console.Error.WriteLine(error);
            response = new() { ["protocolVersion"] = 1, ["success"] = false, ["error"] = error.GetBaseException().Message };
        }
        output.WriteLine(response.ToJsonString());
        return response["success"]?.GetValue<bool>() == true ? 0 : 1;
    }

    private static JsonNode? Component(JsonObject request)
    {
        var operation = request["operation"]!.GetValue<string>();
        var map = request["map"]?.Deserialize<Dictionary<int, int>>(AdapterHostJson.TypeInfo<Dictionary<int, int>>(Json)) ?? [];
        JsonObject result;
        switch (operation)
        {
            case "glyph_plan":
                var codec = Cp932GlyphEncoding.Plan(request["texts"]!.Deserialize<string[]>(AdapterHostJson.TypeInfo<string[]>(Json))!,
                    request["allowedSlots"]!.Deserialize<int[]>(AdapterHostJson.TypeInfo<int[]>(Json))!, request["preservedTexts"]!.Deserialize<string[]>(AdapterHostJson.TypeInfo<string[]>(Json))!, map);
                result = new JsonObject
                {
                    ["map"] = AdapterHostJson.Node(codec.SlotToUnicode, Json)
                };
                break;
            case "glyph_encode":
                var encoder = new Cp932GlyphEncoding(map);
                result = new JsonObject
                {
                    ["values"] = AdapterHostJson.Node(request["texts"]!.Deserialize<string[]>(AdapterHostJson.TypeInfo<string[]>(Json))!.Select(text => Convert.ToBase64String(encoder.GetBytes(text))).ToArray(), Json)
                };
                break;
            case "glyph_decode":
                var decoder = new Cp932GlyphEncoding(map);
                result = new JsonObject
                {
                    ["texts"] = AdapterHostJson.Node(request["values"]!.Deserialize<string[]>(AdapterHostJson.TypeInfo<string[]>(Json))!.Select(value => decoder.GetString(Convert.FromBase64String(value))).ToArray(), Json)
                };
                break;
            case "qlie_font":
                QlieFont.Create(Text(request, "input"), Text(request, "output"), Text(request, "family"), map);
                result = new JsonObject
                {
                    ["output"] = Text(request, "output")
                };
                break;
            case "render_glyphs":
                if (!OperatingSystem.IsWindows()) throw new PlatformNotSupportedException("FNL 字形栅格化需要 Windows GDI 和宋体。");
                using (var raster = new GlyphRasterizer())
                {
                    var values = new List<string>();
                    foreach (var item in request["glyphs"]!.AsArray())
                    {
                        var text = Text(item!, "character");
                        if (text.Length != 1 || char.IsSurrogate(text[0])) throw new InvalidDataException("FNL 字形只接受单个 BMP 字符。");
                        values.Add(Convert.ToBase64String(raster.Render(text[0], item!["width"]!.GetValue<int>(),
                            item["height"]!.GetValue<int>(), Convert.FromBase64String(Text(item, "reference")))));
                    }
                    result = new JsonObject
                    {
                        ["values"] = AdapterHostJson.Node((values).ToArray(), Json)
                    };
                }
                break;
            default: throw new InvalidDataException("未知编码或字体组件操作：" + operation);
        }
        return result;
    }

    private static string Text(JsonNode node, string key) => node[key]?.GetValue<string>()
        ?? throw new InvalidDataException("缺少字段：" + key);
}
