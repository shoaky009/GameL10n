# 引擎库与格式支持

先确认资源格式和加载条件。同名引擎可能使用不同的封包、脚本和字体，兼容性以当前游戏的资源验收和运行结果为准。

## 引擎与格式能力表

| 引擎或格式 | 可用能力 | 适用条件 |
| --- | --- | --- |
| [VNTextPatch 脚本](engines/vntextpatch.md) | 内置解析器 | 已解包脚本，按格式表核对编码和伴随文件 |
| [KiriKiri SMIL / SmileCrypt](engines/kirikiri-smil-smile.md) | 内置适配器 | 已确认过滤器的独立 XP3，支持 PSB SCN 与部分源码脚本 |
| [Siglus / Scene.pck](engines/siglus.md) | 内置适配器 | 已确认场景、显示调用和字库布局 |
| [LiLiM SFA16 / AOS v2](engines/sfa16-aos-v2.md) | 内置适配器 | AOS v2 剧情脚本，中文入口须匹配运行程序 |
| [Ren’Py RPYC2 与混合资源](engines/renpy-rpyc.md) | 组合模板 | 松散脚本、RPA/ARC 3.0 与无路径冲突的源码组合 |
| [YU-RIS](engines/yuris.md) | 内置解析器 | YSTB、YSCF、命令表及可选 SJIS 隧道 |
| [EAGLS / ALIS SCPACK](engines/eagls-scpack.md) | 脚本组件 | 完整成员的解析与回填，封包和中文运行另行确认 |
| [Unity SerializedFile](engines/unity-serialized-text.md) | 容器组件 | Unity 2022 小端 v22，TextAsset 文本布局另行确认 |
| [QLIE 字体与启动](engines/qlie-filepack3-systemfont.md) | 字体和启动组件 | 已确认的字体枚举和启动链 |
| [System4 FNL 补字](engines/system4-ain14-msg1-fnl.md) | 字形组件 | 参考字形尺寸的位图生成，AIN、FNL 及运行补丁需单独适配 |
| [Artemis PFS](engines/artemis-pfs.md) | 覆盖卷构建参考 | 运行时确实加载数字卷，并提供可核对的覆盖顺序 |
| [Valkyria DAT / SDT](engines/valkyria.md) | 参考解析 | 完整语义、变长跳转和运行支持由当前适配器验证 |

## 引擎库管理

在“全局引擎库”按引擎或来源查找版本，查看兼容游戏和同类引擎经验。

- **启停**。停用版本退出新项目匹配，现有项目仍可使用已固定版本。
- **删除版本**。在版本空闲时删除外置包。已有文本、译文和补丁保留，需要重新提取或生成补丁时先识别。
- **版本反馈**。确认当前补丁“效果正常”后，程序可清理旧版本，其他项目仍在用的版本保留。[版本反馈与修复](automatic-adaptation.md#版本反馈与修复)

## 开发入口

适配器身份按引擎或资源结构命名，游戏样本与验证记录放入兼容记录。仓库内置实现位于 `adapters/` 和 Engines，用户发布的版本保存在应用数据目录。[正式适配器](artifacts.md#正式适配器)

- [识别与智能适配](automatic-adaptation.md) · [CLI 协议](protocol.md) · [包识别声明](recognition.md)
- [封包与模板](templates.md) · [文本与运行组件](components.md)
