# QLIE 字体与启动组件

内置 `qlie-sfnt-font` 与 `qlie-japanese-font-launcher` 组件提供字体生成和 Windows 启动入口。应用不内置 `engine.qlie-filepack3-systemfont` 完整适配器；封包密钥、脚本、字体配置与资源加载顺序由当前适配器确认。

补丁生成 `启动汉化.exe`，保留原 EXE，运行期间临时加载配套字体。需要转区时使用本机 Locale Emulator，并保留原 EXE 的 `.exe.le.config`。

## 字体组件接口

`component('qlie_font', request, input=..., output=..., family=..., map=...)` 生成配套 SFNT 字体，通过[组件辅助模块](../components.md#接入与版本管理)调用。

字体度量按 MS Gothic／MS Mincho 比例配置，保留脚本字号和文本框尺寸。适配器按实际字体列表设置名称、保留独立外字槽，不套用固定槽位布局；配置名称须匹配字体 `name` 表 ID 4 的完整 Regular 名称。启动器核对名称与日语枚举结果，正文和明朝体分别使用无衬线与衬线字体。

组件适用于已确认的单字体布局，字体文件及许可由适配器准备。

## 启动组件接入与构建

将预编译 `QlieLauncher.exe` 复制为补丁入口，在 `build.launchExecutable` 声明路径；旁置 `qlie-l10n/launch.xml` 和字体。XML 路径相对启动器目录，字体 `face` 须与内部名称一致。

启动器为独立 Windows x64 Rust 程序，无需额外安装运行时。Locale Emulator 由玩家提供；其他字体枚举或子进程启动链需单独验证。

使用 Rust MSVC 工具链运行 `scripts/build_qlie_launcher.py`，构建并自检，输出 `target/runtime/adapter-components/QlieLauncher.exe`。打包入口见[工具准备](../../development.md#工具准备)。

组件随附对应源码与许可；已有适配器继续使用固定的启动器版本。构建自检不代替游戏内验证。

参考：[QLIE 脚本解析](https://github.com/rafael-vasconcellos/VNTextPatch-net8/blob/e68e174f3db6181e03fc7fabfa7570e1b293d148/VNTextPatch.Shared/Scripts/QlieScript.cs) · [Locale Emulator 字体名称处理](https://github.com/xupefei/Locale-Emulator-Core/blob/ae7160dc5deb97947396abcd784f9b98b6ee38b3/LocaleEmulator/Hooks/Gdi32Hook.cpp#L301)
