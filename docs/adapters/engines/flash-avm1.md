# Flash AVM1 常量池文本

`flash-avm1-pool` 识别 FWS 和 zlib 压缩的 CWS，从 SWF 8 及以上版本的顶层 DoAction、DoInitAction 和 PlaceObject2 剪辑动作中读取 UTF-8 常量池。适用脚本从常量池开始，后接单层动作。资源名不参与识别。自动适配先检查影片内容，再将带有候选文本的影片交给正式资源验收。探测记录包括候选数、选中数、文本数，以及跳过的原因和路径示例。

文本 ID 由相对资源路径、常量池序号和槽位组成。原文回填保留原文件字节。译文使常量池超过单条指令的 65,535 字节限制时，可以在唯一引用该槽位的 Push 操作数中直接写入译文。分支进入常量池字节的脚本保留原池，在已解析的引用处写入译文。超出有符号 16 位距离的分支使用跳转中继。SWF Metadata 中的 XML 处理指令保留回读这些译文所需的槽位、类型及中继位置。

验收核对译文、未修改槽位、Push 操作数和跳转终点。嵌套影片、函数体、按钮及字形文字、AVM2 和启动程序中的独立文字需要分别分析。游戏加载、中文字体和排版需要实机验证。外部动作继承常量池时，内联的译文仅作用于本动作内识别到的引用。

## 格式参考

- GARbro 可读取 FWS/CWS 的容器和媒体标签，归档处理器声明为只读。[ArcSWF](https://github.com/morkt/GARbro/blob/b09ee4570ccb1daf6ac56710ee8934dc0b8baeb0/ArcFormats/Macromedia/ArcSWF.cs)
- Ruffle 将长动作的长度与 Jump/If 的有符号相对偏移分别解析，定义函数时另读函数体长度。[AVM1 指令读取](https://github.com/ruffle-rs/ruffle/blob/0ddd12b6adf211f556a375e96fca70095de5835b/swf/src/avm1/read.rs)
- JPEXS FFDec 在替换常量池时统计引用，尝试内联单次引用的字符串并重新核算池大小。[常量池写回](https://github.com/jindrapetrik/jpexs-decompiler/blob/8140aefdebee20c799f601923228bcd428feaaed/libsrc/ffdec_lib/src/com/jpexs/decompiler/flash/action/Action.java)
