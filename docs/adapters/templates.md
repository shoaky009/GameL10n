# 组件组合模板

模板提供适配器协议入口、资源发现、解析器连接与发布依赖。

| 模板 ID | 封包组件 | 脚本解析器 | 资源规则 |
| --- | --- | --- | --- |
| `loose-renpy` | 无 | Renpy / `.rpy` | `**/*.rpy` |
| `loose-kirikiri-ks` | 无 | KirikiriKs / `.ks` | `**/*.ks` |
| `rpa3-renpy` | `archive.rpa3` / 2 | Renpy / `.rpy` | `**/*.rpa` |
| `loose-renpy-rpyc` | 无 | RPYC2 惰性 AST | `**/*.rpyc` |
| `rpa3-renpy-rpyc` | `archive.rpa3` / 2 | RPYC2 惰性 AST | `**/*.rpa` |
| `renpy-mixed` | `archive.rpa3` / 2（有封包时） | Renpy 源码 + RPYC2 | `.rpy`、`.rpyc`、`.rpa` |
| `xp3-kirikiri-ks` | `archive.xp3` / 1 | KirikiriKs / `.ks` | `**/*.xp3` |

模板检查目标冲突后写入 `src/`，配置位于 `src/tools/adapter-template.json`；相同内容复用，不同内容保留并报错。依赖使用固定共享组件。

生成模板后仍需完整资源验收。首次新建适配可自动完成前六种组合的准备、验收与发布，条件见[首次自动组合](adaptation-internals.md#首次自动组合)。XP3 须独立确认过滤器并声明 `filter: "none"`。

## Ren’Py 混合资源

`renpy-mixed` 读取完整封包索引和松散文件路径，要求封包处于同一加载根，松散脚本位于根内。同名 `.rpy` / `.rpyc`、不同包的重复脚本路径、未知加载根或模块均停止自动处理。

不同章节可分别使用源码和编译脚本。纯媒体包只查索引，构建保留原字节，验收核对全部成员。格式限制见[RPYC2](engines/renpy-rpyc.md)，封包预算见[封包组件](engine-components.md)。

## 连接行为

探测只读取受限头部，完整解析在提取、构建和验证阶段执行。只解包所选脚本，其他成员保留在原包中。

`resourcePath` 指向外层封包，`locator` 保留内层脚本与解析器位置；ID 由外层路径和稳定解析器身份构成，不依赖译文长度。RPYC2 身份来自节点、行号、文本角色和选项位置，不使用 Pickle 字节偏移。

构建先校验 ID，再移除与原文相同的译文。无修改资源复制原字节，有修改资源重建并比较全部成员。支持的模板可通过[校验上下文](components.md#固定解析器与校验上下文)复用本次提取结果，同时保留格式专用检查。

Ren’Py 保留 UTF-8/BOM 策略，Kirikiri KS 按解析器规则写回 UTF-16。字体、转区、过滤器和加载顺序按当前游戏验证；运行状态在实机确认前为 `not-tested`。
