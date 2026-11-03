using System.Security.Cryptography;
using GameL10n.Core.Isolation;

namespace GameL10n.Core.Development;

/// <summary>Conditional research for MESSAGE v4.0; this supplies evidence, never a runtime patch.</summary>
internal static class KaguyaMessage40Guidance
{
    internal const string GraphicsSha256 = "E1292E3D47D1C65F4AE52A30B1159A476137422BD7A9272A5A2E0129F3CB0684";
    internal const string ReferencePath = "reference/Kaguya_MESSAGE_v4_0_runtime.md";
    internal const string EncodingWarning = "KaGuYa MESSAGE v4.0 内置适配器只提供 CP932 脚本提取和回填；可提取不代表已支持中文运行。CP932 无法表示的译文会报错，中文编码、字节遍历和字体绘制仍需验证。";
    private const int MaximumGraphicsBytes = 16 * 1024 * 1024;

    internal static object Describe(object? inspection) => new
    {
        scriptFormat = "[SCR-MESSAGE]ver4.0",
        parserReuse = "优先复用现有 KaguyaScript / BuiltinScriptParser 的提取、回填和复提校验，不重写 MESSAGE 解析器。仅确认版本或结构不兼容时才编辑源码。",
        translationMemory = "沿用原提取结果的 UnitId（协议 id）、ResourcePath、Locator、Context、Speaker 和 Kind；包装适配器不得重新编号或按显示类型重新分类，以免已有翻译记忆失效。",
        runtimeSupport = EncodingWarning,
        nextSteps = new[]
        {
            "在补丁生成时对实际输出的 Unicode 文本统计 CP932 不可编码字符和可用码位；没有译文时先用少量代表中文验证方案，不要求用户先全量翻译。数量随译文变化，不能固化样本缺字数，也不能把映射后的占位字符存回翻译记忆。",
            "F040/F041 在已调查运行库中是玩家姓名控制码；不要分配给中文字形。其他 PUA 码位仍须验证字节遍历、控制码冲突和绘制路径。",
            "先确认实际运行库和 GDI 导入，再协调编码、TextOutA 字节解释与字体方案；仅换字体或把 CP932 改成 CP936 不能证明游戏已支持中文。",
            "未知运行库须沿导入和 TextInitialize/绘制调用链重新定位；静态线索和独立 GDI 字宽测量都不等于游戏内验收。"
        },
        runtimeInspection = inspection,
        referenceSource = ReferencePath
    };

    // Only inspect already discovered Kaguya resources and their ancestor directories, never rescan the game tree.
    internal static object Inspect(string gameDirectory, IEnumerable<string> resources)
    {
        var messages = new List<string>();
        var libraries = new List<object>();
        var directories = new HashSet<string>(StringComparer.OrdinalIgnoreCase);
        var diagnostics = new List<string>();
        string root = Path.TrimEndingDirectorySeparator(Path.GetFullPath(gameDirectory));
        Span<byte> header = stackalloc byte["[SCR-MESSAGE]ver4.0"u8.Length];
        foreach (string resource in resources.Distinct(StringComparer.OrdinalIgnoreCase).Take(8))
        {
            try
            {
                string path = WorkspaceFiles.Resolve(root, resource);
                using var stream = File.OpenRead(path);
                if (stream.Length < header.Length) continue;
                stream.ReadExactly(header);
                if (!header.SequenceEqual("[SCR-MESSAGE]ver4.0"u8)) continue;
                messages.Add(resource.Replace('\\', '/'));
                for (string? parent = Path.GetDirectoryName(path); parent is not null; parent = Path.GetDirectoryName(parent))
                {
                    directories.Add(parent);
                    if (string.Equals(parent, root, StringComparison.OrdinalIgnoreCase) || directories.Count >= 24) break;
                }
            }
            catch (Exception error) when (IsReadError(error))
            {
                if (diagnostics.Count < 3) diagnostics.Add("无法检查 MESSAGE 样本：" + error.Message);
            }
        }

        object? knownRuntime = null;
        foreach (string candidate in directories.Take(24).SelectMany(directory => new[]
                     { Path.Combine(directory, "Graphics.dll"), Path.Combine(directory, "Lib", "Graphics.dll") })
                     .Distinct(StringComparer.OrdinalIgnoreCase))
        {
            try
            {
                string relative = Path.GetRelativePath(root, candidate);
                string path = WorkspaceFiles.Resolve(root, relative);
                if (!File.Exists(path)) continue;
                using var stream = File.OpenRead(path);
                if (stream.Length > MaximumGraphicsBytes)
                {
                    libraries.Add(new { path = relative.Replace('\\', '/'), status = "size-limit", sha256 = (string?)null });
                    continue;
                }
                string hash = Convert.ToHexString(SHA256.HashData(stream));
                var profile = MatchRuntime(hash, messages.Count > 0);
                libraries.Add(new { path = relative.Replace('\\', '/'), status = profile is null ? "unknown-runtime" : "known-runtime", sha256 = hash });
                knownRuntime ??= profile;
            }
            catch (Exception error) when (IsReadError(error))
            {
                if (diagnostics.Count < 3) diagnostics.Add("无法检查相邻 Graphics.dll：" + error.Message);
            }
        }
        return new
        {
            status = knownRuntime is not null ? "known-runtime-static-evidence" : messages.Count > 0 ? "message40-only" : "no-confirmed-message40",
            messageFiles = messages, graphicsLibraries = libraries, knownRuntime, diagnostics,
            runtimeStatus = "not-tested",
            scope = "只读取已发现的最多 8 个 Kaguya 样本文件头；在最多 24 个相邻/祖先目录中仅检查 Graphics.dll 和 Lib/Graphics.dll 两个固定路径，不递归搜索，不修改游戏。"
        };
    }

    internal static object? MatchRuntime(string sha256, bool message40) => message40
        && sha256.Equals(GraphicsSha256, StringComparison.OrdinalIgnoreCase) ? new
        {
            profile = "MESSAGE-v4.0/Graphics-sha256-E1292E3D",
            graphicsSha256 = GraphicsSha256,
            textInitializeChainRva = new[] { "TextInitialize", "0x3A8E0", "0x3AC20" },
            crtIsmbbleadCallRva = "0x3AD8A",
            originalCrtIsmbbleadTargetRva = "0x41248",
            playerNameControls = new[] { "0xF040", "0xF041" },
            application = "以上 RVA 仅是该原始 SHA256 对应运行库的静态调查线索，不能作为可直接应用的补丁。修改前核对原指令和调用关系；其他 SHA256 必须重新定位。",
            runtimeStatus = "not-tested"
        } : null;

    private static bool IsReadError(Exception error) => error is IOException or UnauthorizedAccessException
        or InvalidDataException or ArgumentException or System.Security.SecurityException;

    internal const string Reference = """
        # KaGuYa MESSAGE v4.0：条件性运行研究

        适用脚本头：[SCR-MESSAGE]ver4.0。现有 KaguyaScript 已实现消息、名称、选择项的读写；先复用内置 parser，不重复开发。
        包装层须保留原 UnitId/id、ResourcePath、Locator、Context、Speaker、Kind，以继续使用已有翻译记忆。
        解析和 CP932 原字节往返通过，只证明资源处理；没有证明中文在游戏中可遍历、布局和绘制。

        ## 已调查原始运行库

        Graphics.dll SHA256：E1292E3D47D1C65F4AE52A30B1159A476137422BD7A9272A5A2E0129F3CB0684。
        仅在该哈希及 MESSAGE v4.0 签名均确认时复用以下静态线索：TextInitialize → RVA 0x3A8E0 → 0x3AC20；
        RVA 0x3AD8A 调用 CRT_ismbblead，原目标 RVA 0x41248。它们是调查入口，没有已验收的 PE 补丁。
        未知哈希不套地址，使用 inspect_pe_imports/inspect_pe_code 沿当前导入与调用链重新定位。

        F040/F041 是已调查版本中的玩家姓名控制码。其他 PUA 码位尚须检查遍历、控制码和绘制支持，不能直接当作中文字库槽位。
        TextOutA 的字节解释必须与最终编码/字形方案一致；不能仅更换字体或全局切换 CP936。
        在原字体 +24 参数下，独立 GDI 测得 YaHei 字宽约 18、MS Gothic 约 24；SimHei/SimSun 的独立测量相近。
        这些数据只说明候选字体的测量差异，没有证明实际游戏中的换行、绘制和姓名输入正确。

        ## 下一步验证

        保留完整 Unicode 译文，在补丁生成时对实际输出文本统计严格 CP932 不可编码字符，再检查可用码位容量。
        尚无译文时先用少量代表中文验证机制；不要求先全量翻译，也不把完整文本送给模型计数。
        缺字数量是当前译文的数据，不能固化为通用常量。映射只能在补丁生成边界应用，不覆盖翻译记忆的真实中文。
        针对当前运行库验证实际字节遍历、TextOutA 字形、字体宽度/换行、F040/F041 姓名控制和全部补丁路径。
        完成资源和结构验收后可以发布用于试译的版本，须明确标记 runtimeStatus=not-tested，不能宣称已验证中文运行。
        未经验证的 PE 补丁仍是研究材料，不因脚本提取或字体测量通过而自动打包应用。

        本文是只读参考；copy_adapter 不将它复制到可编译的适配器源码中。
        """;
}
