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

普通 C# 组件作为源码参加适配器编译，启动器源码和已编译 EXE 作为依赖放在 `src/tools/`。组件不引用主程序程序集，也不要求 NuGet 或额外服务。

复制前检查所有目标文件，相同内容复用，不同内容保留现有修改并报告冲突。复制的文件及哈希纳入适配器包，应用更新不会自动替换已发布版本中的组件。

## SCN 文本语义

`kirikiri-scn-text` 连接共享库的 `kirikiri_psb` / `kirikiri_scn` 读写器。`SceneText` 的字段位置沿用原树路径，去除派生缓存不会重排正文 ID；内部 speaker ID 默认只作上下文，不能当作显示姓名翻译。只修改选中的正文及其朗读、搜索和可见长度字段，回填后复提比较译文、保留字段及其他数据。未知布局或缓存规则会报错。

复制组件后先用 `run_engine_python` 执行 `src/tools/kirikiri_scn_text.py`，stdin 为 `{"operation":"verify","input":"work/sample.scn"}`。它检查原字节往返及一条正文的中文修改，输出有界摘要；不运行游戏。正式适配器用 `use_engine_reference` 固定依赖，再导入组件处理已解包字节。Hxv4 等封包过滤、覆盖优先级、字体与游戏显示分别验证。

`run_engine_python` 也支持 `module` 参数直接调用固定库中的 `python.engines.*` / `python.archives.*` CLI，无需临时 runpy 脚本。Python 连接内置 C# 脚本解析器时，可用 `EngineReferencePython.Run("tools/adapter.py", args, BuiltinScriptParser.ExecuteJson)` 作为入口；子进程参数由 `request['builtinCommand']` 提供，JSON 输入输出统一使用 UTF-8。

## CP932 映射

`Cp932GlyphEncoding.Plan` 接受整个补丁最终使用的逻辑文本、已确认可用的码位、其他需保留的文本及可选已有映射。最终文本包含译文和未翻译的原文；保留文本还应包含未提取的姓名、界面文字等。输入是逻辑文字，不是已经替换过的代理码位。

映射方向为“码位 → 真实 Unicode”，对应 `SlotToUnicode`；已有 System4 FNL 模板使用反向映射，接入时需显式转换。分配按字符及码位排序，不随文本读取顺序改变，已有映射保持固定。码位占用、映射冲突、容量不足、损坏的 UTF-16 或未计划字符都会报错；编码能完成但解码不一致的字符也必须纳入映射。

映射随输出资源或字体保存，用于重新提取产物；翻译记忆继续保存真实中文。接口处理完整文本单元，不用于分块流式编码。调用者仍需检查原 EXE 的字符遍历与字形查找、实际字体覆盖，以及试译后的显示和排版。

## 字体与启动器

QLIE 字体模板沿用对应适配器的完整名称及行高度、平均宽度规则；不能直接套用于任意字体引擎。System4 位图模板保留字体替换检测和非空字形检查。组件不包含字体文件，适配器需准备符合许可要求的字体和必要声明。

QLIE 启动组件自带已编译的 `QlieLauncher.exe`。生成补丁时将它复制为独立启动入口，并在 `build.launchExecutable` 声明路径；在启动器旁提供 `qlie-l10n/launch.xml` 和字体。XML 中的路径相对启动器目录，字体 `face` 必须与内部完整名称一致，写 XML 时需正确转义。组件不修改或重新生成原 EXE 的转区配置。

启动器使用 Windows .NET Framework 4.x，目标为 x86。需要转区时使用玩家本机的 Locale Emulator；组件不安装该软件。字体仅在这条已确认的游戏启动链运行期间加载。其他引擎、字体枚举方式、子进程关系和转区工具需要另行验证。

## 开发与打包

普通组件源码嵌入 Engines 程序集。启动器由 `scripts/package-adapter-components.ps1` 使用固定 Roslyn 编译器和 Windows .NET Framework 程序集生成，产物为 `tools/adapter-components/QlieLauncher.exe`；`scripts/publish.ps1` 自动包含此步骤。相同输入重复打包产生相同字节。

组件接入后仍须通过完整适配器验收。复制成功、编译通过或编码可逆，都不能代替游戏实际运行验证。
