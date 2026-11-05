# 固定版本脚本解析器改为进程调用

## 目标与范围

消除 `GameL10n.AdapterHost` 对托管解析器 DLL 的动态加载，为 Native AOT 发布准备调用边界。复用 VNTextPatch 已有的命令行 JSON 协议，保留适配器锁定解析器版本、文件完整性校验、共享本地工具缓存和离线运行的约定。

运行链路为：Python 适配器 → AdapterHost → 固定版本的 VNTextPatch 可执行工具。解析算法继续由 VNTextPatch 维护，Python 的 `parser(request, format=...)` 接口保持稳定。工具在构建及打包阶段准备，分析阶段解析并校验本地依赖。

本任务独立于 JSON 源生成、WinUI AOT 绑定，以及历史 C# 适配器迁移；完成进程调用不代表发行包已经可以删除共享 .NET 运行时。

## 当前实现与入口

- `tools/GameL10n.AdapterHost/BuiltinScriptParser.cs`：用 `Assembly.LoadFrom`、`GetType`、`GetMethod` 和 `Invoke` 调用固定 DLL 的 `Worker.ExecuteJson`。
- `tools/GameL10n.VnTextPatch/Program.cs`、`Protocol.cs`：已有单请求命令行入口，stdin 接收 JSON，stdout 返回 JSON 或响应文件状态，stderr 输出诊断。
- `src/GameL10n.Engines/Development/BuiltinAdapterAssets.Shared.cs`：`tools/vntextpatch-reference.json` 目前使用 schema 1，锁定 DLL 和许可证文件的 SHA-256，按文件清单哈希共享缓存。缺失锁定版本时拒绝用当前版本替代。
- `src/GameL10n.Engines/Development/EngineReferenceToolkit.cs`：解析依赖并提供 `toolPaths`。
- `adapters/components/gamel10n_host.py`：Python 组件入口。
- `adapters/templates/ScriptComponentAdapter.cs`：模板可能多次解析容器内容，然后转换条目标识和处理结果。
- `src/GameL10n.Engines/Adapters/VnTextPatchClient.cs`：常规解析已使用进程协议，可参考其响应文件、诊断及目录隔离约定。
- `scripts/package-vntextpatch-assets.ps1`、`scripts/publish.ps1`：目前分别打包可复用 DLL 和完整命令行工具。

## 实施要求

1. 为完整可执行工具定义新的依赖清单版本，明确入口和所需文件；每个文件继续校验哈希。复用资源打包脚本必须产出完整工具包，不能仅把 DLL 文件名替换成 EXE。
2. 依赖解析层负责版本选择、缓存、完整性和启动命令。组件桥接只负责执行已解析命令及转发协议，不猜测 DLL、EXE 或 `dotnet` 路径。
3. 替换反射调用，保留解析器格式约束和禁止整文件 `probe` 的限制。按资源批次调用，避免按文本条目创建进程。
4. 为桥接内部调用分配独立响应文件；不要直接透传外层 `responseFile`，否则解析器会把中间结果写到适配器最终输出位置。处理协议版本、退出码、错误响应、stderr 诊断及响应大小，确保读取输出不会造成管道死锁。
5. 保留取消和超时后的整个进程树终止能力，临时文件使用现有调用工作区并负责清理。原始游戏目录不可作为临时输出位置。
6. Python 组件、模板及分析脚本共用解析后的命令，保留必要的现有调用接口。新增逻辑继续使用 Python，组件宿主执行固定操作。
7. 先允许现有 .NET 发布方式的完整工具包，经 JSON AOT 改造后再发布原生工具；两个阶段沿用同一进程协议。

## 旧版兼容边界

- 已发布适配器的 schema 1 清单锁定的是 DLL 字节。不能悄悄改指向当前 EXE，也不能覆盖原清单、包哈希或不可变版本目录。
- 旧依赖迁移需恢复原版本逻辑，经过原文回填、写入及重新解析等实际验证后，发布新的适配器版本。
- 如需继续运行旧 DLL，需明确保留独立的托管兼容工具及所需运行时，或先完成迁移。Native AOT 程序本身不能动态加载旧托管 DLL 来恢复该能力。
- 版本缺失或文件被修改时，应明确报错并提示恢复所需工具，不在分析过程中自动下载或升级。

## 验证与交付

优先更新现有共享解析器、模板和常规解析检查，覆盖工具共享、锁定版本不可替代、篡改拒绝、协议与错误转发，以及模板内部响应文件隔离。涉及新进程边界的检查应调用真实 worker，避免只模拟字符串返回。不要为每个 DTO 或每个转发字段另加测试。

Linux 可以完成进程协议改造、托管工具运行及相关验证；Windows Native AOT 编译和最终发行包验证需在 Windows 工具链上完成。按职责边界拆分提交，临时辅助脚本和产物放在忽略的 `target/`，不纳入正式脚本目录。
