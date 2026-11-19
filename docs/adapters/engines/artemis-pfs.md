# Artemis PFS v8 与 AST

组合模板 `artemis-pfs8-ast` 提取 UTF-8 AST 2.0 的日文对白、姓名、选项和标题。识别先检查 PFS 主包和数字卷的索引，仅选择含 AST 的卷。不同卷中有同路径脚本时，需要先确认加载优先级。ASB、Lua、其他语言和字体另行处理。

回填以 `replace` 重建所选卷，保留成员名、顺序、索引附加数据和未改成员的解码字节。AST 只替换显示字符串，保留命令、注释和语音引用。文本中的 `{AST:n}` 标记原脚本中字符串之间的命令边界，译文须保留其顺序。生成的适配器沿用固定 Python 工具库，游戏内加载、中文字体与排版通过试译确认。[组合模板](../templates.md) · [验证与产物](../automatic-adaptation.md#验证与产物)

## 数字覆盖卷

部分 Artemis 运行时支持主包与 `.pfs.NNN` 数字卷，每卷为完整 PFS，后加载卷可覆盖同路径成员。确认游戏实际的挂载顺序后，适配器可选用 `overlay` 输出修改成员和必要字体。现有重建适配器须重新验收后才能采用覆盖卷。[覆盖构建协议](../protocol.md#构建与校验) · [覆盖卷实现参考](../../../crates/engines/references/Artemis_pfs_overlay.md)
