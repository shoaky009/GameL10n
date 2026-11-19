# Whale DAT v2

`archive.whale-dat` 读取 CRC64 成员索引。压缩脚本卷需要 `gameTitle` 提供原始标题，压缩流和解码长度用于验证。未提供原名的脚本使用哈希作为稳定文件名。`rebuild` 可用完整脚本替换纯脚本卷中的对应成员，保留其他成员的存储字节和顺序。

解包脚本时可生成 `filenames.json`，候选来自对白语音和已知背景、音乐、音效、图层、立绘指令。媒体卷在 `filenameManifest` 中接收此清单或已有的 UTF-8 JSON 文件名数组。读取时核对哈希和成员范围。清单未覆盖的媒体仍计入 `unresolvedCount`，完整内容验证需要解析所有文件名。

`whale-script` 可试用便携 Python 内的 CP932 文本解析，覆盖对白、姓名和已知 SELECT 格式。原游戏内的显示、额外指令、动态资源名和其他 DAT 版本需要分别验证。

[脚本资源命名](https://github.com/mnakamura1337/engine_whale/blob/9fe63a4b75baf9cdbfe73aee73b6720cdbc52c96/whale2storyconverter.rb) · [DAT v2 结构](https://github.com/vn-tools/arc_unpacker/blob/456834ecf2e5686813802c37efd829310485c57d/src/dec/whale/dat_archive_decoder.cc)
