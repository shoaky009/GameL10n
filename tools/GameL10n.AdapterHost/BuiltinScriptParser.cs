using System.Reflection;
using System.Text.Json.Nodes;

internal static class BuiltinScriptParser
{
    private static MethodInfo? execute;
    public static string Format { get; private set; } = "";

    public static void Configure(JsonObject request, string format)
    {
        Format = format;
        var path = request["toolPaths"]?["vnTextPatch"]?.GetValue<string>()
            ?? throw new InvalidDataException("缺少固定版本的共享脚本解析器。");
        execute = Assembly.LoadFrom(path).GetType("GameL10n.VnTextPatch.Worker")?.GetMethod("ExecuteJson",
            BindingFlags.Public | BindingFlags.Static, null, [typeof(string)], null)
            ?? throw new InvalidDataException("内置脚本解析器接口缺失。");
    }

    public static string ExecuteJson(string input)
    {
        var request = JsonNode.Parse(input)!.AsObject();
        if (request["operation"]?.GetValue<string>() == "probe")
            throw new InvalidDataException("probe 只能使用文件头或有界范围读取，不能调用整文件脚本解析器。");
        request["format"] = Format;
        return (string)(execute?.Invoke(null, [request.ToJsonString()]) ?? throw new InvalidDataException("脚本解析器没有返回结果。"));
    }
}
