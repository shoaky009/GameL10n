# YU-RIS 脚本与编码

解析器键 `Yuris`，标识 `engine.vntextpatch-yuris`，依赖来源见[VNTextPatch](vntextpatch.md)。

## 资源与支持条件

处理已解包 `.ybn`：`YSTB` 场景依赖同目录 `ysc.ybn` 命令表，`YSCF` 提供配置文字。默认严格 CP932，封包、字体、代理和加载顺序按当前游戏确认。

## 文本来源与分组

`parser(request, "Yuris", operation="extract", includeYurisSources=True)` 可返回每条文本的 `yurisSource`：

| 字段 | 含义 |
| --- | --- |
| `groupIndex` | 原始分组，姓名与正文共享 |
| `role` | `name`、`dialogue` 或 `caption` |
| `attributes` | 命令序号、名称、属性描述符、属性 ID、名称与类型，序号从零开始 |

YSCF 标题的属性数组为空。来源用于按实际显示语义筛选，过滤后保留原 ID、顺序和分组，不重新编号。回填保持姓名与正文边界，括号独白仍保留独立正文。

## 输入、输出与 SJIS 隧道

可选 `yurisEncoding`：

| 字段 | 含义 |
| --- | --- |
| `source`、`output` | `cp932` 或 `sjis-tunnel`，默认 `cp932` |
| `mapping` | 有序 BMP 字符串，对应无 BOM UTF-16LE 的 `sjis_ext.bin` |

提取使用源编码，构建与复提使用输出编码，校验分别处理两端。映射不自动扩容，缺字、重复槽位、NUL 和代理字符报错。

适配器用 `sjis-tunnel` 为全部译文及保留文字规划映射，保留旧槽位并输出 `sjis_ext.bin`。将原始解包脚本直接交给解析器，复用提取与来源元数据筛选。无实际修改时保留输入编码、映射、原字节与伴随文件。

解析器不安装代理或字体，实际加载与中文显示仍需验证。依赖缺少接口时须显式更新并重新验收，保留现有源码和文本身份。映射与代理准备见[组件](../components.md#结构与编码检测)、[VNTextProxy](../components/vntextproxy-runtime.md)。
