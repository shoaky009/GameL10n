# 开发与构建

项目使用 C# / .NET 10 和 WinUI 3。构建需要 .NET SDK 10.0.402、Windows SDK 和 WinUI 构建依赖。

```powershell
./scripts/dotnet.ps1 restore AvgTranslator.sln
./scripts/dotnet.ps1 build AvgTranslator.sln --no-restore
./scripts/dotnet.ps1 test tests/AvgTranslator.Tests --no-restore
./scripts/publish.ps1
```

`scripts/dotnet.ps1` 支持系统 SDK，也支持项目下 `.tools/dotnet` 中的独立 SDK。`scripts/publish.ps1` 输出 `artifacts/AvgTranslator`，包含桌面应用、运行时、文档和自动适配所需的工具链。发行时保留完整目录，单独复制 EXE 不足以运行桌面工具。

工具链打包来源为 `.tools/dotnet`，使用 SDK 10.0.402 与运行时 10.0.13。使用 `-WithoutToolchain` 可省略自动适配工具链；已支持游戏的翻译不需要它。

内置 VNTextPatch 脚本工具随发行包保存在 `tools/vntextpatch`，包含独立 EXE 和必要运行库，不需要安装 SDK。原生运行库直接随包提供，避免每次处理脚本都解压运行时。其上游源码固定版本并随仓库保存，构建时不从 GitHub 动态下载；具体脚本格式与运行限制见[内置脚本适配说明](adapters/vntextpatch.md)。

同目录的小型 `AvgTranslator.VnTextPatch.dll` 与 `sources.zip` 用于智能适配复用，发行脚本通过 `scripts/package-vntextpatch-assets.ps1` 生成。源码统一压缩保存，默认开发任务仅复制解析库和调用代码；需要修改时才展开所选引擎的源码。仅执行桌面项目的 `dotnet build` 不会生成这些发行资产，完整功能验证应使用发行目录。

[编码与运行组件](adapters/components.md)的 C# 源码嵌入 Core；预编译 QLIE 启动器由 `scripts/package-adapter-components.ps1` 生成到 `tools/adapter-components`，发行脚本自动调用。该步骤使用固定 SDK 编译器及 Windows .NET Framework 4.x 程序集，启动器自身不依赖 .NET 10。组件打包集成测试使用 PowerShell 7。

## 代码结构

| 目录 | 内容 |
| --- | --- |
| `src/AvgTranslator.Core` | 适配器、翻译、任务恢复、模型接口、补丁安装 |
| `src/AvgTranslator.Desktop` | WinUI 3 桌面界面 |
| `tests/AvgTranslator.Tests` | 格式、缓存、恢复、安装及接口测试 |
| `tools/AvgTranslator.Diagnostics` | 游戏资源验证与本地工具链诊断 |
| `tools/AvgTranslator.DataTool` | 游戏数据迁移、校验与缓存加载统计 |
| `tools/AvgTranslator.VnTextPatch` | 内置 VNTextPatch 脚本解析与回填工具 |
| `third_party/VNTextPatch` | 固定版本的上游脚本解析源码与许可证 |
| `contracts` | 扩展通信协议 |

## 资源验证

```powershell
./scripts/dotnet.ps1 run --project tools/AvgTranslator.Diagnostics -- verify-darkblue '<游戏目录>' target/verification
./scripts/dotnet.ps1 run --project tools/AvgTranslator.Diagnostics -- toolchain .tools/dotnet target/toolchain-verification
```

诊断产物用于结构、编码和本地编译执行验证，不是完整汉化。

游戏资源、测试产物、SDK 和包缓存位于忽略目录中，不随源码或桌面发行包提供。
