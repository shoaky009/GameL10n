using System.Text;
using System.Text.Json;
using AvgTranslator.Core.Adapters;
using AvgTranslator.Core.Isolation;

namespace AvgTranslator.Core.Development;

public sealed record AdapterTemplateDefinition(string Id, int Version, string Format, string? ArchiveComponent,
    IReadOnlyList<string> ScriptExtensions, IReadOnlyList<string> ResourcePatterns, AdapterFamily Family);
public sealed record AdapterTemplateConfiguration(int SchemaVersion, string TemplateId, int TemplateVersion, string Format,
    string? ArchiveComponent, string? Filter, IReadOnlyList<string> ScriptExtensions, IReadOnlyList<string> ResourcePatterns);

public static class AdapterTemplates
{
    public const string ConfigurationPath = "tools/adapter-template.json";
    public static IReadOnlyList<AdapterTemplateDefinition> All { get; } =
    [
        new("loose-renpy", 1, "Renpy", null, [".rpy"], ["**/*.rpy"], new("template", "loose-renpy", "Ren’Py 源脚本")),
        new("loose-kirikiri-ks", 1, "KirikiriKs", null, [".ks"], ["**/*.ks"], new("template", "loose-kirikiri-ks", "Kirikiri KS 源脚本")),
        new("rpa3-renpy", 1, "Renpy", "archive.rpa3", [".rpy"], ["**/*.rpa"], new("template", "rpa3-renpy", "RPA3 + Ren’Py 源脚本")),
        new("xp3-kirikiri-ks", 1, "KirikiriKs", "archive.xp3", [".ks"], ["**/*.xp3"], new("template", "xp3-kirikiri-ks", "无过滤器 XP3 + Kirikiri KS"))
    ];

    public static AdapterTemplateDefinition Find(string id) => All.FirstOrDefault(item => item.Id == id)
        ?? throw new InvalidDataException("未知适配器组合模板：" + id);

    public static string Source
    {
        get
        {
            using var stream = typeof(AdapterTemplates).Assembly.GetManifestResourceStream("AvgTranslator.Templates.ScriptComponentAdapter.cs")
                ?? throw new InvalidDataException("适配器组合模板入口缺失。");
            using var reader = new StreamReader(stream);
            return reader.ReadToEnd();
        }
    }
}

public sealed partial class AdaptationTools
{
    private async Task<object> ComposeAdapterTemplateAsync(JsonElement args, CancellationToken ct)
    {
        var template = AdapterTemplates.Find(Text(args, "id"));
        var patterns = args.TryGetProperty("resourcePatterns", out _) ? Strings(args, "resourcePatterns") : template.ResourcePatterns;
        if (patterns.Count is < 1 or > 16) throw new InvalidDataException("组合模板需要 1～16 条资源发现规则。");
        GeneratedAdapterResources.ValidatePatterns(patterns);
        var filter = Text(args, "filter", "");
        if (template.ArchiveComponent == "archive.xp3" && filter != "none")
            throw new InvalidDataException("XP3 模板要求先独立确认没有过滤器，再明确指定 filter=none。");
        var config = new AdapterTemplateConfiguration(1, template.Id, template.Version, template.Format,
            template.ArchiveComponent, template.ArchiveComponent == "archive.xp3" ? filter : null, template.ScriptExtensions, patterns);
        var files = await BuiltinAssets.PrepareSharedFilesAsync(template.Format, ct);
        files.Add("src/Program.cs", Encoding.UTF8.GetBytes(AdapterTemplates.Source));
        files.Add("src/EngineReferencePython.cs", Encoding.UTF8.GetBytes(EnginePythonBridge.Source));
        files.Add("src/" + AdapterTemplates.ConfigurationPath, JsonSerializer.SerializeToUtf8Bytes(config, Json));
        if (template.ArchiveComponent is not null)
        {
            var prepared = await PythonAsync(ct);
            if (prepared.Dependency.Reference.Kind != "galtransl") throw new InvalidDataException("此模板需要 GalTransl 共享工具库。");
            files.Add("src/" + EngineReferenceToolkit.DependencyPath, JsonSerializer.SerializeToUtf8Bytes(prepared.Dependency, Json));
            files.Add("src/" + EngineArchiveComponents.ScriptPath, Encoding.UTF8.GetBytes(EngineArchiveComponents.Source));
        }
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
            path.EndsWith(".cs", StringComparison.Ordinal) ? "source" : "runtime", "组件组合模板 " + template.Id + " 的入口、配置及固定依赖")).ToArray();
        RegisterReleaseFiles(release);
        return new
        {
            template, files = files.Keys.Order(StringComparer.Ordinal), releaseFiles = session.ReleaseFiles,
            testArguments = new { kind = template.Family.Kind, key = template.Family.Key, name = template.Family.Name, resourcePatterns = patterns },
            limitations = "只连接已声明的源脚本与封包组件。脚本编码沿用解析器；不生成未经当前游戏验证的字体、转区或运行时补丁。",
            nextAction = "暂存与规则匹配的真实资源，用返回的 testArguments 调用完整 test_adapter。模板生成不代表已验证，也不会发布。"
        };
    }
}
