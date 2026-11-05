# 历史 C# 分析脚本迁移到 Python

## 目标与当前状态

将 GameL10n 已有且需要继续使用的 C# 分析／处理脚本和适配逻辑迁移为 Python。本任务在持有旧数据的机器上按需执行，清单和迁移报告保存在该机器的数据副本或忽略的 `target/` 下，不随仓库分发。已发布适配器可继续运行，不要求全量转换历史探针。

用户允许按实际需要制作独立迁移工具，工具不纳入 GameL10n 主项目、正式脚本目录或发行包。脚本数量少时直接逐组迁移即可，不必先开发通用转换框架。遵循 KISS；按风险判断是否需要新增测试，优先使用真实样本与已有检查。

相关改动为 `05f1ea4`（`refactor: use Python for temporary adaptation analysis scripts`）：

- 已移除 `run_csharp` 工具和 `GeneratedAdapterCompiler.RunCSharpAsync` 临时编译链路。
- 临时分析统一使用 `run_engine_python`，位于 `development` 工具组；支持 `script` 或 `module`、`arguments`、`workingDirectory`、`stdin`、`timeoutSeconds`，使用内置共享 Python，无需系统 Python 或 pip。
- 新的引擎经验评审只导出 Python 辅助脚本；旧的 C# 经验仍可读取、复制作参考。
- 恢复上下文不再推荐历史 `run_csharp` 成功调用。原始会话、源码和日志不会因此被自动转换或删除。

当前新生成的正式适配器也统一使用 Python，入口 `src/adapter.py`，包格式为 Manifest v4；临时脚本与正式入口都不再编译 C#。编码、字体和内置解析器通过预编译宿主调用，发行包不携带 C# 编译器或引用程序集，共享 .NET 运行时仍保留。

已发布 Manifest v1～v3 的 `adapter.dll` 可继续原样运行，不要求批量迁移。需要继续修复、扩展或重新发布的旧实现，在独立工作区迁移为 Python 并通过完整资源验收后发布新版本；不要原地改写旧包。

## 开始前需要的信息

请用户提供实际数据目录、单个游戏项目目录或旧脚本目录；需要运行验证时再确认应用安装目录和可用样本。不要假设旧数据就在代码仓库，也不要因当前机器缺少旧数据而声称迁移完成。

默认数据目录为 `%LOCALAPPDATA%\GameL10n`，但用户可能通过设置或 `GAMEL10N_DATA`／旧变量 `AVG_TRANSLATOR_DATA` 改过位置。以实际目录为准。应用关闭或相关任务停止后，在副本或独立输出目录操作，保留原始 C#、会话和经验文件以便恢复。

## 扫描与分类

| 位置 | 处理方式 |
| --- | --- |
| `games/<项目 ID>/adaptation/current/work/**/*.cs` | 临时脚本候选；结合内容及调用记录确认用途，不能只看扩展名或目录 |
| `games/<项目 ID>/adaptation/current/session.json` | 查 `messages` 中的 `run_csharp` 调用、`successfulProbes` 和 `pendingCalls`，还原源文件组合、引用、参数、标准输入和工作目录 |
| `extensions/engine-knowledge/*.json` | 检查 `tools[].files` 内嵌的 C# 辅助源码、`usage` 和相关指引；不只是扫描磁盘上的 `.cs` |
| `adaptation/current/work/engine-knowledge/` | 已复制到项目的经验辅助源码，可能与全局经验重复或已有本地修改 |
| `adaptation/current/src/`、`src/tools/` | 结合 `releaseFiles` 和调用关系区分正式实现／依赖与误放的临时脚本；需要续接的正式逻辑迁移到 `src/adapter.py` 及 Python 模块，重新选择清单并验收 |
| `extensions/packages/` 及其 `package.json`、`adapter.dll` | 正式适配器及不可变版本，保留原状，不原地改源码、清单或哈希 |

`run_csharp` 的 `arguments` 字段在工具调用记录中是 JSON 字符串，需要解析后读取 `sourceFiles`、`references`、`arguments`、`workingDirectory`、`stdin`、`timeoutSeconds`。旧日志可能另有封装，按实际格式读取，不要硬套一种结构。

按原始调用所需的源文件和依赖组成迁移单元；多文件探针不能逐文件独立猜测转换。先按内容哈希识别重复脚本，再检查本地修改，避免覆盖已分叉的实现。先产出清单，注明临时脚本、正式实现、重复项和待确认项及依据。

## 转换与接入

1. 理解脚本的真实输入、输出、编码、二进制结构与副作用，进行语义迁移。不能用改扩展名或正则替换宣称完成 C# 到 Python 的转换。
2. 优先复用 Python 标准库和应用已固定的引擎库 API。原探针引用正式 C# 解析器时，优先使用现成解析器试用／CLI 或已有组件，不为迁移一个探针复制整套解析器，也不重新引入 C# 动态编译、pythonnet、pip 或运行时下载。
3. 保持参数、工作目录和文件行为；迁移时特别核对小端／大端、有符号整数、溢出、流位置、CP932／CP936、UTF-16 字符长度与字节偏移的区别。探针只处理暂存样本，不修改游戏原文件。
4. Python 临时入口放在对应工作区的 `work/`，保留原 C# 和独立迁移映射。通过 `run_engine_python` 运行，使用工作区相对路径；正式依赖才放在 `src/tools/` 并登记发布角色。
   正式入口使用 `src/adapter.py`，遵循现有 CLI v1 的 probe/extract/build/validate、稳定 ID、原文字节往返及输出声明契约。内置解析器可用 `tools.gamel10n_host.parser`，编码、字体可用 `component` 批量调用。旧包中随适配器编译的私有算法若无现成预编译接口，按实际语义迁移为 Python，不增加新的运行时编译路径。移除发布清单中不再运行的 C# 源码，保留其副本作参考。
5. 为迁移单元记录新的调用，例如：

   ```json
   {"script":"work/probe.py","arguments":["resources/sample.bin"],"workingDirectory":".","timeoutSeconds":120}
   ```

   具体路径和参数由真实脚本决定。不要把它作为所有脚本通用的固定输入，也不要向正式项目加入一次性迁移入口。

### 全局引擎经验

若迁移了 `tools[].files` 中的辅助源码，需要同步更新文件名、相关 Python 源依赖和 `usage`；保留有效的条件、结论、证据来源、启用状态及其他工具。本地已有修改的副本单独处理，不用全局版本覆盖。

经验文件具有内容版本校验。`EngineKnowledgeStore.ReadEntry` 会校验 `revision == ContentRevision(entry)`；直接改 `files` 或 `usage` 会令整条经验无法加载。优先复用现有 `SaveAsync` 保存规则；若独立工具直接写 JSON，必须严格复现 `ContentRevision` 的字段、排序、序列化和哈希规则，并验证实际宿主能够读取。不能用通用的 JSON 排序后哈希替代，也不能为了迁移取消版本校验。存在新版本时重新合并，不能覆盖较新的条目；同时核对经验摘要索引。

### 历史会话

保留 `messages`、工具结果、`providerData` 和原始日志的历史事实，不把已经执行过的 C# 调用批量改写成 Python 调用。历史 `successfulProbes` 可保留，当前程序会过滤旧工具；新 Python 探针仅在真实成功执行后才可记为成功。

检查 `pendingCalls` 中尚未执行的 `run_csharp` 是否影响恢复，结合当前宿主的待执行调用与工具结果协议处理，再通过新的 Python 调用继续。不要伪造工具成功结果，也不要通过清空整个会话来掩盖续接问题。迁移映射应明确指出新入口及参数，让后续智能分析能找到已转换的脚本。

## 验证与交付

- 先做 Python 语法检查；语法通过不代表行为等价。
- 选择能反映脚本用途的真实暂存样本，比较退出状态、关键结构化结果、计数、偏移、文件内容和必要的编码行为。可使用可信的历史结果作依据；需要重跑旧 C# 时复用已有环境，不为迁移新增自动下载的编译工具链。
- 只执行实际需要的样本检查。没有样本或依赖时标记“已转换、待验证”，列出缺少什么，不宣称迁移成功。
- 确认当前 `run_engine_python` 能执行新入口；修改过的全局经验能通过原有版本校验读取；续接时能定位 Python 入口，不再要求运行 `run_csharp`。
- 确认正式包、游戏原文件、译文、原文快照和既有补丁未被迁移操作改变。单纯迁移探针不应触发正式包重编译或重新发布。

交付转换后的文件及简明迁移报告：原路径／哈希、新路径／哈希、所需依赖、新调用参数、验证依据，以及未迁移或待验证项的原因。临时迁移工具、映射、备份和报告置于用户数据副本或独立迁移目录；如必须借用仓库工作目录，使用已忽略的 `target/`。未经单独要求，不将这些真实数据、工具或运行产物提交到 GameL10n。

## 代码与协议入口

- [Python 执行及固定工具链连接](../../src/GameL10n.Engines/Development/AdaptationTools.EngineReference.cs)
- [Python 解释器与库路径、启动参数](../../src/GameL10n.Engines/Development/EngineReferenceToolkit.cs)
- [会话状态及探针恢复上下文](../../src/GameL10n.Engines/Development/AutomaticAdaptationService.cs)
- [经验存储、SaveAsync 和 ContentRevision](../../src/GameL10n.Engines/Development/EngineKnowledgeStore.cs)
- [经验辅助源码复制](../../src/GameL10n.Engines/Development/AdaptationTools.Knowledge.cs)
- [产物边界与目录约定](../../docs/adapters/artifacts.md)
- [智能适配工具使用指引](../adapter-development.md)
