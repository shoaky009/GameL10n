# YU-RIS 脚本与编码

使用 VNTextPatch 的 `Yuris` 解析器处理解包后的 `.ybn`。YSTB 场景须有同目录的 `ysc.ybn` 命令表，YSCF 支持配置文字。默认严格 CP932。[VNTextPatch 支持范围](vntextpatch.md)

提取时可启用 `includeYurisSources`，用 `yurisSource` 的分组、角色和命令属性确认显示字段。筛选后保留原 ID、顺序和姓名与正文的分组。

指定 `yurisEncoding` 可分别选择 `source`、`output` 为 `cp932` 或 `sjis-tunnel`。隧道的 `mapping` 为有序 BMP 字符串，与无 BOM UTF-16LE 的 `sjis_ext.bin` 槽位一致。为译文和保留原文一同规划，保留已有槽位。

封包、代理加载、字体和中文显示须按当前游戏验证。[Yuris 源码](../../../third_party/VNTextPatch/Scripts/Yuris) · [VNTextProxy 运行文件](../components/vntextproxy-runtime.md)
