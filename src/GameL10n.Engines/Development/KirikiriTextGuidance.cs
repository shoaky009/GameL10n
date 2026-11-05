using System.Text.Json.Nodes;
using GameL10n.Core.Serialization;
namespace GameL10n.Core.Development;

/// <summary>Conditional text-format guidance; it does not establish a game's runtime support.</summary>
internal static class KirikiriTextGuidance
{
    internal const string ReferencePath = "reference/Kirikiri_text_encoding.md";
    internal const string Summary = "Kirikiri KS/SOC 内置解析器可读 CP932、带 BOM 的 UTF-8/UTF-16，修改后写 UTF-16；先用 try_builtin_adapter 验证少量解包样本，再 copy_adapter 复用。运行时是否接受 Unicode 须沿实际脚本加载路径确认，未搜到 BOM 常量不能排除支持。不要用 CP932/CP936 混写加私有注释标记代替游戏端解码支持；小样须逐条比较译文与未修改项，编码失败必须明确失败。Unicode 写回后仍要检查原字体是否覆盖中文；有内置日文字体或个别字重叠时，使用 windows-font-metrics 组件并沿字体回退、测宽/绘制及存档设置恢复路径修复，不以全局加字距掩盖缺字。";

    internal const string FontLayout = "Unicode 脚本可读写不等于原日文字体覆盖中文。检查实际 userFace/defaultFace、System.addFont、mapPrerenderedFont 及 font/reset/restore 路径；先用 windows-font-metrics 在同高度/宽度/charset 下比较原字体和覆盖中文的候选字体。缺字时 getTextWidth 与 drawText 可能使用不同回退或度量，造成个别字重叠；需让测宽和绘制使用同一套已覆盖的字形。优先使用引擎已有字体配置或集中字体选择点，保留原字号、风格及正常日文/符号/ASCII；不要插空格、改译文、全局加 pitch、缩小全文或套 QLIE 专用度量。核对 chDefaultFace 等系统设置及读档恢复是否覆盖新配置，不删除用户存档；必要时只迁移已确认的原字体值。原文构建仍须逐字节一致；中文构建才按已确认条件修改字体配置，并独立核对配置已写入实际生效资源。预渲染字库/GDI 以外后端须单独检查；独立测量通过仍需试译确认游戏内排版。";

    internal static bool AppliesTo(string format) => format.Equals("KirikiriKs", StringComparison.OrdinalIgnoreCase)
        || format.Equals("KirikiriSoc", StringComparison.OrdinalIgnoreCase);

    internal static bool MatchesReference(string query) => query == "kag"
        || new[] { "kirikiri", "krkr", "xp3", "吉里吉里", "hxv4", "kagex", "psb", "scn" }
            .Any(query.Contains);

    internal static object ScnSupport() => new JsonObject
    {
        ["component"] = "kirikiri-scn-text",
        ["condition"] = "已解密 PSB v2/v3 的 KAGEX SCN；不能据此认定 XP3 的过滤器相同。Hxv4 与 Adler 等过滤变体分别确认。",
        ["next"] = "inspect_adapter_component(id=kirikiri-scn-text)，copy_adapter_components 后直接 run_engine_python 运行组件的 verify JSON CLI，先验证一个原始解包样本。",
        ["semanticLayer"] = "正文、显示名、选项才是独立译文；朗读/搜索缓存与可见长度由现成 kirikiri_scn writer 同步重算，禁止对它们独立翻译。未知布局要报告具体不兼容项，只扩展该层。",
        ["existingApis"] = EngineJson.Node(new[] { "python.engines.kirikiri_hxv4_text", "python.engines.kirikiri_scn", "python.engines.kirikiri_psb" }, EngineJson.Web),
        ["moduleEntry"] = "run_engine_python(module=python.engines.kirikiri_hxv4_text, arguments=[--help]) 可直接查看现成 CLI，无需编写 runpy 脚本。高层 extract/pack 只解决其声明范围；最终整包重建、字体、脚本覆盖顺序仍需验证。",
        ["fontCheck"] = "Unicode 读写成功后仍需检查原字体的中文覆盖。内置字体或个别字重叠时，inspect_adapter_component(id=windows-font-metrics) 后直接调用诊断脚本，核对字体回退及旧设置恢复。"
    };

    internal static object Describe() => new JsonObject
    {
        ["parserReuse"] = Summary,
        ["archiveSupport"] = "XP3 容器、脚本语义、中文编码和字体是不同能力。现有适配器失败时先检查可复制的源码；容器组件拒绝过滤标记或超出预算，不代表 KS 解析器不可复用。TJS 源码与 TJS2100 编译脚本分别处理。",
        ["minimalTrial"] = "先选一个短 KS 和含姓名/选项的代表样本：原文往返、少量中文及保留符号（如 ♪、・）、局部修改后复提。比较实际译文、ID、位置和所有未修改项；封包重建另核对修改与未修改条目的清单及内容。小样通过后再完整 test_adapter。",
        ["runtimeEvidence"] = "确认脚本字节缓冲实际到达的解码函数及编码选择条件。导入表、宿主 ACP、几种指令字节未命中或自定义 encoder/decoder 往返，都不能证明游戏的脚本编码或显示支持。",
        ["fontLayout"] = FontLayout,
        ["referenceSource"] = ReferencePath
    };

    internal const string Reference = """
        # Kirikiri KS/SOC：解析复用与中文编码

        内置 KirikiriKsScript 读取 CP932 或带 BOM 的 UTF-8/UTF-16，修改后写 UTF-16。
        这是现有解析器的能力，不是对任意游戏运行时的兼容声明。
        先用 try_builtin_adapter 试验代表性解包脚本；可用时用 copy_adapter 复用。
        姓名标签或特定脚本语法有缺口时，仅修改该层，不因容器失败重写全部解析和编码。
        TJS2100 是编译脚本，普通 .tjs 源码不能直接套该格式。

        ## 独立确认运行编码

        优先检查游戏支持的 Unicode、编码设置及既有文本加载实现。
        从脚本文件读取沿实际缓冲和调用关系确认解码分支；缺少某个字符串、BOM 常量或特定 cmp 指令，不能排除相同逻辑的其他实现。
        PE 中出现 MultiByteToWideChar/GetACP，不代表所有 KS/TJS 都按当前宿主 ACP 解码。
        若确需代码页或字体适配，必须接入游戏实际使用的路径，再声明未完成的运行检查。
        在同一文件混写 CP932 原文和 CP936 译文，添加仅由适配器读取的注释标记，只建立了自己的重提取格式；没有给游戏增加解码支持。

        ## 字体覆盖与个别字重叠

        """ + "\n" + FontLayout + "\n\n" + """
        inspect_adapter_component(id="windows-font-metrics") 后复制该组件，直接用 run_engine_python 运行 work/windows_font_metrics.py。
        从实际归档仅提取所用字体和配置；不需解包媒体，也不为测字宽编写或编译 C#。
        样本包含已知正常原文、中文试译中的独有简体字、标点和 ASCII；缺字检查不能只使用中日共有汉字。
        返回的 selectedFace 可能是本地化别名，名字不同不直接等于替换；missingCount 则说明主字体确实缺少被测字形。
        不要仅以“运行时未测试”代替处理已经证实的缺字；字体修复在 build 中确定性完成，不需每次生成补丁再调用模型。

        ## 最小验证闭环

        选择短脚本及有姓名、选项、标签的样本，保留原始文件、条目名和相对路径。
        无修改时核对原字节；修改后逐条核对预期译文、ID、位置及所有未修改项。
        包含中文、ASCII 控制语法、日文及 ♪/・ 等符号，不能只比较文本数量和 ID 集合。
        编码失败应返回具体失败，不能保留原文却计入成功回填。
        XP3 重建必须重新读取索引，确认替换条目仍被登记、原有条目未丢失且内容正确。
        覆盖关系以实际资源搜索路径和归档优先级确认，不能仅靠归档内完整名称相等判断同一脚本。
        小样通过再执行完整 test_adapter。资源验收和游戏内启动、中文显示、字体及换行分别记录。
        """;
}
