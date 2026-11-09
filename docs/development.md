# 开发与构建

## 开发环境

- Rust 2024 edition，最低 Rust 1.92。
- Slint 1.18.1。
- Windows MSVC C++ 工具和 Windows SDK。
- 构建固定 NativeAOT 工具需 .NET 10 SDK；Python 使用项目准备的便携版本。

## 构建与打包

```powershell
cargo check --workspace --locked
cargo build --workspace --locked
cargo test --workspace --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo fmt --all -- --check
./scripts/publish.ps1
```

| 配置 | 命令 | 输出 |
| --- | --- | --- |
| Release | `./scripts/publish.ps1` | `artifacts/GameL10n` |
| Debug | `./scripts/publish.ps1 -Configuration Debug` | `artifacts/GameL10n-debug`，含诊断 CLI |

打包前先完成工具准备。发行目录包含应用、Python、解析工具、适配组件和许可证，使用时保留完整目录。本地打包不生成 ZIP；重复打包会同步 `tools/` 与 `adapters/`，输出目录不要存放个人文件或应用数据。

分发二进制时，按 GPLv3 提供对应源码与构建脚本，保留 `LICENSE`、`THIRD-PARTY-NOTICES.md`、`DISCLAIMER.md` 及第三方声明。打包脚本将这三个根目录文件一并复制到发行目录。[免责声明](../DISCLAIMER.md)说明非官方属性、游戏内容的授权边界与版权投诉方式，不替代第三方授权。

`docs/assets/logo.png` 用于 README；`crates/desktop/assets/icon.png` 是程序图标原图，替换时同步生成同目录的 `GameL10n.ico`。界面与图标资源嵌入应用。

## 工具准备

首次打包依次执行：

```powershell
rustup target add i686-pc-windows-msvc
./scripts/prepare-python.ps1
./scripts/build-vntextpatch.ps1
./target/runtime/python/python.exe -I -X utf8 ./scripts/build_qlie_launcher.py
./scripts/build-fontbridge.ps1
```

| 工具 | 位置与维护方式 |
| --- | --- |
| 便携 Python | `target/runtime/python`；准备脚本支持 `-ArchivePath` 使用固定版本本地归档 |
| VNTextPatch NativeAOT | `target/runtime/vntextpatch`；由仓库内源码构建，附带源码参考包 |
| 共享引擎工具库 | `third_party/engine-tools` 保存版本与固定归档，准备后位于 `target/runtime/engine-tools` |
| 预编译运行组件 | `target/runtime/adapter-components`；[QLIE 启动器](adapters/engines/qlie-filepack3-systemfont.md#启动组件接入与构建)、[GDI+ 字体桥接](adapters/components/gdiplus-font-bridge.md) |

`prepare-python.ps1` 和 `publish.ps1` 自动准备固定归档及宿主脚本；开发时可单独运行 `scripts/prepare-runtime.ps1` 刷新这些文件。`publish.ps1` 将已准备的运行依赖打包到发行目录的 `tools/`。版本固定与恢复规则见[固定工具依赖](adapters/adaptation-internals.md#固定工具依赖)。

## GitHub 发布

[发布流程](../.github/workflows/release.yml)仅在推送 `vX.Y.Z` tag 时触发，例如 `v1.0.0`。

将 [CHANGELOG.md](../CHANGELOG.md) 的待发布内容整理为 `## [1.0.0] - YYYY-MM-DD`，填入实际日期，顶部保留空的 `## [Unreleased]`。提交后推送：

```powershell
git add CHANGELOG.md
git commit -m "chore: release v1.0.0"
git push origin main
git tag v1.0.0
git push origin v1.0.0
```

Actions 使用内置令牌构建 Release，上传 `GameL10n-v1.0.0-win-x64.zip`，发布说明读取更新日志对应版本。版本记录缺失或为空时，在构建前报错。

## 资源静态验证

```powershell
cargo run -p gamel10n-core -- tool ./request.json
cargo run -p gamel10n-core -- --data-directory <数据目录> audit
```

静态验证检查资源和工具接口，游戏启动、显示和存档需单独实测。

## 开发主题索引

[架构](architecture.md) · [桌面交互](desktop.md) · [模型接口](model-providers.md) · [数据存储](data-storage.md) · [适配器](adapters/README.md) · [扩展协议](adapters/protocol.md)
