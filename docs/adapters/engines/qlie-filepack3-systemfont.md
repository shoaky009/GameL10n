# QLIE FilePackVer3.0 + system_fix

标识：`engine.qlie-filepack3-systemfont`。处理匹配外置 `fkey`、EXE 密钥资源和 `system_fix` 字体布局的 `data0.pack`，提取剧情、姓名和选项。

补丁生成 `启动汉化.exe`，保留原 EXE，运行期间临时加载配套字体。需要转区时使用本机 Locale Emulator，并保留原 EXE 的 `.exe.le.config`。

## 字体组件接口

`component('qlie_font', request, input=..., output=..., family=..., map=...)` 生成配套 SFNT 字体，通过[组件辅助模块](../components.md#接入与版本管理)调用。

字体度量按 MS Gothic／MS Mincho 比例配置，保留脚本字号和文本框尺寸。七个正文字体列表使用 `system`，独立外字槽保留；配置名称须匹配字体 `name` 表 ID 4 的完整 Regular 名称。启动器核对名称与日语枚举结果，正文和明朝体分别使用无衬线与衬线字体。

组件适用于已确认的单字体布局，字体文件及许可由适配器准备。

## 启动组件接入与构建

将预编译 `QlieLauncher.exe` 复制为补丁入口，在 `build.launchExecutable` 声明路径；旁置 `qlie-l10n/launch.xml` 和字体。XML 路径相对启动器目录，字体 `face` 须与内部名称一致。

启动器为 Windows x64 NativeAOT，Locale Emulator 由玩家提供；其他字体枚举或子进程启动链需单独验证。

开发机使用 .NET 10 SDK 运行 `scripts/build_qlie_launcher.py`，构建并执行静态自检，输出 `target/runtime/adapter-components/QlieLauncher.exe`。打包入口见[工具准备](../../development.md#工具准备)。

参考：[QLIE 脚本解析](https://github.com/rafael-vasconcellos/VNTextPatch-net8/blob/e68e174f3db6181e03fc7fabfa7570e1b293d148/VNTextPatch.Shared/Scripts/QlieScript.cs) · [Locale Emulator 字体名称处理](https://github.com/xupefei/Locale-Emulator-Core/blob/ae7160dc5deb97947396abcd784f9b98b6ee38b3/LocaleEmulator/Hooks/Gdi32Hook.cpp#L301)
