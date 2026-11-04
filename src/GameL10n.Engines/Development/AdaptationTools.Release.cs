using System.Text.Json;
using GameL10n.Core.Isolation;

namespace GameL10n.Core.Development;

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
        Development.AddDownloadProvenance(next);
        Development.SetReleaseFiles(next.Values.ToArray());
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

}
