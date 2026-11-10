# 引擎与资源模板

适配器按引擎或资源结构组织。同名引擎可能使用不同封包、脚本和字体，实际兼容性以资源验收及游戏运行记录为准。

能力按三个层面维护：通用流程负责识别、错误分类、资源验收和已有文本身份；引擎家族复用封包、脚本与编码组件；具体引擎配置负责已确认的过滤参数、资源布局和显示入口。游戏样本用于确认适用条件，游戏名称不作为格式兼容证据。

## 引擎库管理

“全局引擎库”聚合内置能力、外置扩展和兼容记录，支持搜索、来源及启停筛选。选择条目可查看版本、兼容游戏和引擎经验。

- **启停**：支持多选；停用后不参与新项目匹配，已有项目仍使用固定版本。
- **版本**：外置版本可打开所在文件夹或删除；内置能力随应用更新。
- **兼容记录**：每个游戏保留一条最近使用记录，分别展示资源和运行验证状态。
- **删除**：移除版本文件、兼容记录、激活引用和索引，缺失文件的记录也可删除；当前任务占用的版本须先停止使用。项目文本、译文和补丁保留，再次提取或构建前需重新识别。
- **历史版本**：确认当前补丁“效果正常”后自动清理，其他项目仍在使用的版本保留；具体规则见[版本反馈与修复](automatic-adaptation.md#版本反馈与修复)。

列表读取 `extensions/library-index.json`，选择外置版本后才检查文件，实际执行前校验完整性。发布、启停、删除和总结同步更新索引；缺失或损坏时从注册元数据重建，不扫描游戏资源或扩展源码。

## 开发资料

已知引擎使用 `engine.<引擎和格式>`，未知格式使用资源模板名称，如 `template.json-dialogue-array-v1`。游戏标题、专属参数与验证记录归入兼容记录。

| 主题 | 文档 |
| --- | --- |
| 识别与复用 | [识别声明](recognition.md) |
| 智能适配与验证 | [识别与智能适配](automatic-adaptation.md) |
| 会话、修复任务、能力配方与执行调度 | [执行与恢复机制](adaptation-internals.md) |
| 发布文件与工作目录 | [产物约定](artifacts.md) |
| 编码、字体与启动 | [文本与运行组件](components.md) |
| 封包与脚本组合 | [封包组件](engine-components.md)、[组合模板](templates.md) |

## 引擎与格式能力表

| 引擎／模板 | 标识 | 支持条件 |
| --- | --- | --- |
| [LiLiM SFA16 / AOS v2 / SCR](engines/sfa16-aos-v2.md) | `engine.sfa16.aos-v2` | AOS v2 剧情脚本，运行补丁须匹配已登记 EXE |
| [System4 AIN v14 MSG1 + FNL](engines/system4-ain14-msg1-fnl.md) | `engine.system4-ain14-msg1-fnl` | MSG1 消息、FNL 补字及匹配运行时代码的修补 |
| [QLIE FilePackVer3.0 + system_fix](engines/qlie-filepack3-systemfont.md) | `engine.qlie-filepack3-systemfont` | 匹配主封包、密钥及字体布局 |
| [VNTextPatch：22 个引擎系列／29 个格式](engines/vntextpatch.md) | `engine.vntextpatch-*` | 已解包脚本，封包与中文运行条件见格式表 |
| [YU-RIS](engines/yuris.md) | `engine.vntextpatch-yuris` | YSTB／YSCF、伴随命令表及可选 SJIS 隧道 |
| [Ren’Py RPYC2 与混合资源](engines/renpy-rpyc.md) | `template.loose-renpy-rpyc` / `template.rpa3-renpy-rpyc` / `template.renpy-mixed` | 松散脚本、RPA/ARC 3.0 与无路径冲突的源码组合 |
| [Siglus / Scene.pck](engines/siglus.md) | `engine.siglus` | 已确认场景和显示调用，FKDS 48/441 字库模板 |
| [KiriKiri / SMIL / SmileCrypt](engines/kirikiri-smil-smile.md) | `engine.kirikiri-smil-smile` | 名称映射、已确认过滤器配置、PSB SCN 与静态脚本显示槽 |
| [Unity SerializedFile](engines/unity-serialized-text.md) | `engine.unity-textasset-dialogue-json` | 已确认的对话 JSON、静态字段和动态说明模板 |

[Artemis PFS 覆盖卷](engines/artemis-pfs.md)说明增量补丁的适用条件。
[Valkyria DAT / SDT](engines/valkyria.md)说明参考解析、跳转验证及 GDI+ 字体创建的边界。
