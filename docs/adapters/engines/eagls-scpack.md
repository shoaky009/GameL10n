# EAGLS / ALIS 脚本文本

`eagls-script` 解析完整的已解包脚本成员，支持已确认 EAGLS v1/v2 布局中的对白、选项和字面姓名。其他密钥和标签布局需要显式 `Profile`。菜单等额外字段须有显示证据。

从 `SCPACK.idx` 与 `SCPACK.pak` 读取完整成员，再用小样检查。

```text
try_builtin_adapter(format="eagls-script", directory="work", files=["sample.dat"])
```

通过 `copy_adapter_components` 与 `use_engine_reference` 接入正式适配器。按成员和稳定字段定位原文，保留已有项目的文本身份。组件使用 `ScriptText.fields` 和 `build(translations)` 读写，由共享库处理标签偏移。[脚本接口](../../../adapters/components/eagls_script.py)

严格 CP932 无法表示全部简体译文时，解析和原文往返结果仍可用于调查。正式构建需选择已确认的解码和字体方案，再进行完整资源验收和试译。[文本与运行组件](../components.md)
