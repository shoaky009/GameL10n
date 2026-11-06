# 文档

[开发与构建](development.md)列出本仓库已经接入的功能和实际运行命令。引擎、模型接口和智能适配说明还保留完整产品的行为约定，作为模块迁移与验收依据；存在相应文档不表示功能已经在 Rust 宿主中接入。

| 内容 | 文档 |
| --- | --- |
| Rust 模块、UI 边界和进程协议 | [架构](architecture.md) |
| 构建、检查、工具准备和实现范围 | [开发与构建](development.md) |
| 数据位置、已有数据兼容与新缓存 | [数据说明](data-storage.md) |
| 模型协议、API 链和设置约定 | [模型接口](model-providers.md) |
| 引擎及资源格式的适用条件 | [引擎与资源模板](adapters/README.md) |
| JSON CLI 与扩展产物契约 | [适配器协议](../contracts/adapter-v1.md) |
| 静态识别条件 | [识别声明](adapters/recognition.md) |
| 智能适配与工具调用的目标行为 | [智能适配](adapters/automatic-adaptation.md)、[内部机制](adapters/adaptation-internals.md) |
| 编码、字形、封包及启动兼容 | [运行组件](adapters/components.md)、[封包组件](adapters/engine-components.md) |

仅供智能体使用的维护资料位于 [llm](../llm/README.md)，项目级 skill 位于 `.agents/skills/`。这些资料不自动注入应用的模型请求，不进入桌面发行包。
