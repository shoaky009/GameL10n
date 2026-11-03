namespace GameL10n.Core.Development;

/// <summary>Conditional text-format guidance; it does not establish a game's runtime support.</summary>
internal static class KirikiriTextGuidance
{
    internal const string ReferencePath = "reference/Kirikiri_text_encoding.md";
    internal const string Summary = "Kirikiri KS/SOC 内置解析器可读 CP932、带 BOM 的 UTF-8/UTF-16，修改后写 UTF-16；先用 try_builtin_adapter 验证少量解包样本，再 copy_adapter 复用。运行时是否接受 Unicode 须沿实际脚本加载路径确认，未搜到 BOM 常量不能排除支持。不要用 CP932/CP936 混写加私有注释标记代替游戏端解码支持；小样须逐条比较译文与未修改项，编码失败必须明确失败。";

    internal static bool AppliesTo(string format) => format.Equals("KirikiriKs", StringComparison.OrdinalIgnoreCase)
        || format.Equals("KirikiriSoc", StringComparison.OrdinalIgnoreCase);

    internal static object Describe() => new
    {
        parserReuse = Summary,
        archiveSupport = "XP3 容器、脚本语义、中文编码和字体是不同能力。现有适配器失败时先检查可复制的源码；容器组件拒绝过滤标记或超出预算，不代表 KS 解析器不可复用。TJS 源码与 TJS2100 编译脚本分别处理。",
        minimalTrial = "先选一个短 KS 和含姓名/选项的代表样本：原文往返、少量中文及保留符号（如 ♪、・）、局部修改后复提。比较实际译文、ID、位置和所有未修改项；封包重建另核对修改与未修改条目的清单及内容。小样通过后再完整 test_adapter。",
        runtimeEvidence = "确认脚本字节缓冲实际到达的解码函数及编码选择条件。导入表、宿主 ACP、几种指令字节未命中或自定义 encoder/decoder 往返，都不能证明游戏的脚本编码或显示支持。",
        referenceSource = ReferencePath
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
