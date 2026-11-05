# LLM 参考资料

本目录保存面向智能体的任务指引、工具调用步骤和上下文参考。供人阅读的用户说明、协议和维护文档位于 [docs](../docs/README.md)。

- [智能适配工具使用指引](adapter-development.md)：按任务选择内置能力、调查资源、验收与发布。
- [历史 C# 分析脚本迁移任务](tasks/migrate-csharp-analysis-scripts.md)：在提供旧数据的机器上将临时探针与经验辅助脚本迁移到 Python，保留正式适配器与历史记录。
- [智能适配日志调查 skill](../.agents/skills/review-adaptation-logs/SKILL.md)：用 `$review-adaptation-logs` 调用，先统计、再按失败阶段和调用 ID 定位，控制排查时的上下文用量。

这些文件供按需查阅，不是程序运行时自动加载的提示词，也不随桌面发行包分发。运行时指令与参数定义仍由 `src/GameL10n.Engines/Development/AutomaticAdaptationService.cs`、`AdaptationTools.cs` 和 `AdaptationToolGroups.cs` 提供；修改本目录不会改变模型请求。
