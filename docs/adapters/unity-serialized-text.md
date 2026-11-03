# Unity SerializedFile 对话与已确认文本布局

标识：`engine.unity-textasset-dialogue-json`。适用于已验证的 Unity 2022 SerializedFile v22、对话 JSON、MonoBehaviour 文本字段与 TMP 字体回退组合。Unity 版本相同不能证明业务类的字段布局一致。

已核对游戏：Tiny Rogues，Unity 2022.3.62f2。资源更新工具限定原文指纹及适配源码版本，避免把固定布局套用到其他游戏或覆盖后续开发成果。

已有项目可通过本地更新补入字段，不调用智能分析或翻译 API：

```powershell
# 只检查，不写入项目
.\scripts\update-unity-text-coverage.ps1 -ProjectDirectory 'F:\Projects\games\defd4630872ba47920e71e42'

# 关闭 GameL10n 后应用
.\scripts\update-unity-text-coverage.ps1 -ProjectDirectory 'F:\Projects\games\defd4630872ba47920e71e42' -Apply
```

更新使用项目原文快照，编译并验证新适配器，刷新提取缓存及当前适配状态。游戏安装目录和已有补丁保持原样。应用更新后打开项目，翻译新增文本，再生成和安装补丁即可。

支持的新增布局包括 `Stats.SimpleStat`、`Stats.Dynamic_Stats.DynamicStat` 的动态说明模板、`WorldObjective` 的五个文字字段、`EffectClassAbility` 的标题与说明；同时修正 `CollectionItem` 中 bool 对齐加三个 PPtr 共 40 字节的前缀。内部 key、对象名及其余二进制字段保留。原有 35 个收藏品标题曾被标为 description，更新按明确映射、相同原文和类型迁移译文、校对和试译选区，避免标题译文误用到解锁条件。

此资源版本提取由 3,835 增至 5,943 条。验证包括 12 个独立原文样本、原字节往返、全量中文变长回填及局部试译；游戏实际显示仍需确认。动态模板中的 `(xx0x1)`、`(+x0%x1)`、`(+x0%)`、`(*)` 由主程序作为控制标记保留。

武器属性标签、精通教程、输入设置、部分状态效果和运行时更新的界面仍未完整覆盖；这些内容保存在项目的覆盖待办中。不能直接翻译整个 IL2CPP 字符串表或输入动作名，其中包含程序查找键。截图中的中文字号问题也尚未完成运行验证。

`scripts/repair-translation-fragments.ps1 -ProjectDirectory <项目目录>` 可更新翻译格式保护，并应用已核对的属性说明片段校正。需先关闭程序。校正按原文、上下文、语言和旧译文精确匹配，保留人工编辑及后续译文，不调用 API、不重新提取、不安装游戏补丁。程序默认更新到 `artifacts/GameL10n-projects`，其他已有发行目录可通过 `-ApplicationDirectory` 指定。
