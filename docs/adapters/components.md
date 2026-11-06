# 文本与运行组件

组件提供可复用能力，适配器负责资源发现、封包及当前游戏验证。

| 组件 ID | 能力 | 适用条件 |
| --- | --- | --- |
| `windows-font-metrics` | 缺字、字体尺寸及 GDI+ 字体创建路径对比 | Windows，仅分析时使用 |
| `kaguya-tblstr` | ARC/ARI 检测、解析和变长重建 | XOR-FF、四字节最小对齐、类型 0/2/3 |
| `xp3-adler` | 索引、成员与 Adler 派生过滤器检查 | 至少两个受保护成员通过；不提供回包器 |
| `sjis-tunnel` | VNTextProxy 映射、容量与往返检查 | v0.0.41，最多 3422 个扩展 BMP 字符 |
| `vntextproxy-runtime` | 固定代理 DLL 检查和准备 | Windows x86，确认代理加载链 |
| `gdiplus-font-bridge` | 固定 GDI+ 私有字体创建与导入重定向 | x86 普通导入、明确字体匹配与实际创建路径 |
| `kirikiri-scn-text` | SCN 正文、显示姓名、选项和标题 | 已解密、无资源 PSB v2/v3，明确语言槽 |
| `kirikiri-tft` | 预渲染字库覆盖检查和缺字补充 | Unicode TFT v1、BMP；生成字形需要 Windows GDI |
| `cp932-glyph-map` | 字符映射和严格编解码 | CP932，运行时与字体接受所选码位 |
| `qlie-sfnt-font` | SFNT 字形别名、名称和度量 | 已确认的 QLIE 字体列表，单字体 SFNT |
| `system4-fnl-glyph` | 参考尺寸的宋体单色位图 | Windows GDI、BMP、对应 FNL 布局与 SimSun |
| `qlie-japanese-font-launcher` | 临时字体与日语转区启动 | 已确认的 QLIE 枚举及 Locale Emulator 启动链 |

## 接入与版本管理

Python 适配器通过 `tools/gamel10n_host.py` 调用预编译宿主。运行依赖和素材放在 `src/tools/` 并登记发布清单，仅分析用的组件放在 `work/`。

复制前检查全部目标，相同文件复用，不同内容保留并报告冲突。辅助模块按哈希入包，共用文件只保留一份；解析器版本单独固定。

## 结构与编码检测

`inspect_engine_knowledge(id)` 提供相关组件和解析器入口，候选须用当前样本确认。复制组件后可直接运行：

```text
run_engine_python(script="src/tools/kaguya_tblstr.py", stdin='{"operation":"probe","input":"resources/table.arc","index":"resources/table.ari"}')
run_engine_python(script="src/tools/xp3_adler.py", stdin='{"operation":"probe","input":"resources/scripts.xp3"}')
run_engine_python(script="src/tools/sjis_tunnel.py", stdin='{"operation":"check","texts":["原文かな","简体中文译文"]}')
```

探针返回 `matched`、`rejected` 或 `needs-evidence`。TBLSTR 结构与中文编码分别报告，保留未修改记录填充，不接受超出最小对齐的未知尾部。

XP3 检测只确认已验证的派生过滤器。未知索引块返回 `rebuildRequiresPreservation=true`，重建须保留；分段解码使用 `apply_key_in_place(..., member_offset=...)` 指定成员内相位。

SJIS 检查在内存中规划全部译文与保留原文，构建通过 `Tunnel.serialize()` 保存映射。编码容量与往返检查不包含运行时字符遍历和字体验证。

## VNTextProxy 运行文件准备

固定代理下载、选择、映射和分发见[VNTextProxy 组件](components/vntextproxy-runtime.md)。

GDI+ 私有集合成功而原字体创建失败时，先检查[字体桥接组件](components/gdiplus-font-bridge.md)的适用条件；它独立于 VNTextProxy，不提供文本解码。

## SCN 文本语义

`kirikiri-scn-text` 使用共享库 `kirikiri_psb`／`kirikiri_scn`。`SceneText` 按原树路径定位，内部 speaker ID 只作上下文。修改选中正文及其朗读、搜索和长度缓存，保留其余字段；未知布局报错。

`src/tools/kirikiri_scn_text.py` 的 `{"operation":"verify","input":"work/sample.scn"}` 检查原字节往返与一条中文修改。正式适配器用 `use_engine_reference` 固定依赖；外层过滤器、加载优先级和字体单独验证。

## TFT 预渲染字库

`kirikiri-tft` 支持 Unicode TFT v1 的 BMP 字码和灰度 RLE。先将已解包的字库交给只读探针：

```text
run_engine_python(script="src/tools/kirikiri_tft.py", stdin='{"operation":"probe","input":"work/original.tft","text":"原文かな、。简体译文！？ABC123","sampleLimit":2}')
```

返回格式、字符覆盖和少量原字形度量，不加载字体或写入文件。`index(data)` 与 `bitmap(data, glyph)` 可在各平台解析；`supplement(data, codepoints, renderer)` 仅追加缺字，保留全部原字形、度量和未知字节。所需字码应包含该字库使用的最终译文与保留原文，无缺字时返回原字节。

Windows 上可用 `with GdiRenderer(font_file, face, size, weight=400, italic=False) as renderer` 提供新字形。`size` 为明确的正 em 像素高度；渲染器按需临时加载指定字体，检查实际名称与缺字并在退出时释放。调用者负责按原字形比较风格、可见尺寸、基线及字宽，为每份字库选择参数。

组件不按字库文件名推断字号，不修改脚本或封包。适配器须确认实际字体绑定、文本解码和字库加载路径，再接入回包；静态补字通过不代表游戏中的显示已验证。其他 TFT 版本、非 BMP 字码或损坏数据会明确拒绝。

## 固定解析器与校验上下文

`run_engine_python(module=...)` 可调用固定库的 `python.engines.*`／`python.archives.*` CLI。原生解析器通过 `from tools.gamel10n_host import parser`，调用 `parser(request, 'Renpy')` 等格式入口，按资源批次启动，输入输出为 UTF-8 JSON。

支持校验上下文的适配器在 `extract` 外层响应声明 `validationContextVersion: 1`，在 `validate` 调用 `validation_context(request)`，取得 `originalUnits`、`outputUnits`、`outputFiles` 和 `translations`。上下文仅在本次校验有效；返回 `None` 时执行完整提取。宿主核对版本、目录和资源选择，适配器仍负责非文本结构、编码与字体检查。

`tools/vntextpatch-reference.json` schema 2 使用 `runtime: native`，固定 `GameL10n.VnTextPatch.exe` 与 `capstone.dll` 哈希。历史 schema 2 可省略入口并仅固定 EXE，保留环境时补记配套 DLL。版本缺失或损坏须恢复原文件，或显式更新后重新验收；schema 1 DLL 依赖须转换并重新发布。临时响应不能覆盖适配器最终 `responseFile`。

## CP932 映射

从 `tools.gamel10n_host` 导入 `component`：

| 调用 | 输入 | 输出 |
| --- | --- | --- |
| `glyph_plan` | `texts`、`allowedSlots`、`preservedTexts`、可选已有 `map` | `map` |
| `glyph_encode` | `texts`、`map` | base64 `values` |
| `glyph_decode` | `values`、`map` | `texts` |

例如 `component('glyph_plan', request, texts=..., allowedSlots=..., preservedTexts=..., map=...)`。规划使用整个补丁最终的逻辑文字，包含译文、未翻译文本及其他需保留的字符。

映射方向为“码位 → Unicode”，JSON 键为十进制字符串；System4 FNL 的反向映射需显式转换。分配稳定并保留旧槽位，冲突、容量不足、未计划字符或编解码不一致时报错。映射随产物保存，翻译记忆保存真实中文。接口按完整文本批量调用。

## 字体与启动器

缺字会使测宽与回退绘制使用不同度量，导致重叠。应使用覆盖所需字符的字体，并核对设置、重置和读档后的实际选择，保留原字号和风格。

`windows-font-metrics` 通过 Windows GDI 检查字体：

```json
{"fonts":[{"face":"Original Font","file":"work/font.ttf","height":-24},{"face":"Microsoft YaHei","height":-24}],"text":"原文かな、。简体译文！？ABC123"}
```

使用 `run_engine_python(script="work/windows_font_metrics.py", stdin=...)`，`fonts` 最多 4 项，首项为原字体，其余为候选；`text` 最多 512 字符，包含共同汉字、假名、标点、ASCII 及中文。`file` 可省略，`height` 使用引擎实际有符号高度；可选 `width`、`weight`、`italic`、`charset`、`pitchAndFamily`。

返回实际名称、缺字、字宽、字形高度及 `textMetrics` 中的上升部、下降部、内部与外部留白。`ascent` 是字框顶部到基线的距离，`emHeight = height - internalLeading`；样例 `inkTop` / `inkBottom` 以基线为零、向下为正。正字号指定含内部留白的字框高度，负字号指定字符高度，因此相同数字不保证替换后视觉等大。

`comparisons` 将每个候选与首项比较，`advanceRatio`、`inkWidthRatio`、`inkHeightRatio` 为共同覆盖字符逐字比率的中位数（候选 ÷ 原字体）；`emHeightRatio` 比较字符高度，`baselineDelta` / `cellHeightDelta` 为候选与原字体的像素差。缺字排除，空白字符或空轮廓不参与字形比率；无可比值时返回 `null`，应结合样本数判断。非 BMP 字符标为未检查。

原字体覆盖中文时优先保留；必须替换时选择同类衬线或无衬线、字重和等宽属性的字体。明显的尺寸差异应通过候选选择或实际字体请求调整并复测，同时核对基线、行距和换行；比率不能直接当作通用放大倍率。字体仅在诊断进程临时加载，结果不能证明游戏实际选中该字体，最终外观通过少量试译确认。

GDI+ 游戏在请求顶层加 `"renderer":"gdiplus"`。每项结果的 `gdiplus` 对比 `fromDC`、`fromLogfontA/W`、`byName` 和显式 `privateCollection` 路径，保留原生状态码和创建结果。GDI 已选中字形而 GDI+ 创建失败时，先排查注册与创建路径；`NotTrueTypeFont` 本身不能证明文件不是 TrueType。私有集合成功只证明该调用方式可用，适配器还须让游戏使用同一路径。默认仍只测 GDI，不加载额外后端。

System4 位图使用 `component('render_glyphs', request, glyphs=[...])`，每项包含 `character`、`width`、`height` 和 base64 参考位图 `reference`，返回 base64 `values`。组件检查字体替换和非空字形，字体文件及许可由适配器准备。

QLIE 参数与启动配置见[字体与启动组件](engines/qlie-filepack3-systemfont.md#字体组件接口)。

## 开发与打包

工具构建见[开发文档](../development.md#工具准备)，发布文件选择见[产物约定](artifacts.md#正式适配器)。
