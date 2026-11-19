# 封包组件与组合模板

选择模板前，先确认封包布局、过滤器和脚本类型。模板生成协议入口与依赖配置，当前资源仍需完整验收。

## 封包组件

| 组件 | 操作 | 适用条件 |
| --- | --- | --- |
| `archive.rpa3` | `inspect`、`extract`、`rebuild` | 单分块 RPA/ARC 3.0，使用纯数据 Pickle 索引。 |
| `archive.xp3` | `inspect`、`extract`、`rebuild` | 标准 XP3 v1，需先独立确认无过滤器并声明 `filter: "none"`。 |
| `archive.bgi` | `inspect`、`extract` | PackFile 与 BURIKO ARC20，支持原始成员和 DSC 解码。 |
| `archive.kif` | `inspect`、`extract` | CatSystem2 普通 KIF，独立确认加密命名方案后可用固定 INT API 调查加密变体。 |
| `archive.ypf482` | `inspect`、`extract` | YPF 482，成员名使用 Murmur2 校验和 FF 异或。 |
| `archive.pfs8` | `inspect`、`extract` | Artemis PFS v8，使用索引派生的密钥读取成员。 |
| `archive.nekopack4a` | `inspect`、`extract` | NEKOPACK4A，按索引读取选定成员并验证 zlib 解压长度。 |

索引组件可以只读索引，再按成员名或扩展名提取。清单中的 `length` 为封包中的长度，已读取成员的 SHA-256 取解码后的字节。需要校验全部成员时用 `verifyContents: true`，适用组件的读取和解码预算。支持重建的组件按原成员名替换，并核对未修改成员。[封包接口与预算](../../adapters/components/engine_archive.py)

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

用 `compose_adapter_template` 准备源码和固定依赖。首次识别可自动尝试 Ren’Py、松散 KS 和 Artemis PFS 组合，资源布局或验收不满足条件时进入后续调查。XP3 需要先确认过滤器。NekoSDK 首轮尝试封包成员小样，组合模板供明确脚本和运行编码后使用。[模板注册](../../crates/engines/src/adaptation_assets.rs) · [生成和验收流程](automatic-adaptation.md)

封包模板在注册项中声明候选证据、资源模式和固定模块。模块的 `probe` 返回完整验收需要的资源和已检查索引的指纹。宿主按同一套验收与发布流程处理，并在发布前复查索引。

## Ren’Py 混合资源

封包和松散脚本须处于同一加载根。同名源码和编译脚本、跨包重复路径、自定义模块或未知加载关系需要单独调查。纯媒体包保留原内容。[RPYC2 范围](engines/renpy-rpyc.md)
