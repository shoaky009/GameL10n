using System.Reflection;
using System.Text;
using System.Text.Json.Nodes;

namespace GameL10n.ParserCompat;

/// <summary>Managed compatibility boundary for immutable schema-1 parser DLLs; excluded from Native AOT.</summary>
public static class Program
{
    public static int Main(string[] args)
    {
        Console.InputEncoding = new UTF8Encoding(false);
        Console.OutputEncoding = new UTF8Encoding(false);
        var protocolOutput = Console.Out;
        Console.SetOut(Console.Error);
        JsonObject? request = null;
        JsonObject response;
        try
        {
            var input = Console.In.ReadToEnd();
            request = JsonNode.Parse(input)!.AsObject();
            if (args.Length != 1 || !Path.IsPathFullyQualified(args[0]) || request["protocolVersion"]?.GetValue<int>() != 1)
                throw new InvalidDataException("旧版解析器兼容请求无效。");
            var assembly = Assembly.LoadFrom(args[0]);
            var worker = assembly.GetType("GameL10n.VnTextPatch.Worker") ?? assembly.GetType("AvgTranslator.VnTextPatch.Worker");
            var execute = worker?.GetMethod("ExecuteJson", BindingFlags.Public | BindingFlags.Static,
                null, [typeof(string)], null) ?? throw new InvalidDataException("锁定的旧版解析器缺少调用接口。");
            response = JsonNode.Parse((string)execute.Invoke(null, [input])!)!.AsObject();
        }
        catch (Exception error)
        {
            Console.Error.WriteLine(error);
            response = new() { ["protocolVersion"] = 1, ["success"] = false, ["error"] = error.GetBaseException().Message };
        }
        var result = response.ToJsonString();
        if (request?["responseFile"]?.GetValue<string>() is { } path)
        {
            if (!Path.IsPathFullyQualified(path)) throw new InvalidDataException("响应文件必须使用绝对路径。");
            File.WriteAllText(path, result, new UTF8Encoding(false));
            protocolOutput.WriteLine(response["success"]?.GetValue<bool>() == true ? "ok" : "error");
        }
        else protocolOutput.WriteLine(result);
        return response["success"]?.GetValue<bool>() == true ? 0 : 1;
    }
}
