using System.Text;
using System.Text.Json;
using GameL10n.Core.Adapters;
using GameL10n.Core.Isolation;
using System.Text.Json.Nodes;
using GameL10n.Core.Serialization;

namespace GameL10n.Core.Development;

public sealed partial class AdaptationTools
{
    private async Task<object> InspectSiglusGameAsync(JsonElement args, CancellationToken ct)
    {
        _ = Number(args, "offset", 0, 0, int.MaxValue);
        _ = Number(args, "length", 6000, 1, 12000);
        if (Text(args, "source", "").Length > 256 || Text(args, "query", "").Length > 256)
            throw new InvalidDataException("源码名称或查询词过长。");
        var result = await new SiglusClient(EngineToolkit, runtime.Runner, reportProgress)
            .InvokeAsync("inspect", request.GameDirectory, null, null, args, ct);
        return result.GetProperty("inspection");
    }

    private async Task<object> CopySiglusAsync(CancellationToken ct)
    {
        var pin = await EngineToolkit.EnsureReferenceAsync(SiglusAssets.Pin, ct);
        var prepared = await EngineToolkit.EnsurePythonAsync(pin, null, ct);
        var files = new Dictionary<string, string>
        {
            ["src/" + EngineReferenceToolkit.DependencyPath] = JsonSerializer.Serialize(prepared.Dependency, EngineJson.TypeInfo<EnginePythonDependency>(Json)),
            ["src/adapter.py"] = "import runpy\nrunpy.run_module('tools.adapter', run_name='__main__', alter_sys=True)\n",
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
        Development.RegisterReleaseFiles(files.Keys.Select(path => new AdapterReleaseFile(AdapterReleaseFiles.PackagePath(path),
            path.StartsWith("src/tools/", StringComparison.Ordinal) ? "runtime" : "source", "共享引擎工具链连接与规则")));
        return new JsonObject
        {
            ["files"] = EngineJson.Node((files.Keys).ToArray(), EngineJson.Web),
            ["dependency"] = EngineJson.Node(prepared.Dependency, EngineJson.Web),
            ["resourcePatterns"] = EngineJson.Node(new[] { "Scene.pck", "Gameexe.dat", "*.exe", "*.exe.le.config", "g00/_moji_AN48_*.g00", "g00/_moji_SE48_*.g00" }, EngineJson.Web),
            ["usage"] = "根据已确认的显示调用修改 tools/siglus-rules.json。argument 从 0 开始，精确函数名见 inspect_siglus_game。标准显示与 FKDS 48/441 内置。新模板仍须 test_adapter 验证；未知字库不能仅增加文本规则就视为可用。",
            ["family"] = EngineJson.Node(SiglusAssets.Family, EngineJson.Web)
        };
    }
}
