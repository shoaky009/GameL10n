# 开发与构建

## 环境

主工程使用 Rust 2024 edition，最低 Rust 1.92。Windows 构建需要 MSVC C++ 链接工具和 Windows SDK。Slint 固定为 1.18.1，使用 winit 后端和软件渲染，不引入 WebView 或 .NET 桌面运行时。

```powershell
cargo check --workspace --locked
cargo test --workspace --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo fmt --all -- --check
```

只构建无 UI 核心：

```powershell
cargo build -p gamel10n-core --locked
cargo tree -p gamel10n-core
```

Core 依赖树不得出现 Slint。`cargo build` 默认构建 Core 和 CLI，不强制构建桌面模块。完整目录包使用 `scripts/publish.ps1`，输出 `artifacts/GameL10n`，不生成 ZIP。

## 工具

默认从可执行文件旁的 `tools/` 查找工具。源码运行时显式传入 `--tools-directory ./tools`。

```text
tools/
  vntextpatch/GameL10n.VnTextPatch.exe
  vntextpatch/VNTextPatch.LICENSE
  vntextpatch/VNTextPatch.NOTICE.md
  python/python.exe
  python/python314.zip
  python/…
```

便携 Python 使用 Windows x64 CPython 3.14.10 嵌入式发行包，放在 `tools/python/`，保留原始标准库、DLL 和许可证。`scripts/prepare-python.ps1` 下载或读取本地归档，核对固定 SHA-256 后准备工具。Python 与 AOT 二进制是本地工具产物，不提交 Git。

VNTextPatch 源码位于 `third_party/VNTextPatch/`，JSON 工具入口位于 `tools/GameL10n.VnTextPatch/`。可通过 `scripts/build-vntextpatch.ps1` 重建 NativeAOT；只有该构建步骤需要 .NET 10 SDK、MSVC 和 Windows SDK，用户运行 Rust 桌面包不需要外置 .NET。29 个格式注册不代表全部格式或游戏版本都经过真实资源回归。

## 可用功能

- 读取 v1 项目索引、v2 项目元数据，保留额外字段；登记和搜索游戏项目。
- Slint 项目列表、后台状态、底部滚动日志、提取结果分页检索和项目文件夹入口。
- 显式选择 VNTextPatch 格式处理已解包脚本；保存原文快照并验证原文字节回填。
- 已提交提取结果落盘并缓存，切换项目不自动重新提取。
- 只读加载旧版分资源提取缓存，核对清单、资源哈希、文本身份和顺序；不要求旧引擎在 Rust 中重新执行。
- CLI 回填指定译文，重新解析并验证输出，补丁写入项目目录。
- Rust 通过 JSON CLI 执行 AOT 或明确指定的 Python 脚本；超时、取消、退出码和无效响应处理。

尚未接入：模型 API 与翻译执行、旧版译文缓存和校对界面、完整智能适配会话、引擎库及包管理、字体和启动组件的 Rust 实现、补丁安装和原版恢复。复制保留的引擎、Provider 和智能适配文档描述完整产品契约，不能视为这些模块已经可用。

## 无界面入口

```powershell
cargo run -p gamel10n-core -- --tools-directory ./tools --data-directory ./target/data add 'D:\Games\Example'
cargo run -p gamel10n-core -- --data-directory ./target/data projects
cargo run -p gamel10n-core -- --tools-directory ./tools --data-directory ./target/data extract '<项目ID>' Renpy
cargo run -p gamel10n-core -- --data-directory ./target/data texts '<项目ID>' --query '原文' --offset 0
cargo run -p gamel10n-core -- --tools-directory ./tools --data-directory ./target/data build '<项目ID>' ./translations.json
```

`translations.json` 是文本 ID 到译文的 JSON 字典，未提供的文本保留原文。`build` 只生成补丁，不安装或修改游戏目录。普通 `.assets` 和封包游戏需要对应适配器，不能直接交给任意脚本解析器。

独立协议检查：

```powershell
cargo run -p gamel10n-core -- formats
cargo run -p gamel10n-core -- --tools-directory ./tools tool ./request.json
cargo run -p gamel10n-core -- --tools-directory ./tools tool ./request.json --python-adapter ./adapter.py
```

工具入口从 stdout 读取 JSON，请求不使用 `responseFile`。Python 引导会添加入口目录及其父目录到模块搜索路径，兼容并列 `src/` 和 `tools/` 的导入方式，不要求系统 Python 或 pip。

临时样本和调查脚本放在忽略的 `target/` 中。测试优先覆盖身份兼容、元数据保留、缓存恢复、进程协议和实际回填，不追求覆盖率数字。

只读兼容诊断使用 `audit` 子命令。它检查索引中的项目、已提交提取数量和重复分页的一致性，输出首次加载与缓存读取耗时，不调用解析器或模型，不写回项目目录。

```powershell
cargo run -p gamel10n-core -- --data-directory '<已有数据根目录>' audit
```

`adapters/` 同时保存可复用 Python 代码和格式维护参考；其中的 C# 专用实现不编入 Rust 宿主。复制的参考源码或引擎说明不等于该引擎已经在新运行时中完成接入。
