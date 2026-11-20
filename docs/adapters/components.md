# 文本与运行组件

组件提供局部的解析、编码、字体或启动能力。先确认当前资源适用，再接入适配器并做完整验收。

| 需要 | 组件或入口 |
| --- | --- |
| EAGLS 脚本、KaGuYa 表格、Unity 容器 | `eagls-script`、`kaguya-tblstr`、`unity-serialized-file` |
| XP3 过滤器与重建 | `xp3-adler` 检测过滤器，`xp3-smil` 处理确认的 SMIL 映射 |
| KiriKiri 场景和预渲染字库 | `kirikiri-scn-text`、`kirikiri-tft` |
| CP932 与隧道编码 | `cp932-glyph-map`、`sjis-tunnel`、`vntextproxy-runtime` |
| 字体诊断 | `inspect_font_coverage` 检查 SFNT/TTC 覆盖，`windows-font-metrics` 检查 GDI 和 GDI+ |
| 字体和启动 | `gdiplus-font-bridge`、QLIE 字体与启动器、`system4-fnl-glyph` |

每个组件的参数、示例和限制随注册条目维护。[组件注册表](../../crates/engines/prompts/adapter-components.json)

## 接入与版本管理

用 `copy_adapter_components` 取得所需组件。分析探针放在 `work/`，正式依赖放在 `src/tools/` 并登记发布清单。需要 VNTextPatch 的组件会自动绑定该工具。使用 `use_engine_reference` 固定共享库。预编译组件由项目工具准备脚本构建。[正式适配器](artifacts.md#正式适配器) · [工具准备](../development.md#工具准备)

从 `inspect_adapter_component` 获取参数、配置示例和可用的 `probeCall`。宿主管理预编译启动器的随附源码并一起发布。排查组件故障时，可以通过组件 ID 和具体源码路径检查所需片段。

具有 `sampleCheck` 的组件可用完整小样检查解析、原文往返和中文复提。小样通过后仍需验收当前适配器和全部选中资源。

## 固定解析器与校验上下文

从 `tools.gamel10n_host` 导入 `parser`、`component`，以 JSON 请求调用固定宿主。例 `parser(request, "Renpy")`。[宿主接口](../../adapters/components/gamel10n_host.py)

适配器可在提取响应声明 `validationContextVersion: 1`，验证时调用 `validation_context(request)` 取得本次原文和输出提取结果。返回 `None` 时重新提取。组件自身的结构、编码和字体校验仍需执行。[适配器协议](protocol.md#构建与校验)

## 结构与编码检测

封包签名只用于初筛。XP3 过滤器须以成员内容和校验确认。SMIL 适配保留原始成员别名，回填按该身份定位。[SMIL 支持范围](engines/kirikiri-smil-smile.md)

CP932 码位映射以十进制码位为键、Unicode 字符为值。规划时应覆盖译文及未修改原文，并沿用已有槽位。复提通过后仍须确认运行时按同一路径读到字形。[编码助手](../../adapters/components/gamel10n_host.py) · [VNTextProxy 准备](components/vntextproxy-runtime.md)

## SCN 文本语义

`kirikiri-scn-text` 处理已解密、无嵌入资源的 PSB v2/v3 SCN，按原树位置定位显示字段。用 `verify` 检查原字节往返及中文复提。部分回填会保留未确认的结构，组件记录跳过的字段。[SCN 接口](../../adapters/components/kirikiri_scn_text.py)

`kirikiri-tft` 可为 Unicode TFT v1 追加 BMP 缺字并保留原有字形。生成字形需要 Windows GDI，字体绑定和加载路径须用游戏试译确认。[TFT 接口](../../adapters/components/kirikiri_tft.py)

## 字体与启动器

优先使用覆盖所需字符的原字体。替换时核对实际选中的 face、字宽和基线，按游戏使用的字体创建方式比较，最后在游戏内确认换行和布局。[字体诊断入口](../../adapters/components/windows_font_metrics.py)

- 游戏使用 GDI+ 时，可用 `windows-font-metrics` 的 `renderer: "gdiplus"` 对比 GDI、GDI+ 与私有字体集合。[GDI+ 字体桥接](components/gdiplus-font-bridge.md)
- QLIE 和 System4 使用各自已确认的字体结构与运行条件。[QLIE 字体与启动](engines/qlie-filepack3-systemfont.md) · [System4 FNL 补字](engines/system4-ain14-msg1-fnl.md)
