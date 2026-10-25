using System.Security.Cryptography;
using System.Text;
using AvgTranslator.Core.Isolation;

namespace AvgTranslator.Core.Development;

/// <summary>Small, independently copyable source components; copied files are pinned by the normal adapter package hashes.</summary>
public sealed class AdapterComponentAssets(string? assetsDirectory = null)
{
    private readonly string directory = Path.GetFullPath(assetsDirectory ?? Path.Combine(AppContext.BaseDirectory, "tools", "adapter-components"));
    private sealed record FileEntry(string Name, string Destination, bool Binary = false);
    private sealed record Component(string Id, string Name, string Scope, string Origin, string Api, string Example, FileEntry[] Files);
    private const string Version = "1.0.0";
    private static readonly Component[] Components =
    [
        new("cp932-glyph-map", "CP932 字形映射", "确定性分配、保留原文码位、已有映射复用、严格编解码；不修改 EXE 或字体。调用者必须确认允许码位能被原引擎遍历、查字和显示。",
            "基于现有 QLIE / System4 适配器的映射流程整理；增加全补丁保留文本及严格往返检查。",
            "AvgTranslator.AdapterComponents.Cp932GlyphEncoding.Plan(outputTexts, allowedSlots, preservedTexts, existingMap=null)；返回 Encoding，SlotToUnicode 为码位→真实 Unicode。new Cp932GlyphEncoding(savedMap) 用于重提取。完整文本单元接口，不用于分块流式编码。",
            """
            // Supply ALL final logical text: translations plus unchanged dialogue/names. Reserve known UI/non-extracted literals too.
            // allowedSlots must come from verified runtime + glyph evidence; CP932 support alone is insufficient.
            var codec = AvgTranslator.AdapterComponents.Cp932GlyphEncoding.Plan(allOutputTexts, verifiedSlots, otherPreservedText, existingSlotToUnicode);
            byte[] encoded = codec.GetBytes(chineseText);
            // Persist codec.SlotToUnicode with the output font/resources, not in translation memory.
            // A font alias writer consumes slot -> actual Unicode. Existing FNL maps use the reverse direction: convert explicitly.
            """, [new("Cp932GlyphEncoding.cs", "src/Components/Cp932GlyphEncoding.cs")]),
        new("qlie-sfnt-font", "QLIE SFNT 字体模板", "仅适用于已确认的 QLIE system 字体列表、MS Gothic/Mincho 字高与半角宽度策略。支持原模板使用的单字体 SFNT、cmap 4/12；不是 TTC/WOFF 通用字体工具。保留轮廓，调整映射、名称和该模板的度量。",
            "adapters/qlie-filepack3-systemfont/QlieFont.cs；对应 QLIE 模板曾获得用户中文正常显示反馈，其他版本仍需验证。",
            "QlieFont.Create(inputFont, outputFont, family, IDictionary<int,int> slotToUnicode)。引擎配置完整名为 family + \" Regular\"，须与原转区路径的枚举结果一致。字体文件及其许可由适配器准备。",
            "QlieFont.Create(sourceFont, outputFont, familyName, new System.Collections.Generic.Dictionary<int,int>(codec.SlotToUnicode));",
            [new("QlieFont.cs", "src/QlieFont.cs")]),
        new("system4-fnl-glyph", "System4 FNL 位图字形模板", "Windows GDI / SimSun / BMP 字符；输出底朝上、每行按 4 字节对齐的单色位图，按原字形包围框缩放。适用于已确认的 FNL 位图布局，不提供 FNL 封包或运行时码位修补。",
            "adapters/system4-ain14-msg1-fnl/GlyphRasterizer.cs；保留已使用的原字体替换检测及非空字形检查。",
            "using var raster = new GlyphRasterizer(); raster.Render(character, width, height, originalReferenceBitmap)。需要本机宋体，字体不随组件分发。",
            "using var raster = new GlyphRasterizer(); byte[] glyph = raster.Render('译', cellWidth, cellHeight, referenceGlyph);",
            [new("GlyphRasterizer.cs", "src/GlyphRasterizer.cs")]),
        new("qlie-japanese-font-launcher", "QLIE 日语转区与字体启动器", "独立 x86 .NET Framework EXE；读取 qlie-l10n/launch.xml，临时加载字体并保留日语 Locale Emulator 路径。检查 QLIE SHIFTJIS_CHARSET 完整字体名；等待 LEProc/游戏退出后卸载字体。仅适用 LEProc 等待实际游戏的已确认启动链。",
            "adapters/qlie-filepack3-systemfont/tools/QlieLauncher.cs；直接提供同源码编译的启动器，不调用分析 API 编写或编译。",
            "将 tools/QlieLauncher.exe 复制到补丁的启动入口并声明 build.launchExecutable。launch.xml 的 executable、font.file 相对启动器目录；原 EXE 的 .le.config 保持原样。face 必须与字体内部 name ID 4 一致。Windows .NET Framework 4.x；需要转区时使用玩家本机 Locale Emulator。",
            """
            // Build output example: 启动汉化.exe (copied from tools/QlieLauncher.exe), qlie-l10n/launch.xml, generated fonts.
            // XML values must be escaped with an XML writer. Filenames below are placeholders, not discovery rules.
            <launch executable="game.exe"><font file="qlie-l10n/Serif.ttf" face="Patch Serif Regular" /></launch>
            // Copy the original game/config resources as required by the protocol; test_adapter still gates publication.
            """, [new("QlieLauncher.cs", "src/tools/QlieLauncher.cs"), new("QlieLauncher.exe", "src/tools/QlieLauncher.exe", true)])
    ];

    public object[] List(string query = "") => Components.Where(component => string.IsNullOrWhiteSpace(query)
        || (component.Id + " " + component.Name + " " + component.Scope + " " + component.Origin).Contains(query, StringComparison.OrdinalIgnoreCase))
        .Select(component => (object)new { component.Id, version = Version, component.Name, component.Scope }).ToArray();

    public object Describe(string id)
    {
        var component = Find(id);
        return new
        {
            component.Id, version = Version, component.Name, component.Scope, component.Origin, component.Api, component.Example,
            files = component.Files.Select(file => new
            {
                path = file.Destination, binary = file.Binary,
                available = !file.Binary || File.Exists(Path.Combine(directory, file.Name))
            }).ToArray(),
            usage = "copy_adapter_components 按需复制。源码可直接参加现有 .NET 10 适配器编译，不依赖主程序程序集；tools 下的启动器源码不会成为适配器 Main。组件本身不保证当前游戏兼容，仍需资源回填及运行显示验证。"
        };
    }

    public object ReadSource(string id, string path, int offset = 0, int length = 12000)
    {
        if (offset < 0 || length is < 1 or > 24000) throw new ArgumentOutOfRangeException(nameof(length));
        var file = Find(id).Files.SingleOrDefault(file => file.Destination == path.Replace('\\', '/') && !file.Binary)
            ?? throw new InvalidDataException("源码路径不在此组件索引中。");
        var content = Encoding.UTF8.GetString(ReadSourceBytes(file.Name));
        var start = Math.Min(offset, content.Length);
        var next = Math.Min(content.Length, start + length);
        return new { id, version = Version, path = file.Destination, offset = start, nextOffset = next,
            totalLength = content.Length, more = next < content.Length, content = content[start..next] };
    }

    public async Task<object> CopyAsync(IReadOnlyList<string> ids, string workspace, CancellationToken ct = default)
    {
        if (ids.Count is < 1 or > 4) throw new InvalidDataException("每次选择 1 到 4 个组件。");
        var selected = ids.Distinct(StringComparer.OrdinalIgnoreCase).Select(Find).ToArray();
        var files = new Dictionary<string, byte[]>(StringComparer.OrdinalIgnoreCase);
        foreach (var component in selected)
        foreach (var file in component.Files)
        {
            ct.ThrowIfCancellationRequested();
            if (file.Binary)
            {
                var path = WorkspaceFiles.Resolve(directory, file.Name);
                if (!File.Exists(path)) throw new FileNotFoundException("缺少预编译启动组件，请使用完整发行包或执行 package-adapter-components.ps1。", path);
                files.Add(file.Destination, await File.ReadAllBytesAsync(path, ct));
            }
            else files.Add(file.Destination, ReadSourceBytes(file.Name));
        }
        var root = Path.GetFullPath(workspace);
        var reused = new HashSet<string>(StringComparer.OrdinalIgnoreCase);
        // Preflight the whole selection before writing, preserving any agent modifications on conflict.
        foreach (var (relative, content) in files)
        {
            var path = WorkspaceFiles.Resolve(root, relative);
            if (Directory.Exists(path)) throw new IOException("组件目标已是目录：" + relative);
            for (var parent = Path.GetDirectoryName(path); parent is not null && parent.Length >= root.Length; parent = Path.GetDirectoryName(parent))
                if (File.Exists(parent)) throw new IOException("组件目录被文件占用：" + relative);
            if (!File.Exists(path)) continue;
            var existing = await File.ReadAllBytesAsync(path, ct);
            if (!content.AsSpan().SequenceEqual(existing))
                throw new IOException("组件文件已有不同内容，本次未写入文件：" + relative + "。请先检查已有实现，保留必要修改。");
            reused.Add(relative);
        }
        foreach (var (relative, content) in files)
        {
            ct.ThrowIfCancellationRequested();
            if (reused.Contains(relative)) continue;
            var path = WorkspaceFiles.Resolve(root, relative);
            Directory.CreateDirectory(Path.GetDirectoryName(path)!);
            await using var output = new FileStream(path, FileMode.CreateNew, FileAccess.Write, FileShare.None);
            await output.WriteAsync(content, ct);
        }
        return new
        {
            components = selected.Select(component => Describe(component.Id)).ToArray(),
            files = files.Select(pair => new { path = pair.Key, reused = reused.Contains(pair.Key),
                sha256 = Convert.ToHexStringLower(SHA256.HashData(pair.Value)) }).ToArray()
        };
    }

    private static Component Find(string id) => Components.SingleOrDefault(component => component.Id.Equals(id, StringComparison.OrdinalIgnoreCase))
        ?? throw new InvalidDataException("未知复用组件，请从 list_adapters 的 components 中选择。");

    private static byte[] ReadSourceBytes(string name)
    {
        using var stream = typeof(AdapterComponentAssets).Assembly.GetManifestResourceStream("AvgTranslator.Components." + name)
            ?? throw new InvalidDataException("内置组件源码缺失：" + name);
        using var bytes = new MemoryStream();
        stream.CopyTo(bytes);
        return bytes.ToArray();
    }
}
