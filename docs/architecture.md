# 架构与模块边界

## 模块

| crate | 职责 | 边界 |
| --- | --- | --- |
| `translation` | 文本模型、翻译策略、Provider、翻译记忆、通用校验与文件持久化 | 独立于引擎、项目管理和 UI |
| `engines` | 引擎探测、智能适配、扩展管理、资源验收与解析工具 | 不管理游戏项目和界面状态 |
| `core` | 项目、任务、提取缓存、原文工作资源、补丁及安装编排 | 不引用 UI 类型和事件循环 |
| `desktop` | Slint 展示、输入、导航与窗口集成 | 通过 Core 接口执行业务 |

依赖方向为 `Desktop → Core → Engines → Translation`，Core 也直接使用 Translation。Core 中的诊断 CLI 复用同一套应用服务；Translation 可供其他宿主独立使用。

## 仓库目录

| 位置 | 用途 |
| --- | --- |
| `crates/` | 四个 Rust 模块，各自包含源码、测试和内部资源；诊断 CLI 位于 Core |
| `adapters/` | Python 引擎适配器、格式变体及共享组件 |
| `tools/` | 项目维护的原生工具源码 |
| `third_party/` | 第三方源码、固定归档、版本与许可声明 |
| `scripts/` | 构建、打包与诊断入口 |
| `docs/` | 用户与维护文档，扩展协议见[CLI v1](adapters/protocol.md) |
| `llm/`、`.agents/` | LLM 参考资料与项目 skill |
| `.github/` | GitHub 自动化配置 |
| `target/` | 构建与临时产物；`target/runtime/` 保存开发所用的运行依赖 |
| `artifacts/` | 可直接运行的打包产物 |

## 任务与查询

打开项目只读取元数据和已有结果；识别、翻译和补丁操作由用户启动。项目列表使用摘要索引，文本按需加载、缓存和分页，搜索不重新解析游戏。

Core 管理任务，同一项目的资源修改任务串行，不同项目可并行。切换页面不取消任务。结构化事件经有界通道交给 UI，必要时从任务快照同步状态。

目录删除和短写操作按项目加锁，递归操作不占用全局列表锁。任务开始和结束先提交记录再发出事件；日志或索引更新失败不撤销已提交结果。中断恢复见[数据说明](data-storage.md)。

## 识别与翻译编排

`Application::recognize` 负责常规匹配、原文准备、提取与智能适配交接。`EngineService::discover` 负责候选筛选、资源探测和复用验收，通过调用方提供的回调取得资源，不依赖项目存储。详见[识别流程](adapters/recognition.md#识别流程)。

`TranslationEngine::run` 接收文本、Provider 链、策略和工作目录，处理分批、去重、重试、拒绝切换、请求恢复与译文提交，通过回调报告进度。通用文本校验属于 Translation，格式专用验收属于 Engines，项目原文与补丁编排属于 Core。

`Application::translate_project` 与恢复入口使用同一流程。翻译完成后按需生成补丁，保留项目占用和取消信号，释放翻译并发名额。所选文本和所需补丁均成功后才标记完成；构建失败时复用已提交译文继续。

翻译与校对共用按项目目录的短事务锁，保护记录的读取、合并和提交，不跨模型请求或资源构建。阻塞文件操作在线程池执行。存储布局见[数据说明](data-storage.md)，批次规则见[模型接口配置](model-providers.md#批次失败与继续)。

翻译和智能适配复用 `OpenAiProvider` 协议处理，各自调度并共享 API 账户的并发限制。发布包与项目提取基线的比较由 Core 负责。

## Rust、Python 与原生工具

Rust 管理模型会话、工具调度、预算、状态与验收。Python 执行分析脚本和正式适配器，入口为 `src/adapter.py`，通过 UTF-8 JSON CLI v1 通信。固定 VNTextPatch 解析能力由 NativeAOT EXE 提供。

自研原生工具与运行组件使用 Rust，平台 API 的 FFI 和必要的 `unsafe` 限定在 `tools/` 下的独立组件，通过明确的 ABI 或进程协议接入。现有 C# NativeAOT 工具保留，不向业务模块扩展。

子进程使用明确的程序和参数，提供超时、取消及输出限制。开发与验收使用资源副本，发布前检查语法、原文回填、中文与部分翻译；运行时核对版本和文件完整性。子进程不构成操作系统沙箱。

## 持久化与兼容

数据使用本地 JSON 和文件系统。项目身份、文本 ID、翻译记忆指纹、字段命名及哈希输入属于持久化契约，变更须验证兼容性。

各 crate 通过明确的 Serde 模型维护数据，文件工具负责原子提交；动态工具结果和 API 内容使用 `serde_json::Value`。未知字段按模型约定保留。

Python 扩展使用 CLI v1 和 Manifest v4。旧 DLL 扩展需转换为 Python 或独立原生工具后重新验收，不能原地修改已发布包的版本和哈希。

## UI 状态与日志

Desktop 保留导航、筛选、滚动和编辑草稿。后台结果按项目及查询代次投递；任务状态归 Core。退出程序时取消任务并等待已提交结果落盘。

项目日志由任务和业务模块记录，UI 订阅用户消息。API 诊断与工具轨迹写入项目日志域，成功请求不长期保留独立响应文件。

## 主题文档

[开发与构建](development.md) · [桌面交互](desktop.md) · [模型接口](model-providers.md) · [数据说明](data-storage.md) · [适配器](adapters/README.md)
