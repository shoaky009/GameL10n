# QLIE 字体与启动组件

`qlie-sfnt-font` 和 `qlie-japanese-font-launcher` 适用于已确认的 QLIE 单字体布局与 Windows 日语启动链。封包、脚本和资源加载顺序由当前适配器确认。

## 字体组件接口

用 `component("qlie_font", request, input=..., output=..., family=..., map=...)` 生成配套 SFNT 字体。配置名使用字体 `name` 表 ID 4 的完整 Regular 名称，保留原有外字槽并附字体许可。[组件接口](../components.md#接入与版本管理)

## 启动组件接入与构建

将 `QlieLauncher.exe` 作为补丁入口，旁置 `qlie-l10n/launch.xml` 和字体。配置路径相对启动器，`face` 须和字体内部名称一致。需要转区时使用玩家安装的 Locale Emulator，并保留原 EXE 的转区配置。游戏原 EXE 继续保留。

用 `scripts/build_qlie_launcher.py` 构建 Windows x64 Rust 启动器，输出至 `target/runtime/adapter-components`。启动自检后需在原游戏中确认字体选择与显示。[启动器源码](../../../tools/GameL10n.QlieLauncher) · [工具准备](../../development.md#工具准备)
