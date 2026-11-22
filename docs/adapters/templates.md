# 封包组件与组合模板

选择模板前，先确认封包布局、过滤器和脚本类型。模板生成协议入口与依赖配置，当前资源仍需完整验收。

## 封包组件

| 组件 | 操作 | 适用条件 |
| --- | --- | --- |
| `archive.rpa3` | `inspect`、`extract`、`rebuild` | 单分块 RPA/ARC 3.0，使用纯数据 Pickle 索引。 |
| `archive.xp3` | `inspect`、`extract`、`rebuild` | 标准 XP3 v1，需先独立确认无过滤器并声明 `filter: "none"`。 |
| `archive.bgi` | `inspect`、`extract`、`rebuild` | PackFile 与 BURIKO ARC20，支持原始成员和 DSC 解码。带头 V1 脚本可回填到布局能逐字节复现的原卷。 |
| `archive.kif` | `inspect`、`extract` | CatSystem2 普通 KIF 和可由游戏 EXE 确认成员名的加密脚本卷。加密卷用 `keyExecutable` 指定已暂存的 EXE。 |
| `archive.ypf482` | `inspect`、`extract` | YPF 482，成员名使用 Murmur2 校验和 FF 异或。 |
| `archive.ypf474` | `inspect`、`extract`、`rebuild` | 32 位偏移、FF 文件名异或、名称 CRC32 和成员 Adler32。支持最多 64 MiB 的脚本卷回填。 |
| `archive.will-v1` | `inspect`、`extract`、`rebuild` | 按扩展名分组、13 字节基础名。保留成员的存储字节，最多 64 MiB 的卷可回填。 |
| `archive.pfs8` | `inspect`、`extract` | Artemis PFS v8，使用索引派生的密钥读取成员。 |
| `archive.nekopack4a` | `inspect`、`extract` | NEKOPACK4A，按索引读取选定成员并验证 zlib 解压长度。 |
| `archive.whale-dat` | `inspect`、`extract`、`rebuild` | Whale DAT v2，脚本解码使用 `gameTitle`，媒体成员使用 `filenameManifest` 匹配 CRC64。重建适用于纯脚本卷。 |

索引组件可以只读索引，再按成员名或扩展名提取。清单中的 `length` 为封包中的长度，已读取成员的 SHA-256 取解码后的字节。需要校验全部成员时用 `verifyContents: true`，适用组件的读取和解码预算。支持重建的组件按原成员名替换，并核对未修改成员。[封包接口与预算](../../adapters/components/engine_archive.py)

CatSystem2 加密脚本卷从 EXE 的 PE 资源读取成员名密钥，以 `.cst` 成员和解码后的 `CatScene` 签名交叉验证。首次识别检查封包同目录的小量 EXE，选中结果包含可供组件复用的 EXE 路径。整卷回填或补丁覆盖顺序需要单独确认。

BGI 重建接受按原成员名提交的已解码 V1 脚本。输入和输出封包各限 64 MiB。已改脚本沿用各自的原始或 DSC 封装，其余成员的存储字节、索引顺序及附加字段原样保留。中文编码和游戏加载仍需试译确认。

## 组合模板

| 模板 ID | 资源 |
| --- | --- |
| `loose-renpy` | 松散 `.rpy` |
| `loose-kirikiri-ks` | 松散 `.ks` |
| `rpa3-renpy` | RPA3 中的 `.rpy` |
| `loose-renpy-rpyc` | 松散 RPYC2 |
| `rpa3-renpy-rpyc` | RPA3 中的 RPYC2 |
| `renpy-mixed` | 同一加载根下的 `.rpy`、RPYC2 与 RPA3 |
| `xp3-kirikiri-ks` | 无过滤器 XP3 中的 `.ks` |
| `artemis-pfs8-ast` | PFS v8 中无跨卷路径冲突的 UTF-8 AST 2.0 |
| `nekopack4a-advscript2` | NEKOPACK4A 中无跨卷同名脚本的 CP932 ADVSCRIPT2 |
| `flash-avm1-pool` | [Flash AVM1 顶层动作的 UTF-8 常量池](engines/flash-avm1.md) |
| `system4-ain-msg1` | [AI2 封装内通过消息表和编码检查的 System4 AIN](engines/system4-ain14-msg1-fnl.md) |
| `qlie-pack3-imos` | [带密钥的 QLIE PACK 3.0 和 Hash 1.3 中的 ImoScripter FormatType=1](engines/qlie-filepack3-systemfont.md#封包脚本模板) |
| `qlie-pack31-imos` | [带内置密钥的 QLIE PACK 3.1 和 Hash 1.4 中的 ImoScripter FormatType=1](engines/qlie-filepack3-systemfont.md#封包脚本模板) |

用 `compose_adapter_template` 准备源码和固定依赖。首次识别可自动尝试 Ren’Py、松散 KS 和 Artemis PFS 组合，资源布局或验收不满足条件时进入后续调查。XP3 需要先确认过滤器。NekoSDK 和 QLIE 小样符合各自的验收条件时，可在请求模型前组合模板并验证整卷。已有符合样本的中文回填配方时优先复用该配方。[模板注册](../../crates/engines/src/adaptation_assets.rs) · [生成和验收流程](automatic-adaptation.md)

封包模板在注册项中声明候选证据、资源模式和固定模块。模块的 `probe` 返回完整验收需要的资源和已检查索引的指纹。宿主按同一套验收与发布流程处理，并在发布前复查索引。

## Ren’Py 混合资源

封包和松散脚本须处于同一加载根。同名源码和编译脚本、跨包重复路径、自定义模块或未知加载关系需要单独调查。纯媒体包保留原内容。[RPYC2 范围](engines/renpy-rpyc.md)
