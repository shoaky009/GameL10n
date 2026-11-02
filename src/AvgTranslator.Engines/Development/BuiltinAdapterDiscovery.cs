using System.Security;
using System.Text;
using AvgTranslator.Core.Adapters;
using AvgTranslator.Core.Isolation;

namespace AvgTranslator.Core.Development;

public sealed record BuiltinAdapterCandidate(string Format, string AdapterId, string Name, string EvidenceKind,
    IReadOnlyList<string> Evidence, IReadOnlyList<string> SampleFiles, IReadOnlyList<string> MissingCapabilities);

public sealed record BuiltinAdapterDiscoveryReport(int Revision, int FilesScanned, bool Partial,
    IReadOnlyList<BuiltinAdapterCandidate> Candidates, IReadOnlyList<string> Warnings);

/// <summary>Bounded, read-only format clues. A candidate is not a successful parser probe or a compatibility claim.</summary>
public static class BuiltinAdapterDiscovery
{
    public const int Revision = 5;
    internal const string BsscriptGuidance = "BSScript Rev.6 不是 Bishop GSC 或 KaGuYa 的同一脚本格式，不能按厂商或可见日文直接套解析器。"
        + "先确认头部各段边界、字符串偏移表与池；已有读写和原字节往返结论时直接复用，继续按指令引用区分对白、名称、选择项与资源路径。"
        + "严格 CP932 无法编码简体中文是预期限制；用 inspect_pe_imports 查询实际字体/编码导入和候选调用位置，再确认字节遍历、字符范围及字形路径。"
        + "可检查 cp932-glyph-map 组件，但须验证当前引擎的可用码位和字体映射，不能仅改成 CP936 或把尚未实现的环节判为不支持。";
    private const int MaximumEntries = 20_000;
    private const int MaximumSamples = 128;
    private const int MaximumDepth = 12;
    private static readonly IReadOnlyDictionary<string, VnTextPatchFormat> Formats = VnTextPatchFormatCatalog.All
        .ToDictionary(format => format.Key, StringComparer.Ordinal);
    private static readonly HashSet<string> GenericExtensions = new(StringComparer.OrdinalIgnoreCase)
        { "", ".dat", ".txt", ".bin", ".map", ".s", ".sc", ".src", ".nut", ".tjs" };
    private static readonly HashSet<string> ContainerExtensions = new(StringComparer.OrdinalIgnoreCase)
        { ".xp3", ".rpa", ".rpyc", ".pack", ".pak", ".arc", ".ypf", ".int", ".pfs" };
    private static readonly HashSet<string> InterestingExtensions = new(VnTextPatchFormatCatalog.All
        .SelectMany(format => format.Extensions).Concat(ContainerExtensions), StringComparer.OrdinalIgnoreCase);
    private static readonly HashSet<string> ExcludedDirectories = new(StringComparer.OrdinalIgnoreCase)
        { ".avg-translator", ".git", "backup", "backups", "output", "outputs", "patches" };

    public static async Task<BuiltinAdapterDiscoveryReport> InspectAsync(string gameDirectory, CancellationToken ct = default)
    {
        ct.ThrowIfCancellationRequested();
        var candidates = new Dictionary<string, Candidate>(StringComparer.Ordinal);
        var files = new List<string>();
        var warnings = new List<string>();
        int scanned = 0;
        bool partial = false;
        void Warn(string text)
        {
            partial = true;
            if (warnings.Count < 8 && !warnings.Contains(text, StringComparer.Ordinal)) warnings.Add(text);
        }

        string root;
        try
        {
            root = Path.GetFullPath(gameDirectory);
            WorkspaceFiles.RejectLinkAncestors(root);
            if (!Directory.Exists(root)) throw new DirectoryNotFoundException("游戏目录不存在或无法访问。");
        }
        catch (Exception error) when (IsInspectionError(error))
        {
            Warn("无法检查游戏目录：" + Diagnostic(error.Message));
            return new(Revision, scanned, partial, [], warnings);
        }

        var pending = new Stack<(string Directory, int Depth)>();
        pending.Push((root, 0));
        int entries = 0;
        while (pending.TryPop(out var current))
        {
            ct.ThrowIfCancellationRequested();
            try
            {
                WorkspaceFiles.RejectLinkAncestors(current.Directory);
                foreach (var entry in Directory.EnumerateFileSystemEntries(current.Directory))
                {
                    ct.ThrowIfCancellationRequested();
                    if (++entries > MaximumEntries) break;
                    var name = Path.GetFileName(entry);
                    if (name.EndsWith(".bak", StringComparison.OrdinalIgnoreCase)
                        || name.Contains(".bak.", StringComparison.OrdinalIgnoreCase)
                        || name.Contains(".pending-", StringComparison.OrdinalIgnoreCase)
                        || name.Contains(".damaged-", StringComparison.OrdinalIgnoreCase)) continue;
                    try
                    {
                        var attributes = File.GetAttributes(entry);
                        if ((attributes & FileAttributes.ReparsePoint) != 0) continue;
                        if ((attributes & FileAttributes.Directory) != 0)
                        {
                            if (ExcludedDirectories.Contains(name)) continue;
                            if (current.Depth >= MaximumDepth) Warn("目录层级超过抽样上限，较深的资源尚未检查。");
                            else pending.Push((entry, current.Depth + 1));
                        }
                        else
                        {
                            scanned++;
                            if (InterestingExtensions.Contains(Path.GetExtension(name)))
                                files.Add(Path.GetRelativePath(root, entry).Replace('\\', '/'));
                        }
                    }
                    catch (Exception error) when (IsInspectionError(error))
                    { Warn("无法检查资源 " + Path.GetRelativePath(root, entry) + "：" + Diagnostic(error.Message)); }
                }
            }
            catch (Exception error) when (IsInspectionError(error))
            { Warn("无法枚举目录 " + Path.GetRelativePath(root, current.Directory) + "：" + Diagnostic(error.Message)); }
            if (entries > MaximumEntries)
            {
                Warn("目录扫描达到 20,000 项上限，候选线索可能不完整。");
                break;
            }
        }

        // Round-robin extensions so many scripts of one kind do not hide a different engine's container.
        var samples = files.OrderBy(ResourcePriority).ThenBy(path => path, StringComparer.OrdinalIgnoreCase)
            .GroupBy(Path.GetExtension, StringComparer.OrdinalIgnoreCase)
            .SelectMany(group => group.Select((path, index) => (Path: path, Index: index)))
            .OrderBy(item => item.Index)
            .ThenBy(item => ContainerExtensions.Contains(Path.GetExtension(item.Path)) ? 0 : 1)
            .ThenBy(item => item.Path, StringComparer.OrdinalIgnoreCase)
            .Take(MaximumSamples).ToArray();
        if (files.Count > MaximumSamples) Warn("仅抽样读取前 128 个候选资源的文件头／尾，其他资源仍需调查。");
        var identified = new HashSet<string>(StringComparer.OrdinalIgnoreCase);
        var nekoSamples = new List<string>();
        var bsscriptSamples = new List<string>();
        foreach (var sample in samples)
        {
            ct.ThrowIfCancellationRequested();
            try
            {
                var fullPath = WorkspaceFiles.Resolve(root, sample.Path);
                WorkspaceFiles.RejectLinkAncestors(fullPath);
                await using var stream = new FileStream(fullPath, FileMode.Open, FileAccess.Read, FileShare.ReadWrite | FileShare.Delete,
                    4096, FileOptions.Asynchronous | FileOptions.RandomAccess);
                var header = new byte[(int)Math.Min(256, stream.Length)];
                await stream.ReadExactlyAsync(header, ct);
                var tail = new byte[(int)Math.Min(64, stream.Length)];
                stream.Position = stream.Length - tail.Length;
                await stream.ReadExactlyAsync(tail, ct);
                if (header.AsSpan().StartsWith("BSScript Rev.6\0"u8))
                {
                    bsscriptSamples.Add(sample.Path);
                    identified.Add(sample.Path);
                }
                if (header.AsSpan().StartsWith("NEKOPACK4A"u8) || header.AsSpan().StartsWith("NEKOPACK4S"u8)
                    || header.AsSpan().StartsWith("NEKOSDK_ADVSCRIPT2"u8))
                {
                    nekoSamples.Add(sample.Path);
                    identified.Add(sample.Path);
                }
                if (Identify(header, tail) is { } match)
                {
                    Add(match.Key, sample.Path, match.Container ? "container-signature" : "script-signature",
                        match.Signature, match.Container);
                    identified.Add(sample.Path);
                }
            }
            catch (Exception error) when (IsInspectionError(error))
            { Warn("无法读取资源特征 " + sample.Path + "：" + Diagnostic(error.Message)); }
        }

        foreach (var file in files)
        {
            ct.ThrowIfCancellationRequested();
            if (identified.Contains(file)) continue;
            var extension = Path.GetExtension(file);
            if (extension.Equals(".xp3", StringComparison.OrdinalIgnoreCase))
                Add("KirikiriKs", file, "container-extension-hint", "只有 .xp3 后缀线索，未确认 XP3 签名", true);
            else if (extension.Equals(".rpa", StringComparison.OrdinalIgnoreCase))
                Add("Renpy", file, "container-extension-hint", "只有 .rpa 后缀线索，未确认 RPA 签名", true);
            else if (extension.Equals(".rpyc", StringComparison.OrdinalIgnoreCase))
                Add("Renpy", file, "extension-hint", ".rpyc 编译脚本后缀；内置解析器只处理 .rpy 源码", false, true);
            else if (!GenericExtensions.Contains(extension))
                foreach (var format in VnTextPatchFormatCatalog.All.Where(format => format.Extensions.Contains(extension, StringComparer.OrdinalIgnoreCase)))
                    Add(format.Key, file, "extension-hint", $"只有 {extension.ToLowerInvariant()} 后缀线索，尚未解析内容", false);
        }

        if (candidates.Count > 6) Warn("候选格式超过 6 种，仅保留证据较强的候选；其他资源仍需调查。");
        var result = candidates.Values.OrderByDescending(candidate => candidate.Strength)
            .ThenBy(candidate => candidate.Format.Key, StringComparer.Ordinal).Take(6).Select(candidate => candidate.ToRecord()).ToList();
        if (nekoSamples.Count > 0)
            result.Insert(0, new("NekoSDK", "reference.nekosdk", "NekoSDK / NEKOPACK4 / ADVSCRIPT2", "format-signature",
                ["文件头匹配 NEKOPACK4A/S 或 NEKOSDK_ADVSCRIPT2；共享工具含容器 reader/writer 和局部文本字段 reader/writer。先 inspect_engine_reference(engine=nekosdk)，常规适配器尚未内置。"],
                nekoSamples.OrderBy(ResourcePriority).ThenBy(path => path, StringComparer.OrdinalIgnoreCase).Take(4).ToArray(),
                ["确认脚本记录变体、选择项与中文运行支持；不能把容器可读视为汉化完成。"]));
        if (bsscriptSamples.Count > 0)
            result.Insert(0, new("BSScriptRev6", "reference.bsscript-rev6", "BSScript Rev.6", "script-signature",
                ["文件头匹配 BSScript Rev.6；这是格式线索，尚未内置完整适配器。", BsscriptGuidance],
                bsscriptSamples.Take(4).ToArray(), ["指令引用分类、变长回填和中文运行支持仍需验证。"]));
        var siglus = Path.Combine(root, "Scene.pck");
        if (File.Exists(siglus) && File.Exists(Path.Combine(root, "Gameexe.dat")))
        {
            try
            {
                WorkspaceFiles.RejectLinkAncestors(siglus);
                await using var input = File.OpenRead(siglus);
                var header = new byte[92];
                if (await input.ReadAsync(header, ct) == 92 && System.Buffers.Binary.BinaryPrimitives.ReadUInt32LittleEndian(header) == 92)
                    result.Insert(0, new("Siglus", SiglusAssets.Family.AdapterId, SiglusAssets.Family.Name, "container-header",
                        ["Scene.pck 的 92 字节文件头与 Gameexe.dat；可调用 inspect_siglus_game 检查实际场景和显示模板。"],
                        ["Scene.pck", "Gameexe.dat"], ["需要检查当前版本的文本调用和中文字库。"]));
            }
            catch (Exception error) when (IsInspectionError(error)) { Warn("无法检查 Siglus：" + Diagnostic(error.Message)); }
        }
        if (result.Count > 0) warnings.Add("候选只来自文件名或局部字节特征，尚未运行解析器、解包资源或确认游戏兼容。");
        return new(Revision, scanned, partial, result, warnings);

        void Add(string key, string file, string kind, string evidence, bool container, bool compiled = false)
        {
            if (!candidates.TryGetValue(key, out var candidate)) candidates.Add(key, candidate = new Candidate(Formats[key]));
            candidate.Add(file, kind, evidence, container, compiled);
        }
    }

    private static (string Key, string Signature, bool Container)? Identify(ReadOnlySpan<byte> head, ReadOnlySpan<byte> tail)
    {
        if (head.StartsWith(new byte[] { 0x58, 0x50, 0x33, 0x0D, 0x0A, 0x20, 0x0A, 0x1A, 0x8B, 0x67, 0x01 }))
            return ("KirikiriKs", "文件头匹配 XP3 签名（58 50 33 0D 0A 20 0A 1A 8B 67 01）", true);
        if (head.StartsWith("RPA-2.0 "u8) || head.StartsWith("RPA-3.0 "u8)) return ("Renpy", "文件头匹配 RPA-2.0／RPA-3.0 封包签名", true);
        if (tail.Length >= 28 && (tail[^28..].StartsWith("FilePackVer1.0"u8)
            || tail[^28..].StartsWith("FilePackVer2.0"u8) || tail[^28..].StartsWith("FilePackVer3.0"u8)
            || tail[^28..].StartsWith("FilePackVer3.1"u8)))
            return ("Qlie", "文件末尾 -28 字节处匹配 " + Encoding.ASCII.GetString(tail.Slice(tail.Length - 28, 14)), true);
        if (head.StartsWith("MajiroArcV1.000\0"u8) || head.StartsWith("MajiroArcV2.000\0"u8) || head.StartsWith("MajiroArcV3.000\0"u8))
            return ("Majiro", "文件头匹配 MajiroArc 封包签名", true);
        if (head.Length >= 8 && head.StartsWith("YPF\0"u8)) return ("Yuris", "文件头匹配 YPF 封包签名", true);
        if (head.Length >= 8 && head.StartsWith("KIF\0"u8)) return ("CatSystem", "文件头匹配 KIF 封包签名", true);
        if (head.StartsWith("BURIKO ARC20"u8)) return ("Ethornell", "文件头匹配 BURIKO ARC20 封包签名", true);
        if (head.StartsWith("CatScene"u8)) return ("CatSystem", "文件头匹配 CatScene 脚本签名", false);
        if (head.StartsWith("[SCR-MESSAGE]ver4.0"u8)) return ("Kaguya", "文件头匹配 [SCR-MESSAGE]ver4.0 消息表签名", false);
        if (head.StartsWith("BurikoCompiledScriptVer1.00\0"u8)) return ("Ethornell", "文件头匹配 BurikoCompiledScriptVer1.00 脚本签名", false);
        if (head.StartsWith("TJS2100\0"u8)) return ("KirikiriTjs", "文件头匹配 TJS2100 编译脚本签名", false);
        if (head.StartsWith("MajiroObjV1.000\0"u8) || head.StartsWith("MajiroObjX1.000\0"u8)) return ("Majiro", "文件头匹配 MajiroObj 脚本签名", false);
        if (head.StartsWith("SHSysSC\0"u8)) return ("ShSystem", "文件头匹配 SHSysSC 脚本签名", false);
        if (head.Length >= 9 && head.StartsWith("ASB\0\0"u8)) return ("ArtemisAsb", "文件头匹配 ASB 脚本签名", false);
        if (head.Length >= 8 && head.StartsWith("SYS4"u8)) return ("Age", "文件头匹配 ArcGameEngine SYS4 签名（不是 System4 AIN）", false);
        if (head.Length >= 8 && (head.StartsWith("YSTB"u8) || head.StartsWith("YSCF"u8))) return ("Yuris", "文件头匹配 YU-RIS YSTB／YSCF 脚本签名", false);
        return null;
    }

    // Names only prioritize inspection; format evidence still requires recognized bytes.
    private static int ResourcePriority(string path) => Path.GetFileNameWithoutExtension(path).ToLowerInvariant() switch
    {
        "scr" or "script" or "scenario" or "scene" or "message" or "bsx" => 0,
        "cfg" or "config" or "system" or "gameexe" => 1,
        _ => 2
    };

    private static bool IsInspectionError(Exception error) => error is IOException or UnauthorizedAccessException or SecurityException
        or ArgumentException or NotSupportedException;
    private static string Diagnostic(string message) => message.Length <= 240 ? message : message[..240] + "…";

    private sealed class Candidate(VnTextPatchFormat format)
    {
        public VnTextPatchFormat Format { get; } = format;
        private readonly List<(string Path, string Kind, string Evidence, int Strength)> evidence = [];
        private bool hasContainer;
        private bool hasCompiledScript;
        public int Strength => evidence.Count == 0 ? 0 : evidence[0].Strength;

        public void Add(string path, string kind, string detail, bool container, bool compiled)
        {
            hasContainer |= container;
            hasCompiledScript |= compiled;
            int strength = kind switch { "script-signature" => 4, "container-signature" => 3, "container-extension-hint" => 2, _ => 1 };
            if (evidence.Any(item => item.Path.Equals(path, StringComparison.OrdinalIgnoreCase))) return;
            evidence.Add((path, kind, detail, strength));
            evidence.Sort((left, right) => left.Strength != right.Strength ? right.Strength.CompareTo(left.Strength)
                : StringComparer.OrdinalIgnoreCase.Compare(left.Path, right.Path));
            if (evidence.Count > 3) evidence.RemoveAt(3);
        }

        public BuiltinAdapterCandidate ToRecord()
        {
            var missing = new List<string> { "待验证：实际解析脚本并核对版本、指令与文本结构。", "待验证：游戏资源加载、中文编码、字体与排版。" };
            if (hasContainer) missing.Insert(0, "待补足：封包的解包／回包，或游戏可加载的松散资源覆盖方式；尚未检查封包内脚本。");
            if (hasContainer && Format.Key == "KirikiriKs") missing.Add("待确认：XP3 内是否为 KS／SOC／TJS；SCN 格式未内置。");
            if (hasCompiledScript || hasContainer && Format.Key == "Renpy") missing.Add("待确认：是否有可用 .rpy 源码；只有 .rpyc 时需要额外反编译支持。");
            return new(Format.Key, Format.Family.AdapterId, Format.Family.Name, evidence[0].Kind,
                evidence.Select(item => item.Path + "：" + item.Evidence).ToArray(),
                evidence.Select(item => item.Path).ToArray(), missing);
        }
    }
}
