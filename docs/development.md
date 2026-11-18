# 开发与构建

使用 Windows、Rust 1.92 或更新版本、MSVC C++ 工具和 Windows SDK。构建 VNTextPatch NativeAOT 还需要 .NET 10 SDK。便携 Python 由项目脚本准备。

## 构建与打包

```powershell
./scripts/publish.ps1
```

打包脚本准备运行工具并生成 `artifacts/GameL10n`。用 `-Configuration Debug` 生成含诊断 CLI 的 `artifacts/GameL10n-debug`。重复打包会刷新运行依赖，个人文件或应用数据应放在发行目录之外。

分发时保留完整程序目录、许可证和第三方声明，并按 GPLv3 提供对应源码与构建脚本。[许可声明](../THIRD-PARTY-NOTICES.md) · [免责声明](../DISCLAIMER.md)

## 工具准备

```powershell
./scripts/prepare-tools.ps1
```

开发工具保存在 `target/runtime/`，发布后位于程序目录的 `tools/`。找不到 .NET SDK 时可通过 `-Dotnet` 指定 `dotnet.exe`。已有固定版本的本地 Python 归档可传给 `prepare-python.ps1 -ArchivePath`。[工具准备脚本](../scripts/prepare-tools.ps1)

## 检查

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --locked
```

独立原生工具使用对应的 Cargo 清单。窗口捕获与 OCR 的实机测试需要交互式 Windows 桌面、OCR 语言包及样例游戏。[PR 检查](../.github/workflows/ci.yml) · [运行检查测试](../tools/GameL10n.GameCheck/tests/runtime.rs)

### 资源静态验证

```powershell
cargo run -p gamel10n-core -- tool ./request.json
cargo run -p gamel10n-core -- --data-directory 'D:\GameL10n Data' audit
```

`tool` 执行适配器协议请求，`audit` 只读核对已有项目与提取缓存。更多命令见 `cargo run -p gamel10n-core -- --help`。

## GitHub 发布

在 [CHANGELOG.md](../CHANGELOG.md) 整理 `[X.Y.Z]` 的变更条目。提交后推送 `vX.Y.Z` 发布正式版，推送 `vX.Y.Z-rcN` 发布候选版。发布流程将变更条目和 Windows x64 ZIP 放到 GitHub Releases。[发布流程](../.github/workflows/release.yml)
