using System.Globalization;
using System.Text;
using System.Text.Json;
using System.Text.Json.Nodes;
using GameL10n.AdapterComponents;
using GameL10n.Core.Development;
using GameL10n.Core.Validation;

namespace GameL10n.VnTextPatch;

/// <summary>Versioned fixed inspector and font operations. No model, compiler or game execution.</summary>
internal static class AdvancedOperations
{
    internal static async Task<int> RunAsync(string[] args)
    {
        Console.InputEncoding = new UTF8Encoding(false);
        Console.OutputEncoding = new UTF8Encoding(false);
        var output = Console.Out;
        Console.SetOut(Console.Error);
        JsonObject response;
        try
        {
            var request = JsonNode.Parse(await Console.In.ReadToEndAsync())?.AsObject()
                ?? throw new InvalidDataException("请求内容为空。");
            if (request["protocolVersion"]?.GetValue<int>() != 1 || args.Length != 1)
                throw new InvalidDataException("不支持的固定组件协议。");
            var result = args[0] switch
            {
                "component" => Component(request),
                "inspect" => await InspectAsync(request),
                _ => throw new InvalidDataException("未知固定工具命令。")
            };
            response = new() { ["protocolVersion"] = 1, ["success"] = true, ["result"] = result };
        }
        catch (Exception error)
        {
            response = new() { ["protocolVersion"] = 1, ["success"] = false, ["error"] = error.GetBaseException().Message };
            Console.Error.WriteLine(error);
        }
        await output.WriteLineAsync(response.ToJsonString());
        return response["success"]?.GetValue<bool>() == true ? 0 : 1;
    }

    private static async Task<JsonNode?> InspectAsync(JsonObject request)
    {
        var path = Text(request, "path");
        var query = request["query"]?.GetValue<string>() ?? "";
        var offset = request["offset"]?.GetValue<int>() ?? 0;
        var limit = request["limit"]?.GetValue<int>() ?? 4;
        var operation = Text(request, "operation");
        object result = operation switch
        {
            "pe_imports" => await PeImportInspection.InspectAsync(path, query, offset, Math.Min(limit, 40), CancellationToken.None),
            "pe_code" => await PeCodeInspection.InspectAsync(path, request["addressKind"]?.GetValue<string>() ?? "va",
                request["ranges"]!.AsArray().Select(item => new PeCodeRange(Address(Text(item!, "address")), item!["length"]!.GetValue<int>())).ToArray(),
                request["instructionLimit"]?.GetValue<int>() ?? 80, CancellationToken.None, compact: true),
            "unity_schema" => await UnitySchemaInspection.InspectAsync(path, query, offset, Math.Min(limit, 8), CancellationToken.None,
                request["mode"]?.GetValue<string>() ?? "types"),
            "unity_references" => await UnityTextReferenceInspection.InspectAsync(path, Text(request, "assemblyPath"), query,
                request["queryKind"]?.GetValue<string>() ?? "literal", offset, Math.Min(limit, 8), CancellationToken.None),
            "executable_check" => ExecutableChecks.Validate(request["original"]?.GetValue<string>() is { } original
                ? await File.ReadAllBytesAsync(original) : null, await File.ReadAllBytesAsync(path)),
            _ => throw new InvalidDataException("未知静态分析操作。")
        };
        return VnTextPatchJson.Node(result, VnTextPatchJson.Web);
    }

    private static JsonNode Component(JsonObject request)
    {
        var map = request["map"]?.Deserialize(VnTextPatchJson.TypeInfo<Dictionary<int, int>>(VnTextPatchJson.Web)) ?? [];
        var operation = Text(request, "operation");
        switch (operation)
        {
            case "glyph_plan":
                var codec = Cp932GlyphEncoding.Plan(Strings(request, "texts"),
                    request["allowedSlots"]!.Deserialize(VnTextPatchJson.TypeInfo<int[]>(VnTextPatchJson.Web))!,
                    Strings(request, "preservedTexts"), map);
                return new JsonObject { ["map"] = VnTextPatchJson.Node(codec.SlotToUnicode, VnTextPatchJson.Web) };
            case "glyph_encode":
                var encoder = new Cp932GlyphEncoding(map);
                return new JsonObject { ["values"] = VnTextPatchJson.Node(Strings(request, "texts").Select(text => Convert.ToBase64String(encoder.GetBytes(text))).ToArray(), VnTextPatchJson.Web) };
            case "glyph_decode":
                var decoder = new Cp932GlyphEncoding(map);
                return new JsonObject { ["texts"] = VnTextPatchJson.Node(Strings(request, "values").Select(value => decoder.GetString(Convert.FromBase64String(value))).ToArray(), VnTextPatchJson.Web) };
            case "qlie_font":
                QlieFont.Create(Text(request, "input"), Text(request, "output"), Text(request, "family"), map);
                return new JsonObject { ["output"] = Text(request, "output") };
            case "render_glyphs":
                if (!OperatingSystem.IsWindows()) throw new PlatformNotSupportedException("FNL 字形栅格化需要 Windows GDI 和宋体。");
                using (var raster = new GlyphRasterizer())
                {
                    var values = new List<string>();
                    foreach (var item in request["glyphs"]!.AsArray())
                    {
                        var text = Text(item!, "character");
                        var width = item!["width"]!.GetValue<int>();
                        var height = item["height"]!.GetValue<int>();
                        if (text.Length != 1 || char.IsSurrogate(text[0]) || width is < 1 or > 512 || height is < 1 or > 512)
                            throw new InvalidDataException("FNL 字形只接受单个 BMP 字符及 1–512 的有效尺寸。");
                        values.Add(Convert.ToBase64String(raster.Render(text[0], width, height, Convert.FromBase64String(Text(item, "reference")))));
                    }
                    return new JsonObject { ["values"] = VnTextPatchJson.Node(values.ToArray(), VnTextPatchJson.Web) };
                }
            default: throw new InvalidDataException("未知编码或字体组件操作：" + operation);
        }
    }

    private static string Text(JsonNode request, string key) => request[key]?.GetValue<string>() ?? throw new InvalidDataException("缺少字段：" + key);
    private static string[] Strings(JsonNode request, string key) => request[key]?.Deserialize(VnTextPatchJson.TypeInfo<string[]>(VnTextPatchJson.Web)) ?? [];
    private static ulong Address(string value) => value.StartsWith("0x", StringComparison.OrdinalIgnoreCase)
        ? ulong.Parse(value.AsSpan(2), NumberStyles.HexNumber, CultureInfo.InvariantCulture) : ulong.Parse(value, CultureInfo.InvariantCulture);
}
