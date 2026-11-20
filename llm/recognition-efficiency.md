# 识别速度与模型用量

## 优先减少模型往返

常规识别命中已有适配器时可以零模型调用完成。智能适配也已有自动样本试验、组合模板及小样通过后自动完整验收。下一步最有机会同时缩短耗时和减少 Token 的，是把**工具和参数已经能由资源证据确定**的工作接到已有流程里。每省去一轮，都同时省去一次请求、该轮输入和等待模型回答的时间。[常规识别](../crates/core/src/engine_tasks.rs) · [自动样本与模板](../crates/engines/src/adaptation_automatic.rs) · [宿主验收](../crates/engines/src/adaptation_workflow.rs)

首次模型请求前，宿主用固定库选择少量脚本与必要伴随文件。首轮的 `automaticArchiveTrials` 给出成员来源、工作样本与各阶段结果。符合条件的 QLIE 和 NekoSDK 小样还可触发模板组装与整卷原文验收。已有原文往返证据时，后续调查可从译文编码、覆盖和运行条件开始。[自动试用](../crates/engines/src/adaptation_automatic.rs) · [封包组件与模板](../docs/adapters/templates.md)

## 两项可单独验证的开销

| 开销 | 当前实现与基线 | 可比较的改动 |
| --- | --- | --- |
| 本地探测 | 宿主按格式从同一份目录清单选择脚本和同目录伴随文件。含 1 个普通 `story.txt` 和 10,000 个 `.ogg` 的合成目录触发 5 个候选，均未命中时累计传入 5 个路径，旧清单为 50,005 个。 | 比较同一份目录的原生探测耗时及识别总耗时。核对无扩展名脚本、Softpal 的 `TEXT.DAT` 和 `POINT.DAT`，并比较最终资源和文本 ID。[候选清单](../crates/engines/src/discovery.rs) · [调用方](../crates/engines/src/service.rs) · [原生工具](../tools/GameL10n.VnTextPatch/Worker.cs) |
| 模型输入 | 智能适配每轮发送 45 个工具定义。按宿主紧凑 JSON 计，定义占 45,462 字节，固定指令占 29,864 字节。首轮还附按路径排序的前 100 个文件，上述目录中这 100 项全是音频。 | 首轮优先给出候选脚本、封包和失败样本，同类媒体只报数量。固定指令里的专项操作说明可随相应工具结果提供，保持工具次序稳定。对比整次任务的请求数、未缓存输入和输出 Token，避免精简前缀后反而多花一轮查询。[请求构造](../crates/engines/src/adaptation_flow.rs) · [指令](../crates/engines/prompts/adaptation.md) |

路径数和字节数描述工作量。磁盘缓存、模型的分词方式及供应商前缀缓存都会影响实际收益，不能直接换算成秒数、Token 或费用。

## 引擎试点

- **Ren’Py 作为对照。** 项目已有 RPA3 和混合脚本模板。固定样例能从一个松散编译脚本和一个包内编译脚本共提取 8 条文本，完成中文回填与验证；同名冲突和不明加载根有独立样例。用它比较从识别到可试译的总耗时，核对原文、文本 ID 和已有译文，再判断新的自动步骤是否值得推广。[混合资源样例](../crates/engines/tests/fixtures/check_renpy_rpyc_adapter.py)
- **Artemis PFS v8。** AST 2.0 的固定模板可从索引选卷并走完整回填验收，优先比较从识别到可试译的耗时和用量。未满足模板条件时，用样本和验收诊断定位具体差异。已有适配器的部分试译仅改变说话人时，宿主可重放相关成员。多卷覆盖以游戏侧证据为准。[模板范围](../docs/adapters/engines/artemis-pfs.md) · [验收](../crates/engines/src/adaptation_verification.rs)
- **EAGLS SCPACK。** 比较首次定位可用脚本布局与中文回读限制所需的模型请求数。CP932 可用的脚本仍需为简体译文确认解码和字体方案。[EAGLS 脚本](../docs/adapters/engines/eagls-scpack.md)

## 衡量结果

分别对照已支持的游戏首次识别、同一游戏再次识别、确需模型调查的游戏。固定游戏资源、模型配置和验收目标，从点击识别计时到相同范围的正确文本可供试译。记录墙钟耗时、请求次数（含零次）、输入／缓存／输出 Token；缓存用量缺失时标为未知。慢案例再拆分本地准备、模型等待和验收耗时；可复用现有消耗指标与适配日志。[消耗指标](../docs/consumption-metrics.md) · [日志摘要](../scripts/analyze-adaptation-log.ps1)
