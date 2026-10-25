# 模型接口配置

在“API 与术语”点击“新增 API 配置”，填写名称、协议、服务商地址、模型名称和 API Key。保存后，可在两个用途的下拉框中选择该配置。

## 按用途选择

| 用途 | 使用场景 |
| --- | --- |
| 游戏分析 / 自动适配 | 未知引擎或资源格式的模型分析、适配器代码生成 |
| 文本翻译 | 对白、旁白、角色名和选项的批量翻译 |

第一份配置默认供两个用途共用。也可以新增第二份配置，为分析选择编程模型、为翻译选择擅长语言的模型，两者的地址、协议和密钥均可不同。新增配置不会自动改掉已有选择。已支持游戏的常规识别和资源提取不调用分析模型。

选择会自动保存，重启后继续生效。编辑配置保留其用途关联；删除正在使用的配置会清空相关选择，不会自动换到另一个服务。运行中的任务继续使用启动时的配置，修改对后续任务生效。

勾选“记住 API Key”时，密钥使用 Windows 当前用户的数据保护功能加密保存，设置文件不包含明文密钥。将设置迁移到另一台电脑或另一位 Windows 用户时，需要重新填写密钥。取消勾选则仅在本次运行中使用，重启后需要重新填写密钥，其他配置和用途选择仍保留。编辑时清空 API Key 可移除该配置的已保存密钥。

## 接口地址

| 协议 | 基础地址示例 | 完整地址示例 |
| --- | --- | --- |
| Chat Completions | `https://api.openai.com/v1` | `https://api.openai.com/v1/chat/completions` |
| OpenAI Responses | `https://api.openai.com/v1` | `https://api.openai.com/v1/responses` |

使用其他服务商时替换地址并填写其支持的模型。完整接口地址必须与所选协议对应。应用不会在失败后自动切换协议，以避免重复请求。

## 协议行为

Chat Completions 使用 `messages` 和 `max_completion_tokens`。Responses 使用 `instructions`、`input` 和 `max_output_tokens`，以 `store: false` 请求不保存供后续 API 检索的响应；这不等于服务商承诺零保留。两种协议均使用非流式响应。

Responses 从 `output` 中的助手消息读取 `output_text`，跳过推理项。未完成、拒绝或格式不正确的响应不会提交到翻译缓存。应用不为翻译请求启用工具调用或多轮会话。

Token 统计优先采用服务商返回的用量；缺少用量时标记为估算。Responses 的输出 Token 已包含服务商统计的推理消耗，不重复累加。失败响应中返回的用量也会计入统计。

两种协议的翻译请求遇到 HTTP 错误、网络中断和无效响应时，自动重试 5 次，每次等待 10 秒。仍失败则暂停任务，继续全量翻译或重新提交相同试译选区只处理未完成的文本。游戏分析沿用独立的重试策略。

模型明确拒绝处理内容时立即暂停，不重复提交。除协议的 `refusal` 字段和内容过滤状态外，也识别普通正文及单独的 `error` / `refusal` JSON 对象中的明确拒绝说明；正常译文中的“我不能”等对白不参与拒绝判断。拒绝记录保存在游戏任务的 `responses/*.refusal.json`，绑定该批次的模型接口、文本、上下文和翻译规则。重新开始相同批次会读取记录并提示原因，不再产生 API 调用。旧版误记为 JSON 格式错误的明确拒绝也会从已有日志中识别。被拒绝的批次不会写入译文缓存，其他已完成译文继续保留。应先处理被拒绝的内容；拒绝说明不能当作译文或成功结果使用。

拒绝响应可能不符合正常输出的 JSON 结构，参见 [OpenAI Docs：拒绝响应](https://developers.openai.com/api/docs/guides/structured-outputs#refusals-with-structured-outputs)。

翻译结果接受纯 JSON 或完整的 JSON 代码块，仍须包含全部请求 ID，且不能包含未知 ID、重复 ID 或非字符串译文。截断的响应不会作为完整译文提交。

界面显示简短的失败原因和重试进度，例如网络异常、请求超时、限流或额度限制、身份验证失败、返回格式不正确、文本 ID 重复或输出被截断。重试耗尽后也会保留最后一次失败原因。开发诊断写入程序目录的 `logs/api-*.json`，包含协议、模型、用量、错误分类及翻译响应片段；不记录请求头、API Key 或独立的推理字段。片段最多保留开头和结尾各 8,192 个字符，可能包含剧情译文。程序目录不可写时，日志保存在 `%LOCALAPPDATA%/AvgTranslator/logs`。JSON 语法错误、ID 不匹配和输出达到 Token 上限在日志中分别标记，便于定位服务商兼容问题。

接口定义：[OpenAI Responses](https://developers.openai.com/api/reference/resources/responses/methods/create)、[Chat Completions](https://developers.openai.com/api/reference/resources/chat/subresources/completions/methods/create)。
