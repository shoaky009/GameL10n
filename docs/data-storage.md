# 数据目录与迁移

默认数据目录是 `%LOCALAPPDATA%\GameL10n`。在“全局 → 程序设置 → 更换保存位置”中选择目录后，可选择“迁移并切换”或“仅切换”，完成后立即生效，无需重启。

从 AvgTranslator 升级时，如果新目录尚无位置设置或游戏数据，程序继续读取 `%LOCALAPPDATA%\AvgTranslator` 及其中的 `location.json`，原有游戏、译文、API 配置和扩展仍可使用，不自动搬动数据。更换位置后，启动位置设置保存在 `%LOCALAPPDATA%\GameL10n\location.json`。数据工具和桌面程序使用同一套位置规则。

“迁移并切换”将当前项目、译文、原文快照、补丁、日志、共享适配器和 API 配置直接复制到新目录，校验后立即加载。原目录保留，不额外创建整份备份或归档。已经完整导入分片的旧版逐条缓存，以及已有有效项目对应的旧整份 `.bak` 目录，留在原目录，不重复迁移。确认译文已提交的成功请求和响应也不重复迁移；异常诊断与未完成请求保留。

首次迁移需要空目录。失败时继续使用原目录，目标保留已经复制的文件；再次选择相同目标会核对并复用一致文件，继续完成复制。“仅切换”直接加载目标目录已有的数据，空目录显示为空游戏库，不复制当前数据。切换前会暂停运行中的任务并等待状态保存，切换后可在项目中继续翻译。

`GAMEL10N_DATA` 环境变量优先，也兼容旧变量 `AVG_TRANSLATOR_DATA`；两者均设置时使用新变量。否则程序读取上述目录下 `location.json` 的 `dataDirectory`。通过环境变量启动时，界面切换仅影响当前运行，不修改默认位置设置；下次启动仍以环境变量为准。

```text
<数据目录>/
  settings.json                全局 API 配置、受保护的密钥、默认设置与并发限制
  games/index.json             游戏列表摘要：名称、路径、最近状态与 API 配置引用
  games/<游戏 ID>/
    game.json                  版本 2 标记、游戏路径、名称与独立翻译设置
    installation.json          补丁安装记录、原文件备份位置和中断恢复状态
    source.json                原文快照索引；directory 相对当前游戏数据目录
    sources/<指纹>/            原文备份
    extraction/<缓存键>/       提取清单与按资源拆分的文本
    memory/00.json … ff.json    按原缓存键前两位分片的翻译记忆
    translation-edits.json     手工译文、校对状态和待处理候选，按文本 ID 集中保存
    translation-scope.json     上次翻译范围、是否连续生成补丁和完成状态
    trial-selection.json       跨页选择的文本 ID
    responses/                 未完成请求的临时记录、异常响应与拒绝记录
    patches/                   生成的补丁、报告、所用译文快照和用户运行验证记录
    adaptation/                可续接的适配开发资料，以及单次执行临时目录
    logs/                      当前游戏的运行与 API 诊断日志
    task-state.json            最近任务状态
    job.json                   翻译进度
    tasks/                     翻译任务的进度与 Token 汇总
  extensions/                  共享适配器、兼容记录与版本固定信息
    engine-tools/              已安装的共享工具版本、VNTextPatch DLL 与许可、版本记录
```

游戏列表只读取 `games/index.json`，搜索和排序在内存中完成。打开项目时才读取其完整设置、任务记录及已有结果，读取后供本次运行复用。索引丢失或损坏时，仅根据各项目的 `game.json` 重建；不会扫描提取文本或适配会话。项目文件被手工移走后，已有索引条目仍会显示，打开时提示记录不可用，也可从列表移除。

适配开发资料保存在项目的 `adaptation/current/`，正式适配器发布到全局 `extensions/packages/`。源码、分析脚本、样本、补丁与日志的用途及保留规则见[引擎分析产物与目录约定](adapters/artifacts.md)。

游戏 ID 继续使用规范化游戏绝对路径的 SHA-256 前 24 位。修改安装路径会得到不同 ID；迁移时使用原游戏路径。翻译记忆保留完整缓存键、原文、上下文、译文、服务商和时间戳。每个缓存键只作为分片字典的键保存一次，条目内部不重复保存 `key`。旧分片在加载时自动转换，不改变缓存键或译文，不额外生成转换备份。

正常请求和响应只在译文提交前临时落盘，用于中断恢复；译文及本批进度、Token 汇总保存后删除。重新识别或继续翻译时，也会清理能够与已提交译文核对一致的历史成功记录。异常、拒绝、结果未知和无法确认已提交的记录保留；清理失败不会使已保存译文失效。

当前项目更换保存位置使用界面的“迁移并切换”。需要转换旧版单项目数据结构或独立校验已有数据时，关闭所有 GameL10n 窗口，在发行包根目录直接运行数据工具：

```powershell
# 原目录升级；旧游戏目录保存为 games/<ID>.bak，旧 memory/<ID> 保留
./tools/data/GameL10n.DataTool.exe migrate --game-directory 'I:\LiLiM\DARK BLUE'

# 复制到新位置，验证成功后设置下次启动的数据目录
./tools/data/GameL10n.DataTool.exe migrate --all `
  --destination-root 'D:\GameL10n Data' --activate `
  --report './migration-report.json'

# 指定来源；不加 --activate 时只迁移，不改变程序使用的数据位置
./tools/data/GameL10n.DataTool.exe migrate --game-directory 'I:\LiLiM\DARK BLUE' `
  --source-root 'D:\AvgTranslator Data' --destination-root 'D:\GameL10n Data'

# 重新校验源文件、读取提取缓存并统计当前设置下的译文缓存命中
./tools/data/GameL10n.DataTool.exe verify --game-directory 'I:\LiLiM\DARK BLUE' `
  --data-root 'D:\GameL10n Data'
```

发行包自带数据工具和运行时，不需要安装 SDK。源码工作目录可使用 `./scripts/dotnet.ps1 run --project tools/GameL10n.DataTool -- <命令及参数>` 构建并运行该工具。

工具先将游戏数据复制到目标的 `.migration/<ID>`，用 SHA-256 校验文件，再转换并对照提取文本和翻译记忆，最后写入 `game.json` 并提交目录。中断后重复同一命令会从保留的原数据重新构建临时目录；已经完成升级的游戏只校验，不覆盖之后产生的译文、设置或任务。并发的程序或工具会被进程检查及数据目录锁拒绝。来源与目标不能互相包含，也不能位于游戏安装目录内。

迁移不会删除旧的 `memory/<ID>`、旧提取文件或来源目录。原位置升级时，旧游戏目录保存在 `games/<ID>.bak`；提交目录失败会自动恢复它。保留备份直到确认译文、补丁和恢复任务都可正常使用。若要手工回退，关闭程序，先移走当前 `games/<ID>`，再将 `.bak` 改回原名。已有 `.bak` 冲突时工具会停止，避免覆盖备份。

迁移到新数据目录时，原有全局设置、共享适配器和版本固定信息一并复制；目标已有不同文件时会停止并保留双方文件。已有目标 `settings.json` 优先，不会被覆盖。API Key 的 Windows 当前用户保护内容保持原样，仍需使用原 Windows 用户；工具不解密或输出密钥。未持久保存的会话密钥需重新输入。

配置、识别结果和任务状态通过临时文件写入后原子替换，不生成 `.bak`。译文分片保留一份恢复副本，防止丢失已付费翻译的数据；安装时替换游戏原文件的备份也保留。提取缓存损坏时从原文快照重新生成。

项目运行日志按天追加，自动适配失败保留具体诊断。任务完成、失败或暂停后，`adaptation/current/` 的源码、样本、工作资料和会话仍会保留。单次适配器调用、C# 探针编译及验收的临时目录在操作退出时尽力删除；程序不扫描清理历史开发目录。需要重新分析时，停止任务后手动删除 `adaptation/current/` 即可，不影响译文、已发布适配器和已有补丁。迁移报告仅包含路径、条目数量、校验状态和耗时，不包含译文或 API 配置内容。

`--all` 根据每个游戏的描述文件或任务记录读取原安装路径；找不到对应路径时停止并报告游戏 ID。切换数据目录会影响整个游戏库，建议迁移全部游戏后再使用 `--activate`。已升级的游戏可直接复制到新位置，包括之后写入的分片译文。
