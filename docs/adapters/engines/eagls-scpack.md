# EAGLS / ALIS 脚本文本

`eagls-script` 是可复用的脚本组件，连接固定共享库的解析、解密、标签修复和回填能力。

## 范围与入口

同目录的 `SCPACK.idx`、`SCPACK.pak` 提供引擎线索，须先核对索引与成员边界。封包使用 `python.archives.eagls.read_index`／`pack_archive`，按成员偏移读取少量完整加密脚本；重封包传入完整加密成员。仅有配对文件不代表格式已经验证。

组件按标签区的零填充和百字节分配线索，检查至多 1 MiB 的头部并产生少量文本起点候选，同时保留标准 3600 字节布局。EAGLS v1／v2 参数必须唯一通过标签与正文校验。其他 EAGLS／ALIS 布局通过共享 `Profile` 显式提供，未知密钥或标签布局不自动推测。

```text
try_builtin_adapter(format="eagls-script", directory="work", files=["sample.dat"])
```

正式适配器通过 `copy_adapter_components(ids=["eagls-script"])` 和 `use_engine_reference` 固定组件与共享依赖，复用 `ScriptText.fields`、`build(translations)`。JSON CLI 支持 `inspect`、`verify`、`build`，单文件至多 32 MiB；`profile` 接受共享 Profile 字段，`key` 为 ASCII 字符串。

## 字段与身份

- 解析已确认的对白、选项和字面姓名；空选项、冒号姓名变量、资源路径和其他引号参数不作为译文。
- 字段定位使用指令 ID 与同类序号，长度变化后保持稳定。`resourcePath` 指外层 PAK，成员身份放入 `locator` 或 `context`。
- `speaker_refs` 提供字面姓名字段的定位或冒号变量引用，适配器据此建立稳定关联。译后姓名与协议中的原始说话者元数据分别处理；已有项目保留原 ID、context、kind 和 speaker，并完成基线兼容验收。

菜单、标题和其他界面赋值须依据实际显示用途选择，组件不按日文字符或引号形状批量推定。

## 小样与中文

`verify` 分别记录解析、原文字节往返和中文修改复提。回填核对全部字段、稳定定位和姓名关联，并由共享库更新标签偏移、保留未修改内容。

严格 CP932 无法表示完整简体中文时，中文阶段明确失败，已通过的解析与原文往返仍可复用。字符映射、解码、字体与实际加载使用已确认的运行路径，不能仅替换存储编码。相关组件见[文本与运行组件](../components.md)。

小样结果不授予适配器发布资格；完整资源继续执行 `test_adapter`，游戏启动和显示另行确认。

## 参考实现

- [SExtractor / EAGLS_script_tool](https://github.com/satan53x/SExtractor/tree/main/tools/EAGLS)：参数检测、SCPACK 解封包和标签偏移修复；固定共享库已经复用相关字节算法。
- [GARbro EAGLS](https://github.com/morkt/GARbro/blob/master/ArcFormats/Eagls/ArcEAGLS.cs)：归档读取和加密变体参考，不提供脚本文本回填。
- [Eagls_engine_tool](https://github.com/vagmr/Eagls_engine_tool/tree/b7e4c68c71e8796c0e215c9559ef375fbfdde863)：文本清单、回填与 GBK 显示补丁参考。其文本规则和程序补丁有明确样本及特征条件，不能仅凭 EAGLS 名称推广；源码中的特征定位与字符集、断字处理可供运行适配核对。
