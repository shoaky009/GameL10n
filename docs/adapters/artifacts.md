# 引擎分析产物与目录约定

以下路径相对应用数据目录，目录按需创建。保存位置见[数据说明](../data-storage.md#保存位置)。

## 可续接的开发资料

项目工作区为 `games/<项目 ID>/adaptation/current/`：

| 路径 | 用途 | 是否发布 |
| --- | --- | --- |
| `src/adapter.py`、`src/**/*.py` | 正式适配源码 | 按清单发布 |
| `src/tools/` | 运行依赖、素材与许可 | 按清单发布到 `tools/` |
| `work/` | 探针、解包结果、报告和可选 `status.md` | 否 |
| `resources/` | 游戏资源副本，作为验收输入 | 否 |
| `build/` | 最近完整构建 | 验收通过后发布 |
| `trial-build/` | 小范围试验包 | 否 |
| `session.json`、`session-history.jsonl` | 恢复状态及完整消息 | 否 |

分析脚本放在 `work/`，通过 `run_engine_python` 执行。正式依赖从 `request['adapterDirectory']` 定位，不能引用工作区绝对路径。任务结束后开发资料保留，可继续修复或总结。

## 正式适配器

Manifest v4 包位于：

```text
extensions/packages/<引擎或模板 ID>/<版本>/
  src/adapter.py
  src/                         其他正式 Python 模块
  tools/engine-reference.json  固定共享依赖
  tools/                       运行依赖、素材与许可
  package.json                 身份、版本、规则、文件用途与哈希
  verification.json            宿主验收记录
```

发布输入保存在 `session.json.releaseFiles`，发布后写入 `package.json.releaseFiles`。仅选中文件参与打包和验收，路径须为包内具体文件，不接受通配符、重复或越界。

| 角色 | 用途 | 包内位置示例 |
| --- | --- | --- |
| `source` | Python 实现 | `src/adapter.py` |
| `runtime` | 识别、提取、回填和校验依赖 | `tools/parser.py` |
| `asset` | 补丁所需启动器、字体或数据 | `tools/Launcher.exe` |
| `license` | 许可、来源及必要源码声明 | `tools/LICENSE.txt` |

宿主可登记已写入的正式源码和已知组件依赖；外部脚本产物及间接依赖须显式选择。`package.json`、`verification.json` 由宿主生成，不属于输入清单。清单、源码、依赖或资源变化后重新验收。

发布版本固定，不能原地删改文件；精简或修复须重新选择文件、验收并发布。Python 入口由固定共享解释器执行，兼容字段 `assemblySha256` 保存入口哈希。

### 游戏文件与清理边界

原版 EXE、封包和字库放在 `resources/` 或项目 `sources/`，修改后的游戏文件属于补丁输出。可复用包只携带解析和修改逻辑、通用依赖及必要素材。宿主会拒绝与已暂存原文件哈希相同的发布输入，此检查不能识别所有改名或修改后的游戏内容。

“全局引擎库 → 版本 → 验证发布包…”可选空闲的同引擎项目，使用已有原文和提取基线验证包在独立位置的行为。结果写入该项目 `package-verification.json`，不修改正式包或切换版本，也不运行游戏。

工作区、原文工作资源和旧补丁可在[项目完工后清理](../data-storage.md#项目完工后清理)。发布后继续使用正式包，但清理开发目录会失去原会话和调查证据。

| 全局位置 | 用途 |
| --- | --- |
| `extensions/catalog/<引擎 ID>/games.json` | 每个游戏一条兼容记录 |
| `extensions/games/` | 当前与上一版本引用 |
| `extensions/library-index.json` | 可重建的展示索引 |
| `<应用目录>/tools/engine-tools/` | 发行包固定工具库 |
| `extensions/engine-tools/` | 按哈希保留的共享原生工具与依赖 |
| `extensions/downloads/` | 额外工具和资料缓存 |

共享依赖属于运行条件，复制适配器包时也需提供相应版本。下载缓存可单独删除，已复制到工作区或包内的工具仍可使用。

## 汉化运行文件与补丁记录

补丁位于 `games/<项目 ID>/patches/`：

| 路径 | 用途 |
| --- | --- |
| `<构建标识>/files/` | 游戏资源、启动器、字体和运行依赖，内部路径与游戏目录一致 |
| `<构建标识>/patch-manifest.json` | 安装路径、哈希及构建模式 |
| `<构建标识>/runtime-test.json` | 用户记录的游戏内验证结果 |
| `<构建标识>.result.json` | 生成结果、限制与结构验证说明 |
| `<构建标识>.texts.json` | 本次使用的译文快照 |

“打开补丁文件”直接打开 `files/`。将其中的全部内容复制到游戏安装目录并覆盖即可手动安装，不要把 `files` 文件夹本身放入游戏目录。游戏运行不依赖清单、报告或 GameL10n；安装与原版恢复规则见[数据说明](../data-storage.md#保存与清理)。

## 单次执行临时文件

| 项目内位置 | 用途 | 处理 |
| --- | --- | --- |
| `adaptation/current/verification/` | 完整验收输出 | 结束后删除，结论写回会话 |
| `adaptation/current/work/.resource-trial-*/` | 小范围试验副本 | 结束后删除 |
| `adaptation/reuse-*`、`adaptation/reuse-check/` | 复用验收 | 结束后删除 |
| `adaptation/run-*` | 单次调用输入输出 | 正式输出转入补丁目录，其余删除 |
| 构建旁 `.build-*`、全局包旁 `.pending-*` | 尚未提交的构建 | 成功后移动提交，正常失败时删除 |

强制退出、权限或占用可能留下临时目录。脚本主动写入 `work/` 的报告属于开发资料，清理由用户发起。

## 日志与全局经验

| 位置 | 用途 |
| --- | --- |
| 项目 `logs/adaptation-日期.jsonl` | API、工具、Token、耗时和状态 |
| 项目 `logs/` 异常诊断 | 失败原因与有限响应片段 |
| 项目 `engine-review.json` | 总结结论、证据指纹、成本及全局引用 |
| 项目 `logs/engine-review-日期.jsonl` | 总结的请求和工具统计 |
| `extensions/engine-knowledge/*.json` | 同类引擎的条件、指引与辅助源码 |

“总结”沿用智能适配 API 和预算，可新增、更新经验或判定无优化内容。证据及相关经验未变化时复用结果；无关引擎更新不使已确定引擎的总结失效。指纹覆盖会话、源码内容、验收与发布信息，不包含任务日志和总结自身输出。

经验引用 1～8 份已读取证据，可通过 `read_evidence`、`read_tool_result` 查询源码、日志及历史工具结果。辅助源码保存在全局经验内，使用时按需复制。经验供条件匹配和调查参考，复用代码仍须当前游戏验收。

智能识别先提供相关经验的短索引，详细指引、来源和源码按需读取；规则见[引擎经验索引](adaptation-internals.md#引擎经验索引)。界面可查看总结产物与来源，引用丢失时提示。

## 删除与重新开始

停止任务后删除项目 `adaptation/current/` 可重新分析，会丢失其中会话、源码、样本和证据，译文及正式包保留。

正式版本通过[引擎库](README.md#引擎库管理)删除。删除整个项目数据会同时移除译文、补丁及安装记录，范围大于重新分析。
