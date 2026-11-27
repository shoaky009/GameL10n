# Unity SerializedFile 与 TextAsset

`unity-serialized-file` 提供小端 Unity SerializedFile v22 容器与 TextAsset 字节读写。UnityFS 和其他序列化版本需要对应解析器。

通过 `copy_adapter_components` 取得后，用 `SerializedFile.text_assets()` 读取稳定的对象 ID、名称和脚本字节。以对象 ID 调用 `rebuild_text_assets`，保留未修改对象、引用和名称。[容器实现](../../../adapters/unity/serialized_file.py)

确认 TextAsset 的脚本语义、编码和实际显示字段后再提取。MonoBehaviour、场景 UI、程序集字符串和字体需要单独调查。显示姓名与其他文本身份分别处理。
