# Valkyria DAT / SDT

固定参考库提供 DAT 容器、SDT 候选文本与定长回写。完整语义提取、变长重定位及运行支持由智能适配补齐，不能把参考解析通过视为整款游戏兼容。

## 文本与跳转

追加式回填须依据实际指令核对跳转基准、返回位置和原入口。从输出的真实跳转目标解析正文、姓名和选项，与预期译文比较；辅助索引中的偏移必须指向这些位置。仅按生成索引读到译文，无法证明引擎会执行该段内容。

原文直接复制通过只说明未修改分支保留了原文件。修改分支仍需验证指令边界、目标和返回，实际执行、选项及存档兼容性通过试译确认。

## 字体创建

先检查当前 EXE 的字体创建、测量与绘制调用，不能按厂商推断渲染后端。使用 GDI+ 的变体需调用 [windows-font-metrics](../components.md#字体与启动器)，设置 `renderer:"gdiplus"`。

[VNTextProxy v0.0.41](../components/vntextproxy-runtime.md) 不提供 GDI+ 字体钩子。若候选字体在 GDI 可用、`GdipCreateFontFromDC` / `GdipCreateFontFromLogfontA/W` 失败而私有集合成功，需让实际创建路径使用该集合并维持生命周期。更换 OTF/TTF 或调整 GDI 字号不能代替这一步。

普通 x86 `GdipCreateFontFromDC` / `GdipCreateFontFromLogfontA/W` 可优先复用 [gdiplus-font-bridge](../components/gdiplus-font-bridge.md)。它只重定向显式选中的导入，保留原程序名、IAT 地址、代码和 ASLR；不依赖游戏地址或生成机器码。动态加载、延迟导入及特殊字体变换不在组件范围，需单独调查。

独立字体创建成功仍需游戏内确认显示和布局，不能据此认定整个 SDT 运行路径已通过。
