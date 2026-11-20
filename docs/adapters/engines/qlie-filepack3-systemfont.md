# QLIE 封包适配、字体与启动组件

## 封包脚本模板

`qlie-pack3-imos` 适用于同一启动目录下的 `FilePackVer3.0`、`HashVer1.3` 与 ImoScripter `FormatType=1`、`ReturnCode=[n]`。模板用 EXE 的 `IconKeyImage` 和 `DLL/key.fkey` 读取每卷的内部密钥，并核对各卷的脚本配置。模板从索引中选出脚本及配置所需的卷，匹配的 EXE 和转区配置按原字节保留。

模板按原封包和成员路径区分同名脚本，提取对白、人名、选项及已识别的显示字段。回填复用原封包的成员顺序和查找表，校验改写内容和未改动的成员。CP932 字符范围之外的译文需要对应的编码和字体方案。

匹配的游戏可先在本地生成模板并验收原文往返。中文试写仍使用完整资源验收，未完成时可从现有源码和已通过的阶段继续。系统文本和实际游戏显示需单独验收。[智能适配流程](../automatic-adaptation.md)

## 字体组件接口

`qlie-sfnt-font` 和 `qlie-japanese-font-launcher` 适用于已确认的 QLIE 单字体布局与 Windows 日语启动链。

用 `component("qlie_font", request, input=..., output=..., family=..., map=...)` 生成配套 SFNT 字体。配置名使用字体 `name` 表 ID 4 的完整 Regular 名称，保留原有外字槽并附字体许可。[组件接口](../components.md#接入与版本管理)

复制 `qlie-sfnt-font` 后，运行组件说明中的 `probeCall`，传入一个解包的 CP932 脚本、候选 SFNT 字体和已确认可用的码位。小样会试写简体字、复读译文并生成配套字体。正式适配仍需验收整包回填和游戏显示。

## 启动组件接入与构建

将 `QlieLauncher.exe` 作为补丁入口，旁置 `qlie-l10n/launch.xml` 和字体。配置路径相对启动器，`face` 须和字体内部名称一致。需要转区时使用玩家安装的 Locale Emulator，并保留原 EXE 的转区配置。游戏原 EXE 继续保留。

用 `scripts/build_qlie_launcher.py` 构建 Windows x64 Rust 启动器，输出至 `target/runtime/adapter-components`。启动自检后需在原游戏中确认字体选择与显示。[启动器源码](../../../tools/GameL10n.QlieLauncher) · [工具准备](../../development.md#工具准备)
