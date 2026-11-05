using GameL10n.Core.Isolation;
using GameL10n.Core.Storage;
using GameL10n.Core.Serialization;

namespace GameL10n.Core.Development;

public sealed record BuiltinParserDependency(int SchemaVersion, IReadOnlyDictionary<string, string> Files,
    string? EntryPoint = null, string? Runtime = null);

public sealed record ResolvedParserTool(string Path, IReadOnlyList<string> Command);

public sealed partial class BuiltinAdapterAssets
{
    public const string SharedDependencyPath = "tools/vntextpatch-reference.json";
    public const string ToolManifestFile = "parser-tool.json";
    private static readonly SemaphoreSlim SharedPreparation = new(1, 1);

    private async Task<BuiltinParserDependency> SharedDependencyAsync(CancellationToken ct)
    {
        var path = Asset(ToolManifestFile);
        if (new FileInfo(path).Length > 64 * 1024) throw new InvalidDataException("共享解析器工具清单过大。");
        var dependency = await LocalFiles.ReadJsonAsync<BuiltinParserDependency>(path,
            EngineJson.TypeInfo<BuiltinParserDependency>(EngineJson.Storage), ct)
            ?? throw new InvalidDataException("共享解析器工具清单为空。");
        ValidateDependency(dependency);
        if (dependency.SchemaVersion != 2) throw new InvalidDataException("发行包缺少完整的进程版解析器工具。");
        foreach (var (name, hash) in dependency.Files)
            if (await LocalFiles.HashFileAsync(Asset(name), ct) != hash)
                throw new InvalidDataException("发行包的解析器工具与清单不一致：" + name);
        return dependency;
    }

    public async Task<string> ResolveSharedAsync(string sharedRoot, BuiltinParserDependency dependency, CancellationToken ct = default)
    {
        var entryPoint = ValidateDependency(dependency);
        var target = WorkspaceFiles.Resolve(sharedRoot, "vntextpatch/" + GeneratedAdapterResources.HashEntries(dependency.Files));
        await SharedPreparation.WaitAsync(ct);
        try
        {
            if (!Directory.Exists(target))
            {
                // Never substitute the application's newer parser for an adapter's pinned version.
                foreach (var (file, hash) in dependency.Files)
                    if (await LocalFiles.HashFileAsync(Asset(file), ct) != hash)
                        throw new InvalidDataException("缺少适配器固定版本的共享 VNTextPatch，请从原发行包恢复共享工具；不会自动升级解析器。");
                var pending = target + ".pending-" + Guid.NewGuid().ToString("N");
                try
                {
                    foreach (var file in dependency.Files.Keys)
                        await GeneratedAdapterResources.CopyAsync(Asset(file), WorkspaceFiles.Resolve(pending, file), ct);
                    foreach (var (file, hash) in dependency.Files)
                        if (await LocalFiles.HashFileAsync(WorkspaceFiles.Resolve(pending, file), ct) != hash)
                            throw new InvalidDataException("复制共享 VNTextPatch 时文件发生变化。");
                    Directory.Move(pending, target);
                }
                finally { GeneratedGameAdapter.DeleteStagedTree(pending); }
            }
            foreach (var (file, hash) in dependency.Files)
                if (await LocalFiles.HashFileAsync(WorkspaceFiles.Resolve(target, file), ct) != hash)
                    throw new InvalidDataException("共享 VNTextPatch 文件缺失或已改变：" + file);
            return WorkspaceFiles.Resolve(target, entryPoint);
        }
        finally { SharedPreparation.Release(); }
    }

    public async Task<ResolvedParserTool> ResolveToolAsync(string sharedRoot, BuiltinParserDependency dependency,
        PinnedDotNetToolchain toolchain, CancellationToken ct = default, string? legacyHostPath = null)
    {
        var path = await ResolveSharedAsync(sharedRoot, dependency, ct);
        if (dependency.SchemaVersion == 2 && dependency.Runtime == "native") return new(path, [path]);
        if (!File.Exists(toolchain.ExecutablePath)) throw new InvalidDataException("缺少固定版本的 .NET 解析器运行环境。");
        if (dependency.SchemaVersion == 2) return new(path, [toolchain.ExecutablePath, path]);
        // Old manifests pin only DLL bytes. Keep their reflection in a separate managed compatibility process.
        var compatibilityHost = legacyHostPath ?? Path.Combine(AppContext.BaseDirectory, "tools", "parser-compat", "GameL10n.ParserCompat.dll");
        if (!File.Exists(compatibilityHost))
            throw new InvalidDataException("此旧适配器需要托管解析器兼容工具，请使用包含 tools/parser-compat 和共享 .NET 运行时的完整发行包；不会替换锁定的解析器。");
        return new(path, [toolchain.ExecutablePath, Path.GetFullPath(compatibilityHost), path]);
    }

    private static string ValidateDependency(BuiltinParserDependency dependency)
    {
        if (dependency.Files is null || dependency.Files.Count is < 3 or > 128
            || dependency.Files.Keys.Distinct(StringComparer.OrdinalIgnoreCase).Count() != dependency.Files.Count
            || dependency.Files.Values.Any(hash => hash is not { Length: 64 } || hash.Any(c => !char.IsAsciiHexDigit(c))))
            throw new InvalidDataException("共享 VNTextPatch 依赖清单无效。");
        foreach (var file in dependency.Files.Keys)
        {
            if (file != GeneratedAdapterResources.Normalize(file) || file.Split('/').Any(part => part is "" or "." or "..")
                || Path.IsPathRooted(file) || file.Contains(':')) throw new InvalidDataException("共享解析器清单含无效路径。");
        }
        if (dependency.SchemaVersion == 1)
        {
            var assembly = dependency.Files.ContainsKey(AssemblyFile) ? AssemblyFile : "AvgTranslator.VnTextPatch.dll";
            if (!new[] { assembly, "VNTextPatch.LICENSE", "VNTextPatch.NOTICE.md" }.ToHashSet(StringComparer.Ordinal).SetEquals(dependency.Files.Keys))
                throw new InvalidDataException("旧版共享 VNTextPatch 依赖清单无效。");
            return assembly;
        }
        if (dependency.SchemaVersion != 2 || dependency.Runtime is not ("dotnet" or "native")
            || dependency.EntryPoint is not { Length: > 0 } entry || !dependency.Files.ContainsKey(entry)
            || !dependency.Files.ContainsKey("VNTextPatch.LICENSE") || !dependency.Files.ContainsKey("VNTextPatch.NOTICE.md"))
            throw new InvalidDataException("共享解析器工具入口或版本无效。");
        if (dependency.Runtime == "dotnet" && (!entry.EndsWith(".dll", StringComparison.Ordinal)
            || !dependency.Files.ContainsKey(Path.ChangeExtension(entry, ".runtimeconfig.json").Replace('\\', '/'))
            || !dependency.Files.ContainsKey(Path.ChangeExtension(entry, ".deps.json").Replace('\\', '/'))))
            throw new InvalidDataException("托管解析器工具缺少运行配置或依赖清单。");
        return entry;
    }
}
