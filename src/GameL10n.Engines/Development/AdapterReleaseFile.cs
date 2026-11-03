using GameL10n.Core.Isolation;

namespace GameL10n.Core.Development;

/// <summary>One explicitly selected release input, using its final package-relative path.</summary>
public sealed record AdapterReleaseFile(string Path, string Role, string Purpose);

internal static class AdapterReleaseFiles
{
    internal static AdapterReleaseFile[] Validate(IReadOnlyList<AdapterReleaseFile>? files, bool requireSource = true)
    {
        if (files is not { Count: > 0 and <= 512 })
            throw new GeneratedAdapterException("发布清单需包含 1 到 512 项。请用 update_release_files 登记必要文件，或通过 releaseFiles 整体指定；分析脚本、样本和报告不发布。");
        var paths = new HashSet<string>(StringComparer.OrdinalIgnoreCase);
        foreach (var file in files)
        {
            if (file is null || string.IsNullOrWhiteSpace(file.Path) || file.Path.Length > 512
                || file.Path.Contains('\\') || file.Path.Split('/').Any(part => part is "" or "." or "..")
                || string.IsNullOrWhiteSpace(file.Purpose) || file.Purpose.Length > 240)
                throw new GeneratedAdapterException("发布文件必须使用规范的相对路径，并说明不超过 240 字符的用途。");
            _ = WorkspaceFiles.Resolve(System.IO.Path.GetTempPath(), file.Path);
            if (!paths.Add(file.Path)) throw new GeneratedAdapterException("发布文件路径重复：" + file.Path);
            var source = file.Role == "source" && file.Path.StartsWith("src/", StringComparison.Ordinal)
                && !file.Path.StartsWith("src/tools/", StringComparison.OrdinalIgnoreCase)
                && file.Path.EndsWith(".cs", StringComparison.OrdinalIgnoreCase);
            var dependency = file.Role is "runtime" or "asset" or "license"
                && file.Path.StartsWith("tools/", StringComparison.Ordinal);
            if (!source && !dependency)
                throw new GeneratedAdapterException("发布角色或路径无效：" + file.Path
                    + "。source 使用 src/*.cs；runtime、asset、license 使用 tools/ 下的明确文件，不允许通配符。");
        }
        if (requireSource && !files.Any(file => file.Role == "source")) throw new GeneratedAdapterException("发布清单至少需要一个 source 源文件。");
        return files.OrderBy(file => file.Path, StringComparer.Ordinal).ToArray();
    }

    internal static string SourcePath(string sourceDirectory, AdapterReleaseFile file) =>
        WorkspaceFiles.Resolve(sourceDirectory, file.Role == "source" ? file.Path["src/".Length..] : file.Path);

    internal static string PackagePath(string workspacePath) => workspacePath.StartsWith("src/tools/", StringComparison.Ordinal)
        ? workspacePath["src/".Length..] : workspacePath;
}
