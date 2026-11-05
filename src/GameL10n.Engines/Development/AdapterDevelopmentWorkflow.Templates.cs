using System.Text;
using System.Text.Json;
using GameL10n.Core.Isolation;

namespace GameL10n.Core.Development;

public sealed record AdapterTemplateComposition(AdapterTemplateDefinition Template, IReadOnlyList<string> Files,
    IReadOnlyList<string> ResourcePatterns);

public sealed partial class AdapterDevelopmentWorkflow
{
    public async Task<AdapterTemplateComposition> ComposeAsync(string id, IReadOnlyList<string>? resourcePatterns, string? filter, CancellationToken ct)
    {
        var template = AdapterTemplates.Find(id);
        var patterns = resourcePatterns ?? template.ResourcePatterns;
        if (patterns.Count is < 1 or > 16) throw new InvalidDataException("组合模板需要 1～16 条资源发现规则。");
        GeneratedAdapterResources.ValidatePatterns(patterns);
        if (template.ArchiveComponent == "archive.xp3" && filter != "none")
            throw new InvalidDataException("XP3 模板要求先独立确认没有过滤器，再明确指定 filter=none。");
        var config = new AdapterTemplateConfiguration(1, template.Id, template.Version, template.Format,
            template.ArchiveComponent, template.ArchiveComponent == "archive.xp3" ? filter : null, template.ScriptExtensions, patterns);
        var files = await BuiltinAssets.PrepareSharedFilesAsync(template.Format, ct);
        files.Add("src/adapter.py", Encoding.UTF8.GetBytes(AdapterTemplates.PythonEntry));
        files.Add("src/" + AdapterTemplates.ConfigurationPath, JsonSerializer.SerializeToUtf8Bytes(config, Json));
        var prepared = await PythonAsync(ct);
        if (prepared.Dependency.Reference.Kind != "galtransl") throw new InvalidDataException("此模板需要 GalTransl 共享工具库。");
        files.Add("src/" + EngineReferenceToolkit.DependencyPath, JsonSerializer.SerializeToUtf8Bytes(prepared.Dependency, Json));
        if (template.ArchiveComponent is not null)
            files.Add("src/" + EngineArchiveComponents.ScriptPath, Encoding.UTF8.GetBytes(EngineArchiveComponents.Source));
        // Check the complete composition before writing any parser, entry point or dependency.
        var root = Path.GetFullPath(workspace);
        foreach (var (path, content) in files)
        {
            var target = WorkspaceFiles.Resolve(root, path);
            if (Directory.Exists(target)) throw new IOException("模板文件目标已是目录，未写入任何文件：" + path);
            for (var parent = Path.GetDirectoryName(target); parent is not null && parent.Length >= root.Length; parent = Path.GetDirectoryName(parent))
                if (File.Exists(parent)) throw new IOException("模板目录已被文件占用，未写入任何文件：" + path);
            if (File.Exists(target) && !(await File.ReadAllBytesAsync(target, ct)).AsSpan().SequenceEqual(content))
                throw new IOException("保留已有修改，未写入任何文件：" + path + "。可继续修改现有实现，或在新工作区组合模板。");
        }
        var parser = JsonSerializer.Deserialize<BuiltinParserDependency>(files["src/" + BuiltinAdapterAssets.SharedDependencyPath], Json)!;
        await BuiltinAssets.ResolveSharedAsync(Path.Combine(store.RootDirectory, "engine-tools"), parser, ct);
        session.VerifiedPackage = null;
        foreach (var (path, content) in files)
        {
            ct.ThrowIfCancellationRequested();
            var target = WorkspaceFiles.Resolve(workspace, path);
            if (File.Exists(target)) continue;
            Directory.CreateDirectory(Path.GetDirectoryName(target)!);
            await using var output = new FileStream(target, FileMode.CreateNew, FileAccess.Write, FileShare.None);
            await output.WriteAsync(content, ct);
        }
        var release = files.Keys.Select(path => new AdapterReleaseFile(AdapterReleaseFiles.PackagePath(path),
            path.StartsWith("src/tools/", StringComparison.Ordinal) ? "runtime" : "source", "组件组合模板 " + template.Id + " 的入口、配置及固定依赖")).ToArray();
        RegisterReleaseFiles(release);
        return new(template, files.Keys.Order(StringComparer.Ordinal).ToArray(), patterns);
    }
}
