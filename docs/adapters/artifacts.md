# 适配器与补丁产物

项目数据目录下的 `games/<项目 ID>/` 保存工作区与补丁。共享适配器位于 `extensions/packages/`。在“程序设置 → 打开数据目录”查看实际位置。[数据目录与迁移](../data-storage.md)

## 可续接的开发资料

`games/<项目 ID>/adaptation/current/` 保存 `src/` 中的适配器源码、`work/` 中的脚本和报告，以及会话记录。原资源按路径和哈希引用，旧项目的资源副本仍可作为输入。

原文件引用由 `stage_resources` 登记，分析脚本用 `resource_paths[相对路径]` 读取。实验结果写到 `work/`，正式依赖放在 `src/tools/`。清理开发资料会失去该次会话和证据。[清理影响](../data-storage.md#项目完工后清理)

## 正式适配器

发布包位于 `extensions/packages/<适配器 ID>/<版本>/`。Manifest v4 使用 CLI v1。宿主从 `family` 的 `kind` 与 `key` 生成 `<kind>.<key>` 标识，按发布内容固定版本。

`package.json` 中的 `releaseFiles` 逐个登记包内路径、角色和用途，`packageFiles` 记录对应文件的 SHA-256。清单使用包内具体相对文件路径。验收和发布使用同一份文件选择。

| 角色 | 发布内容 |
| --- | --- |
| `source` | `src/adapter.py` 及正式 Python 模块 |
| `runtime` | `tools/` 中的运行依赖及固定库声明 `engine-reference.json` |
| `asset` | 制作补丁所需的字体、启动器或其他素材 |
| `license` | 许可、来源与所需的源码声明 |

工作区 `src/tools/` 发布到包内 `tools/`。选入全部间接依赖及适用许可。包只发布通用实现和必要运行素材，游戏原件与修改后的游戏文件分别作为资源输入和补丁输出。

`verification.json` 记录资源验收结果，可选的 `capability-profile.json` 提供复用与复验资料。以新版本发布修改后的源码或依赖，已有项目保留其固定版本。固定的 VNTextPatch 须与当前发行版匹配，使用旧工具的包需重新识别和验收。[发布实现](../../crates/engines/src/adaptation_verification.rs)

引擎库中的“验证发布包…”可用空闲项目的原文和提取基线单独验收某个版本。结果可在版本详情或项目任务中查看。[引擎库管理](README.md#引擎库管理)

## 汉化运行文件与补丁记录

补丁位于 `games/<项目 ID>/patches/`。“打开补丁文件”直接打开可部署的 `files/`。手动安装时先备份，再将其中的内容按原层次复制到游戏目录。游戏只需这些运行文件。

清单和记录保存在 `files/` 外，包括生成时的译文快照、运行反馈及结构校验失败时的完整 `*.validation.json`。通过程序安装可用“恢复原版”管理其备份。[安装和恢复](../data-storage.md#安装和恢复原版) · [补丁运行的风险](execution-isolation.md#补丁运行的风险)

## 共享下载缓存

`extensions/downloads/` 保存可跨项目复用的公开资料和工具，按来源与内容哈希核对。删除缓存后，已复制到工作区或发布包的依赖仍可使用。更新依赖后需重新验收。[下载实现](../../crates/engines/src/research.rs)
