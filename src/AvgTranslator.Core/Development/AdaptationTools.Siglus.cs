using System.Text;
using System.Text.Json;
using AvgTranslator.Core.Adapters;
using AvgTranslator.Core.Isolation;

namespace AvgTranslator.Core.Development;

public sealed partial class AdaptationTools
{
    private async Task<object> InspectSiglusGameAsync(JsonElement args, CancellationToken ct)
    {
        _ = Number(args, "offset", 0, 0, int.MaxValue);
        _ = Number(args, "length", 6000, 1, 12000);
        if (Text(args, "source", "").Length > 256 || Text(args, "query", "").Length > 256)
            throw new InvalidDataException("源码名称或查询词过长。");
        var result = await new SiglusClient(EngineToolkit, compiler.Runner, reportProgress)
            .InvokeAsync("inspect", request.GameDirectory, null, null, args, ct);
        return result.GetProperty("inspection");
    }

    private async Task<object> CopySiglusAsync(CancellationToken ct)
    {
        var pin = await EngineToolkit.EnsureReferenceAsync(SiglusAssets.Pin, ct);
        var prepared = await EngineToolkit.EnsurePythonAsync(pin, null, ct);
        var files = new Dictionary<string, string>
        {
            ["src/" + EngineReferenceToolkit.DependencyPath] = JsonSerializer.Serialize(prepared.Dependency, Json),
            ["src/EngineReferencePython.cs"] = EnginePythonBridge.Source,
            ["src/Program.cs"] = "return EngineReferencePython.Run(\"tools/adapter.py\");\n",
            ["src/tools/adapter.py"] = SiglusAssets.EntryScript,
            ["src/tools/siglus-rules.json"] = "{\"commandRules\":[]}\n"
        };
        foreach (var (path, content) in files)
        {
            var target = WorkspaceFiles.Resolve(workspace, path);
            if (File.Exists(target) && await File.ReadAllTextAsync(target, ct) != content)
                throw new IOException("保留已有修改，未覆盖文件：" + path);
        }
        foreach (var (path, content) in files)
        {
            var target = WorkspaceFiles.Resolve(workspace, path);
            Directory.CreateDirectory(Path.GetDirectoryName(target)!);
            if (!File.Exists(target)) await File.WriteAllTextAsync(target, content, new UTF8Encoding(false), ct);
        }
        return new { files = files.Keys, prepared.Dependency,
            resourcePatterns = new[] { "Scene.pck", "Gameexe.dat", "*.exe", "*.exe.le.config", "g00/_moji_AN48_*.g00", "g00/_moji_SE48_*.g00" },
            usage = "根据已确认的显示调用修改 tools/siglus-rules.json。argument 从 0 开始，精确函数名见 inspect_siglus_game。标准显示与 FKDS 48/441 内置。新模板仍须 test_adapter 验证；未知字库不能仅增加文本规则就视为可用。",
            family = SiglusAssets.Family };
    }
}
