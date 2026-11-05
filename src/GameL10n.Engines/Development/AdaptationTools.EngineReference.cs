using System.Text;
using System.Text.Json;
using GameL10n.Core.Isolation;
using System.Text.Json.Nodes;
using GameL10n.Core.Serialization;

namespace GameL10n.Core.Development;

public sealed partial class AdaptationTools
{
    private EngineReferenceToolkit EngineToolkit => Development.EngineToolkit;
    private Task<string?> AnalysisParserPathAsync(CancellationToken ct) => session.ReleaseFiles is not null
        && !session.ReleaseFiles.Any(file => file.Path == BuiltinAdapterAssets.SharedDependencyPath)
        ? Task.FromResult<string?>(null) : EngineToolkit.ResolveVnTextPatchAsync(SourceDirectory, ct, BuiltinAssets);

    private async Task<object> InspectEngineReferenceAsync(JsonElement args, CancellationToken ct)
    {
        var pin = await Development.ReferenceAsync(args.TryGetProperty("retryDownload", out var retry) && retry.ValueKind == JsonValueKind.True, ct);
        var engine = Text(args, "engine");
        using var catalog = JsonDocument.Parse(EngineToolkit.Read(pin, "catalog/engines.json"));
        var selected = catalog.RootElement.GetProperty("engines").EnumerateArray()
            .FirstOrDefault(item => Text(item, "id", "").Equals(engine, StringComparison.OrdinalIgnoreCase));
        if (selected.ValueKind == JsonValueKind.Undefined)
            throw new InvalidDataException("请使用 search_engine_references 返回的精确引擎 ID。");
        var path = Text(args, "path", Text(selected, "document"));
        _ = ReferenceUrl(path);
        if (!(path.StartsWith("python/") || path.StartsWith("engines/") || path.StartsWith("guides/")
            || path.StartsWith("catalog/") || path.StartsWith("provenance/") || path.StartsWith("LICENSE")))
            throw new InvalidDataException("请读取工具库中的指南、目录或 Python 源码。");
        var content = EngineToolkit.Read(pin, path);
        var offset = Number(args, "offset", 0, 0, content.Length);
        var length = Number(args, "length", 8000, 1, 16000);
        var query = Text(args, "query", "");
        if (query.Length > 256) throw new InvalidDataException("检索词过长。");
        var found = query.Length == 0 ? offset : content.IndexOf(query, offset, StringComparison.OrdinalIgnoreCase);
        if (found >= 0 && query.Length > 0) offset = Math.Max(offset, found - 240);
        var end = Math.Min(content.Length, offset + length);
        return new JsonObject
        {
            ["engine"] = engine,
            ["revision"] = pin.Revision,
            ["path"] = path,
            ["publicSymbols"] = EngineJson.Node(selected.TryGetProperty("public_symbols", out var symbols) ? symbols.Clone() : (JsonElement?)null, EngineJson.Web),
            ["pythonModules"] = EngineJson.Node(ReferenceStrings(selected, "python_references").ToArray(), EngineJson.Web),
            ["readySupport"] = EngineJson.Node(engine == "kirikiri" ? KirikiriTextGuidance.ScnSupport() : null, EngineJson.Web),
            ["content"] = found < 0 ? "" : content[offset..end],
            ["queryFound"] = found >= 0,
            ["offset"] = offset,
            ["nextOffset"] = end,
            ["more"] = end < content.Length,
            ["totalCharacters"] = content.Length,
            ["usage"] = "先用现成 CLI（run_engine_python 的 module 参数）或 work/*.py 小样本调用 python.engines / python.archives 的现成函数，工具自动提供共享解释器和库。"
                + "模块接口按引擎和版本不同，请按 publicSymbols/指南定位，不要把整个算法改写为 C#。"
                + "只补资源选择、容器与脚本连接及版本差异。需要正式复用时 use_engine_reference 生成固定版本依赖和 Python 组件接口，"
                + "将 JSON CLI 入口放到 src/adapter.py，再通过 test_adapter 验收。可选第三方模块不自动安装，缺依赖需明确报告。"
        };
    }

    private async Task<object> RunEnginePythonAsync(JsonElement args, CancellationToken ct)
    {
        var relative = Text(args, "script", "").Replace('\\', '/');
        var module = Text(args, "module", "");
        if ((relative.Length == 0) == (module.Length == 0))
            throw new InvalidDataException("script 和 module 必须且只能指定一个。");
        string? script = null;
        if (relative.Length > 0)
        {
            if (!(relative.StartsWith("work/", StringComparison.Ordinal) || relative.StartsWith("src/tools/", StringComparison.Ordinal))
                || !relative.EndsWith(".py", StringComparison.OrdinalIgnoreCase))
                throw new InvalidDataException("Python 脚本必须在 work/ 或 src/tools/ 中。");
            script = WorkspaceFiles.Resolve(workspace, relative);
            var normalized = Path.GetRelativePath(workspace, script).Replace('\\', '/');
            if (!normalized.StartsWith("work/", StringComparison.Ordinal) && !normalized.StartsWith("src/tools/", StringComparison.Ordinal))
                throw new InvalidDataException("Python 脚本必须在 work/ 或 src/tools/ 中。");
            if (!File.Exists(script)) throw new FileNotFoundException("请先写入 Python 连接脚本。", relative);
        }
        else if (!(module.StartsWith("python.engines.", StringComparison.Ordinal) || module.StartsWith("python.archives.", StringComparison.Ordinal))
            || module.Split('.').Any(part => part.Length == 0 || !char.IsAsciiLetter(part[0]) && part[0] != '_'
                || part.Any(character => !char.IsAsciiLetterOrDigit(character) && character != '_')))
            throw new InvalidDataException("module 必须是固定工具库内 python.engines / python.archives 的完整模块名。");
        var arguments = args.TryGetProperty("arguments", out _) ? Strings(args, "arguments") : [];
        if (arguments.Count > 128 || arguments.Sum(value => value.Length) > 64 * 1024)
            throw new InvalidDataException("Python 参数过大。");
        var prepared = await Development.PythonAsync(ct);
        if (script is null) _ = EngineToolkit.Read(prepared.Dependency.Reference, module.Replace('.', '/') + ".py");
        var working = Text(args, "workingDirectory", ".");
        working = working == "." ? workspace : WorkspaceFiles.Resolve(workspace, working);
        Directory.CreateDirectory(working);
        reportProgress?.Invoke("分析游戏资源", "解析工具已就绪，正在运行资源分析脚本。");
        AdapterProcessResult result;
        try
        {
            result = await runtime.Runner.RunAsync(new(prepared.Paths.Python,
                script is null ? EngineReferenceToolkit.PythonModuleArguments(prepared.Paths, module, arguments)
                    : EngineReferenceToolkit.PythonArguments(prepared.Paths, script, arguments), working, Text(args, "stdin", ""),
                TimeSpan.FromSeconds(Number(args, "timeoutSeconds", 120, 1, 600)), 1024 * 1024,
                VnTextPatchAssemblyPath: await AnalysisParserPathAsync(ct),
                AdapterHostCommand: EngineReferenceToolkit.AdapterHostCommand(runtime.Toolchain)), ct);
        }
        catch (AdapterProcessLimitException error)
        {
            ct.ThrowIfCancellationRequested();
            throw new AdapterProcessLimitException(error.Message, "run", error.TimedOut,
                error.StandardOutput, error.StandardError, error);
        }
        return new JsonObject
        {
            ["revision"] = prepared.Dependency.Reference.Revision,
            ["exitCode"] = result.ExitCode,
            ["stdout"] = Limit(result.StandardOutput, 12000),
            ["stderr"] = Limit(result.StandardError, 6000)
        };
    }

    private async Task<object> UseEngineReferenceAsync(JsonElement args, CancellationToken ct)
    {
        var prepared = await Development.PythonAsync(ct);
        var files = new Dictionary<string, string>
        {
            ["src/" + EngineReferenceToolkit.DependencyPath] = JsonSerializer.Serialize(prepared.Dependency, EngineJson.TypeInfo<EnginePythonDependency>(Json)),
            ["src/" + EnginePythonBridge.HelperPath] = EnginePythonBridge.HelperSource
        };
        foreach (var (path, content) in files)
        {
            var target = WorkspaceFiles.Resolve(workspace, path);
            if (File.Exists(target) && await File.ReadAllTextAsync(target, ct) != content)
                throw new IOException("保留已有修改，未覆盖文件：" + path);
        }
        session.VerifiedPackage = null;
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
            ["example"] = "src/adapter.py: import json, sys; request = json.load(sys.stdin). Return the standard JSON response through tools.gamel10n_host.write_response(request, response).",
            ["composedParserExample"] = "copy_adapter 复用内置解析器后，from tools.gamel10n_host import parser; "
                + "response = parser(request, format='Renpy') 调用固定解析器；自行实现仅使用文件头或有界范围读取的 probe。组件路径由宿主提供，无需 C# 入口。",
            ["protocol"] = "src/adapter.py reads one JSON request from stdin and writes the standard adapter response to request['responseFile']. "
                + "Use from python.engines.<module> import ... and the existing sourceDirectory/resourceFiles/probeRanges. "
                + "The host supplies pinned paths in toolPaths, including after publication. Never hardcode data paths or download at runtime. "
                + "Only source/license data and a shared interpreter are provided; engine-specific decoding, fonts and runtime still need verification."
        };
    }
}

public static class EnginePythonBridge
{
    public const string HelperPath = "tools/gamel10n_host.py";
    public static string HelperSource
    {
        get
        {
            using var stream = typeof(EnginePythonBridge).Assembly.GetManifestResourceStream("GameL10n.Components.gamel10n_host.py")
                ?? throw new InvalidDataException("Python 组件调用接口缺失。");
            using var reader = new StreamReader(stream);
            return reader.ReadToEnd();
        }
    }

    public static string Source
    {
        get
        {
            using var stream = typeof(EnginePythonBridge).Assembly.GetManifestResourceStream("GameL10n.Components.EngineReferencePython.cs")
                ?? throw new InvalidDataException("Python 调用桥参考源码缺失。");
            using var reader = new StreamReader(stream);
            return reader.ReadToEnd();
        }
    }
}
