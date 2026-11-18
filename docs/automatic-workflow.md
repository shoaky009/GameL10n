# 全自动汉化流程

在“识别游戏”中勾选“识别后自动试译验证、全量翻译并安装补丁”。程序会启动游戏、检查译文并安装补丁。每次从项目页打开识别对话框时默认关闭此选项。

## 默认行为

1. 识别游戏并提取文本。
2. 选择开场验证文本。优先使用“运行检查”中保存的唯一目标。否则从原版游戏画面中用 Windows OCR 匹配一条已提取对白或旁白。
3. 翻译目标文本，生成并安装试译补丁。
4. 启动游戏，按保存的场景操作进入目标画面，检查译文并关闭本次启动的进程。
5. 试译检查和进程关闭通过后，全量翻译已提取文本，并生成、安装全量补丁。

需要按键或点击才能进入对白时，先在“运行检查”保存操作。OCR 结果不稳定、目标有歧义或无法定位时，流程会等待补充。选定一个目标或调整场景后重新运行。[游戏运行检查](desktop.md#游戏运行检查)

自动从画面定位目标前会尝试恢复本程序安装的补丁，以读取原文。恢复冲突时停止。开始检查前需关闭原先运行的游戏。

## 失败、取消与恢复

失败或中断时，已保存的译文和补丁仍可使用。重新运行从配置入口开始，会复用有效结果。已安装的试译补丁可能仍在游戏目录，可用“恢复原版”。阶段和原因见任务详情及日志。[恢复规则](data-storage.md#保存与清理)

OCR 通过只表示目标译文在当前画面可读。全量安装后仍需确认实际显示、分支和存档。启动补丁时会在宿主环境运行其中的可执行代码。[补丁运行的风险](adapters/execution-isolation.md#补丁运行的风险)

## 调整流程

在“项目设置 → 编辑自动流程配置…”打开 `workflow.json`。程序会生成默认配置，可直接修改现有步骤，保存后下一次运行生效。

配置包含 `schemaVersion: 1`、`entry` 和 `steps`。每步的 `id` 唯一。成功后默认继续下一步，可用 `onSuccess`、`onFailure` 和 `onUnavailable` 指向已有步骤。缺少对应分支时，失败会结束，信息不足会等待补充。

| 动作 | 可调参数 |
| --- | --- |
| `recognize` | `instruction` 补充识别要求 |
| `locateText` | `unitId` 指定目标，`settings` 指定原文 OCR 与场景 |
| `translate`、`buildPatch` | 默认处理目标。设置 `scope: "all"` 改为全量。 |
| `installPatch` | 安装本次流程生成的最新补丁 |
| `checkGame` | 检查译文并关闭进程。用 `settings` 覆盖检查场景。 |
| `waitForInput`、`complete` | 等待补充或成功结束。可在等待步骤填写 `message`。 |

`settings` 支持 `language`、`region`、`steps` 和 `timeoutSeconds`。下面的定位步骤使用日语 OCR，启动后按一次 Enter。

```json
{
  "id": "locate",
  "action": "locateText",
  "onUnavailable": "input",
  "settings": {
    "language": "ja",
    "steps": [{ "action": "key", "key": "Enter" }]
  }
}
```

若只配置定位场景，译文检查复用其操作和区域，并使用“运行检查”保存的目标语言。已有提取结果的项目可将入口改为默认配置中的 `locate`。调整分支后，全量翻译和安装仍要求本次试译检查通过。

命令行使用 `gamel10n workflow <项目ID>`，支持 `--intelligent` 和 `--instruction`。[配置字段与校验](../crates/core/src/workflow.rs)
