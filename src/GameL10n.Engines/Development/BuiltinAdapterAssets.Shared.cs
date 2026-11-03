using GameL10n.Core.Isolation;
using GameL10n.Core.Storage;

namespace GameL10n.Core.Development;

public sealed record BuiltinParserDependency(int SchemaVersion, IReadOnlyDictionary<string, string> Files);

public sealed partial class BuiltinAdapterAssets
{
    public const string SharedDependencyPath = "tools/vntextpatch-reference.json";
    private static readonly string[] SharedNames = [AssemblyFile, "VNTextPatch.LICENSE", "VNTextPatch.NOTICE.md"];
    private static readonly SemaphoreSlim SharedPreparation = new(1, 1);

    private async Task<BuiltinParserDependency> SharedDependencyAsync(CancellationToken ct)
    {
        var files = new Dictionary<string, string>(StringComparer.Ordinal);
        foreach (var name in SharedNames) files.Add(name, await LocalFiles.HashFileAsync(Asset(name), ct));
        return new(1, files);
    }

    public async Task<string> ResolveSharedAsync(string sharedRoot, BuiltinParserDependency dependency, CancellationToken ct = default)
    {
        var assemblyFile = dependency.Files?.ContainsKey(AssemblyFile) == true ? AssemblyFile : "AvgTranslator.VnTextPatch.dll";
        string[] names = [assemblyFile, "VNTextPatch.LICENSE", "VNTextPatch.NOTICE.md"];
        if (dependency.SchemaVersion != 1 || dependency.Files is null
            || !names.ToHashSet(StringComparer.Ordinal).SetEquals(dependency.Files.Keys)
            || dependency.Files.Values.Any(hash => hash is not { Length: 64 } || hash.Any(c => !char.IsAsciiHexDigit(c))))
            throw new InvalidDataException("共享 VNTextPatch 依赖清单无效。");
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
                    foreach (var file in names)
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
            return Path.Combine(target, assemblyFile);
        }
        finally { SharedPreparation.Release(); }
    }
}
