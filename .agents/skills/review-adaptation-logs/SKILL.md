---
name: review-adaptation-logs
description: 调查 GameL10n 游戏项目的智能适配记录，评审 LLM 决策、Token 消耗、工具耗时和验收失败，并提出同类引擎及主流程的优化方向。用户提供项目目录要求分析识别过程、排查耗时或审查模型路线时使用。
---

# 智能适配日志调查

用最少的必要证据定位问题，并形成适用于同类引擎的改进。命令在仓库根目录执行，复用 `scripts/analyze-adaptation-log.ps1`，不另写同类统计脚本。本技能供开发维护按需调用，不追加到游戏分析模型的运行时提示词。

## 先取摘要

```powershell
.\scripts\analyze-adaptation-log.ps1 -ProjectDirectory '<项目目录>'
```

默认读取截止调用开始时的日志，输出紧凑 JSON：已记录的 Token、失败与缺失计数、最近六轮、最后源码修改和验收、耗时最多的工具及少量失败。它不会读取 session、源码或游戏资源，也不调用 API。

失败请求的用量可能记录在后续 `output-limit-recovery` 事件中，需与原请求关联后计入一次。`missingUsage` 表示未知，零值不应自行估算；总 Token 不是计费金额，缓存计费另需供应商数据。

先判断瓶颈属于模型请求、本地工具、重试、实现缺陷还是验收失败。不要把所有工具调用都解释成无效工作。

## 缩小窗口

用摘要中的 runId 或失败时间过滤；需要对照完整工具统计及最近 checkpoint 时才加 `-Detail`。

```powershell
.\scripts\analyze-adaptation-log.ps1 -ProjectDirectory '<项目目录>' -RunId '<runId>' -Detail
.\scripts\analyze-adaptation-log.ps1 -ProjectDirectory '<项目目录>' -Since '<ISO时间>' -Until '<ISO时间>'
```

记录调查截止时间。缺少 run-end 只说明没有观察到结束事件；不能据此断言任务还在运行。相同参数也可能在源码或输入改变后执行，判断重复应对照修改记录、结果和耗时。

checkpoint 是否有效看相邻模型请求的 `visibleHistoryBytes` 和输入 Token；不能按持久化 Messages 数量判断。旧消息为恢复而保留，不代表每轮全部发送。用量按请求归属过滤，`hostRunTokens` 则是完整轮次总量；截取时间窗口后两者可能不同。

## 按调用取证

1. 从摘要挑最后一次相关验收失败、异常耗时或重复失败，用 callId 定位对应的少量 JSONL 行。
2. 日志只有元数据时，才解析一次 `adaptation/current/session.json`。优先取 `verificationDiagnostics`、`notes`、发布清单及相关调用，不输出整个对象。
3. 工具输出按 `messages[].toolCallId` 精确查找；参数先查 `toolCalls`，为空时从 `providerData` 中只取 `type=function_call` 的 name、call_id、arguments。Chat Completions 格式则取其 tool_calls；不要转储 reasoning、加密内容或整个 providerData。
4. 沿实际报错的文件和行号读取源码片段。需要比较产物时，在本地计算数量、ID、实际译文和未修改项差异，只返回统计及必要的少量样例，通常最多两条。不要把数万条文本交给模型比较。
5. 一次验证一个假设。超时先区分编译和运行，确认输入规模、最后输出及循环游标是否前进；仅延长超时不能证明定位正确。

调查默认只读，不干预正在运行的任务、启动游戏或触发付费 API；用户已授权的验证、修改或重跑按其范围执行。不为恢复上下文重跑已经有结果的实验。

## 判断与改进

区分三层证据：资源可解析和往返、实际译文正确写入、原游戏运行显示。编码器与解码器共享同一个错误时也可能自洽；数量或 ID 相同不代表译文正确。原文直接复制通过不等于修改分支已被验证。

检查现成解析器、组件和固定参考库是否真正试用。检索未命中应核对别名和词拆分；组件拒绝某个容器版本不能推导同引擎的脚本解析器不可用。二进制中未找到几种指令或常量，不足以排除某种能力。

改进优先放在确定性检索、诊断字段、可复用工具或有条件的引擎指引中。只修当前缺口，不把单个游戏名称、地址、固定文本数量写成通用规则；无关引擎不增加提示词。

结论保留：观察截止、已确认根因与证据位置、当前实现是否已修复、未验证风险和按收益排序的改进。证据足够回答问题后停止扩大调查；不要为完整流水账继续读取历史。
