# AI5V / SILV 字表式 MES

AI5V 的 `.mes` 文件包含 CP932 双字节字符表和字节码。`ai5v-mes` 组件读取使用 `0x80` 起始字表索引的版本，通过完整脚本检查区分字表引用、内联字符、控制指令与参数字符串。文件后缀和疑似字表只能用于候选筛选。

用 `try_builtin_adapter(format="ai5v-mes")` 检查解包后的完整成员，或用 `copy_adapter_components` 复制解析器到适配器。`ScriptText(data).fields` 用文本段序号定位内联文本。`build` 更新目标段并保留参数、控制字节和现有字表索引。无变更时返回原文件。字表无法容纳新增字符、输入含未知指令或字符无法编码时返回错误。

本组件覆盖已知的字表式字节码，AI6WIN、Silky’s+ 和其他 MES 变体按对应组件处理。原始封包、额外指令和游戏内中文显示按游戏确认。

[AI5V 指令说明](https://github.com/mnakamura1337/engine_ai5v/blob/9fad70bd14d6a0de553bb6320d2eb74315c686d9/elf_mes.ksy) · [字表与指令格式](https://github.com/FuzionCD/lime-juice/blob/1b862f087e3b9b985f17af3ee744aca2607c72be/doc/mes-format.md#ai5-format)
