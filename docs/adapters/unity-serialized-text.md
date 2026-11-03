# Unity SerializedFile 对话与已确认文本布局

标识：`engine.unity-textasset-dialogue-json`。适用于已验证的 Unity 2022 SerializedFile v22、对话 JSON、MonoBehaviour 文本字段与 TMP 字体回退组合。Unity 版本相同不能证明业务类的字段布局一致。

已核对游戏：Tiny Rogues，Unity 2022.3.62f2。固定布局仅适用于已确认的资源版本，其他游戏或版本需要单独验证。

支持的布局包括 `Stats.SimpleStat`、`Stats.Dynamic_Stats.DynamicStat` 的动态说明模板、`WorldObjective` 的五个文字字段、`EffectClassAbility` 的标题与说明；`CollectionItem` 使用 bool 对齐加三个 PPtr 共 40 字节的前缀。内部 key、对象名及其余二进制字段保留，收藏品标题与解锁条件分别提取。

此资源版本提取 5,943 条。验证包括 12 个独立原文样本、原字节往返、全量中文变长回填及局部试译；游戏实际显示仍需确认。动态模板中的 `(xx0x1)`、`(+x0%x1)`、`(+x0%)`、`(*)` 由主程序作为控制标记保留。

武器属性标签、精通教程、输入设置、部分状态效果和运行时更新的界面仍未完整覆盖；这些内容保存在项目的覆盖待办中。不能直接翻译整个 IL2CPP 字符串表或输入动作名，其中包含程序查找键。截图中的中文字号问题也尚未完成运行验证。
