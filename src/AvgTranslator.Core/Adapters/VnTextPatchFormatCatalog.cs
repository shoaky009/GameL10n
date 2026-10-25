namespace AvgTranslator.Core.Adapters;

/// <summary>A registered game script parser and the loose resources considered during discovery.</summary>
public sealed record VnTextPatchFormat(string Key, AdapterFamily Family, string ParserType,
    IReadOnlyList<string> Extensions, IReadOnlyList<string> CompanionFileNames);

public static class VnTextPatchFormatCatalog
{
    public const string Version = "1.0.1";
    public const string UpstreamCommit = "e68e174f3db6181e03fc7fabfa7570e1b293d148";

    // Empty extensions select extensionless files. Aliases still require the worker's format checks.
    public static IReadOnlyList<VnTextPatchFormat> All { get; } =
    [
        Format("AdvHd", "AdvHD / WS2", "AdvHd", [".ws2"]),
        Format("Age", "ArcGameEngine / BIN", "ArcGameEngine", [".bin"], ["CHINIT.BIN"]),
        Format("ArtemisAsb", "Artemis / ASB", "Artemis", [".asb"]),
        Format("ArtemisAst", "Artemis / AST", "Artemis", [".ast"]),
        Format("ArtemisTxt", "Artemis / text", "Artemis", [".art", ".txt"]),
        Format("CatSystem", "CatSystem2 / CST", null, [".cst"]),
        Format("CSystem", "Cyberworks C,system / A0", null, [".a0"]),
        Format("Ethornell", "Ethornell / BGI", "Ethornell", [".bgi", ""]),
        Format("Kaguya", "KaGuYa / message.dat", null, [".dat"]),
        Format("KirikiriKs", "Kirikiri / KS", "Kirikiri", [".ks", ".txt"]),
        Format("KirikiriSoc", "Kirikiri / SOC", "Kirikiri", [".soc"]),
        Format("KirikiriTjs", "Kirikiri / compiled TJS", "Kirikiri", [".tjs"]),
        Format("Majiro", "Majiro / MJO", "Majiro", [".mjo"]),
        Format("Musica", "Musica / SC", null, [".sc"]),
        Format("Mware", "Mware / NUT", "Mware", [".nut"]),
        Format("Propeller", "Propeller / Stuff Script Engine / MSC", "Propeller", [".msc"]),
        Format("Qlie", "QLIE / S", null, [".s"]),
        Format("RealLive", "RealLive / scenario", "RealLive", [".rl", ".txt"]),
        Format("Renpy", "Ren'Py / RPY", null, [".rpy"]),
        Format("ShSystem", "ShSystem / HST", "ShSystem", [".hst"]),
        Format("SilkysMap", "Silky's / AI6WIN / MAP", "Silkys", [".map"]),
        Format("SilkysMes", "Silky's / AI6WIN / MES", "Silkys", [".mes"]),
        Format("Softpal", "Softpal / SRC + DAT", "Softpal", [".src"], ["TEXT.DAT", "POINT.DAT"]),
        Format("SystemNnnDev", "SystemNNN / NNN", "SystemNnn", [".nnn"]),
        Format("SystemNnnRelease", "SystemNNN / SPT", "SystemNnn", [".spt"]),
        Format("TmrHiroAdvSystemCode", "TmrHiroAdvSystem / SRP", "TmrHiroAdvSystem", [".srp"]),
        Format("TmrHiroAdvSystemText", "TmrHiroAdvSystem / text table", "TmrHiroAdvSystem", ["", ".dat", ".txt"]),
        Format("Whale", "Whale / text", null, [".txt", ""]),
        Format("Yuris", "YU-RIS / YBN", "Yuris", [".ybn"], ["ysc.ybn"])
    ];

    private static VnTextPatchFormat Format(string key, string name, string? parserNamespace,
        string[] extensions, string[]? companions = null) =>
        new(key, new AdapterFamily("engine", "vntextpatch-" + key.ToLowerInvariant(), name),
            "VNTextPatch.Shared.Scripts." + (parserNamespace is null ? "" : parserNamespace + ".") + key + "Script",
            extensions, companions ?? []);
}
