# Unity SerializedFile 对话与文本布局

标识：`engine.unity-textasset-dialogue-json`。支持已确认的 Unity 2022 SerializedFile v22、对话 JSON、MonoBehaviour 文本字段与 TMP 字体回退组合；业务类型和资源布局须逐项匹配。

提取对白、已确认字段的标题和说明及部分动态模板，保留内部 key、对象名和引用。运行时拼接、输入动作名及 IL2CPP 字符串需确认语义，不能按字段名或整张表翻译。

动态模板 `(xx0x1)`、`(+x0%x1)`、`(+x0%)`、`(*)` 由程序作为控制标记保留。验收包含独立原文、原字节往返、变长中文回填和局部试译；遗漏记录于项目覆盖待办，字体与布局需实机确认。
