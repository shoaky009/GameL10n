using System.Text;
using System.Text.Json;
using AvgTranslator.Core.Isolation;

namespace AvgTranslator.Core.Development;

public sealed partial class AdaptationTools
{
    private EngineReferenceToolkit? engineToolkit;
    private EngineReferenceToolkit EngineToolkit => engineToolkit ??= new(Path.Combine(store.RootDirectory, "engine-tools"), researchClient, reportProgress);
    private string? toolkitFailure;
    private Task<string?> AnalysisParserPathAsync(CancellationToken ct) => session.ReleaseFiles is not null
        && !session.ReleaseFiles.Any(file => file.Path == BuiltinAdapterAssets.SharedDependencyPath)
        ? Task.FromResult<string?>(null) : EngineToolkit.ResolveVnTextPatchAsync(SourceDirectory, ct, BuiltinAssets);

    private async Task<EngineReferencePin> ReferenceAsync(bool retry, CancellationToken ct)
    {
        if (toolkitFailure is not null && !retry)
            throw new InvalidDataException("共享引擎工具准备失败，当前会话不自动重复准备：" + toolkitFailure
                + "。可继续使用内置解析器或在线参考资料；恢复完整发行包的工具文件后 inspect_engine_reference(retryDownload=true)。");
        try
        {
            var dependency = await SourceDependencyAsync(ct);
            var selected = dependency?.Reference.Kind == "galtransl" ? dependency.Reference
                : session.EngineReference?.Kind == "galtransl" ? session.EngineReference : null;
            var pin = await EngineToolkit.EnsureReferenceAsync(selected, ct);
            session.EngineReference = pin;
            toolkitFailure = null;
            return pin;
        }
        catch (Exception error) when (error is HttpRequestException or IOException
            || error is OperationCanceledException && !ct.IsCancellationRequested)
        { toolkitFailure = Limit(error.Message, 500); throw; }
    }

    private async Task<object> InspectEngineReferenceAsync(JsonElement args, CancellationToken ct)
    {
        var pin = await ReferenceAsync(args.TryGetProperty("retryDownload", out var retry) && retry.ValueKind == JsonValueKind.True, ct);
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
        return new
        {
            engine, revision = pin.Revision, path,
            publicSymbols = selected.TryGetProperty("public_symbols", out var symbols) ? symbols.Clone() : (JsonElement?)null,
            pythonModules = ReferenceStrings(selected, "python_references").ToArray(),
            content = found < 0 ? "" : content[offset..end], queryFound = found >= 0,
            offset, nextOffset = end, more = end < content.Length, totalCharacters = content.Length,
            usage = "先用 work/*.py 小样本调用 python.engines / python.archives 的现成函数，run_engine_python 自动提供共享解释器和库。"
                + "模块接口按引擎和版本不同，请按 publicSymbols/指南定位，不要把整个算法改写为 C#。"
                + "只补资源选择、容器与脚本连接及版本差异。需要正式复用时 use_engine_reference 生成固定版本依赖和 C# 调用桥，"
                + "将 JSON CLI 连接脚本放到 src/tools/*.py，再通过 test_adapter 验收。可选第三方模块不自动安装，缺依赖需明确报告。"
        };
    }

    private async Task<(EnginePythonDependency Dependency, EnginePythonPaths Paths)> PythonAsync(CancellationToken ct)
    {
        var dependency = await SourceDependencyAsync(ct);
        var pin = dependency?.Reference.Kind == "siglus"
            ? await EngineToolkit.EnsureReferenceAsync(dependency.Reference, ct) : await ReferenceAsync(false, ct);
        try { return await EngineToolkit.EnsurePythonAsync(pin, dependency, ct); }
        catch (Exception error) when (error is HttpRequestException or IOException
            || error is OperationCanceledException && !ct.IsCancellationRequested)
        { toolkitFailure = Limit(error.Message, 500); throw; }
    }

    private Task<EnginePythonDependency?> SourceDependencyAsync(CancellationToken ct) =>
        AvgTranslator.Core.Storage.LocalFiles.ReadJsonAsync<EnginePythonDependency>(
            WorkspaceFiles.Resolve(SourceDirectory, EngineReferenceToolkit.DependencyPath), ct);

    private async Task<object> RunEnginePythonAsync(JsonElement args, CancellationToken ct)
    {
        var relative = Text(args, "script").Replace('\\', '/');
        if (!(relative.StartsWith("work/", StringComparison.Ordinal) || relative.StartsWith("src/tools/", StringComparison.Ordinal))
            || !relative.EndsWith(".py", StringComparison.OrdinalIgnoreCase))
            throw new InvalidDataException("Python 脚本必须在 work/ 或 src/tools/ 中。");
        var script = WorkspaceFiles.Resolve(workspace, relative);
        if (!File.Exists(script)) throw new FileNotFoundException("请先写入 Python 连接脚本。", relative);
        var arguments = args.TryGetProperty("arguments", out _) ? Strings(args, "arguments") : [];
        if (arguments.Count > 128 || arguments.Sum(value => value.Length) > 64 * 1024)
            throw new InvalidDataException("Python 参数过大。");
        var prepared = await PythonAsync(ct);
        reportProgress?.Invoke("分析游戏资源", "解析工具已就绪，正在运行资源分析脚本。");
        var result = await compiler.Runner.RunAsync(new(prepared.Paths.Python,
            EngineReferenceToolkit.PythonArguments(prepared.Paths, script, arguments), workspace, Text(args, "stdin", ""),
            TimeSpan.FromSeconds(Number(args, "timeoutSeconds", 120, 1, 600)), 1024 * 1024), ct);
        return new { revision = prepared.Dependency.Reference.Revision, result.ExitCode,
            stdout = Limit(result.StandardOutput, 12000), stderr = Limit(result.StandardError, 6000) };
    }

    private async Task<object> UseEngineReferenceAsync(JsonElement args, CancellationToken ct)
    {
        var prepared = await PythonAsync(ct);
        var files = new Dictionary<string, string>
        {
            ["src/" + EngineReferenceToolkit.DependencyPath] = JsonSerializer.Serialize(prepared.Dependency, Json),
            ["src/EngineReferencePython.cs"] = EnginePythonBridge.Source
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
        RegisterReleaseFiles(files.Keys.Select(path => new AdapterReleaseFile(AdapterReleaseFiles.PackagePath(path),
            path.StartsWith("src/tools/", StringComparison.Ordinal) ? "runtime" : "source", "共享引擎工具链连接与规则")));
        return new { files = files.Keys, prepared.Dependency,
            example = "Program.cs: return EngineReferencePython.Run(\"tools/adapter.py\");",
            protocol = "tools/adapter.py reads one JSON request from stdin and writes the standard adapter response to request['responseFile']. "
                + "Use from python.engines.<module> import ... and the existing sourceDirectory/resourceFiles/probeRanges. "
                + "The host supplies pinned paths in toolPaths, including after publication. Never hardcode data paths or download at runtime. "
                + "Only source/license data and a shared interpreter are provided; engine-specific decoding, fonts and runtime still need verification."
        };
    }
}

public static class EnginePythonBridge
{
    public static string Source => """
        using System;
        using System.Diagnostics;
        using System.IO;
        using System.Text;
        using System.Text.Json;
        using System.Threading.Tasks;

        internal static class EngineReferencePython
        {
            public static int Run(string relativeScript)
            {
                using var input = new StreamReader(Console.OpenStandardInput(), new UTF8Encoding(false, true));
                var request = input.ReadToEnd();
                return Run(relativeScript, request);
            }
            public static int Run(string relativeScript, string request)
            {
                using var json = JsonDocument.Parse(request);
                var paths = json.RootElement.GetProperty("toolPaths");
                var root = Path.GetFullPath(AppContext.BaseDirectory).TrimEnd(Path.DirectorySeparatorChar) + Path.DirectorySeparatorChar;
                var script = Path.GetFullPath(Path.Combine(root, relativeScript));
                if (!script.StartsWith(root, StringComparison.OrdinalIgnoreCase))
                    throw new InvalidDataException("Python entry point must be inside the adapter package.");
                var start = new ProcessStartInfo(paths.GetProperty("python").GetString()!)
                {
                    UseShellExecute = false, CreateNoWindow = true,
                    RedirectStandardInput = true, RedirectStandardOutput = true, RedirectStandardError = true,
                    StandardInputEncoding = new UTF8Encoding(false), StandardOutputEncoding = Encoding.UTF8,
                    StandardErrorEncoding = Encoding.UTF8
                };
                foreach (var arg in new[] { "-I", "-B", "-X", "utf8", "-c", BOOTSTRAP,
                    paths.GetProperty("engineLibrary").GetString()!, script }) start.ArgumentList.Add(arg);
                using var process = Process.Start(start)!;
                var stdout = process.StandardOutput.BaseStream.CopyToAsync(Console.OpenStandardOutput());
                var stderr = process.StandardError.BaseStream.CopyToAsync(Console.OpenStandardError());
                process.StandardInput.Write(request);
                process.StandardInput.Close();
                Task.WhenAll(stdout, stderr, process.WaitForExitAsync()).GetAwaiter().GetResult();
                return process.ExitCode;
            }
        }
        """.Replace("BOOTSTRAP", JsonSerializer.Serialize(EngineReferenceToolkit.Bootstrap), StringComparison.Ordinal);
}
