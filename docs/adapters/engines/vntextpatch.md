# VNTextPatch 游戏脚本

应用内置 VNTextPatch-net8 的全部 29 个已注册游戏脚本解析器，覆盖 22 个引擎系列。适配器按引擎及脚本格式登记，不按游戏标题登记；格式已内置并不表示所有游戏版本都已验证。

来源为 [VNTextPatch-net8](https://github.com/rafael-vasconcellos/VNTextPatch-net8)，固定版本 `e68e174f3db6181e03fc7fabfa7570e1b293d148`，采用 MIT 许可证。解析器随应用发布，使用时不需要下载上游仓库或自行安装 VNTextPatch。

## 资源与运行条件

下表列出的是**已经解包的脚本资源**。这些解析器不负责 XP3、ARC、RPA、PFS、SEEN.TXT 等游戏容器的通用解包和回包；游戏只提供封包时，需要先准备对应的解包／回包流程，或使用应用已有的专用封包适配器。扩展名只是候选筛选条件，探测还会解析内容并检查结构，不能通过修改后缀把未知文件变成受支持格式。

“SJIS 运行支持”表示：游戏需要额外的代码页或文字绘制适配才能显示 CP932 不支持的中文。**当前内置通用解析器会拒绝写入原编码无法表示的译文**，不会仅生成 `sjis_ext.bin` 就宣称支持中文。上游的 SJIS 扩展映射还依赖与游戏版本、位数和文字绘制接口匹配的运行时代理；仅安装中文字体不足以实现它。此类游戏需要通过智能适配或专用适配器补足运行支持。“Unicode 字体”表示脚本编码能存储中文，仍需游戏实际使用的字体包含字形，并验证换行与排版。

| 引擎／脚本格式 | 解析器键 | 候选资源 | 中文运行前提与格式限制 |
| --- | --- | --- | --- |
| AdvHD | `AdvHd` | `.ws2` | SJIS 运行支持；对应的指令版本 |
| ArcGameEngine | `Age` | `.bin` | SJIS 运行支持；同目录的 `CHINIT.BIN` 可提供角色名 |
| Artemis 二进制 | `ArtemisAsb` | `.asb` | UTF-8、Unicode 字体；对应的 ASB 指令结构 |
| Artemis Lua 表 | `ArtemisAst` | `.ast` | UTF-8、Unicode 字体；需要 `ast` 数据表 |
| Artemis 文本 | `ArtemisTxt` | `.art`、`.txt` | SJIS 运行支持；Artemis 命令与文本结构 |
| CatSystem2 | `CatSystem` | `.cst` | SJIS 运行支持；CST 脚本，不含外层封包 |
| Cyberworks C,system | `CSystem` | `.a0` | SJIS 运行支持；对应的脚本布局 |
| Ethornell／BGI | `Ethornell` | `.bgi`、无扩展名 | SJIS 运行支持；可识别的 BGI 脚本结构 |
| KaGuYa | `Kaguya` | `.dat`，常见 `message.dat` | SJIS 运行支持；仅消息表格式，不泛指所有 DAT |
| Kirikiri KS | `KirikiriKs` | `.ks`、`.txt` | 回填 UTF-16；运行时需接受该编码并配置中文字体；角色名标签依游戏而异 |
| Kirikiri SOC | `KirikiriSoc` | `.soc` | 同 KS 文本处理；运行时需接受 UTF-16 |
| Kirikiri 编译 TJS | `KirikiriTjs` | `.tjs` | UTF-16 字符串、Unicode 字体；仅 `TJS2100` 编译格式，不含任意 TJS 源码 |
| Majiro | `Majiro` | `.mjo` | SJIS 运行支持；对应的 MJO 字节码版本 |
| Musica | `Musica` | `.sc` | SJIS 运行支持；对应的文本命令格式 |
| Mware | `Mware` | `.nut` | Squirrel v2 编译脚本；按文件编码需要 SJIS 运行支持或 Unicode 字体 |
| Propeller／Stuff Script Engine | `Propeller` | `.msc` | SJIS 运行支持；对应的 MSC 指令版本 |
| QLIE | `Qlie` | `.s` | SJIS 文件需要运行支持；Unicode 文件需要中文字体；与 FilePackVer3.0 专用适配器分开登记 |
| RealLive | `RealLive` | `.rl`、`.txt` | SJIS 运行支持；已经解包的单个场景二进制，不是整个 `SEEN.TXT` 容器 |
| Ren'Py | `Renpy` | `.rpy` | UTF-8、中文字体；对白、旁白及菜单文字；不处理 `.rpyc`、`.rpa` 或 Python／资源路径字符串 |
| ShSystem | `ShSystem` | `.hst` | SJIS 运行支持；对应的指令结构 |
| Silky's／AI6WIN 选项表 | `SilkysMap` | `.map` | SJIS 运行支持；对应的 MAP 结构 |
| Silky's／AI6WIN 消息 | `SilkysMes` | `.mes` | SJIS 运行支持；对应的 MES 指令版本 |
| Softpal | `Softpal` | `.src`，常见 `script.src` | SJIS 运行支持；同目录必须同时有 `TEXT.DAT`、`POINT.DAT`，回填会更新脚本及文字表 |
| SystemNNN 开发格式 | `SystemNnnDev` | `.nnn` | SJIS 运行支持；对应的开发脚本布局 |
| SystemNNN 发布格式 | `SystemNnnRelease` | `.spt` | SJIS 运行支持；对应的发布脚本布局 |
| TmrHiroAdvSystem 指令 | `TmrHiroAdvSystemCode` | `.srp` | SJIS 运行支持；对应的 SRP 指令结构 |
| TmrHiroAdvSystem 文字表 | `TmrHiroAdvSystemText` | 无扩展名、`.dat`、`.txt` | SJIS 运行支持；逐条长度前缀的文字表，须完整满足长度及文本检查；上游没有固定扩展名 |
| Whale | `Whale` | `.txt`、无扩展名 | SJIS 运行支持；需要 Whale 命令特征；上游没有固定扩展名 |
| YU-RIS | `Yuris` | `.ybn` | SJIS 运行支持；`YSTB` 场景依赖同目录 `ysc.ybn` 命令表，另支持 `YSCF` 配置文字 |

每项适配器标识为 `engine.vntextpatch-<小写解析器键>`，例如 `engine.vntextpatch-kirikiriks`。上游没有固定后缀的格式仅按表中的候选文件名进行自动探测；其他命名需要增加对应发现规则。某一游戏的私有加密、修改过的指令集、额外控制标签或定制资源布局仍可能需要适配。

## 使用与验证

选择包含脚本的游戏目录后，应用按资源结构探测可用格式。资源有目录层次时，应保留原目录和伴随文件；Softpal 的三类文件及 YU-RIS 场景与命令表不能拆开。含多种脚本格式的目录会分别登记对应的适配器。

先完成原文回填验证，再试译少量对白、角色名和选项。无文本修改时保留原文件字节；有修改时由对应解析器重建文本与必要的长度、偏移或指令数据，部分格式会重新序列化整个文件。结构验证通过后，仍需进入游戏检查中文显示、选项跳转、控制标签和换行；这一步通过后再做全量翻译。

脚本回填与游戏运行验证是不同的能力。当前这些通用解析器不自动证明某个游戏的中文字体、运行时代理或资源加载顺序可用；兼容记录应分别保留资源验证状态与游戏运行状态。因编码限制而构建失败的翻译仍保留在任务缓存中，补足适配后可继续使用。图片、视频中的文字及解析器未识别的界面文本不包含在通用脚本翻译范围中。

## 未纳入的上游功能

- Kirikiri `.scn`：该 .NET 8 分支已禁用此解析器，缺少 FreeMote 依赖；不列为内置支持。
- JSON、JSONL、YAML、Excel、Google Sheets：它们是上游翻译交换／存储格式，不是游戏引擎解析器；本集成使用应用自己的翻译任务和数据格式。
- 上游命令行的联网表格、字体测量配置和自动换行设置不直接移植为应用功能。现有脚本的控制符和换行由应用与解析器共同保留，仍需按具体游戏试译检查。

这些适配器目前提供格式级集成验证，尚未建立覆盖全部引擎的真实游戏运行兼容清单。
