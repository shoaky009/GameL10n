# GDI+ 字体桥接

适用于 Windows x86 游戏通过普通导入调用指定 GDI+ 构造函数的情况。先用字体探针确认实际构造入口，并比较原路径与私有字体集合的结果。[字体诊断](../components.md#字体与启动器)

## 接入

用 `copy_adapter_components(ids=["gdiplus-font-bridge"])` 复制组件。向 `gdiplus_font_bridge.py` 发送 `inspect` 请求得到候选入口和原 EXE 哈希，再按实际选择调用 `prepare`。

```json
{
  "operation": "prepare",
  "input": "resources/Game.exe",
  "expectedSha256": "inspect 返回的 sha256",
  "relativePath": "Game.exe",
  "functions": ["GdipCreateFontFromDC"],
  "fontFile": "src/tools/candidate.ttf",
  "fontFace": "Candidate Family",
  "mode": "fallback",
  "outputDirectory": "work/font-bridge"
}
```

将返回清单中的原名 EXE、桥接 DLL、配置、字体和许可一同纳入补丁。字体需附适用许可。`fontFace` 为内部家族名，可用 `matchFace` 指定引擎请求名。

`fallback` 仅在匹配字体遇到受支持的失败状态时使用私有集合。`replace-matching` 直接为匹配字体使用该集合。支持的入口、PE 条件与配置字段见源码。[组件实现](../../../adapters/components/gdiplus_font_bridge.py)

动态或延迟导入、x64 和特殊字体变换需要其他方案。生成后仍须在原游戏中试译检查。[Windows 桥接实现](../../../tools/GameL10n.FontBridge)

## 构建

需要 MSVC、Windows SDK 和 `i686-pc-windows-msvc` Rust 目标。运行 `./scripts/build-fontbridge.ps1 -Test`，产物在 `target/runtime/adapter-components`。[工具准备](../../development.md#工具准备)
