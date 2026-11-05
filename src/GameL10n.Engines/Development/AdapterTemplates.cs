using GameL10n.Core.Adapters;

namespace GameL10n.Core.Development;

public sealed record AdapterTemplateDefinition(string Id, int Version, string Format, string? ArchiveComponent,
    IReadOnlyList<string> ScriptExtensions, IReadOnlyList<string> ResourcePatterns, AdapterFamily Family);
public sealed record AdapterTemplateConfiguration(int SchemaVersion, string TemplateId, int TemplateVersion, string Format,
    string? ArchiveComponent, string? Filter, IReadOnlyList<string> ScriptExtensions, IReadOnlyList<string> ResourcePatterns);

public static class AdapterTemplates
{
    public const string ConfigurationPath = "tools/adapter-template.json";
    public const string PythonEntry = """
        import json, sys
        from tools.gamel10n_host import invoke, write_response
        request = json.load(sys.stdin)
        response = invoke('template', request)
        write_response(request, response)
        """;
    public const string RpycEntry = "import sys\nfrom tools.renpy_rpyc_adapter import main\nsys.exit(main())\n";
    public const string RpycScriptPath = "tools/renpy_rpyc_adapter.py";
    public const string MixedRenpyEntry = "import sys\nfrom tools.renpy_mixed_adapter import main\nsys.exit(main())\n";
    public const string MixedRenpyScriptPath = "tools/renpy_mixed_adapter.py";
    public static IReadOnlyList<AdapterTemplateDefinition> All { get; } =
    [
        new("loose-renpy", 1, "Renpy", null, [".rpy"], ["**/*.rpy"], new("template", "loose-renpy", "Ren’Py 源脚本")),
        new("loose-kirikiri-ks", 1, "KirikiriKs", null, [".ks"], ["**/*.ks"], new("template", "loose-kirikiri-ks", "Kirikiri KS 源脚本")),
        new("rpa3-renpy", 1, "Renpy", "archive.rpa3", [".rpy"], ["**/*.rpa"], new("template", "rpa3-renpy", "RPA3 + Ren’Py 源脚本")),
        new("loose-renpy-rpyc", 1, "Renpy", null, [".rpyc"], ["**/*.rpyc"], new("template", "loose-renpy-rpyc", "Ren’Py RPYC2 编译脚本")),
        new("rpa3-renpy-rpyc", 1, "Renpy", "archive.rpa3", [".rpyc"], ["**/*.rpa"], new("template", "rpa3-renpy-rpyc", "RPA3 + Ren’Py RPYC2 编译脚本")),
        new("renpy-mixed", 1, "Renpy", "archive.rpa3", [".rpy", ".rpyc"], ["**/*.rpy", "**/*.rpyc", "**/*.rpa"],
            new("template", "renpy-mixed", "Ren’Py 无冲突混合资源")),
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

    internal static string RpycSource
    {
        get
        {
            using var stream = typeof(AdapterTemplates).Assembly.GetManifestResourceStream("GameL10n.Components.renpy_rpyc_adapter.py")
                ?? throw new InvalidDataException("内置 RPYC2 适配入口缺失。");
            using var reader = new StreamReader(stream);
            return reader.ReadToEnd();
        }
    }

    internal static string MixedRenpySource
    {
        get
        {
            using var stream = typeof(AdapterTemplates).Assembly.GetManifestResourceStream("GameL10n.Components.renpy_mixed_adapter.py")
                ?? throw new InvalidDataException("内置 Ren’Py 混合资源入口缺失。");
            using var reader = new StreamReader(stream);
            return reader.ReadToEnd();
        }
    }
}
