# VNTextProxy 运行文件准备

`vntextproxy-runtime` 检查并准备 VNTranslationTools v0.0.41 的官方 x86 代理，附带 VNTranslationTools 与 Detours 的 MIT 声明。

```text
copy_adapter_components(ids=["vntextproxy-runtime"])
run_engine_python(script="src/tools/vntextproxy_runtime.py", stdin='{"operation":"describe"}')
run_engine_python(script="src/tools/vntextproxy_runtime.py", stdin='{"operation":"check","archive":"work/vntools.zip","proxy":"d3d9.dll"}')
run_engine_python(script="src/tools/vntextproxy_runtime.py", stdin='{"operation":"prepare","archive":"work/vntools.zip","proxy":"d3d9.dll","output":"src/tools/vntextproxy"}')
```

通过 `download_tool` 获取[官方 ZIP](https://github.com/arcusmaximus/VNTranslationTools/releases/download/v0.0.41/VNTranslationTools.zip)，SHA256 为 `37fc44f2c2fb73190b783df44bb58b60ce44e70131942f262ee3bf17dfa12439`。

`check` 只读核对归档、成员哈希与 PE32。`prepare` 仅提取指定 DLL 和许可，输出须为工作区内相对路径；已有相同文件复用，冲突、链接与越界拒绝。将实际 DLL、许可和映射登记发布清单，完整 ZIP 不入包。

代理名称须依据实际 EXE 加载链选择，不能互相改名或覆盖已有同名 DLL；x64 游戏不适用。该版本代理没有 INI 或自动转区功能。

`sjis_ext.bin` 放在游戏 EXE 旁，为无 BOM 的有序 UTF-16LE 映射。用 `sjis-tunnel` 为译文与保留原文统一规划，保留已有槽位。

可选字体从 EXE 目录依次查找首个 TTF、TTC、OTF，私有加载，以文件名首个点之前的部分请求字体名，须与内部 face 匹配。避免多个候选歧义，并核对字体覆盖与许可。

替换字体会影响代理覆盖的原文和译文。先确认原字体风格、字重和等宽属性，再用 [windows-font-metrics](../components.md#字体与启动器) 比较原字体与候选的实际尺寸；代理保留字号数值不代表字形等大，正高度请求尤其受内部留白影响。该版本没有字号补偿 INI；需要校准时检查引擎已有设置或已确认的字体创建入口，并保证测宽与绘制一致。

该版本只钩取 GDI 和 Direct2D，不支持 GDI+ 字体创建。`AddFontResourceExW` 加载成功不等于 `GdipCreateFontFromDC` / `GdipCreateFontFromLogfontA/W` 可用。游戏使用 GDI+ 时，先用 `windows-font-metrics` 的 `renderer:"gdiplus"` 对比原字体和候选。若只有私有字体集合路径成功，运行时必须显式使用该集合并保持其生命周期；不能凭 `NotTrueTypeFont` 就断定 OTF 不兼容或不断换 TTF。字体创建通过仍不代表游戏显示已验证。

准备结果为 `runtimeTested=false`；代理旁加载、字符遍历、字体与转区须在当前游戏验证。

普通 x86 `GdipCreateFontFromDC` / `GdipCreateFontFromLogfontA/W` 调用可按[字体桥接组件](gdiplus-font-bridge.md)接入已确认的私有集合路径。桥接负责字体创建，代理仍负责所需的文本解码与 GDI 注册；两者不互相替代。
