# GameL10n

<p align="center"><img src="logo.png" alt="GameL10n" width="280" /></p>

游戏文本本地化工具，使用 Rust 主宿主、Python 解析脚本和可替换的 Slint 桌面界面。翻译、引擎能力和项目管理分别独立；Core 不依赖任何 UI 框架。

当前可用：游戏项目登记与检索、旧版提取结果只读浏览、已解包脚本的 VNTextPatch 原生工具提取、原文字节回填检查、提取缓存、文本分页检索、通过 CLI 回填指定译文，以及 Python JSON CLI 调用。LLM 翻译、完整智能适配、校对编辑和补丁安装尚未接入。

## 快速开始

需要 Rust 1.92 或更高版本，以及 Windows C++ 链接工具。工具目录包含 `tools/vntextpatch/GameL10n.VnTextPatch.exe` 和 `tools/python/python.exe`。VNTextPatch 使用 NativeAOT，不依赖外置 .NET 运行时；从源码重建该工具另需 .NET SDK 和 NativeAOT 构建工具。

```powershell
cargo run -p gamel10n-desktop -- --tools-directory ./tools
```

在左侧添加游戏目录，选择脚本格式并点击“提取脚本文本”。这里只处理已经解包的脚本；各格式的封包、编码和中文显示条件见[脚本格式说明](docs/adapters/engines/vntextpatch.md)。打开已有项目只读取保存结果，不自动重新提取。

需要单独的数据目录时，加上 `--data-directory ./target/data`。无界面入口：

```powershell
cargo run -p gamel10n-core -- --help
cargo run -p gamel10n-core -- formats
```

## 文档

- [文档索引](docs/README.md)
- [架构与模块边界](docs/architecture.md)
- [开发、构建与可用功能](docs/development.md)
- [数据格式与兼容边界](docs/data-storage.md)
- [引擎与脚本格式](docs/adapters/README.md)
- [第三方许可证](THIRD-PARTY-NOTICES.md)
