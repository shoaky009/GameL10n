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

## 代码结构

| 目录 | 内容 |
| --- | --- |
| `src/AvgTranslator.Core` | 适配器、翻译、任务恢复、模型接口、补丁安装 |
| `src/AvgTranslator.Desktop` | WinUI 3 桌面界面 |
| `tests/AvgTranslator.Tests` | 格式、缓存、恢复、安装及接口测试 |
| `tools/AvgTranslator.Diagnostics` | 只读游戏资源验证与隔离诊断 |
| `contracts` | 扩展通信协议 |

## 资源验证

```powershell
./scripts/dotnet.ps1 run --project tools/AvgTranslator.Diagnostics -- verify-darkblue '<游戏目录>' target/verification
./scripts/dotnet.ps1 run --project tools/AvgTranslator.Diagnostics -- sandbox .tools/dotnet target/sandbox-verification
```

诊断产物只用于结构和编码测试，不是完整汉化。操作系统隔离必须用原生诊断验证，模拟执行器测试不能证明沙箱生效。

游戏资源、测试产物、SDK 和包缓存位于忽略目录中，不随源码或桌面发行包提供。
