# Unity SerializedFile 与文本资源

## 容器与 TextAsset 复用

`unity-serialized-file` 组件提供 Unity 2022 小端 SerializedFile v22 的容器读写能力。可通过 `list_adapters(query="Unity")`、Unity 类型检查结果或引擎经验中的组件入口发现，再使用 `copy_adapter_components` 复制。UnityFS 封包和其他序列化版本需要对应解析器，不能套用此布局。

```text
copy_adapter_components(ids=["unity-serialized-file"])
run_engine_python(script="src/tools/unity_serialized_file.py", stdin='{"operation":"probe","input":"resources/data.assets","limit":8}')
```

探针只确认容器结构，返回对象类型统计、TextAsset 总数及名称分页；支持名称查询，每页默认 8 项、最多 32 项，单文件至多 512 MiB，不返回文本正文。Python 适配器通过 `SerializedFile.text_assets()` 读取名称、稳定的 `object.path_id` 和原始 `script` 字节，再通过 `rebuild_text_assets({path_id: bytes})` 回填。未修改时保持原文件字节；变长修改保留名称、对象身份、引用、未知尾部和其他对象。

TextAsset 不一定是玩家文本。编码、脚本语法、语言列和显示字段由适配器确认，内部 key、哈希表和示例数据不能整表翻译。容器复用不代表 MonoBehaviour 字段、场景内 UI、字体或完整覆盖已验证。显示姓名作为独立可译字段时，其他文本单元的身份信息不能依赖会随翻译变化的姓名；部分翻译仍须通过身份与未修改文本检查。

## 已确认的文本布局

标识：`engine.unity-textasset-dialogue-json`。支持已确认的 Unity 2022 SerializedFile v22、对话 JSON、MonoBehaviour 文本字段与 TMP 字体回退组合；业务类型和资源布局须逐项匹配。

提取对白、已确认字段的标题和说明及部分动态模板，保留内部 key、对象名和引用。运行时拼接、输入动作名及 IL2CPP 字符串需确认语义，不能按字段名或整张表翻译。

动态模板 `(xx0x1)`、`(+x0%x1)`、`(+x0%)`、`(*)` 由程序作为控制标记保留。验收包含独立原文、原字节往返、变长中文回填和局部试译；遗漏记录于项目覆盖待办，字体与布局需实机确认。
