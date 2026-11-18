# 引擎识别声明

## 识别流程

显式识别时，程序优先检查当前项目已固定的适配器及同一游戏近期通过资源验收的版本，再尝试其他候选。每次使用仍需核对包、资源和文本提取。不同游戏复用发布版本时，还需要该版本已有游戏运行通过记录。

资源不兼容时会尝试其他候选。缺少工具、读写故障或完整性检查失败时会显示具体错误。自动候选会略过常见说明和许可文件，显式指定的资源仍按其声明处理。[识别源码](../../crates/engines/src/discovery.rs)

## 包清单

`resourcePatterns` 选择资源。可选的 `recognition` 以独立的 `schemaVersion: 1` 声明静态证据，最终适用性由 CLI `probe` 确认。

```json
{
  "schemaVersion": 1,
  "required": [{
    "patterns": ["**/*.pak", "**/*.arc"],
    "minimumSize": 16,
    "signatures": [{ "origin": "start", "offset": 0, "hex": "5041434B" }]
  }],
  "hints": [{ "patterns": ["engine.ini"] }]
}
```

- `required` 的每条规则都需命中。同一条规则的路径、大小和全部签名须由同一文件满足。
- `hints` 只影响排序。证据不可读或版本未知时，候选仍可进入探测。
- 路径相对游戏目录，忽略大小写。使用 `/`，支持 `*`、`?` 和 `**`。`**/*.pak` 也匹配根目录。
- `signatures` 使用十六进制字节。默认 `origin: "start"`，`offset` 从文件头量到签名开头。用 `"end"` 时从文件尾量到签名末尾。
- 规则以格式证据为依据。大小边界可通过 `minimumSize` 和 `maximumSize` 指定，均包含边界。

向 `test_adapter` 提交 `recognition` 会替换声明，省略则复用。发布时声明参与版本校验。[字段校验](../../crates/engines/src/discovery.rs) · [适配工具接口](../../crates/engines/prompts/adaptation-tools.json)

## 常规识别日志

在项目日志搜索 `[识别诊断]`，用 `runId` 区分运行。探测匹配后仍有提取和验收步骤，最终可用性以复用结论为准。[候选筛选与验收](../../crates/engines/src/service.rs)
