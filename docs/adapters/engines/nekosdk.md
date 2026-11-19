# NekoSDK NEKOPACK4A 与 ADVSCRIPT2

`NEKOPACK4A` 标识封包，`NEKOSDK_ADVSCRIPT2` 标识解出的二进制脚本。脚本通常使用 `.txt` 后缀。`archive.nekopack4a` 先读索引，提取时按成员读取和解压。首次智能适配会自动试用被选中的完整 ADVSCRIPT2 成员。

`nekosdk-script` 按记录读取显示指令及姓名、正文等长度前缀字段。显示指令可以附带参数。回填按原记录和字段定位，保留命令参数、语音引用、控制记录和未修改字节。固定工具的其他格式可从[封包组件](../templates.md#封包组件)进入。

`nekopack4a-advscript2` 组合模板适用于 `.txt` 成员都是 ADVSCRIPT2 的脚本卷。探测只读索引，选择含脚本的卷。回填保留成员顺序并核对未改内容，原文往返保留整包字节。组合模板和跨项目自动匹配遇到不同目录的同名脚本卷时，会要求按当前启动程序确认范围。跨卷同名脚本的加载顺序也需要按游戏确认。

文本按 CP932 严格读写。选择项、非脚本界面文字和简体中文运行需按游戏单独验证。支持 `A` 型封包，其他 NEKOPACK 变体需分别确认。

[ADVSCRIPT2 格式说明](https://github.com/mnakamura1337/engine_nekosdk/blob/fed00a7ea4c1ea36f5499c083fb448b4d505f495/nekosdk_advscript2.ksy) · [显示指令参考](https://github.com/mnakamura1337/engine_nekosdk/blob/fed00a7ea4c1ea36f5499c083fb448b4d505f495/nekosdk2storyconverter.rb)
