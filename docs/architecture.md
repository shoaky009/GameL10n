# 架构与模块边界

依赖方向为 `Desktop → Core → Engines → Translation`。Core 也直接使用 Translation。桌面和命令行通过同一套 Core 服务处理项目和任务。

| 模块 | 职责与源码入口 |
| --- | --- |
| Translation | 文本模型、翻译请求、校验和译文存储。[translation](../crates/translation/src/lib.rs) |
| Engines | 引擎探测、适配、扩展和资源验收。[engines](../crates/engines/src/lib.rs) |
| Core | 项目、任务、翻译与安装编排。按项目控制正在进行的修改与取消。[core](../crates/core/src/application.rs) |
| Desktop | Slint 界面、窗口和输入。UI 类型和事件循环在此层使用。[desktop](../crates/desktop/src/main.rs) |

## 适配器与原生工具

Rust 管理模型会话、预算、工具调度和验收。Python 负责格式调查和资源解析。正式适配器通过 UTF-8 JSON CLI v1 与宿主通信。[适配器协议](adapters/protocol.md)

自研原生工具位于 `tools/`，FFI 和平台 API 封装在独立组件，通过进程协议或明确 ABI 接入。现有 VNTextPatch 使用 NativeAOT。运行检查工具负责游戏窗口与 Windows OCR，执行隔离工具使用 AppContainer 限制生成代码的权限。[原生工具](../tools) · [执行隔离](adapters/execution-isolation.md)

## 任务与数据

Core 管理项目状态和操作顺序。同一项目的资源修改串行执行，切换页面仍可查看运行进度。全自动流程复用识别、翻译、补丁和运行检查服务，已提交结果随项目数据保存。[全自动汉化流程](automatic-workflow.md) · [数据目录与迁移](data-storage.md)

游戏项目的版本升级集中在 Core 的 `project_migrations` 模块。每个游戏独立记录已完成版本，新增升级步骤与对应的数据校验、恢复逻辑一同放在该目录。[项目迁移入口](../crates/core/src/project_migrations/mod.rs)

仓库目录和技术路线见 [AGENTS.md](../AGENTS.md)。
