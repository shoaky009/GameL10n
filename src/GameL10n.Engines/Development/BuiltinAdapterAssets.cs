using System.IO.Compression;
using System.Text;
using System.Text.Json;
using GameL10n.Core.Adapters;
using GameL10n.Core.Isolation;

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
        var generated = GeneratedSources(selected);
        return new
        {
            format = selected.Key, name = selected.Family.Name, parserType = selected.ParserType,
            upstreamCommit = VnTextPatchFormatCatalog.UpstreamCommit, extensions = selected.Extensions,
            companionFileNames = selected.CompanionFileNames,
            operations = new[] { "extract", "build", "validate" },
            binaryAvailable = File.Exists(Path.Combine(directory, AssemblyFile)),
            sourceAvailable = sourceError is null,
            sourceBundleIssue = sourceError is null ? null : new
            {
                code = sourceError.Code, asset = sourceError.AssetName, message = sourceError.Message,
                recovery = "源码包故障不代表引擎不受支持。binaryAvailable=true 时，继续用 try_builtin_adapter 验证兼容性，并以 copy_adapter(editableSource=false) 复用二进制；需要修改源码时先恢复源码包。"
            },
            sourceFiles = bundledSources.Select(entry => new { path = entry.Path, bytes = entry.Bytes })
                .Concat(generated.Select(pair => new { path = pair.Key, bytes = (long)Utf8.GetByteCount(pair.Value) }))
                .Concat(References(selected).Select(pair => new { path = pair.Key, bytes = (long)Utf8.GetByteCount(pair.Value) }))
                .OrderBy(item => item.path, StringComparer.Ordinal).ToArray(),
            limitations = Limitations(selected), usage = Usage(selected), example = Example(selected),
            engineGuidance = selected.Key == "Kaguya" ? KaguyaMessage40Guidance.Describe(engineInspection) : null
        };
    }

    public object ReadSource(string format, string path, int offset = 0, int length = 12000)
    {
        if (offset < 0 || length is < 1 or > 24000) throw new ArgumentOutOfRangeException(nameof(length), "源码读取偏移必须非负，长度须为 1–24000 个字符。");
        var selected = FindFormat(format);
        string normalized = path.Replace('\\', '/');
        string content;
        if (GeneratedSources(selected).TryGetValue(normalized, out string? generated)) content = generated;
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
        return new
        {
            format = selected.Key, path = normalized, offset = start, length = count,
            totalLength = content.Length, nextOffset = start + count, truncated = start + count < content.Length,
            content = content.Substring(start, count)
        };
    }

    public async Task<object> CopyAsync(string format, string workspace, bool editableSource, CancellationToken ct = default, ICollection<AdapterReleaseFile>? releaseFiles = null)
    {
        ct.ThrowIfCancellationRequested();
        var selected = FindFormat(format);
        var files = editableSource ? new Dictionary<string, byte[]>(StringComparer.OrdinalIgnoreCase)
        {
            ["src/BuiltinScriptParser.cs"] = Utf8.GetBytes(Bridge(selected))
        } : await PrepareSharedFilesAsync(format, ct);
        if (editableSource)
        {
            foreach (string name in new[] { "VNTextPatch.LICENSE", "VNTextPatch.NOTICE.md" })
                files.Add("src/tools/vntextpatch/" + name, await File.ReadAllBytesAsync(Asset(name), ct));
            using var archive = OpenSources();
            try
            {
                foreach (var entry in SourceEntries(archive, selected))
                {
                    await using var input = entry.Open();
                    using var contents = new MemoryStream();
                    await input.CopyToAsync(contents, ct);
                    files.Add("src/BuiltinVnTextPatch/" + entry.FullName, contents.ToArray());
                }
            }
            catch (Exception error) when (error is IOException or InvalidDataException or UnauthorizedAccessException)
            { throw SourceError(error); }
            foreach (var (path, content) in GeneratedSources(selected).Where(pair => pair.Key != "generated/BuiltinScriptParser.cs"))
                files.Add("src/BuiltinVnTextPatch/" + path, Utf8.GetBytes(content));
        }

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
            releaseFiles?.Add(new(AdapterReleaseFiles.PackagePath(path),
                !path.StartsWith("src/tools/", StringComparison.Ordinal) ? "source"
                    : path == "src/" + SharedDependencyPath ? "runtime" : "license",
                "内置 " + selected.Key + " 解析器实现及依赖"));
        return new
        {
            format = selected.Key, editableSource, files = files.Keys.Order(StringComparer.Ordinal).ToArray(),
            reused = reused.Order(StringComparer.Ordinal).ToArray(), usage = Usage(selected), example = Example(selected),
            limitations = Limitations(selected), directApi = editableSource ? DirectApi(selected) : null,
            librarySourceFiles = editableSource ? files.Keys.Where(path => path.EndsWith(".cs", StringComparison.OrdinalIgnoreCase))
                .Order(StringComparer.Ordinal).ToArray() : null,
            analysisUsage = editableSource
                ? "For run_csharp, use sourceFiles=[your work/Probe.cs entry point, ...librarySourceFiles]. Include the entire returned list (including generated/GlobalUsings.cs); do not rediscover dependencies or include src/Program.cs. Existing parser modifications remain available."
                : "Use the copied binary bridge with test_adapter. For in-process run_csharp probes requiring parser changes, copy editableSource=true and use its librarySourceFiles."
        };
    }

    internal async Task<Dictionary<string, byte[]>> PrepareSharedFilesAsync(string format, CancellationToken ct) => new(StringComparer.OrdinalIgnoreCase)
    {
        ["src/BuiltinScriptParser.cs"] = Utf8.GetBytes(Bridge(FindFormat(format))),
        ["src/" + SharedDependencyPath] = JsonSerializer.SerializeToUtf8Bytes(await SharedDependencyAsync(ct), new JsonSerializerOptions(JsonSerializerDefaults.Web))
    };

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
            && (entry.FullName is "worker/Worker.cs" or "worker/Protocol.cs" or "vendor/Scripts/PlainTextScript.cs" or "vendor/Scripts/Range.cs"
                || entry.FullName.StartsWith("vendor/Util/", StringComparison.Ordinal)
                || entry.FullName.StartsWith("vendor/", StringComparison.Ordinal) && !entry.FullName[7..].Contains('/')
                || (separator < 0 ? entry.FullName == enginePath : entry.FullName.StartsWith(enginePath, StringComparison.Ordinal))))
            .OrderBy(entry => entry.FullName, StringComparer.Ordinal);
    }

    private static string[] Limitations(VnTextPatchFormat format) =>
    [
        "仅提供已解包脚本的提取、回填和复提校验；资源包的解包/重打包、运行时编码补丁、启动代理及字体支持需要由适配器实现并验证。",
        "保留内置解析器的原编码约束；Shift-JIS 无法表示的中文会明确报错，不生成未验证的 sjis_ext.bin。",
        "生成适配器的 probe 只获得最多 65536 字节文件头，必须由 Program 按签名、长度和版本识别；调用桥禁止直接使用整文件解析器执行 probe。",
        "源码模式仅复制目标引擎组和公共工具，支持框架引用及 /unsafe- 编译；没有 Main，必须自行实现协议入口和资源包处理。",
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
        $"在 src/Program.cs 实现独立协议入口。使用 BuiltinScriptParser.ExecuteJson(requestJson) 调用 {format.Key}；调用桥固定 format，返回完整 JSON 字符串。" +
        "先解包到独立工作目录，再传入该目录及相对 resourceFiles；build 的 outputDirectory 必须与 sourceDirectory 分开，随后把结果重新封包并声明所有最终输出。" +
        "二进制模式只登记固定版本的共享 DLL，由宿主提供路径，不复制到项目；无需添加编译引用，不要复制自包含 EXE。需要修改解析器时使用源码模式 src/BuiltinVnTextPatch；优先使用已编译进适配器的源码。" +
        "调用桥不写 responseFile；Program 应按协议把最终响应写到 responseFile，并把诊断写 stderr。";

    private static string Example(VnTextPatchFormat format) => $$"""
        // No Main is supplied. Implement header-only probe in your protocol entry point.
        string parserResponse = BuiltinScriptParser.ExecuteJson(System.Text.Json.JsonSerializer.Serialize(new
        {
            protocolVersion = 1, operation = "extract", format = {{JsonSerializer.Serialize(format.Key)}},
            sourceDirectory = unpackedScriptsDirectory, resourceFiles = scriptRelativePaths
        }));
        // For build, also provide outputDirectory and translationsFile (or translations).
        // Repack output into the game's resource format and return final resource paths.
        """;

    private static string DirectApi(VnTextPatchFormat format) => $$"""
        // Prefer BuiltinScriptParser.ExecuteJson for normal extraction/build (includes untouched-file and re-extraction checks).
        // When editing the parser requires direct calls, use these actual signatures, not string path overloads:
        var parser = new global::{{format.ParserType}}();
        parser.Load(VNTextPatch.Shared.ScriptLocation.FromFilePath(inputPath, {{JsonSerializer.Serialize(format.Key)}}));
        VNTextPatch.Shared.ScriptString[] originals = parser.GetStrings().ToArray();
        // Keep all original strings/types; replace selected entries only. Preserve companions in the unpacked folder.
        parser.WritePatched(updatedStrings, VNTextPatch.Shared.ScriptLocation.FromFilePath(outputPath, {{JsonSerializer.Serialize(format.Key)}}));
        // Independently check original-byte roundtrip, changed strings and every unselected string after reload.
        """;

    // Research notes are visible through inspect_builtin_adapter, but never copied into adapter packages.
    private static Dictionary<string, string> References(VnTextPatchFormat format) => format.Key == "Kaguya"
        ? new(StringComparer.Ordinal) { [KaguyaMessage40Guidance.ReferencePath] = KaguyaMessage40Guidance.Reference }
        : new(StringComparer.Ordinal);

    private static Dictionary<string, string> GeneratedSources(VnTextPatchFormat format) => new(StringComparer.Ordinal)
    {
        ["generated/GlobalUsings.cs"] = "global using System;\nglobal using System.Collections.Generic;\nglobal using System.IO;\nglobal using System.Linq;\nglobal using System.Net.Http;\nglobal using System.Threading;\nglobal using System.Threading.Tasks;\n",
        ["generated/BuiltinScriptParser.cs"] = Bridge(format),
        ["generated/Formats.cs"] = $$"""
            using VNTextPatch.Shared;
            namespace GameL10n.VnTextPatch;
            internal sealed record Format(string Key, Func<IScript> Create, string[] Extensions, string[] Companions)
            {
                public bool Matches(string path) => Extensions.Contains(Path.GetExtension(path), StringComparer.OrdinalIgnoreCase);
                public static Format Get(string key) => All.FirstOrDefault(format => format.Key.Equals(key, StringComparison.OrdinalIgnoreCase))
                    ?? throw new InvalidDataException("This adapter contains only {{format.Key}}.");
                public static readonly Format[] All =
                [new({{JsonSerializer.Serialize(format.Key)}}, () => new global::{{format.ParserType}}(),
                    {{JsonSerializer.Serialize(format.Extensions)}}, {{JsonSerializer.Serialize(format.CompanionFileNames)}})];
            }
            """,
        ["generated/FolderScriptCollection.cs"] = """
            namespace VNTextPatch.Shared.Scripts;
            public sealed class FolderScriptCollection : IScriptCollection
            {
                public FolderScriptCollection(string folderPath, string? extension, string? format = null)
                {
                    FolderPath = folderPath;
                    Extension = extension ?? string.Empty;
                    Format = format;
                }
                public string Name => FolderPath;
                public string FolderPath { get; }
                public string Extension { get; }
                public string? Format { get; }
                public IScript GetTemporaryScript() => (Format is null
                    ? GameL10n.VnTextPatch.Format.All[0] : GameL10n.VnTextPatch.Format.Get(Format)).Create();
                public IEnumerable<string> Scripts => Directory.EnumerateFiles(FolderPath, "*" + Extension, SearchOption.AllDirectories)
                    .Select(path => Path.GetRelativePath(FolderPath, path));
                public bool Exists(string scriptName) => File.Exists(Path.Combine(FolderPath, scriptName));
                public void Add(string scriptName)
                {
                    string path = Path.Combine(FolderPath, scriptName);
                    Directory.CreateDirectory(Path.GetDirectoryName(path)!);
                    using var stream = File.Create(path);
                }
                public void Add(string scriptName, ScriptLocation copyFrom)
                {
                    string path = Path.Combine(FolderPath, scriptName);
                    Directory.CreateDirectory(Path.GetDirectoryName(path)!);
                    File.Copy(copyFrom.ToFilePath(), path, true);
                }
            }
            """
    };

    private static string Bridge(VnTextPatchFormat format) => $$"""
        using System;
        using System.IO;
        using System.Reflection;
        using System.Text.Json.Nodes;
        public static class BuiltinScriptParser
        {
            public const string Format = {{JsonSerializer.Serialize(format.Key)}};
            private static readonly Lazy<MethodInfo> Execute = new(() =>
            {
                const string typeName = "GameL10n.VnTextPatch.Worker";
                Type? worker = typeof(BuiltinScriptParser).Assembly.GetType(typeName);
                if (worker is null)
                {
                    string path = Environment.GetEnvironmentVariable("GAMEL10N_VNTEXTPATCH")
                        ?? Path.Combine(AppContext.BaseDirectory, "tools", "vntextpatch", "GameL10n.VnTextPatch.dll");
                    worker = Assembly.LoadFrom(path).GetType(typeName);
                }
                return worker?.GetMethod("ExecuteJson", BindingFlags.Public | BindingFlags.Static, null, new[] { typeof(string) }, null)
                    ?? throw new InvalidOperationException("The built-in parser API is missing.");
            });
            public static string ExecuteJson(string requestJson)
            {
                JsonObject request = JsonNode.Parse(requestJson)?.AsObject() ?? throw new InvalidDataException("Expected a JSON request.");
                if (request["operation"]?.GetValue<string>() == "probe")
                    throw new InvalidOperationException("Implement probe using the available 65536-byte headers; the script parser requires complete files.");
                request["format"] = Format;
                return (string)(Execute.Value.Invoke(null, new object[] { request.ToJsonString() })
                    ?? throw new InvalidDataException("The parser did not return a response."));
            }
        }
        """;
}
