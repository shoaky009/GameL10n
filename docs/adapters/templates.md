# 封包组件与组合模板

选择模板前，先确认封包布局、过滤器和脚本类型。模板生成协议入口与依赖配置，当前资源仍需完整验收。

## 封包组件

| 组件 | 适用条件 |
| --- | --- |
| `archive.rpa3` | 单分块 RPA/ARC 3.0，使用纯数据 Pickle 索引。未知混淆或多分块需另行适配。 |
| `archive.xp3` | 标准 XP3 v1，需先独立确认无过滤器并声明 `filter: "none"`。带加密、私有过滤器或内嵌 EXE 的封包需另行适配。 |

两者支持 `inspect`、`extract` 和 `rebuild`，按成员名或扩展名选择提取。重建按精确成员名替换，并核对未修改成员。RPA 索引读取和选中成员提取可分开执行，需要校验全部成员时用 `verifyContents: true`。[封包接口与预算](../../adapters/components/engine_archive.py)

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

用 `compose_adapter_template` 准备源码和固定依赖。首次识别可自动尝试前六种组合，资源布局或验收不满足条件时进入后续调查。XP3 需要先确认过滤器再接入。[模板注册](../../crates/engines/src/adaptation_assets.rs) · [生成和验收流程](automatic-adaptation.md)

## Ren’Py 混合资源

封包和松散脚本须处于同一加载根。同名源码和编译脚本、跨包重复路径、自定义模块或未知加载关系需要单独调查。纯媒体包保留原内容。[RPYC2 范围](engines/renpy-rpyc.md)
