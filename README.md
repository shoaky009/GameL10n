# GameL10n

<p align="center">
  <img src="docs/assets/logo.png" alt="GameL10n logo" width="360" />
</p>

GameL10n 是面向游戏文本汉化的 Windows 桌面工具，提供从资源识别、文本提取到 AI 翻译、译文校对和汉化补丁生成的完整流程。

## 主要功能

- **识别与智能适配**：复用已有引擎适配器，通过 AI 分析未知资源格式并开发新适配器。
- **翻译与校对**：批量翻译、选段试译、译文编辑、术语管理、译文复用与断点续译。
- **补丁制作**：生成、安装汉化补丁，并记录游戏内的验证结果。
- **多游戏管理**：独立管理多个游戏项目，复用适配器和同类引擎经验。
- **模型配置**：支持远程 API 和本地翻译模型，分析与翻译可分别配置。

## 快速开始

1. 从 [GitHub Releases](https://github.com/shoaky009/GameL10n/releases) 下载并解压，运行 `GameL10n.exe`，在“API 与任务默认值”中配置模型服务。
2. 点击“添加游戏”选择安装目录，在“识别与适配”中提取文本。
3. 在“翻译与校对”中选段试译或批量翻译，并编辑、校对译文。
4. 生成补丁，在“补丁与验证”中安装并进入游戏确认效果。

## 文档

- [支持的引擎与资源格式](docs/adapters/README.md)
- [模型接口配置](docs/model-providers.md)
- [更新日志](CHANGELOG.md)
- [开发与构建](docs/development.md)
- [全部文档](docs/README.md)

## 许可

项目采用 [GPL-3.0-only](LICENSE)。第三方组件许可见[第三方许可证](THIRD-PARTY-NOTICES.md)。
