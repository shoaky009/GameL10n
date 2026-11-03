# 文档

日常操作提示已集成在界面中。这里提供需要进一步查阅的功能边界、数据说明和开发资料。

## 使用与排查

| 想了解的问题 | 文档 |
| --- | --- |
| 如何配置模型、本地翻译服务、API 链与重试 | [模型接口配置](model-providers.md) |
| 数据保存在哪里，如何迁移、备份与恢复 | [数据目录与迁移](data-storage.md) |
| 各引擎支持哪些资源，如何管理适配器版本 | [引擎与资源模板](adapters/README.md) |
| 何时需要智能适配，是否联网，如何判断验证结果 | [识别与智能适配](adapters/automatic-adaptation.md) |
| 分析文件、正式适配器和补丁各有什么用，能否清理 | [产物与目录约定](adapters/artifacts.md) |

## 开发与维护

| 想了解的问题 | 文档 |
| --- | --- |
| 构建、打包与项目依赖 | [开发与构建](development.md) |
| 外置适配器如何与宿主通信 | [适配器 CLI 协议](../contracts/adapter-v1.md) |
| 如何声明引擎识别条件 | [引擎识别声明](adapters/recognition.md) |
| 智能适配怎样提供工具、复用结果、恢复会话及记录耗时 | [执行与恢复机制](adapters/adaptation-internals.md) |
| 可复用的编码、字体与启动实现 | [编码与运行组件](adapters/components.md) |
| 固定封包读写能力及资源预算 | [封包组件](adapters/engine-components.md) |
| 已支持的封包与脚本组合如何接入 | [组件组合模板](adapters/templates.md) |

适配器文档按引擎、资源格式和版本条件描述能力。资源结构检查与实际游戏运行验证分别说明；同名引擎不保证所有游戏兼容。
