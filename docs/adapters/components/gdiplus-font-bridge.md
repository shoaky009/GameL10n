# GDI+ 字体桥接

`gdiplus-font-bridge` 为已确认的 Windows x86 GDI+ 字体创建路径提供私有字体集合。先用 `windows-font-metrics` 的 `renderer:"gdiplus"` 确认原入口失败而私有集合成功，再选择实际调用的构造入口。

## 接入

```text
copy_adapter_components(ids=["gdiplus-font-bridge"])
run_engine_python(script="src/tools/gdiplus_font_bridge.py", stdin='{"operation":"inspect","input":"resources/Game.exe"}')
```

`inspect` 返回源文件哈希和普通导入中的候选入口。`prepare` 接受：

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

适配器也可导入 `prepare(request)`，在独立输出子目录生成文件后按返回清单组装补丁。输出包含原名程序、`GameL10n.FontBridge.dll`、UTF-16LE 的 INI、字体和 MIT 许可；字体自身的许可须另行保存。已有相同输出复用，不同内容报冲突。

`functions` 只支持 `GdipCreateFontFromDC`、`GdipCreateFontFromLogfontA`、`GdipCreateFontFromLogfontW`。只选实际确认的调用，不默认全选。

若程序也普通导入 `GdipGetFamily`，会自动接入配套的家族查询处理：桥接创建的私有字体返回可由调用方释放的家族引用，避免正常查询、释放后影响后续创建。系统字体查询保持原行为。

## 匹配与字号

`fontFace` 是字体内部家族名，可用 `matchFace` 指定引擎请求的另一个名字。匹配忽略大小写，不做模糊或全局替换。

- `fallback`：先调用原入口；仅在匹配字体且返回 14／16 时使用私有集合。
- `replace-matching`：明确替换匹配字体；其他字体继续调用原入口。

私有集合在进程内延迟初始化，并保持独立的 GDI+ 生命周期。ANSI 字体名沿用当前区域设置；不会安装系统字体或自动转区。字号使用实际 GDI 的 `tmHeight - tmInternalLeading`，保留正、负和零高度的含义。原有 GDI 字体注册及文本解码仍由引擎或其他组件提供。

## 边界与验证

导入重定向保留原 IAT 地址、代码、原节字节和重定位，只追加导入数据节并更新 PE 头。不修改调用指令，不新增启动入口。

不支持 x64、动态／延迟导入、签名或 CLR 程序、缺少节头空间的 PE。桥接不处理非零字宽、旋转、特殊 HDC 坐标变换；这些情况返回明确状态，不能当作已兼容。

`prepare` 始终返回 `runtimeTested:false`。结构验证及独立 GDI+ 绘制不代表原游戏显示、布局、换行或存档已验证；通过少量试译确认。

## 构建

```powershell
rustup target add i686-pc-windows-msvc
./scripts/build-fontbridge.ps1 -Test
./target/runtime/python/python.exe -I -X utf8 ./crates/engines/python/test_gdiplus_font_bridge.py
```

组件源码位于 `tools/GameL10n.FontBridge`，为独立的 Rust `cdylib`；需 Windows SDK、MSVC 链接器及上述 Rust 目标。默认 Release，`-Configuration Debug` 用于调试。DLL 使用静态 CRT，输出到 `target/runtime/adapter-components`。Rust 检查程序验证原生构造、绘制、错误状态，Python 验证重定向后的 Windows 加载行为，不启动游戏。
