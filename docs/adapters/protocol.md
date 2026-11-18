# 适配器 CLI 协议 v1

Python 适配器入口为 `src/adapter.py`。从标准输入读取一份 UTF-8 JSON 请求，将结果写到 `responseFile`。旧式请求可用标准输出返回 JSON，诊断写到标准错误。字段使用 camelCase。[请求与响应类型](../../crates/engines/src/protocol.rs)

## 请求与结果

请求包含 `protocolVersion: 1`、`operation`、`sourceDirectory`，按操作提供 `resourceFiles`、`outputDirectory` 等字段。输入目录只读，产物路径相对输出目录。响应包含 `protocolVersion: 1` 和 `success`。

| `operation` | 成功响应字段 | 内容 |
| --- | --- | --- |
| `probe` | `probe` | 识别信息及非空 `requiredFiles`。资源路径须来自候选文件。不支持的资源返回 `probe: null`。 |
| `extract` | `extraction` | `units` 和 `warnings`。 |
| `build` | `build` | 输出的 `files`、`warnings`，以及可选的 `mode` 和启动配置。 |
| `validate` | `validation` | `passed`、`issues` 和 `runtimeStatus`。 |

`build` 使用按文本 ID 索引的译文字典。如果有 `translationsFile`，从该文件读取，其他请求使用内联 `translations`。缺少译文的文本保留原文和身份。

常规识别时，资源操作的 `errorKind: "incompatible"` 或旧式未分类失败可尝试下一候选。依赖或 I/O 故障使用 `"dependency"` 或 `"io"`，宿主会停止并报告 `error`。

## 探测与提取

`probeFiles` 提供候选路径、文件长度和可用头部字节数。读取头部以外的内容时，可先在响应中提交 `probeReads: [{path, offset, length}]`，随后从 `probeRanges[].dataPath` 的起点读取相应片段。根据请求中的 `probeReadLimits` 选择范围，完成后返回最终的 `probe`。[识别规则](recognition.md) · [完整字段](../../crates/engines/prompts/adapter-protocol.md)

每条文本有稳定且唯一的 `id`、`resourcePath`、`locator`、`sourceText`、`context`、`kind`，以及可选的 `speaker`。修复或扩展后保留已有文本身份和匹配字段。只提取有显示证据的玩家文字，空白显示槽在资源中原样保留。

需要保留的运行时替换标记可在本条文本的 `protectedTokens` 中声明。标记须在原文中出现，翻译时会恢复并核对。[文本类型](../../crates/translation/src/lib.rs)

## 构建与校验

- `replace` 是默认模式。原文回填保留源资源字节，翻译回填保持原路径与非文本数据。
- `overlay` 输出新增覆盖资源。复提时同时读取原资源和补丁，以已验证的游戏加载顺序合并。安装时会核对所依赖的原始资源。
- `build.files` 声明全部产物，包括重读所需的伴随文件。校验请求的 `resourceFiles` 指向原始输入，`buildMode` 和 `outputFiles` 来自构建结果。
- `validation.issues` 中每项都是阻塞错误，包含 `code`、`message` 和可选的 `unitId`。通过时返回 `passed: true` 和空数组。覆盖限制放入 `warnings`，资源验收的 `runtimeStatus` 为 `"not-tested"`。
- 宿主独立检查源文件哈希、原文往返和翻译后复提。完整验收会检查部分翻译是否保留其他文本及身份。编码、字体和实际显示需单独验证。[验收与运行记录](automatic-adaptation.md#验证与产物)

## 启动声明

`probe.launch` 可声明已确认的原游戏入口。修改原 EXE 时，将其登记为 `originalExecutable`，并纳入 `requiredFiles`。配套的已有 `.le.config` 需随所选 EXE 纳入验收并保留原内容。

`build.launch` 的 `location` 为 `"game"` 或 `"patch"`，分别指向已有游戏 EXE 或 `build.files` 中的 EXE。配置含相对 `executable`、可选的 `arguments` 和 `workingDirectory`。工作目录相对安装后的游戏根目录。旧式 `launchExecutable` 指向补丁内的 EXE。未声明入口时沿用原配置。新增可执行输出须明确声明 EXE 入口。

## 依赖与发布

宿主提供包根目录 `adapterDirectory` 和固定工具的 `toolPaths`。通过 `tools/gamel10n_host.py` 使用 `parser`、`component` 和 `write_response`。[宿主接口](../../adapters/components/gamel10n_host.py)

当前执行 Manifest v4 Python 包。固定依赖需要同版本文件，更新后需重新验收。正式包和补丁使用不同的发布目录与清单。[正式适配器](artifacts.md#正式适配器) · [协议字段参考](../../crates/engines/prompts/adapter-protocol.md)
