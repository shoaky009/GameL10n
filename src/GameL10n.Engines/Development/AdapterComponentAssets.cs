using System.Security.Cryptography;
using System.Text;
using GameL10n.Core.Isolation;
using System.Text.Json.Nodes;
using GameL10n.Core.Serialization;

namespace GameL10n.Core.Development;

/// <summary>Python helpers and precompiled patch assets pinned by the adapter package hashes.</summary>
public sealed class AdapterComponentAssets(string? assetsDirectory = null)
{
    private readonly string directory = Path.GetFullPath(assetsDirectory ?? Path.Combine(AppContext.BaseDirectory, "tools", "adapter-components"));
    private sealed record FileEntry(string Name, string Destination, bool Binary = false);
    private sealed record Component(string Id, string Name, string Scope, string Origin, string Api, string Example,
        FileEntry[] Files, string[]? EngineFamilies = null);
    private const string Version = "2.1.0";
    private static readonly Component[] Components =
    [
        new("windows-font-metrics", "Windows 字体覆盖与字宽检查", "仅用于分析：检测指定 GDI 字体的缺字、实际选中名称、字宽与字形包围框。可临时加载工作副本 TTF/TTC/OTF；不安装字体、不运行游戏、不修改资源。独立测量不等于游戏绘制验证，非 GDI 字库需单独检查。",
            "应用内置 Python ctypes / Windows GDI；仅使用标准库。辅助脚本在 work/，不进入适配器发布包或游戏目录。",
            "copy_adapter_components 后，run_engine_python(script='work/windows_font_metrics.py', stdin=JSON)。fonts 为 1..4 项 {face,file?,height?,width?,weight?,italic?,charset?,pitchAndFamily?}，height 默认 -24，须采用引擎实际有符号高度；file 为暂存字体。text 为 1..512 字符的原文及中文试译样本。返回缺字计数/样例、selectedFace、字宽范围及有限度量样例。缺字的 measuredAdvance 不能证明回退绘制宽度正确；异名可能是本地化别名。BMP 以外字符明确列为未检查，不计入通过。",
            """
            // After extracting the actual font/config from a staged archive, compare the original and a suitable installed candidate:
            run_engine_python(script="work/windows_font_metrics.py", stdin='{"fonts":[{"face":"Original Font","file":"work/unpacked/font.ttf","height":-24},{"face":"Microsoft YaHei","height":-24}],"text":"原文かな、。简体译文喂么过这试！？ABC123"}')
            // If the primary font lacks Chinese, trace engine font fallback/reset and make measurement and rendering use a covering font.
            // Do not fix this by inserting spaces, replacing characters, globally increasing pitch, or copying the QLIE-only metric policy.
            """, [new("windows_font_metrics.py", "work/windows_font_metrics.py")], ["kaguya", "kirikiri"]),
        new("kaguya-tblstr", "KaGuYa TBLSTR 对齐文本表", "ARC/ARI 配对、四字节对齐、XOR-FF 与严格 CP932 的 TBLSTR 变体；核对类型 0/2/3 与主记录索引。只确认表结构，不代表其他脚本、中文运行或整个引擎兼容。",
            "根据已验证的 TBLSTR 解析与原字节回填实现整理；Python 标准库，不依赖游戏名称或固定资源路径。",
            "run_engine_python(script='src/tools/kaguya_tblstr.py', stdin=JSON)。JSON 为 {operation:'probe',input:'resources/table.arc',index:'resources/table.ari'}，可选 sampleLimit=0..2。返回结构、索引、编码、原字节回环与中文编码限制。适配器可直接复用 read_table/units/rebuild；未修改记录保留原字节，类型语义仍须当前样本确认。",
            """
            copy_adapter_components(ids=["kaguya-tblstr"])
            run_engine_python(script="src/tools/kaguya_tblstr.py", stdin='{"operation":"probe","input":"resources/table.arc","index":"resources/table.ari"}')
            # In an adapter: from tools.kaguya_tblstr import read_table, units, rebuild
            # Chinese output needs an independently verified runtime route; inspect sjis-tunnel only when applicable.
            """, [new("kaguya_tblstr.py", "src/tools/kaguya_tblstr.py")], ["kaguya"]),
        new("xp3-adler", "XP3 Adler 派生过滤器检测", "标准 XP3 索引、28 字节 segm 与已知 Adler 派生 XOR 变体。至少两个不同受保护成员的解压长度与明文 Adler 一致才确认过滤器；不推断 KS 标签、注音、覆盖顺序或游戏运行兼容。",
            "根据已验证的 XP3 Adler 派生过滤流程整理；Python 标准库，只读索引与有界成员，不重包资源。",
            "run_engine_python(script='src/tools/xp3_adler.py', stdin=JSON)。JSON 为 {operation:'probe',input:'resources/scripts.xp3'}，可选 members（最多 6 个精确成员名）、sampleLimit=0..2。返回过滤器证据、未知索引块及保留要求。generate_key(adler) 与 apply_key_in_place(data,offset,count,key,member_offset=...) 可用于当前适配器；单独分段 buffer 必须传成员内位置。",
            """
            copy_adapter_components(ids=["xp3-adler"])
            run_engine_python(script="src/tools/xp3_adler.py", stdin='{"operation":"probe","input":"resources/scripts.xp3"}')
            # matched confirms the tested filter only. Preserve unknown index metadata before any rebuild.
            # Check custom script consumers separately; an archive filter cannot establish their semantics.
            """, [new("xp3_adler.py", "src/tools/xp3_adler.py")], ["kirikiri"]),
        new("sjis-tunnel", "VNTextProxy SJIS 隧道编码", "VNTextProxy v0.0.41 的 3422 个扩展 BMP 字符映射；复用已有映射，严格编解码。引擎字符遍历、GDI 调用、字体、旁加载和转区须分别核对。不会修改 EXE、安装字体或下载运行文件。",
            "VNTextProxy v0.0.41 兼容映射算法（arcusmaximus，MIT）；在 Python 中提供独立编解码与容量检查。",
            "from tools.sjis_tunnel import Tunnel。Tunnel(existing_mapping_bytes).encode/decode/serialize 保持已有码位。JSON CLI 为 {operation:'check',texts:[...] ,mappingInput?:'work/sjis_ext.bin'}，只在内存检查容量和往返，不输出或写入完整映射。另用 inspect_pe_imports 核对候选调用，windows-font-metrics 检查字体；静态导入命中不代表游戏能使用隧道尾字节。",
            """
            copy_adapter_components(ids=["sjis-tunnel"])
            run_engine_python(script="src/tools/sjis_tunnel.py", stdin='{"operation":"check","texts":["原文かな","简体中文译文"]}')
            # Collect ALL final translated and preserved text before serializing the map into patch output.
            # Use a hash-pinned VNTextProxy runtime with its license only after checking the actual character path.
            """, [new("sjis_tunnel.py", "src/tools/sjis_tunnel.py")], ["kaguya"]),
        new("kirikiri-scn-text", "Kirikiri PSB / KAGEX SCN 文本", "已解密、无资源的 PSB v2/v3 SCN；显式选择 language_index，内部 speaker ID 默认仅作上下文。只提取正文、显示名、选项和场景标题，派生朗读/搜索缓存不送翻译。未知元组、控制语法或缓存规则会明确失败，不猜测回填。",
            "连接固定 GalTransl python.engines.kirikiri_psb / kirikiri_scn 语义读写器；不复制或改写 PSB 编解码算法。",
            "use_engine_reference 固定共享依赖后，from kirikiri_scn_text import SceneText, tokens。SceneText(data, language_index=0, speaker_name=False).fields 为 (path,text,kind,node)；build({path:translation}) 返回 bytes 或无修改时 None，自动重算缓存/可见长度，复提核对修改与保留字段。语义路径用于稳定 ID。容器过滤、资源覆盖顺序、字体和游戏运行另行验证。",
            """
            // After copy_adapter_components(ids=["kirikiri-scn-text"]), verify one staged plain SCN without writing a wrapper:
            run_engine_python(script="src/tools/kirikiri_scn_text.py", stdin='{"operation":"verify","input":"work/sample.scn","speakerName":false}')
            // In src/tools/adapter.py:
            from kirikiri_scn_text import SceneText
            scene = SceneText(plain_scn_bytes, language_index=0, speaker_name=False)
            # Existing fields such as scenes/0/texts/1/1/0/1 keep their IDs. Do not translate derived caches separately.
            patched = scene.build(translations_by_semantic_path)
            """, [new("kirikiri_scn_text.py", "src/tools/kirikiri_scn_text.py")], ["kirikiri"]),
        new("cp932-glyph-map", "CP932 字形映射", "确定性分配、保留原文码位、已有映射复用、严格编解码；不修改 EXE 或字体。调用者必须确认允许码位能被原引擎遍历、查字和显示。",
            "基于现有 QLIE / System4 适配器的映射流程整理；增加全补丁保留文本及严格往返检查。",
            "from tools.gamel10n_host import component。glyph_plan 接受 texts、allowedSlots、preservedTexts、map，返回 map（码位→真实 Unicode，JSON 键为十进制字符串）；glyph_encode 接受 texts/map，返回 base64 values；glyph_decode 接受 values/map，返回 texts。一次处理完整文本单元，按批次调用预编译组件。",
            """
            from tools.gamel10n_host import component
            # Include ALL final logical text and reserve known UI/non-extracted literals.
            # allowedSlots needs runtime + glyph evidence; CP932 support alone is insufficient.
            mapping = component("glyph_plan", request, texts=all_output_texts, allowedSlots=verified_slots,
                                preservedTexts=other_preserved_text, map=existing_slot_to_unicode)["map"]
            encoded = component("glyph_encode", request, texts=output_texts, map=mapping)["values"]
            # Persist mapping with output fonts/resources; FNL maps use the reverse direction: convert explicitly.
            """, [new("gamel10n_host.py", "src/tools/gamel10n_host.py")]),
        new("qlie-sfnt-font", "QLIE SFNT 字体模板", "仅适用于已确认的 QLIE system 字体列表、MS Gothic/Mincho 字高与半角宽度策略。支持原模板使用的单字体 SFNT、cmap 4/12；不是 TTC/WOFF 通用字体工具。保留轮廓，调整映射、名称和该模板的度量。",
            "adapters/qlie-filepack3-systemfont/QlieFont.cs；对应 QLIE 模板曾获得用户中文正常显示反馈，其他版本仍需验证。",
            "component('qlie_font', request, input=source_font, output=output_font, family=family, map=slot_to_unicode)。引擎配置完整名为 family + \" Regular\"，须与原转区路径的枚举结果一致。字体文件及其许可由适配器准备。",
            "from tools.gamel10n_host import component\ncomponent('qlie_font', request, input=source_font, output=output_font, family=family_name, map=slot_to_unicode)",
            [new("gamel10n_host.py", "src/tools/gamel10n_host.py")]),
        new("system4-fnl-glyph", "System4 FNL 位图字形模板", "Windows GDI / SimSun / BMP 字符；输出底朝上、每行按 4 字节对齐的单色位图，按原字形包围框缩放。适用于已确认的 FNL 位图布局，不提供 FNL 封包或运行时码位修补。",
            "adapters/system4-ain14-msg1-fnl/GlyphRasterizer.cs；保留已使用的原字体替换检测及非空字形检查。",
            "component('render_glyphs', request, glyphs=[{character,width,height,reference}]) 返回 base64 values。reference 为原字形位图的 base64；一次批量生成多个字形。需要本机宋体，字体不随组件分发。",
            "from tools.gamel10n_host import component\nglyphs = component('render_glyphs', request, glyphs=[dict(character='译', width=cell_width, height=cell_height, reference=reference_base64)])['values']",
            [new("gamel10n_host.py", "src/tools/gamel10n_host.py")]),
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
        .Select(component => (object)new JsonObject
        {
            ["id"] = component.Id,
            ["version"] = Version,
            ["name"] = component.Name,
            ["scope"] = component.Scope
        }).ToArray();

    public object[] Related(IEnumerable<string> engineKeys)
    {
        var roots = engineKeys.Select(EngineKnowledgeStore.EngineRoot).Where(root => root is not null)
            .ToHashSet(StringComparer.OrdinalIgnoreCase);
        return Components.Where(component => component.EngineFamilies?.Any(roots.Contains) == true)
            .Select(component => (object)new JsonObject
            {
                ["id"] = component.Id,
                ["version"] = Version,
                ["scope"] = component.Scope,
                ["inspectCall"] = new JsonObject
                {
                    ["tool"] = "inspect_adapter_component",
                    ["arguments"] = new JsonObject { ["id"] = component.Id }
                }
            }).ToArray();
    }

    public object Describe(string id)
    {
        var component = Find(id);
        return new JsonObject
        {
            ["id"] = component.Id,
            ["version"] = Version,
            ["name"] = component.Name,
            ["scope"] = component.Scope,
            ["origin"] = component.Origin,
            ["api"] = component.Api,
            ["example"] = component.Example,
            ["files"] = EngineJson.Array(component.Files.Select(file => new JsonObject
            {
                ["path"] = file.Destination,
                ["binary"] = file.Binary,
                ["available"] = !file.Binary || File.Exists(Path.Combine(directory, file.Name))
            }).ToArray()),
            ["usage"] = "copy_adapter_components 按需复制 Python 辅助文件或预编译启动器。work/ 中的诊断工具仅用于分析，不进入发布清单；src/ 中的组件供适配器复用。支持的 JSON CLI 可用 run_engine_python 直接运行。组件本身不保证当前游戏兼容，仍需资源回填及运行显示验证。"
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
        return new JsonObject
        {
            ["id"] = id,
            ["version"] = Version,
            ["path"] = file.Destination,
            ["offset"] = start,
            ["nextOffset"] = next,
            ["totalLength"] = content.Length,
            ["more"] = next < content.Length,
            ["content"] = content[start..next]
        };
    }

    public async Task<object> CopyAsync(IReadOnlyList<string> ids, string workspace, CancellationToken ct = default, ICollection<AdapterReleaseFile>? releaseFiles = null)
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
                files.TryAdd(file.Destination, await File.ReadAllBytesAsync(path, ct));
            }
            else files.TryAdd(file.Destination, ReadSourceBytes(file.Name));
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
        foreach (var path in files.Keys)
        {
            if (!path.StartsWith("src/", StringComparison.Ordinal)) continue;
            // The editable launcher source is development material; its precompiled EXE is the patch asset.
            if (path.StartsWith("src/tools/", StringComparison.Ordinal) && path.EndsWith(".cs", StringComparison.OrdinalIgnoreCase)) continue;
            releaseFiles?.Add(new(AdapterReleaseFiles.PackagePath(path),
                path.EndsWith(".py", StringComparison.OrdinalIgnoreCase) ? "runtime" : "asset", "内置文本、编码、字体或启动组件"));
        }
        return new JsonObject
        {
            ["components"] = EngineJson.Node(selected.Select(component => Describe(component.Id)).ToArray(), EngineJson.Web),
            ["files"] = EngineJson.Array(files.Select(pair => new JsonObject
            {
                ["path"] = pair.Key,
                ["reused"] = reused.Contains(pair.Key),
                ["sha256"] = Convert.ToHexStringLower(SHA256.HashData(pair.Value))
            }).ToArray())
        };
    }

    private static Component Find(string id) => Components.SingleOrDefault(component => component.Id.Equals(id, StringComparison.OrdinalIgnoreCase))
        ?? throw new InvalidDataException("未知复用组件，请从 list_adapters 的 components 中选择。");

    private static byte[] ReadSourceBytes(string name)
    {
        using var stream = typeof(AdapterComponentAssets).Assembly.GetManifestResourceStream("GameL10n.Components." + name)
            ?? throw new InvalidDataException("内置组件源码缺失：" + name);
        using var bytes = new MemoryStream();
        stream.CopyTo(bytes);
        return bytes.ToArray();
    }
}
