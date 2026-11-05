using System.IO.Compression;
using System.Text;
using System.Text.Json;
using GameL10n.Core.Adapters;
using GameL10n.Core.Isolation;
using System.Text.Json.Nodes;
using GameL10n.Core.Serialization;

namespace GameL10n.Core.Development;

/// <summary>Exposes the shipped parser sources and copies dependencies into an adapter development workspace.</summary>
public sealed partial class BuiltinAdapterAssets(string? assetsDirectory = null)
{
    private const string AssemblyFile = "GameL10n.VnTextPatch.dll";
    private static readonly UTF8Encoding Utf8 = new(false);
    private readonly string directory = Path.GetFullPath(assetsDirectory ?? Path.Combine(AppContext.BaseDirectory, "tools", "vntextpatch"));

    public object Describe(string format, object? engineInspection = null)
    {
        var selected = FindFormat(format);
        var bundledSources = Array.Empty<(string Path, long Bytes)>();
        BuiltinAdapterAssetException? sourceError = null;
        try
        {
            using var archive = OpenSources();
            bundledSources = SourceEntries(archive, selected).Select(entry => (entry.FullName, entry.Length)).ToArray();
        }
        catch (BuiltinAdapterAssetException error) { sourceError = error; }
        var helper = EnginePythonBridge.HelperSource;
        return new JsonObject
        {
            ["format"] = selected.Key,
            ["name"] = selected.Family.Name,
            ["parserType"] = selected.ParserType,
            ["upstreamCommit"] = VnTextPatchFormatCatalog.UpstreamCommit,
            ["extensions"] = EngineJson.Node((selected.Extensions).ToArray(), EngineJson.Web),
            ["companionFileNames"] = EngineJson.Node((selected.CompanionFileNames).ToArray(), EngineJson.Web),
            ["operations"] = EngineJson.Node(new[] { "extract", "build", "validate" }, EngineJson.Web),
            ["binaryAvailable"] = File.Exists(Path.Combine(directory, AssemblyFile)),
            ["sourceAvailable"] = sourceError is null,
            ["sourceBundleIssue"] = EngineJson.Node(sourceError is null ? null : new JsonObject
            {
                ["code"] = sourceError.Code,
                ["asset"] = sourceError.AssetName,
                ["message"] = sourceError.Message,
                ["recovery"] = "源码包故障不代表引擎不受支持。binaryAvailable=true 时，继续用 try_builtin_adapter 验证兼容性，并以 copy_adapter 复用固定解析器；恢复源码包后可读取结构参考，新增逻辑使用 Python。"
            }, EngineJson.Web),
            ["sourceFiles"] = new JsonArray(bundledSources.Select(entry => (path: entry.Path, bytes: entry.Bytes))
                .Concat([(path: EnginePythonBridge.HelperPath, bytes: (long)Utf8.GetByteCount(helper))])
                .Concat(References(selected).Select(pair => (path: pair.Key, bytes: (long)Utf8.GetByteCount(pair.Value))))
                .OrderBy(item => item.path, StringComparer.Ordinal)
                .Select(item => (JsonNode)new JsonObject { ["path"] = item.path, ["bytes"] = item.bytes }).ToArray()),
            ["limitations"] = EngineJson.Node(Limitations(selected), EngineJson.Web),
            ["usage"] = Usage(selected),
            ["example"] = Example(selected),
            ["engineGuidance"] = EngineJson.Node(selected.Key == "Kaguya" ? KaguyaMessage40Guidance.Describe(engineInspection)
                : KirikiriTextGuidance.AppliesTo(selected.Key) ? KirikiriTextGuidance.Describe() : null, EngineJson.Web)
        };
    }

    public object ReadSource(string format, string path, int offset = 0, int length = 12000)
    {
        if (offset < 0 || length is < 1 or > 24000) throw new ArgumentOutOfRangeException(nameof(length), "源码读取偏移必须非负，长度须为 1–24000 个字符。");
        var selected = FindFormat(format);
        string normalized = path.Replace('\\', '/');
        string content;
        if (normalized == EnginePythonBridge.HelperPath) content = EnginePythonBridge.HelperSource;
        else if (References(selected).TryGetValue(normalized, out string? reference)) content = reference;
        else
        {
            using var archive = OpenSources();
            var entry = SourceEntries(archive, selected).SingleOrDefault(item => item.FullName == normalized)
                ?? throw new InvalidDataException("该路径不在所选解析器的源码索引中，请先读取 inspect_builtin_adapter 返回的 sourceFiles。");
            try
            {
                using var reader = new StreamReader(entry.Open(), Utf8, detectEncodingFromByteOrderMarks: true);
                content = reader.ReadToEnd();
            }
            catch (Exception error) when (error is IOException or InvalidDataException or UnauthorizedAccessException)
            { throw SourceError(error); }
        }
        int start = Math.Min(offset, content.Length);
        int count = Math.Min(length, content.Length - start);
        return new JsonObject
        {
            ["format"] = selected.Key,
            ["path"] = normalized,
            ["offset"] = start,
            ["length"] = count,
            ["totalLength"] = content.Length,
            ["nextOffset"] = start + count,
            ["truncated"] = start + count < content.Length,
            ["content"] = content.Substring(start, count)
        };
    }

    public async Task<object> CopyAsync(string format, string workspace, bool editableSource, CancellationToken ct = default, ICollection<AdapterReleaseFile>? releaseFiles = null)
    {
        ct.ThrowIfCancellationRequested();
        var selected = FindFormat(format);
        if (editableSource)
            throw new InvalidDataException("不再复制可编译的 C# 解析器源码。可用 inspect_builtin_adapter 读取结构参考，新增或修改的适配逻辑请使用 Python。");
        var files = await PrepareSharedFilesAsync(format, ct);

        // Check every destination before creating any files; a conflict must not leave a half-copied source tree.
        string root = Path.GetFullPath(workspace);
        var reused = new List<string>();
        foreach (var (path, content) in files)
        {
            string target = WorkspaceFiles.Resolve(root, path);
            if (Directory.Exists(target)) throw new IOException($"解析器复制目标已是目录：{path}");
            for (string? parent = Path.GetDirectoryName(target); parent is not null && parent.Length >= root.Length; parent = Path.GetDirectoryName(parent))
                if (File.Exists(parent)) throw new IOException($"解析器复制目录已被文件占用：{Path.GetRelativePath(root, parent)}");
            if (!File.Exists(target)) continue;
            byte[] existing = await File.ReadAllBytesAsync(target, ct);
            if (!content.AsSpan().SequenceEqual(existing))
                throw new IOException($"解析器文件已存在且内容不同，未写入任何文件：{path}。请保留现有修改，或选择新的开发工作区。");
            reused.Add(path);
        }
        foreach (var (path, content) in files)
        {
            ct.ThrowIfCancellationRequested();
            if (reused.Contains(path, StringComparer.OrdinalIgnoreCase)) continue;
            string target = WorkspaceFiles.Resolve(root, path);
            Directory.CreateDirectory(Path.GetDirectoryName(target)!);
            await using var output = new FileStream(target, FileMode.CreateNew, FileAccess.Write, FileShare.None);
            await output.WriteAsync(content, ct);
        }
        foreach (var path in files.Keys)
            releaseFiles?.Add(new(AdapterReleaseFiles.PackagePath(path), "runtime", "固定版本内置脚本解析器与 Python 调用接口"));
        return new JsonObject
        {
            ["format"] = selected.Key,
            ["destination"] = "src",
            ["reused"] = EngineJson.Node(reused.ToArray(), EngineJson.Web),
            ["files"] = new JsonArray(files.OrderBy(pair => pair.Key, StringComparer.Ordinal)
                .Select(pair => (JsonNode)new JsonObject { ["path"] = pair.Key, ["bytes"] = pair.Value.Length }).ToArray()),
            ["limitations"] = EngineJson.Node(Limitations(selected), EngineJson.Web),
            ["analysisUsage"] = "from tools.gamel10n_host import parser; response = parser(request, format='" + selected.Key
                + "')。入口为 src/adapter.py，probe 由 Python 检查文件头或有界范围；解析器修改逻辑也使用 Python，不编译 C#。"
        };
    }

    internal async Task<Dictionary<string, byte[]>> PrepareSharedFilesAsync(string format, CancellationToken ct)
    {
        _ = FindFormat(format);
        return new(StringComparer.OrdinalIgnoreCase)
        {
            ["src/" + EnginePythonBridge.HelperPath] = Utf8.GetBytes(EnginePythonBridge.HelperSource),
            ["src/" + SharedDependencyPath] = JsonSerializer.SerializeToUtf8Bytes(await SharedDependencyAsync(ct), EngineJson.TypeInfo<BuiltinParserDependency>(new JsonSerializerOptions(EngineJson.Web)))
        };
    }

    private string Asset(string name)
    {
        string path = WorkspaceFiles.Resolve(directory, name);
        if (!File.Exists(path)) throw AssetError(name, "missing");
        return path;
    }
    private ZipArchive OpenSources()
    {
        const string name = "sources.zip";
        ZipArchive? archive = null;
        try
        {
            archive = ZipFile.OpenRead(WorkspaceFiles.Resolve(directory, name));
            // ZipArchive loads the directory lazily; classify invalid indexes before returning it.
            _ = archive.Entries.Count;
            return archive;
        }
        catch (Exception error) when (error is IOException or InvalidDataException or UnauthorizedAccessException)
        {
            archive?.Dispose();
            throw SourceError(error);
        }
    }

    private static BuiltinAdapterAssetException SourceError(Exception error) => AssetError("sources.zip", error switch
    {
        FileNotFoundException or DirectoryNotFoundException => "missing",
        InvalidDataException => "corrupt",
        _ => "unavailable"
    }, error);

    private static BuiltinAdapterAssetException AssetError(string name, string code, Exception? inner = null) => new(name, code, code switch
    {
        "missing" => $"内置解析器复用资产 {name} 缺失；请从完整发行包恢复 tools/vntextpatch。",
        "corrupt" => $"内置解析器源码包 {name} 已损坏；请重新生成或从完整发行包恢复 tools/vntextpatch/{name}。",
        _ => $"内置解析器源码包 {name} 暂时无法读取；请等待打包完成或解除文件占用、访问限制后重试。"
    }, inner);
    private static VnTextPatchFormat FindFormat(string key) => VnTextPatchFormatCatalog.All.FirstOrDefault(format =>
        format.Key.Equals(key, StringComparison.OrdinalIgnoreCase)) ?? throw new ArgumentException("未知内置脚本格式：" + key, nameof(key));

    private static IEnumerable<ZipArchiveEntry> SourceEntries(ZipArchive archive, VnTextPatchFormat format)
    {
        const string root = "VNTextPatch.Shared.Scripts.";
        string parser = format.ParserType[root.Length..];
        int separator = parser.LastIndexOf('.');
        string enginePath = separator < 0 ? "vendor/Scripts/" + parser + ".cs"
            : "vendor/Scripts/" + parser[..separator].Replace('.', '/') + "/";
        return archive.Entries.Where(entry => entry.FullName.EndsWith(".cs", StringComparison.Ordinal)
            && (entry.FullName is "worker/Worker.cs" or "worker/Protocol.cs" or "worker/Serialization/VnTextPatchJson.cs"
                or "vendor/Scripts/PlainTextScript.cs" or "vendor/Scripts/Range.cs"
                || entry.FullName.StartsWith("vendor/Util/", StringComparison.Ordinal)
                || entry.FullName.StartsWith("vendor/", StringComparison.Ordinal) && !entry.FullName[7..].Contains('/')
                || (separator < 0 ? entry.FullName == enginePath : entry.FullName.StartsWith(enginePath, StringComparison.Ordinal))))
            .OrderBy(entry => entry.FullName, StringComparer.Ordinal);
    }

    private static string[] Limitations(VnTextPatchFormat format) =>
    [
        "仅提供已解包脚本的提取、回填和复提校验；资源包的解包/重打包、运行时编码补丁、启动代理及字体支持需要由适配器实现并验证。",
        KirikiriTextGuidance.AppliesTo(format.Key)
            ? "KS/SOC 支持读取 CP932、带 BOM 的 UTF-8/UTF-16，修改后写 UTF-16；须验证游戏实际脚本加载路径接受该编码，原文无修改仍保留原字节。"
            : "保留内置解析器的原编码约束；Shift-JIS 无法表示的中文会明确报错，不生成未验证的 sjis_ext.bin。",
        "生成适配器的 probe 只获得最多 65536 字节文件头，必须由 Python 按签名、长度和版本识别；调用桥禁止直接使用整文件解析器执行 probe。",
        "解析器源码仅供了解格式；发行包不含 C# 编译器，需要修改的适配逻辑使用 Python，固定解析器通过预编译组件调用。",
        format.Key switch
        {
            "Kaguya" => KaguyaMessage40Guidance.EncodingWarning,
            "Renpy" => "Ren’Py 仅处理单行对白、旁白和菜单；Python、界面及动态表达式不在此解析器范围。",
            "Yuris" => "YSTB 的指令、属性描述、值、行号区分别重置 XOR 游标；属性描述为 12 字节，类型位于 +2，值长度 +4，值区相对偏移 +8。无属性的脚本不含可提取文本，也无法用首属性推断密钥，应保留原字节。命令和属性含义以当前 ysc.ybn 为准；清点文本请复用 YurisCommandList/YurisScenarioScript，勿把资源路径或变量标识作为显示文字。YPF 容器和中文运行支持仍需单独验证。",
            "KirikiriTjs" or "Mware" => "编译脚本沿用上游文本识别规则并排除明显资源路径，试译前需要复核变量和表达式。",
            "TmrHiroAdvSystemText" => "文本表没有引擎签名，只验证记录和文本结构；不能仅凭兼容记录认定游戏引擎。",
            _ => "解析器版本兼容性与游戏实际加载行为仍须针对当前游戏验证。"
        }
    ];

    private static string Usage(VnTextPatchFormat format) =>
        $"在 src/adapter.py 实现协议入口。from tools.gamel10n_host import parser, write_response；parser(request, '{format.Key}') 调用固定解析器并返回响应字典。" +
        "先解包到独立工作目录，再传入该目录及相对 resourceFiles；build 的 outputDirectory 必须与 sourceDirectory 分开，随后把结果重新封包并声明所有最终输出。" +
        "仅登记固定版本的共享 DLL，由预编译宿主加载，不复制到项目。解析器不支持的逻辑使用 Python 实现。" +
        "最终响应用 write_response(request, response) 写入 responseFile；诊断写 stderr。probe 由入口独立实现，不能用整文件解析器猜测截断输入。";

    private static string Example(VnTextPatchFormat format) => $$"""
        from tools.gamel10n_host import parser, write_response
        # Implement bounded probe separately; this full parser handles extract/build/validate.
        response = parser(request, {{JsonSerializer.Serialize(format.Key, EngineJson.TypeInfo<string>(EngineJson.Default))}}, sourceDirectory=unpacked_scripts_directory,
                          resourceFiles=script_relative_paths, responseFile=None)
        # Build also needs outputDirectory and translationsFile (or translations).
        # Repack into the game's resource format and return final resource paths before write_response.
        """;

    private static Dictionary<string, string> References(VnTextPatchFormat format) => format.Key == "Kaguya"
        ? new(StringComparer.Ordinal) { [KaguyaMessage40Guidance.ReferencePath] = KaguyaMessage40Guidance.Reference }
        : KirikiriTextGuidance.AppliesTo(format.Key)
            ? new(StringComparer.Ordinal) { [KirikiriTextGuidance.ReferencePath] = KirikiriTextGuidance.Reference }
            : new(StringComparer.Ordinal);

}
