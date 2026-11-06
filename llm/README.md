# LLM 参考资料

这里保存智能体按需读取的工具使用约定和历史兼容任务。用户说明、架构、协议和维护文档从 [docs](../docs/README.md)进入；当前可执行功能以[开发文档](../docs/development.md)为准。

- [适配工具使用约定](adapter-development.md)：资源调查、工具选择、验收与发布的完整目标行为。
- [历史 C# 逻辑迁移参考](tasks/migrate-csharp-analysis-scripts.md)：旧数据及旧 DLL 的迁移边界。
- [解析器进程协议参考](tasks/native-parser-process.md)：固定依赖、离线工具和原生工具协议。
- [智能适配日志调查 skill](../.agents/skills/review-adaptation-logs/SKILL.md)：按摘要、失败阶段和调用 ID 定位问题。

本目录不自动加入运行时提示词，也不随桌面包分发。Rust 的运行时提示词和工具定义由 Engines 模块负责，完整会话尚未接入；修改本目录不会改变模型行为。历史维护任务保留其原工程上下文，不能直接当作本仓库已实现的功能。
