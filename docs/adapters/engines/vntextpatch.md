# VNTextPatch 游戏脚本

内置 29 个脚本解析器，覆盖 22 个引擎系列。来源为 MIT 许可的 [VNTextPatch-net8 固定版本](https://github.com/rafael-vasconcellos/VNTextPatch-net8/tree/e68e174f3db6181e03fc7fabfa7570e1b293d148)。

## 资源与运行条件

表中资源均为已解包脚本，封包处理见[封包组件](../engine-components.md)。扩展名用于筛选，实际匹配还需检查结构。

“SJIS”表示中文需要额外的运行时编码与绘制支持，通用解析器会拒绝原编码无法表示的译文；可通过智能适配补足代理和字体。“Unicode 字体”表示编码可存储中文，仍需字体覆盖。

| 引擎／脚本格式 | 解析器键 | 候选资源 | 条件与限制 |
| --- | --- | --- | --- |
| AdvHD | `AdvHd` | `.ws2` | SJIS；对应的指令版本 |
| ArcGameEngine | `Age` | `.bin` | SJIS；同目录的 `CHINIT.BIN` 可提供角色名 |
| Artemis 二进制 | `ArtemisAsb` | `.asb` | UTF-8、Unicode 字体；对应的 ASB 指令结构 |
| Artemis Lua 表 | `ArtemisAst` | `.ast` | UTF-8、Unicode 字体；需要 `ast` 数据表 |
| Artemis 文本 | `ArtemisTxt` | `.art`、`.txt` | SJIS；Artemis 命令与文本结构 |
| CatSystem2 | `CatSystem` | `.cst` | SJIS；CST 脚本，不含外层封包 |
| Cyberworks C,system | `CSystem` | `.a0` | SJIS；对应的脚本布局 |
| Ethornell／BGI | `Ethornell` | `.bgi`、无扩展名 | SJIS；可识别的 BGI 脚本结构 |
| KaGuYa | `Kaguya` | `.dat`，常见 `message.dat` | SJIS；仅消息表格式，不泛指所有 DAT |
| Kirikiri KS | `KirikiriKs` | `.ks`、`.txt` | 回填 UTF-16；运行时需接受该编码并配置中文字体；角色名标签依游戏而异 |
| Kirikiri SOC | `KirikiriSoc` | `.soc` | 同 KS 文本处理；运行时需接受 UTF-16 |
| Kirikiri 编译 TJS | `KirikiriTjs` | `.tjs` | UTF-16 字符串、Unicode 字体；仅 `TJS2100` 编译格式，不含任意 TJS 源码 |
| Majiro | `Majiro` | `.mjo` | SJIS；对应的 MJO 字节码版本 |
| Musica | `Musica` | `.sc` | SJIS；对应的文本命令格式 |
| Mware | `Mware` | `.nut` | Squirrel v2 编译脚本；按文件编码需要 SJIS或 Unicode 字体 |
| Propeller／Stuff Script Engine | `Propeller` | `.msc` | SJIS；对应的 MSC 指令版本 |
| QLIE | `Qlie` | `.s` | SJIS 文件需要运行支持；Unicode 文件需要中文字体；与 FilePackVer3.0 专用适配器分开登记 |
| RealLive | `RealLive` | `.rl`、`.txt` | SJIS；已经解包的单个场景二进制，不是整个 `SEEN.TXT` 容器 |
| Ren'Py | `Renpy` | `.rpy` | UTF-8、中文字体；对白、旁白及菜单文字；不处理 `.rpyc`、`.rpa` 或 Python／资源路径字符串 |
| ShSystem | `ShSystem` | `.hst` | SJIS；对应的指令结构 |
| Silky's／AI6WIN 选项表 | `SilkysMap` | `.map` | SJIS；对应的 MAP 结构 |
| Silky's／AI6WIN 消息 | `SilkysMes` | `.mes` | SJIS；对应的 MES 指令版本 |
| Softpal | `Softpal` | `.src`，常见 `script.src` | SJIS；同目录必须同时有 `TEXT.DAT`、`POINT.DAT`，回填会更新脚本及文字表 |
| SystemNNN 开发格式 | `SystemNnnDev` | `.nnn` | SJIS；对应的开发脚本布局 |
| SystemNNN 发布格式 | `SystemNnnRelease` | `.spt` | SJIS；对应的发布脚本布局 |
| TmrHiroAdvSystem 指令 | `TmrHiroAdvSystemCode` | `.srp` | SJIS；对应的 SRP 指令结构 |
| TmrHiroAdvSystem 文字表 | `TmrHiroAdvSystemText` | 无扩展名、`.dat`、`.txt` | SJIS；逐条长度前缀的文字表，须完整满足长度及文本检查；上游没有固定扩展名 |
| Whale | `Whale` | `.txt`、无扩展名 | SJIS；需要 Whale 命令特征；上游没有固定扩展名 |
| [YU-RIS](yuris.md) | `Yuris` | `.ybn` | 默认严格 CP932，可显式传入固定 SJIS 隧道映射；`YSTB` 场景依赖同目录 `ysc.ybn` 命令表，另支持 `YSCF` 配置文字；运行支持需另验 |

标识为 `engine.vntextpatch-<小写解析器键>`。私有加密、指令和资源布局需进一步适配，无固定后缀的格式需符合候选发现规则。

## 使用与验证

保留原目录层次及伴随文件，尤其 Softpal 三类文件和 YU-RIS 命令表。先完成原文回填，再选段试译，确认显示、选项、控制标签与换行后全量翻译。

未修改资源保留原字节，修改后由解析器更新文本、长度、偏移和指令。编码构建失败仍保留译文，可在补足运行支持后复用。

## 引擎详细说明

[YU-RIS](yuris.md)说明来源元数据、分组和隧道编码；[组合模板](../templates.md)说明封包与脚本的接入。

## 未纳入的上游功能

- VNTextPatch 的 SCN 解析器未启用；相关能力见[SCN 组件](../components.md#scn-文本语义)。
- 上游表格交换格式、联网表格及自动换行配置不作为本应用功能提供。
