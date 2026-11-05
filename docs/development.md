# 开发与构建

项目使用 C# / .NET 10 和 WinUI 3。构建需要 .NET SDK 10.0.402、Windows SDK 和 WinUI 构建依赖。

本文说明构建与代码结构。智能适配的工具分组、会话恢复、缓存及耗时排查见[执行与恢复机制](adapters/adaptation-internals.md)，适配器协议和组件资料可从[文档索引](README.md)查阅。

```powershell
./scripts/dotnet.ps1 restore GameL10n.sln
./scripts/dotnet.ps1 build GameL10n.sln --no-restore
./scripts/dotnet.ps1 test tests/GameL10n.Tests --no-restore
./scripts/publish.ps1
```

API 请求与会话回放的回归检查还需使用与桌面端相同的 JSON 配置，关闭默认反射回退：

```powershell
./scripts/dotnet.ps1 test tests/GameL10n.Tests -c Release --no-restore -p:JsonSerializerIsReflectionEnabledByDefault=false --filter FullyQualifiedName~AgentProviderTests
```

`scripts/dotnet.ps1` 支持系统 SDK，也支持项目下 `.tools/dotnet` 中的独立 SDK。`scripts/publish.ps1` 输出 `artifacts/GameL10n`，包含桌面应用、运行时、文档和自动适配所需的工具链。发行时保留完整目录，单独复制 EXE 不足以运行桌面工具。

根目录的 `logo.png` 用于 README，`icon.png` 是程序图标原图。Windows 使用由原图转换的多尺寸 `src/GameL10n.Desktop/Assets/GameL10n.ico`；替换原图时同步更新 ICO。构建将其嵌入 EXE 并复制到应用的 `Assets` 目录，运行时窗口使用同一文件。

构建使用 SDK 10.0.402，发行运行时从 `.tools/dotnet` 提取 10.0.13。桌面程序、数据工具、预编译适配组件宿主和 VNTextPatch 共用 `toolchain/` 中的固定版本 .NET 运行时，不依赖用户安装 .NET。WinUI 的原生运行组件仍随包提供；项目按需引用 WinUI、DWrite 和 Runtime，不携带 Windows AI、机器学习或搜索组件。智能适配通过 HTTP 调用模型，无需 ONNX Runtime 或 DirectML。

界面未使用内嵌浏览器，WebView2 的间接依赖资产不参与构建或发行。控件语言资源仅保留简体中文和英文回退；该筛选同时覆盖托管卫星程序集和 WinUI 原生 MUI 文件。游戏解析、翻译和字体处理仍保留完整编码及 Unicode 支持。

智能分析的临时脚本和新生成的正式适配器统一使用内置 Python，入口为 `src/adapter.py`。宿主检查语法、打包明确选中的文件，并执行资源发现和回填验收；内置 C# 解析、字体及编码组件在应用构建时预编译，通过 `tools/adapter-host/GameL10n.AdapterHost.dll` 提供固定接口。

发行包不包含 C# 编译器、Roslyn 或框架引用程序集，无需额外开关。旧 Manifest v1～v3 的 `adapter.dll` 仍通过共享 .NET 运行时执行；继续修改这类适配器时需把相关逻辑迁移为 Python，再验收发布新版本。开发诊断和旧格式维护仍可使用开发机的 SDK。

发行脚本会重建已有的发行目录，避免旧依赖残留；非空目录必须能识别为桌面发行包，且不能包含应用数据。请勿在发行输出目录中保存个人文件。

内置 VNTextPatch 脚本工具随发行包保存在 `tools/vntextpatch`，包含独立 EXE，并使用包内的共享运行时，不需要安装 SDK，也不在每次处理脚本时解压运行时。辅助程序从各自 EXE 所在位置查找 `../../toolchain`，移动发行包时应保留相对目录结构。其上游源码固定版本并随仓库保存，构建时不从 GitHub 动态下载；具体脚本格式与运行限制见[内置脚本适配说明](adapters/engines/vntextpatch.md)。

同目录的小型 `GameL10n.VnTextPatch.dll` 与 `sources.zip` 用于智能适配复用，发行脚本通过 `scripts/package-vntextpatch-assets.ps1` 生成。源码统一压缩保存，供按需读取结构参考；开发任务保存 Python 调用代码与固定依赖记录，解析库按内容哈希跨项目共享。需要补充或修改的逻辑使用 Python。仅执行桌面项目的 `dotnet build` 不会生成这些发行资产，完整功能验证应使用发行目录。

[编码与运行组件](adapters/components.md)的 C# 实现编入 `GameL10n.AdapterHost`，Python 辅助模块嵌入 Engines，供开发工作区按需复制。预编译 QLIE 启动器由 `scripts/package-adapter-components.ps1` 生成到 `tools/adapter-components`，发行脚本自动调用。该构建步骤使用固定 SDK 编译器及 Windows .NET Framework 4.x 程序集，启动器自身不依赖 .NET 10。组件打包集成测试使用 PowerShell 7。

## 代码结构

| 目录 | 内容 |
| --- | --- |
| `src/GameL10n.Translation` | 文本模型、翻译执行、Provider、译文缓存、通用格式校验与文件持久化基础工具 |
| `src/GameL10n.Engines` | 引擎适配器、资源探测、智能适配、扩展管理、资源与程序验证 |
| `src/GameL10n.Core` | 项目管理、游戏流水线、任务调度、原文快照、补丁生成编排、安装与发布包项目验收 |
| `src/GameL10n.Desktop` | WinUI 3 桌面界面 |
| `tests/GameL10n.Tests` | 格式、缓存、恢复、安装及接口测试 |
| `tools/GameL10n.Diagnostics` | 游戏资源验证与本地工具链诊断 |
| `tools/GameL10n.DataTool` | 游戏数据迁移、校验与缓存加载统计 |
| `tools/GameL10n.VnTextPatch` | 内置 VNTextPatch 脚本解析与回填工具 |
| `third_party/VNTextPatch` | 固定版本的上游脚本解析源码与许可证 |
| `contracts` | 扩展通信协议 |

依赖方向为 `Desktop → Core → Engines → Translation`，Core 同时直接使用 Translation。Translation 仅依赖 .NET 基础库，不引用 Core、Engines、WinUI 或引擎原生工具；引擎脚本、字形组件和分析运行库随 Engines 提供。类型使用 `GameL10n.*` 命名空间，程序集归属由上述项目边界确定。

常规识别由 Core 的 `GameAnalysisService` 编排原文快照和提取结果提交；Engines 的 `AdapterDiscoveryService` 负责候选筛选、探测、复用验收和失败证据，常规识别与智能适配共用。Engines 通过调用方提供的资源准备回调取得待验证目录，不依赖项目存储。Desktop 负责发起任务和展示结果。

`TranslationEngine.RunAsync` 接收文本列表、Provider 链和翻译策略，返回译文、缓存命中数与 Token 用量；它独立处理分批、去重、重试、拒绝切换、请求恢复和有效译文提交。调用方提供工作目录，并串行管理同一目录中的任务。通用校验在翻译核心中执行，资源专用校验通过可选回调传入；游戏原文哈希、Adapter、补丁构建和安装由 Core 编排。进度通过可等待的回调交给调用方保存和展示。

全量翻译保留成功结果并继续处理失败文本之后的批次，返回成功与失败计数；`LoadFailuresAsync` 按原文和策略指纹查询当前失败原因。需要完整译文的调用方可设置 `RequireAllTranslations`，在所有批次尝试结束后报告失败，避免把未完成的全量翻译自动生成补丁。依赖项目原文快照和提取基线的 `PublishedPackageVerifier` 由 Core 提供，Engines 中的资源验收不依赖项目存储。

翻译使用 `OpenAiCompatibleProvider`，智能适配使用 Engines 中的 `OpenAiCompatibleAgentModel`。两者复用 HTTP 协议处理，并通过各自调度包装共享同一 API 账户的并发限制。现有项目、缓存、校对记录和扩展协议的持久化格式不变。

JSON 序列化与反序列化使用各模块的 `JsonSerializerContext` 生成类型信息，由模块自己的上下文维护数据契约，并按项目依赖组合下层解析器。`LocalFiles` 接收调用方提供的 `JsonTypeInfo<T>`，只负责文件读写、原子替换及备份恢复。动态工具结果和 API 请求使用 `JsonObject`、`JsonArray`；以 `object` 传递的具体数据类型也需要在所属模块登记。新增调用应使用明确的类型信息，避免隐式依赖运行时反射。

构造 JSON 数组时使用显式 `JsonNode`，字符串可用 `new JsonArray(JsonValue.Create("value"))`。字符串集合初始化器和 `Add<T>` 可能在建树阶段查询默认解析器；稍后序列化整棵树时传入的类型信息不能补救这一步。

存储文件沿用 Web 命名与缩进配置，哈希输入中使用默认大小写的序列化保持独立配置；调整属性顺序、编码或命名规则会影响已有缓存键和适配器版本。桌面应用、数据工具、AdapterHost 及当前 VNTextPatch 命令行入口关闭 JSON 的默认反射回退。旧版解析器 DLL 通过独立的托管兼容进程调用，该进程保留反射回退。开发诊断与测试输入构造可使用显式反射解析器，不属于发行应用的数据契约。

## 资源验证

```powershell
./scripts/dotnet.ps1 run --project tools/GameL10n.Diagnostics -- verify-darkblue '<游戏目录>' target/verification
./scripts/dotnet.ps1 run --project tools/GameL10n.Diagnostics -- toolchain .tools/dotnet target/toolchain-verification
```

诊断产物用于结构、编码和本地编译执行验证，不是完整汉化。

游戏资源、测试产物、SDK 和包缓存位于忽略目录中，不随源码或桌面发行包提供。

### 发行包中的共享引擎工具

`scripts/publish.ps1` 将固定版本的 GalTransl 源码工具库与 Windows x64 便携 Python 放入 `tools/engine-tools`。首次打包会访问 GitHub 和 python.org；后续复用 `.tools/engine-tools` 中的版本与哈希记录，也可用 `-EngineToolsCacheDirectory` 指向已准备的缓存以离线打包。更换工具库版本应使用独立缓存目录并验证对应引擎能力。缓存中的 `current.json` 固定 GalTransl commit，`runtime.json` 记录 Python 文件哈希；复制后再次校验，工具不完整则打包失败。

正常智能适配只读取发行包或已有共享工具，不自动下载内置依赖。自行构建运行时也需在程序输出目录提供 `tools/engine-tools`；可调用 Diagnostics 的 `prepare-engine-tools <缓存目录> <输出目录>` 准备。标准包会增加工具库与 Python 的体积，所有项目共用一份。额外研究工具仍可由模型按需下载，与内置依赖分开记录。
