# QLIE 封包小样、字体与启动组件

## 封包和脚本小样

智能适配可在当前启动目录结合 `GameData/*.pack`、`DLL/key.fkey` 和 EXE 的 `IconKeyImage`，试用共享库读取 `FilePackVer3.0`。在封包索引和 `HashVer1.3` 结构通过检查，且配置确认 `FormatType=1` 与 `ReturnCode=[n]` 时，程序检查一份 ImoScripter 脚本的提取、原字节回填和译文读回。

会话保存小样结论，继续适配时按当前制作输入复查。严格 CP932 无法表示的译文需要验证实际编码及字体方案。完整封包回填、文本覆盖和游戏内加载仍需验收。[智能适配流程](../automatic-adaptation.md)

## 字体组件接口

`qlie-sfnt-font` 和 `qlie-japanese-font-launcher` 适用于已确认的 QLIE 单字体布局与 Windows 日语启动链。

用 `component("qlie_font", request, input=..., output=..., family=..., map=...)` 生成配套 SFNT 字体。配置名使用字体 `name` 表 ID 4 的完整 Regular 名称，保留原有外字槽并附字体许可。[组件接口](../components.md#接入与版本管理)

## 启动组件接入与构建

将 `QlieLauncher.exe` 作为补丁入口，旁置 `qlie-l10n/launch.xml` 和字体。配置路径相对启动器，`face` 须和字体内部名称一致。需要转区时使用玩家安装的 Locale Emulator，并保留原 EXE 的转区配置。游戏原 EXE 继续保留。

用 `scripts/build_qlie_launcher.py` 构建 Windows x64 Rust 启动器，输出至 `target/runtime/adapter-components`。启动自检后需在原游戏中确认字体选择与显示。[启动器源码](../../../tools/GameL10n.QlieLauncher) · [工具准备](../../development.md#工具准备)
