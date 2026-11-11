# 智能适配工具使用指引

供获得 GameL10n 智能适配工具的 LLM 按任务查阅。工具参数以当前请求提供的定义为准；此文件不由程序自动加载。

## 选择能力与继续任务

先核对当前目标、已有源码、资源、失败证据及发布状态。修复已有适配时，用 `inspect_existing_text` 查看相关原文基线，保留 ID、位置、原文、上下文、说话人、类型和保护标记。

全部工具定义按固定顺序提供，可直接调用。`load_tool_groups` 仅维护 `reuse`、`development`、`research`、`native`、`unity`、`siglus` 或 `all` 分类，不裁减 API 工具集，也不运行工具或下载依赖。检查点、恢复和历史压缩不改变定义及顺序。

优先复用当前样本试验和已完成验收。旧结果被缩短时，用 `read_tool_result` 按调用 ID 回读；旧分析或参数用 `read_session_history` 查回，不为恢复上下文重复执行实验。

## 内置脚本解析器

智能体可以用 `list_adapters` 检索内置和已发布的能力，用 `inspect_builtin_adapter` 查看相关解析器的限制、调用说明及指定源码片段。`try_builtin_adapter` 直接处理工作副本中的少量脚本，不需要先生成调用程序：每次最多 12 个文件、合计 64 MiB，返回提取数量、少量文本预览、具体解析错误，以及默认执行的原文逐字节回填检查。伴随文件也应纳入样本。这个限制只约束开发阶段的快速试验，不限制正式任务规模。

解析器兼容时，`copy_adapter` 写入 Python 调用辅助模块和固定依赖清单，VNTextPatch NativeAOT EXE 与许可文件按内容哈希跨项目共享。`src/adapter.py` 调用预编译组件，补充资源包、格式变体或中文运行支持；新增逻辑统一使用 Python，不复制可编译 C# 源码。上游源码可按需读取结构参考。探测只使用文件头和有界范围，完整脚本解析用于提取、回填和结构验证。

候选线索及最近样本试验保存在现有 `session.json`，包含观察时间和试验输入哈希；继续适配和压缩上下文后仍可使用，不另建逐次报告。资源变化后可通过 `list_adapters` 的 `refreshCandidates` 刷新线索，必要时重新试验。样本检查不替代完整 `test_adapter` 验收，也不代表中文字体或游戏启动已经验证。

## 封包组件

先用 `inspect_engine_component` 确认支持范围与预算；`list_adapters.engineComponents` 也可查看目录。仅在当前资源符合限制时使用固定组件。详见[封包组件支持范围](../docs/adapters/engine-components.md)。

先 `stage_resources`，然后调用 `run_engine_component`，不必生成 Python 脚本：

```json
{
  "id": "archive.rpa3",
  "operation": "extract",
  "archive": "resources/game/scripts.rpa",
  "output": "work/rpa-scripts",
  "extensions": [".rpy"]
}
```

输出目录必须尚未存在。完整成员名、长度、SHA256 保存在 `work/rpa-scripts/manifest.json`，模型只收到最多 12 个成员的摘要。选中的脚本位于 `files/`，可以直接交给 `try_builtin_adapter`。`members` 可按精确成员名选择，`extensions` 按小写扩展名选择；省略时提取全部成员。所有成员都会先检查路径冲突和越界，即使没有被选中。

回填使用 `operation: "rebuild"`，`replacements` 是“精确成员名 → work/ 下替换文件”的对象。输出为 `rebuilt/<原封包文件名>`。未知成员会报错；组件重读生成的封包，对比所有替换和未替换成员。没有实际修改时保留原封包字节，XP3 有修改时使用标准 v1 布局重建。

输入封包上限 512 MiB，单成员 64 MiB，解码总量 256 MiB，最多 10,000 个成员，成员名合计最多 2 MiB。超限是当前组件预算限制；模块缺失或接口变化是共享工具版本问题，均不应解释为整个引擎不兼容。源码及解释器版本与哈希沿用现有共享依赖固定机制；后续发布的适配器不会自动换用新版本。

XP3 必须由独立证据确认没有过滤器，再显式传 `filter: "none"`；标记为零不足以证明这一点。

## 组合模板

`list_adapters.templates` 列出固定模板。核对[模板范围](../docs/adapters/templates.md)后，用 `compose_adapter_template` 生成协议入口、资源发现规则和固定依赖。例如：

```json
{"id":"rpa3-renpy","resourcePatterns":["game/scripts*.rpa"]}
```

模板先检查全部目标文件；出现不同内容或目录冲突时停止，保留现有修改。相同内容直接复用。返回的 `testArguments` 可交给 `test_adapter`；先暂存规则匹配的真实资源，完成独立样本核对及完整验收。生成模板不会自动验证、发布或启用适配器。

RPYC2 编译脚本优先使用 `loose-renpy-rpyc` 或 `rpa3-renpy-rpyc`，直接连接固定惰性 AST 读写，不先下载反编译器或生成解析脚本。源码／编译脚本或松散／封包混合时，路径无冲突且加载根明确可直接使用 `renpy-mixed`，无需另写组合入口。重复脚本路径、未知加载根或结构时先依据诊断确认实际加载关系；固定库缺模块应恢复完整发行包，不是当前引擎不受支持的证据。验收先少量中文试写，通过后才做全量中文回填。

## 编码、字体与启动组件

`list_adapters` 的 `components` 返回可选组件。`inspect_adapter_component` 查看接口、限制及文件列表；传入列表中的 `path` 可按字符偏移阅读源码片段。

`copy_adapter_components` 接受 1 到 4 个组件 ID。编码和字体组件共用 `src/tools/gamel10n_host.py`，调用应用预编译宿主；启动器提供现成 EXE。通过 `from tools.gamel10n_host import component` 使用按批次处理的 JSON 接口，不编译 C#。接口见返回的示例及[组件说明](../docs/adapters/components.md)。

复制前检查全部目标文件。相同内容直接复用；已有不同内容时停止复制并说明冲突，保留现有修改。不会产生备份或每次一份的新目录。复制后的文件及哈希纳入现有适配器包，更新应用不会自动替换已发布适配器中的组件。

组件接口、字符映射方向、字体与启动限制见[编码与运行组件](../docs/adapters/components.md)。编码可逆不能证明游戏能够显示新增字形。

## 引擎资料与额外依赖

`search_engine_references` 按引擎名称、别名或脚本格式检索 [GalTransl-GalGameReverse-Skill](https://github.com/GalTransl/GalTransl-GalGameReverse-Skill) 的索引，返回文档、模块、公开函数和适用范围。已有共享工具库时直接查询本地固定版本，否则读取在线索引。索引条目不代表本工具已支持该引擎，也不代表该游戏验证通过。

`inspect_engine_reference` 按精确引擎 ID 读取指南，支持按路径、关键词和字符范围读取所需 API。发行包附带固定 commit 的工具库，将 Python 源码、目录、指南和许可文件保留在一个共享 ZIP 中。`run_engine_python` 自动提供便携 Python，直接运行 `work/*.py` 或 `src/tools/*.py` 连接脚本，复用 `python.archives` / `python.engines`，无需将已有算法重新实现为 C#。不同引擎和版本的函数接口不统一，仍需按指南选择并用样本验证。

`use_engine_reference` 将固定依赖记录与 Python 辅助模块写入适配器源码。实现 `src/adapter.py` 的 JSON 协议、资源选择及确实缺失的版本处理，再执行 `test_adapter`。内置解析器通过 `parser(request, format)` 调用预编译组件；发布后的适配器使用相同共享库，常规提取与回填不调用模型。继续适配保留所用源码版本，已安装的库可离线使用；工具缺失时需恢复对应版本的完整发行包或共享工具，内容改变会明确报错，不自动替换成新版。

未命中时可继续查询格式目录和上游注册清单。`fetch_url` 支持 `query` 关键词定位及 `offset` / `length` 字符分页，大型目录在本地搜索，只将相关片段交给模型；读取完整条目后再判断能力。GitHub 仓库搜索只查仓库元数据，不查源码正文。Bishop 的 GSC 资料与 BSScript Rev.6 等不同格式不能因厂商相同而直接混用。

先复用发行包与项目固定的依赖。额外工具使用 `download_tool`，默认复用共享缓存；只有确需更新时才显式传 `refresh=true`，更新后重新验收。修改过的本地文件不会被默认覆盖。资料用 `fetch_url` 分页读取，并核对返回的获取时间与 `stale`；缓存和限制见[共享下载缓存](../docs/adapters/adaptation-internals.md#共享下载缓存)。

按容器、解码、文本语义、回填、重封包和运行显示定位实际缺口。未找到现成工具不能作为无法适配的唯一依据。读取当前固定版本的指南与源码，核对许可并保留必要声明。

## 引擎专项调查

### Siglus

`inspect_siglus_game` 返回场景数量、文本覆盖、显示调用及内嵌源码目录；`source`、`query`、`offset`、`length` 可以读取有界源码片段。查询不解包出大量临时文件。

`copy_adapter` 使用 `engine.siglus` 和版本号可生成现成的 JSON 调用桥及固定版本依赖。新的文本显示规则写入 `tools/siglus-rules.json`：

```json
{"commandRules":[{"command":"$display_text","argument":1,"kind":"dialogue"}]}
```

`argument` 从 0 开始。必须从源码或已验证的字节码语义确认参数；只增加提取规则不能解决新字体或新显示系统的问题。`inspect_builtin_adapter(format="Siglus")` 提供工具源码索引。修改后仍须通过 `test_adapter`，才能发布。

### Unity

用 `inspect_unity_schema` 查询 IL2CPP v31 元数据或托管/dummy DLL 的字段、父类、关联结构与文本方法线索。字段声明不等同于资源中的序列化布局，方法名称也不证明实际显示用途；用独立资源样本确认偏移及文本语义。

装备或特质缺少直接描述时，沿效果引用、状态效果和提示生成方法检查动态文本来源。不要翻译元数据中的资源路径、查找键或调试信息。自定义替换标记需结合调用方确认并保留。

### 原生程序与启动环境

`inspect_pe_imports` 直接读取 EXE/DLL 的普通 PE 导入表，支持按函数名或 DLL 名筛选、分页，并返回 IAT 地址、文件偏移和 x86/x64 间接调用／跳转的候选字节位置。每页默认 20 项、最多 40 项；过大的正整数页大小会收窄至 40 项，通过 `nextOffset` 继续读取。参数协议声明范围，数值错误会说明收到的值和允许范围。它不运行游戏、不下载工具，也不生成文件。候选位置尚未确认指令边界和运行可达性，动态解析、延迟导入及其他寻址方式可能遗漏；可用这些位置缩小后续反汇编范围，不能据此认定中文显示通过。BSScript Rev.6 按文件头识别并提供专用调查指引，不能直接套用 Bishop GSC 或 KaGuYa 的脚本解析器。

`inspect_pe_code` 使用随程序附带的 Capstone 5.0.6，在一次调用中反汇编 1–8 个已知代码范围。Windows x64 宿主可分析 x86/x64 游戏，无需准备 Python、下载反汇编器或编写地址专用脚本。地址通过 `addressKind=va/rva/file` 明确区分首选映像地址、相对虚拟地址和文件偏移；工具按 PE 节表换算，不假设 RVA 等于文件偏移。结果包括指令字节、三类地址、直接分支目标及继续读取地址。

每段最多 4096 字节、合计最多 16384 字节，每段默认 80 条、最多 200 条指令；单段地址错误不会丢弃其他段的结果。工具从指定起点线性解码，起点仍需调用目标等证据支持，解码成功不代表代码可达或实际运行过。合成字节反例应再对照实际游戏缓冲的来源与处理路径，不能仅因任意拼接可能出错就无限扩大调查范围。许可证随反汇编库一同发布，库在首次加载时校验固定哈希。

适配时需检查原有启动方式和区域设置配置，例如 `*.exe.le.config`。脚本文本编码与原生进程处理日文文件名、视频路径、标题和界面的 ANSI 代码页是不同要求，`chcp` 或 .NET 文化设置不能替代原有转区方式。新增启动器或修改 EXE 时，应保留原有转区入口、配置和工作目录；独立字形探针不能证明游戏启动及资源路径正常。

默认保留原 EXE 的名称和启动入口。确有运行时需求才修改 EXE，并以原相对路径输出到补丁目录、采用 replace 模式；安装备份由宿主负责，适配器不得修改原始资源或自行备份，也不应只为保留原程序另存一份改名 EXE。只有启动时确需额外的字体或转区处理时才增加独立启动器，并在 `build.launchExecutable` 中声明相对路径；留空会沿用原启动入口。启动器临时注册的字体须保持到实际游戏进程退出，不能因转区中间程序提前结束而卸载。

程序会拒绝新增 EXE、CMD、BAT 或 PowerShell 文件却未声明 EXE 启动入口的产物。所选 EXE 已有的 `.le.config` 必须纳入资源验证，中文回填不得修改原转区配置。启动环境摘要保存在现有会话状态中，继续分析和压缩上下文后仍会提供；这些本地检查不额外调用模型，也不启动游戏。

“文件或视频不存在”的报错应先核对实际文件、原脚本引用字节、工作目录和转区环境。字体显示异常还需检查游戏是否重置了字体配置。上述静态检查能发现遗漏，不能证明启动器实际传递了正确的运行环境；游戏运行结果仍单独标记为尚未验证。

## 本地探针与工作目录

临时分析和处理脚本统一放在 `work/*.py`，通过 `run_engine_python` 使用发行包中的共享 Python 解释器运行，也可直接调用固定工具库的模块。参数及标准输入输出使用 UTF-8，可用 `workingDirectory` 指定工作区内的运行目录；不使用系统 Python、pip 或临时下载的运行时。正式适配器也使用 Python，入口为 `src/adapter.py`；`test_adapter` 检查语法、打包并执行完整资源验收，自动准备固定共享 Python 依赖。发行包不含 C# 编译器。`run_tool` 用于调用已有工具和文件编排。

Python 探针使用 `run_engine_python`，其他本地工具使用 `run_tool`。临时脚本、报告和解包结果放在 `work/`；原始资源副本放在 `resources/`。只有正式解析代码和运行依赖进入 `src/` 及 `src/tools/`。完整路径和保留规则见[产物说明](../docs/adapters/artifacts.md)。

参数已知的独立只读查询可以批量提交，最多 6 项，包含 `inspect_engine_reference`；读取与进程运行须拆批。写入与运行批次最多 8 项，前面的项为 `write_file` 或 `replace_text`，最后一项也可为一次 `run_engine_python`、`run_tool` 或 `test_adapter`。前项失败或中断时先核对结果，再继续。另可按 `stage_resources → compose_adapter_template → test_adapter` 顺序组合，首末项可省略；依赖复制、发布和 `checkpoint` 单独调用。

带 `reportFile=work/*.json` 的探针可直接调用执行器注入的 `write_report(status='completed', summary='实际结论', details=results)`，无需导入。按实际结果选择 `completed`／`partial`／`failed`；退出码为零不代替报告结论。手动写入 `GAMEL10N_REPORT_FILE` 也须包含合法 `status`。报告错误时先查看 `reportError`、写入提示和已保留的输出，避免为恢复已有结果重跑实验。

## 验收与发布文件

用 `checkpoint` / `test_adapter` 的 `coverageSamples` 提供独立定位的原文样本，核对资源路径、字段与原文。`pendingCoverage` 保存已确认的漏提文字；`runtimeChecks` 保存需实机检查的事项。省略不会清除已有记录。有已知待办时，`finish` 需显式选择 `allowPartial=true` 并说明原因。

`test_adapter` 的 `discoveryOnly=true` 仅检查探测，不授予发布资格。完整验收要求提取、原文逐字节回填、完整与部分中文回填、身份稳定和路径检查通过。样本通过、编译成功和编码可逆都不能替代这一步。

实现尚在迭代时，优先用 `trialFiles` 选 1～12 个已 `stage_resources` 的完整文件及所需伴随文件，保持正式 `resourcePatterns` 不变，先验证小范围提取和中文往返。例如在原有 `test_adapter` 参数中增加 `"trialFiles": ["scripts/chapter01.bin", "scripts/index.dat"]`。宿主只提供所选文件的隔离副本，只核对所选路径的独立样本与修复基线；`stage=trial` 不能发布。资源不能独立处理时补齐依赖或直接完整验收，不截断封包。试验与完整验收分别缓存；通过后扩大资源集，省略 `trialFiles` 完整验收再 `finish`。此参数与 `discoveryOnly` 互斥。

旧工作区首次登记或需要整体替换时，`test_adapter` 的发布输入可以声明为：

```json
{ "releaseFiles": [
  { "path": "src/adapter.py", "role": "source", "purpose": "适配器入口与资源发现" },
  { "path": "tools/parser.py", "role": "runtime", "purpose": "提取与回填脚本消息表" },
  { "path": "tools/Launcher.exe", "role": "asset", "purpose": "复制为汉化启动入口" },
  { "path": "tools/LICENSE.txt", "role": "license", "purpose": "解析依赖的许可声明" }
] }
```

清单维护规则：

- `write_file` 新建 `src/` 下的 Python 实现时自动登记为 `source`；`work/` 中的分析脚本不登记。直接由外部脚本生成的文件需要登记，不扫描目录自动纳入。
- 复制正式适配器时继承其发布清单；复制内置解析器、编码/字体/启动组件或共享工具链连接代码时，由程序登记已知必要文件。组件附带的分析、编辑辅助文件不因被复制而自动发布。
- 新依赖可在 `write_file` / `download_tool` 中一次提供 `releaseRole` 和 `purpose`；后续编辑继承已有登记。下载依赖的来源记录随依赖保留。
- `update_release_files` 的 `files` 按路径增补或更新，`remove` 只排除发布、不删除文件。无参数时可查看清单及未登记文件的数量、体积和部分路径；未登记并不等于可安全删除。
- 验收失败、源码修改、重启和会话压缩保留清单，只撤销失效的验收结果。恢复上下文只提供登记数量，模型无需每轮重列清单。
- 旧包已有清单时直接继承；没有清单的旧工作区需要一次确认，不能把历史 `src/tools/` 全部视为正式依赖。

发布角色与路径见[正式适配器](../docs/adapters/artifacts.md#正式适配器)。逐项核对间接模块、原生依赖、素材与许可；游戏原文件、分析报告和探针不得充当正式依赖。清单变更后重新验收，通过后才调用 `finish`。

已通过验收时，读取文件或更新 `work/` 笔记不会单独使其失效；源码、依赖、资源或发布清单有变化才需重验。明确区分资源验证与实机启动、显示、分支和存档验证。

## 进展、摘要与阻碍

有关键证据或方向变化时，随正常工具调用报告简短中文进展；结论被推翻时明确更正。`checkpoint` 保存已确认布局、证据位置、排除的方案、覆盖缺口和下一步，优先控制在 3000 字内，不重复抄写源码或反汇编。

宿主在会话末尾追加状态变化，字段逐层合并，`null` 表示移除旧字段；历史记录不代表当前状态。检查点只保存笔记，由容量预算触发压缩并保留最近完整交互；更早的原始证据通过历史工具定向读取，避免重复调查。

需要用户信息时，用 `report_blocker` 说明具体需求。报告无法适配前列出实际尝试、结果及证据位置；仍有本地可执行工作时继续调查或实现。编码限制、字体分析未完成、缺少现成解析器或搜索失败，本身不足以证明无法适配。
