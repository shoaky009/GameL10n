# LLM 参考资料

本目录保存面向智能体的任务指引、工具调用步骤和上下文参考。供人阅读的用户说明、协议和维护文档位于 [docs](../docs/README.md)。

- [智能适配工具使用指引](adapter-development.md)：按任务选择内置能力、调查资源、验收与发布。
- [待定改进方向](pending-improvements.md)：常规适配范围与游戏内补丁可用性的待确认问题、证据和边界。
- [智能适配日志调查 skill](../.agents/skills/review-adaptation-logs/SKILL.md)：用 `$review-adaptation-logs` 调用，先统计、再按失败阶段和调用 ID 定位，控制排查时的上下文用量。

这些文件供按需查阅，不是程序运行时自动加载的提示词，也不随桌面发行包分发。运行时指令与参数定义仍由 `crates/engines/src/adaptation_flow.rs`、`adaptation.rs` 和 `adaptation-tools.json` 提供；修改本目录不会改变模型请求。
