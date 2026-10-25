using VNTextPatch.Shared;
using VNTextPatch.Shared.Scripts;
using VNTextPatch.Shared.Scripts.AdvHd;
using VNTextPatch.Shared.Scripts.ArcGameEngine;
using VNTextPatch.Shared.Scripts.Artemis;
using VNTextPatch.Shared.Scripts.Ethornell;
using VNTextPatch.Shared.Scripts.Kirikiri;
using VNTextPatch.Shared.Scripts.Majiro;
using VNTextPatch.Shared.Scripts.Mware;
using VNTextPatch.Shared.Scripts.Propeller;
using VNTextPatch.Shared.Scripts.RealLive;
using VNTextPatch.Shared.Scripts.ShSystem;
using VNTextPatch.Shared.Scripts.Silkys;
using VNTextPatch.Shared.Scripts.Softpal;
using VNTextPatch.Shared.Scripts.SystemNnn;
using VNTextPatch.Shared.Scripts.TmrHiroAdvSystem;
using VNTextPatch.Shared.Scripts.Yuris;

namespace AvgTranslator.VnTextPatch;

internal sealed record Format(string Key, Func<IScript> Create, string[] Extensions, string[] Companions)
{
    public bool Matches(string path) => Extensions.Contains(Path.GetExtension(path), StringComparer.OrdinalIgnoreCase);
    public static Format Get(string key) => All.FirstOrDefault(x => x.Key.Equals(key, StringComparison.OrdinalIgnoreCase))
        ?? throw new InvalidDataException("未找到该引擎的内置脚本解析器。");

    public static readonly Format[] All =
    [
        new("AdvHd", () => new AdvHdScript(), [".ws2"], []),
        new("ArtemisAsb", () => new ArtemisAsbScript(), [".asb"], []),
        new("ArtemisAst", () => new ArtemisAstScript(), [".ast"], []),
        new("ArtemisTxt", () => new ArtemisTxtScript(), [".art", ".txt"], []),
        new("Age", () => new AgeScript(), [".bin"], ["CHINIT.BIN"]),
        new("CatSystem", () => new CatSystemScript(), [".cst"], []),
        new("CSystem", () => new CSystemScript(), [".a0"], []),
        new("Ethornell", () => new EthornellScript(), [".bgi", ""], []),
        new("Kaguya", () => new KaguyaScript(), [".dat"], []),
        new("KirikiriKs", () => new KirikiriKsScript(), [".ks", ".txt"], []),
        new("KirikiriSoc", () => new KirikiriSocScript(), [".soc"], []),
        new("KirikiriTjs", () => new KirikiriTjsScript(), [".tjs"], []),
        new("Majiro", () => new MajiroScript(), [".mjo"], []),
        new("Musica", () => new MusicaScript(), [".sc"], []),
        new("Mware", () => new MwareScript(), [".nut"], []),
        new("Propeller", () => new PropellerScript(), [".msc"], []),
        new("Qlie", () => new QlieScript(), [".s"], []),
        new("RealLive", () => new RealLiveScript(), [".rl", ".txt"], []),
        new("Renpy", () => new RenpyScript(), [".rpy"], []),
        new("ShSystem", () => new ShSystemScript(), [".hst"], []),
        new("SilkysMap", () => new SilkysMapScript(), [".map"], []),
        new("SilkysMes", () => new SilkysMesScript(), [".mes"], []),
        new("Softpal", () => new SoftpalScript(), [".src"], ["TEXT.DAT", "POINT.DAT"]),
        new("SystemNnnDev", () => new SystemNnnDevScript(), [".nnn"], []),
        new("SystemNnnRelease", () => new SystemNnnReleaseScript(), [".spt"], []),
        new("TmrHiroAdvSystemCode", () => new TmrHiroAdvSystemCodeScript(), [".srp"], []),
        new("TmrHiroAdvSystemText", () => new TmrHiroAdvSystemTextScript(), ["", ".dat", ".txt"], []),
        new("Whale", () => new WhaleScript(), [".txt", ""], []),
        new("Yuris", () => new YurisScript(), [".ybn"], ["ysc.ybn"])
    ];
}
