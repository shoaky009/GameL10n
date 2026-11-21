# QLIE 封包适配、字体与启动组件

## 封包脚本模板

两个模板复用 ImoScripter `FormatType=1`、`ReturnCode=[n]` 的脚本处理，按同一启动目录下 `GameData` 的封包版本选择索引、密钥和回包实现。

- `qlie-pack3-imos` 适用于 `FilePackVer3.0` 与 `HashVer1.3`。模板用 EXE 的 `IconKeyImage` 和 `DLL/key.fkey` 读取每卷的内部密钥，按原字节保留匹配的 EXE 和转区配置。
- `qlie-pack31-imos` 适用于 `FilePackVer3.1`、`HashVer1.4` 和首项内置密钥布局。按成员识别脚本编码，UTF-16 保留字节序和 BOM，CP932 脚本可使用下述字形映射。`[rb,正文,注音]` 作为完整标签保护。

模板核对各卷的脚本配置，从索引中选出脚本和配置所需的卷。按原封包及成员路径区分同名脚本，提取对白、人名、选项及已识别的显示字段。回包保留成员顺序及查找表，验收会回读改写成员并校验其他成员的原字节。卷的覆盖顺序、系统文本、中文显示及排版需单独确认。

自动组合 3.1 时，目录外的同名副本需要与安装卷大小和 SHA-256 一致。根目录的卸载器封包需要有卸载程序引用，且索引中的资源限于位图、图标和文本。其余封包需要确认加载和文字覆盖范围。遇到未知或混合的 FilePack 版本时，诊断会记录各卷版本和索引阶段，供后续确认读写布局。[智能适配流程](../automatic-adaptation.md)

## 系统字体布局

3.0 模板检查外置 `GameData/System/config.txt` 和卷内唯一的 `system/font/fontdata.dat`。支持 0 到 7 号单层字体布局，使用 MS Gothic 或 MS Mincho 的 `system`、`system_fix` 字体，可保留独立的 6 号外字位图槽。其他布局仍可使用严格 CP932 的提取和回填。

对于使用 Locale Emulator 的 3.1 游戏，模板按根目录中成对出现的原 EXE 和同名 `.exe.le.config` 确定启动目标。支持卷内 `system/font/fontdata.txt` 的双层布局，其中 0 号槽使用 `system` 和 `system_ext`，1 到 7 号槽继承 `base`。生成配套字体时，模板同步更新外置配置、卷内 `Avg/ImoScripter_FontList.txt` 和 `Avg/Setting.txt` 中的字体名，保留各配置的编码、继承项、字号和样式。补丁安装后双击 `QlieLauncher.exe`，启动器加载字体并使用原 EXE 的日语 LE 配置启动游戏。

遇到 CP932 无法往返的译文时，模板跳过脚本及字体配置中已用的双字节码位，给新增字符分配字形，生成对应的 Noto SC 衍生字体和启动配置。补丁保留字形映射和 OFL 许可。再次提取时按原封包、成员和字段回读译文，沿用既有文本 ID。

匹配的游戏可在首轮模型请求前完成原文逐字节验收、少量中文试写和全量中文回填验收。需要补字时，宿主从固定上游获取对应的字体和许可，校验 SHA-256 后存入共享下载缓存。首次缺缓存时需要联网。游戏补丁随附生成的字体及许可。系统文本、实际游戏显示及排版仍需确认。[智能适配流程](../automatic-adaptation.md) · [字体来源和哈希](../../../third_party/qlie-fonts/sources.json)

部分和全量试译都会用宿主内置的 ImoScripter 读取器核对改写的字段。脚本使用 CP932 私用码位承载中文时，宿主会沿卷内字体表、外置配置和配套启动器核对游戏选择的字体，并检查替代码位与目标汉字在字体中指向同一字形。缺少显示所需的资源时，验收会指出失败的字符或字体，阻止激活。已有 FilePack 适配器首次复用时会补做本地验收，同一适配器、资源内容和验收版本可复用通过结果。

## 字体组件接口

`qlie-sfnt-font` 提供其他已确认单字体布局所需的独立字体试验。模板使用按需准备的字体素材和 `qlie-japanese-font-launcher`。单独检查素材时可用 `inspect_adapter_component` 查询 `qlie-systemfont-assets` 的固定下载地址及哈希，使用 `download_tool` 下载到 `work/`。

用 `component("qlie_font", request, input=..., output=..., family=..., map=...)` 生成配套 SFNT 字体。配置名使用字体 `name` 表 ID 4 的完整 Regular 名称，保留原有外字槽并附字体许可。[组件接口](../components.md#接入与版本管理)

复制 `qlie-sfnt-font` 后，运行组件说明中的 `probeCall`，传入一个解包的 CP932 脚本、候选 SFNT 字体和已确认可用的码位。小样会试写简体字、复读译文并生成配套字体。正式适配仍需验收整包回填和游戏显示。

## 启动组件接入与构建

通过补丁的 `QlieLauncher.exe` 启动。它读取 `qlie-l10n/launch.xml` 并为当前游戏加载配套字体。配置路径相对启动器，`face` 须和字体内部名称一致。需要转区时使用玩家安装的 Locale Emulator，并保留原 EXE 和对应转区配置。

用 `scripts/build_qlie_launcher.py` 构建 Windows x64 Rust 启动器，输出至 `target/runtime/adapter-components`。启动自检后需在原游戏中确认字体选择与显示。[启动器源码](../../../tools/GameL10n.QlieLauncher) · [工具准备](../../development.md#工具准备)
