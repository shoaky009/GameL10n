# VNTextProxy 运行文件准备

`vntextproxy-runtime` 准备固定的 VNTranslationTools 官方 x86 代理及许可。按游戏实际加载的 DLL 名称选择代理，使用前核对同名文件冲突。[固定版本与哈希](../../../adapters/components/vntextproxy_runtime.py)

用 `copy_adapter_components(ids=["vntextproxy-runtime"])` 取得脚本。运行 `describe` 获取归档 URL 和哈希，再用 `check` 或 `prepare` 检查或提取选定 DLL。发布时登记实际使用的 DLL、映射与许可。

`sjis_ext.bin` 放在游戏 EXE 旁，使用 `sjis-tunnel` 为译文及保留原文规划映射。可选字体使用组件支持的私有加载规则，按内部 face 名称和实际覆盖选择。

代理支持相应的 GDI 和 Direct2D 路径。GDI+ 字体创建需独立验证。实际旁加载、字形、排版和转区均通过游戏试译确认。[字体诊断](../components.md#字体与启动器) · [GDI+ 字体桥接](gdiplus-font-bridge.md)
