# QLIE 封包适配、字体与启动组件

## 封包脚本模板

`qlie-pack3-imos` 适用于同一启动目录下的 `FilePackVer3.0`、`HashVer1.3` 与 ImoScripter `FormatType=1`、`ReturnCode=[n]`。模板用 EXE 的 `IconKeyImage` 和 `DLL/key.fkey` 读取每卷的内部密钥，并核对各卷的脚本配置。模板从索引中选出脚本及配置所需的卷，匹配的 EXE 和转区配置按原字节保留。

模板按原封包和成员路径区分同名脚本，提取对白、人名、选项及已识别的显示字段。回填复用原封包的成员顺序和查找表，校验改写内容和未改动的成员。

## 系统字体布局

模板检查外置 `GameData/System/config.txt` 和卷内唯一的 `system/font/fontdata.dat`。支持 0 到 7 号单层字体布局，使用 MS Gothic 或 MS Mincho 的 `system`、`system_fix` 字体，可保留独立的 6 号外字位图槽。其他布局仍可使用严格 CP932 的提取和回填。

遇到 CP932 无法往返的译文时，模板跳过脚本及字体配置中已用的双字节码位，给新增字符分配字形，生成对应的 Noto SC 衍生字体和启动配置。补丁保留字形映射和 OFL 许可。再次提取时按原封包、成员和字段回读译文，沿用既有文本 ID。

匹配的游戏可在首轮模型请求前完成原文逐字节验收、少量中文试写和全量中文回填验收。系统文本、实际游戏显示及排版仍需确认。[智能适配流程](../automatic-adaptation.md)

## 字体组件接口

`qlie-sfnt-font` 提供其他已确认单字体布局所需的独立字体试验。模板使用随附的字体素材和 `qlie-japanese-font-launcher`。

用 `component("qlie_font", request, input=..., output=..., family=..., map=...)` 生成配套 SFNT 字体。配置名使用字体 `name` 表 ID 4 的完整 Regular 名称，保留原有外字槽并附字体许可。[组件接口](../components.md#接入与版本管理)

复制 `qlie-sfnt-font` 后，运行组件说明中的 `probeCall`，传入一个解包的 CP932 脚本、候选 SFNT 字体和已确认可用的码位。小样会试写简体字、复读译文并生成配套字体。正式适配仍需验收整包回填和游戏显示。

## 启动组件接入与构建

通过补丁的 `QlieLauncher.exe` 启动。它读取 `qlie-l10n/launch.xml` 并为当前游戏加载配套字体。配置路径相对启动器，`face` 须和字体内部名称一致。需要转区时使用玩家安装的 Locale Emulator，并保留原 EXE 和对应转区配置。

用 `scripts/build_qlie_launcher.py` 构建 Windows x64 Rust 启动器，输出至 `target/runtime/adapter-components`。启动自检后需在原游戏中确认字体选择与显示。[启动器源码](../../../tools/GameL10n.QlieLauncher) · [工具准备](../../development.md#工具准备)
