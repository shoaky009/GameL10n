using System.Text.Json;
using AvgTranslator.Core.Isolation;

namespace AvgTranslator.Core.Development;

public sealed partial class AdaptationTools
{
    private static AdapterReleaseFile? DeclaredReleaseFile(JsonElement args, string relative)
    {
        if (!args.TryGetProperty("releaseRole", out _)) return null;
        if (!relative.StartsWith("src/", StringComparison.Ordinal))
            throw new InvalidDataException("只有 src/ 中的正式实现或依赖可以登记发布；分析材料保留在 work/。");
        var file = new AdapterReleaseFile(AdapterReleaseFiles.PackagePath(relative), Text(args, "releaseRole"), Text(args, "purpose"));
        AdapterReleaseFiles.Validate([file], requireSource: false);
        return file;
    }

    private void RegisterReleaseFiles(IEnumerable<AdapterReleaseFile> files)
    {
        var additions = files.ToArray();
        if (additions.Length == 0) return;
        AdapterReleaseFiles.Validate(additions, requireSource: false);
        var next = (session.ReleaseFiles ?? session.VerifiedPackage?.ReleaseFiles ?? [])
            .ToDictionary(file => file.Path, StringComparer.OrdinalIgnoreCase);
        foreach (var file in additions) next[file.Path] = file;
        AddDownloadProvenance(next);
        SetReleaseFiles(next.Values.ToArray());
    }

    private void SetReleaseFiles(IReadOnlyList<AdapterReleaseFile> files)
    {
        var next = files.Count == 0 ? [] : AdapterReleaseFiles.Validate(files, requireSource: false);
        var current = session.ReleaseFiles ?? session.VerifiedPackage?.ReleaseFiles ?? [];
        if (!current.OrderBy(file => file.Path, StringComparer.Ordinal).SequenceEqual(next))
            session.VerifiedPackage = null;
        session.ReleaseFiles = next;
    }

    private object UpdateReleaseFiles(JsonElement args)
    {
        var next = (session.ReleaseFiles ?? []).ToDictionary(file => file.Path, StringComparer.OrdinalIgnoreCase);
        if (args.TryGetProperty("files", out var declared))
        {
            var additions = declared.Deserialize<AdapterReleaseFile[]>(Json)
                ?? throw new InvalidDataException("files 必须为发布文件数组。");
            if (additions.Length > 0) AdapterReleaseFiles.Validate(additions, requireSource: false);
            foreach (var file in additions)
            {
                if (!File.Exists(AdapterReleaseFiles.SourcePath(SourceDirectory, file)))
                    throw new FileNotFoundException("发布文件不存在：" + file.Path);
                next[file.Path] = file;
            }
        }
        if (args.TryGetProperty("remove", out _))
            foreach (var path in Strings(args, "remove"))
            {
                _ = WorkspaceFiles.Resolve(workspace, path);
                next.Remove(path);
            }
        AddDownloadProvenance(next);
        SetReleaseFiles(next.Values.ToArray());
        var selected = next.Keys.ToHashSet(StringComparer.OrdinalIgnoreCase);
        var unregistered = Directory.Exists(SourceDirectory)
            ? GeneratedGameAdapter.EnumerateFiles(SourceDirectory)
                .Select(path => (Path: AdapterReleaseFiles.PackagePath("src/" + Path.GetRelativePath(SourceDirectory, path).Replace('\\', '/')), Bytes: new FileInfo(path).Length))
                .Where(file => !selected.Contains(file.Path)).ToArray() : [];
        return new { releaseFiles = session.ReleaseFiles,
            unregisteredCount = unregistered.Length, unregisteredBytes = unregistered.Sum(file => file.Bytes),
            unregistered = unregistered.Take(30).Select(file => new { path = file.Path, bytes = file.Bytes }),
            nextAction = "未登记文件不会发布；只补充确实需要的依赖。分析脚本与样本保留在工作区即可，不必登记或删除。" };
    }

    private void AddDownloadProvenance(Dictionary<string, AdapterReleaseFile> files)
    {
        if (files.Keys.Any(path => path.StartsWith("tools/", StringComparison.Ordinal))
            && File.Exists(Path.Combine(SourceDirectory, "tools", "sources.json")))
            files.TryAdd("tools/sources.json", new("tools/sources.json", "license", "下载依赖的来源与校验记录"));
    }

    private void CheckReleaseGameInputs(GeneratedAdapterPackage package)
    {
        // Known staged originals can be identified even after renaming. This is not a
        // heuristic for executables: custom launchers remain valid release assets.
        var originals = session.ResourceHashes.Values.ToHashSet(StringComparer.OrdinalIgnoreCase);
        var copies = package.ReleaseFiles!.Where(file => originals.Contains(package.PackageFiles![file.Path]))
            .Select(file => file.Path).ToArray();
        if (copies.Length > 0)
            throw new InvalidDataException("正式引擎包包含当前游戏的原文件副本：" + string.Join("、", copies.Take(8))
                + "。从发布清单移除，改由适配器读取当前游戏资源；修改后的 EXE/资源写入补丁输出，不作为引擎依赖携带。");
    }
}
