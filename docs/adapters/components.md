# 文本与运行组件

本文面向维护适配器的开发者，说明可复用的文本语义、编码、字体和启动实现。它们按能力或引擎模板提供，适配器负责资源发现、封包以及当前游戏的兼容验证。

| 组件 ID | 能力 | 适用条件 |
|---|---|---|
| `kirikiri-scn-text` | PSB SCN 正文、显示姓名、选项、场景标题；缓存及长度自动重算 | 已解密、无资源的 PSB v2/v3 SCN，明确选择语言槽；依赖固定共享 Python 工具库 |
| `cp932-glyph-map` | 全补丁字符映射、严格编解码、复用已有映射 | 已确认脚本使用 CP932，且运行时与字库接受指定码位 |
| `qlie-sfnt-font` | 添加 SFNT 字形别名、调整字体名称及度量 | 已确认的 QLIE system 字体列表、MS Gothic／Mincho 替换策略、单字体 SFNT |
| `system4-fnl-glyph` | 用宋体生成与参考字形尺寸匹配的单色位图 | Windows GDI、BMP 字符及对应 FNL 位图布局；本机须有 SimSun |
| `qlie-japanese-font-launcher` | 独立启动器、临时字体加载、日语 Locale Emulator 启动 | QLIE 的字体枚举规则，以及 LEProc 等待实际游戏退出的启动链 |

组件提供版本、来源、接口示例和适用限制，不代表所有同名引擎版本都兼容。字体、FNL 容器和 EXE 码位处理属于不同能力；不能仅因 CP932 编码成功就认定原引擎可以显示新增字形。

## 接入与版本管理

新适配器使用 Python。编码和字体组件通过 `tools/gamel10n_host.py` 调用应用预编译宿主，不在分析或适配时编译 C#。启动器提供现成 EXE；辅助模块和启动素材放在 `src/tools/`，按用途登记到发布清单。

复制前检查所有目标文件，相同内容复用，不同内容保留现有修改并报告冲突。多个组件共用同一辅助模块，只复制一份。辅助文件及哈希纳入适配器包；解析器依赖单独固定，预编译宿主随应用更新。

## SCN 文本语义

`kirikiri-scn-text` 连接共享库的 `kirikiri_psb` / `kirikiri_scn` 读写器。`SceneText` 的字段位置沿用原树路径，去除派生缓存不会重排正文 ID；内部 speaker ID 默认只作上下文，不能当作显示姓名翻译。只修改选中的正文及其朗读、搜索和可见长度字段，回填后复提比较译文、保留字段及其他数据。未知布局或缓存规则会报错。

复制组件后先用 `run_engine_python` 执行 `src/tools/kirikiri_scn_text.py`，stdin 为 `{"operation":"verify","input":"work/sample.scn"}`。它检查原字节往返及一条正文的中文修改，输出有界摘要；不运行游戏。正式适配器用 `use_engine_reference` 固定依赖，再导入组件处理已解包字节。Hxv4 等封包过滤、覆盖优先级、字体与游戏显示分别验证。

`run_engine_python` 也支持 `module` 参数直接调用固定库中的 `python.engines.*` / `python.archives.*` CLI。连接内置脚本解析器时，使用 `from tools.gamel10n_host import parser`，再调用 `parser(request, 'Renpy')` 等固定格式接口；资源发现由 Python 独立实现。宿主及解析器命令由应用提供，JSON 输入输出统一使用 UTF-8。解析器按资源批次启动进程，不按文本条目调用。

`tools/vntextpatch-reference.json` 的 schema 2 锁定完整工具的文件哈希、`entryPoint` 和 `runtime`（`dotnet` 或 `native`）。托管工具同时包含 DLL、`.deps.json`、`.runtimeconfig.json` 及许可声明；启动命令由依赖解析层生成。组件宿主不加载解析器 DLL，也不推测工具路径。中间响应写入调用工作区的独立文件，完成后移除，不能覆盖适配器最终的 `responseFile`。

已发布 schema 1 清单继续锁定原 DLL；它们通过独立的 `tools/parser-compat` 托管进程调用，需要共享 .NET 运行时。旧清单和包哈希保持原样，不自动改指向新版工具。版本缺失或文件被修改时应恢复原锁定文件，或在独立工作区迁移、验收并发布新适配器版本。

## CP932 映射

`component('glyph_plan', request, texts=..., allowedSlots=..., preservedTexts=..., map=...)` 接受整个补丁最终使用的逻辑文本、已确认可用的码位、其他需保留的文本及可选已有映射，返回 `map`。最终文本包含译文和未翻译的原文；保留文本还应包含未提取的姓名、界面文字等。输入是逻辑文字，不是已经替换过的代理码位。辅助函数从 `tools.gamel10n_host` 导入。

映射方向为“码位 → 真实 Unicode”，JSON 键是十进制字符串；已有 System4 FNL 模板使用反向映射，接入时需显式转换。`glyph_encode` 接受 `texts` 和 `map`，返回 base64 `values`；`glyph_decode` 接受 `values` 和 `map`，返回 `texts`。分配按字符及码位排序，不随文本读取顺序改变，已有映射保持固定。码位占用、映射冲突、容量不足、损坏的 UTF-16 或未计划字符都会报错；编码能完成但解码不一致的字符也必须纳入映射。按批次调用，避免为每个字启动子进程。

映射随输出资源或字体保存，用于重新提取产物；翻译记忆继续保存真实中文。接口处理完整文本单元，不用于分块流式编码。调用者仍需检查原 EXE 的字符遍历与字形查找、实际字体覆盖，以及试译后的显示和排版。

## 字体与启动器

QLIE 字体模板沿用对应适配器的完整名称及行高度、平均宽度规则；不能直接套用于任意字体引擎。System4 位图模板保留字体替换检测和非空字形检查。组件不包含字体文件，适配器需准备符合许可要求的字体和必要声明。

`component('qlie_font', request, input=..., output=..., family=..., map=...)` 生成字体；`component('render_glyphs', request, glyphs=[...])` 批量生成位图。每项字形包含 `character`、`width`、`height` 和 base64 原参考位图 `reference`，返回 base64 `values`。

QLIE 启动组件自带已编译的 `QlieLauncher.exe`。生成补丁时将它复制为独立启动入口，并在 `build.launchExecutable` 声明路径；在启动器旁提供 `qlie-l10n/launch.xml` 和字体。XML 中的路径相对启动器目录，字体 `face` 必须与内部完整名称一致，写 XML 时需正确转义。组件不修改或重新生成原 EXE 的转区配置。

启动器使用 Windows .NET Framework 4.x，目标为 x86。需要转区时使用玩家本机的 Locale Emulator；组件不安装该软件。字体仅在这条已确认的游戏启动链运行期间加载。其他引擎、字体枚举方式、子进程关系和转区工具需要另行验证。

## 开发与打包

组件宿主由 `tools/GameL10n.AdapterHost` 构建，发布到 `tools/adapter-host`，复用发行包共享 .NET 运行时。启动器由 `scripts/package-adapter-components.ps1` 在开发机使用固定 Roslyn 编译器和 Windows .NET Framework 程序集生成，产物为 `tools/adapter-components/QlieLauncher.exe`；`scripts/publish.ps1` 自动包含这些步骤。相同输入重复打包产生相同字节。

组件接入后仍须通过完整适配器验收。复制成功、编译通过或编码可逆，都不能代替游戏实际运行验证。
