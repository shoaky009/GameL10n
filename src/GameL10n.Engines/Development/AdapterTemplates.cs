using GameL10n.Core.Adapters;

namespace GameL10n.Core.Development;

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
            using var stream = typeof(AdapterTemplates).Assembly.GetManifestResourceStream("GameL10n.Templates.ScriptComponentAdapter.cs")
                ?? throw new InvalidDataException("适配器组合模板入口缺失。");
            using var reader = new StreamReader(stream);
            return reader.ReadToEnd();
        }
    }
}
