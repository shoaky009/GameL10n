# 开发与构建

## 开发环境

- Rust 2024 edition，最低 Rust 1.92。
- Slint 1.18.1。
- Windows MSVC C++ 工具和 Windows SDK。
- 构建 VNTextPatch NativeAOT 工具需 .NET 10 SDK；QLIE 启动器和字体桥接使用 Rust，Python 使用项目准备的便携版本。

## 构建与打包

```powershell
cargo check --workspace --locked
cargo build --workspace --locked
cargo test --workspace --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo fmt --all -- --check
./scripts/publish.ps1
```

独立 Rust 工具使用各自的 Cargo 清单运行检查，例如：

```powershell
cargo test --manifest-path tools/GameL10n.QlieLauncher/Cargo.toml --locked
```

[PR 检查](../.github/workflows/ci.yml)在 Windows 上执行格式、静态检查和回归测试；tag 发布由独立发布流程处理。

| 配置 | 命令 | 输出 |
| --- | --- | --- |
| Release | `./scripts/publish.ps1` | `artifacts/GameL10n` |
| Debug | `./scripts/publish.ps1 -Configuration Debug` | `artifacts/GameL10n-debug`，含诊断 CLI |

`publish.ps1` 自动准备并构建全部运行工具，首次构建或 `cargo clean` 后可直接执行。发行目录包含应用、Python、解析工具、适配组件和许可证，使用时保留完整目录。本地打包不生成 ZIP；重复打包会同步 `tools/` 与 `adapters/`，输出目录不要存放个人文件或应用数据。

分发二进制时，按 GPLv3 提供对应源码与构建脚本，保留 `LICENSE`、`THIRD-PARTY-NOTICES.md`、`DISCLAIMER.md` 及第三方声明。打包脚本将这三个根目录文件一并复制到发行目录。[免责声明](../DISCLAIMER.md)说明非官方属性、游戏内容的授权边界与版权投诉方式，不替代第三方授权。

`docs/assets/logo.png` 用于 README；`crates/desktop/assets/icon.png` 是程序图标原图，替换时同步生成同目录的 `GameL10n.ico`。界面与图标资源嵌入应用。

## 工具准备

开发时可单独准备全部运行工具：

```powershell
./scripts/prepare-tools.ps1
```

准备脚本恢复便携 Python 和固定归档，安装缺失的 Windows Rust 目标，并依次构建 VNTextPatch、QLIE 启动器、字体桥接和运行检查组件。重复执行复用编译缓存。MSVC 工具和 .NET SDK 需预先安装；SDK 自动从 `DOTNET_ROOT_X64`、`DOTNET_ROOT`、PATH、用户目录的 `.dotnet` 和系统安装目录中查找，跳过低于 .NET 10 的版本。可通过 `./scripts/publish.ps1 -Dotnet 'C:/path/to/dotnet.exe'` 或 `prepare-tools.ps1`、`build-vntextpatch.ps1` 的同名参数指定 SDK。仅恢复 Python 时，`prepare-python.ps1 -ArchivePath` 支持使用固定版本的本地归档。

| 工具 | 位置与维护方式 |
| --- | --- |
| 便携 Python | `target/runtime/python`；准备脚本支持 `-ArchivePath` 使用固定版本本地归档 |
| VNTextPatch NativeAOT | `target/runtime/vntextpatch`；由仓库内源码构建，附带源码参考包 |
| 共享引擎工具库 | `third_party/engine-tools` 保存版本与固定归档，准备后位于 `target/runtime/engine-tools` |
| 预编译运行组件 | `target/runtime/adapter-components`；[QLIE 启动器](adapters/engines/qlie-filepack3-systemfont.md#启动组件接入与构建)、[GDI+ 字体桥接](adapters/components/gdiplus-font-bridge.md) |
| 游戏运行检查 | `target/runtime/game-check/GameL10n.GameCheck.exe`；独立 Rust 工具，依赖 Windows 捕获与 OCR 能力 |

`prepare-python.ps1`、`prepare-tools.ps1` 和 `publish.ps1` 自动准备固定归档及宿主脚本；开发时可单独运行 `scripts/prepare-runtime.ps1` 刷新这些文件。`publish.ps1` 将运行依赖打包到发行目录的 `tools/`。版本固定与恢复规则见[固定工具依赖](adapters/adaptation-internals.md#固定工具依赖)。

运行检查组件的原生窗口与 OCR 测试需要交互式 Windows 桌面及英语 OCR 语言包：先构建 `game_fixture` 示例，将示例 EXE 路径设为 `GAMEL10N_GAME_CHECK_FIXTURE`，再运行该工具的 `runtime` 集成测试并指定 `--ignored --test-threads=1`。测试窗口会自动清理。Core 的进程协议测试使用 `check_driver` 示例，由 `GAMEL10N_GAME_CHECK_DRIVER` 指定其路径。

## GitHub 发布

[发布流程](../.github/workflows/release.yml)仅在推送 `vX.Y.Z` tag 时触发，例如 `v1.0.0`。

将 [CHANGELOG.md](../CHANGELOG.md) 的待发布内容整理为 `## [1.0.0]`，顶部保留空的 `## [Unreleased]`。提交后推送：

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
