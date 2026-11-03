# QLIE FilePackVer3.0 + system_fix

适配器标识为 `engine.qlie-filepack3-systemfont`，处理带外置 `fkey`、EXE 密钥资源和 `system_fix` 字体布局的 QLIE FilePackVer3.0 主脚本封包。仅处理 `data0.pack` 中已支持的剧情、姓名和选项，不改动图像、语音、视频或原 EXE；其他 QLIE 格式和字体布局需要单独验证。

已验证资源样本为《美少女万華鏡 -呪われし伝説の少女-》，按资源特征识别，不依赖游戏名称。

汉化补丁生成独立的 `启动汉化.exe`，临时加载配套字体，并沿用原游戏 EXE 的日语运行配置。需要转区时使用本机 Locale Emulator；保留原 EXE 的 `.exe.le.config`，无需给汉化启动器另设转区。字体只在游戏运行期间加载，不永久安装。

字体的行高度、基线和平均字符宽度按原 MS Gothic／MS Mincho 的比例设置，以适应引擎的正字高及半角宽度请求，避免替换字体后正文整体缩小或被横向压缩。游戏脚本中的字号与文本框尺寸保持原样。

补丁将七个正文字体列表由仅允许等宽字体的 `system_fix` 调整为 `system`，并使用引擎枚举得到的完整 `Regular` 字体名称；独立外字槽保持原样。生成字体的内部完整名称（`name` 表 ID 4）与配置保持一致，兼容 Locale Emulator 对字体名称的本地化处理。启动器会检查内部名称及日语字体枚举结果，避免字体已加载却无法匹配。正文和明朝体分别使用配套的无衬线与衬线字体。

旧版 CMD/PowerShell 启动方案可能绕过原游戏的转区配置，导致已有日文文件或视频被报为缺失。旧补丁还可能因字体列表过滤或名称不匹配回退到原字体，将中文私用码位显示为希腊字母、勾号或方框。

资源回填、中文写入和启动程序结构需通过校验；实际游戏启动、日文资源路径、中文显示、排版和存档兼容性仍需通过试译确认。

相关开源实现：[VNTextPatch-net8 的 QLIE 脚本处理](https://github.com/rafael-vasconcellos/VNTextPatch-net8/blob/e68e174f3db6181e03fc7fabfa7570e1b293d148/VNTextPatch.Shared/Scripts/QlieScript.cs)、[VNTranslationTools 的 SJIS 转换与运行时代理](https://github.com/arcusmaximus/VNTranslationTools#vntextproxy)、[Locale Emulator 的字体名称处理](https://github.com/xupefei/Locale-Emulator-Core/blob/ae7160dc5deb97947396abcd784f9b98b6ee38b3/LocaleEmulator/Hooks/Gdi32Hook.cpp#L301)。当前适配器使用字体映射方案，未集成 VNTextProxy。
