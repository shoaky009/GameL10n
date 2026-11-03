# LLM 参考资料

本目录保存面向智能体的任务指引、工具调用步骤和上下文参考。供人阅读的用户说明、协议和维护文档位于 [docs](../docs/README.md)。

- [智能适配工具使用指引](adapter-development.md)：按任务选择内置能力、调查资源、验收与发布。

这些文件供按需查阅，不是程序运行时自动加载的提示词，也不随桌面发行包分发。运行时指令与参数定义仍由 `src/GameL10n.Engines/Development/AutomaticAdaptationService.cs`、`AdaptationTools.cs` 和 `AdaptationToolGroups.cs` 提供；修改本目录不会改变模型请求。
