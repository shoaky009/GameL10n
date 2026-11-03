# 可直接调用的引擎组件

`inspect_engine_component` 返回固定入口的版本、读写能力、支持范围、输入输出和依赖。`list_adapters.engineComponents` 也列出这些组件。它们调用发行包已有的固定版本共享 Python 与 GalTransl 源码，不执行 pip，也不在分析期间补下载工具。已有脚本解析器继续使用 `try_builtin_adapter`，无需另写连接脚本。

| ID / 版本 | 支持范围 | 操作 |
|---|---|---|
| `archive.rpa3` / 1 | RPA/ARC 3.0，单分块，安全数据型 pickle 索引 | inspect、extract、rebuild |
| `archive.xp3` / 1 | 标准 XP3 v1，已独立确认没有过滤器 | inspect、extract、rebuild |

RPA 不支持多分块、混淆和编译后的 RPYC。XP3 不支持私有过滤器、加密、链式索引和 EXE 内嵌封包；标记为零不能证明没有过滤器，调用者必须提供 `filter: "none"`。组件成功只说明封包结构通过检查，不证明脚本覆盖、游戏补丁优先级或中文显示兼容。

## 调用

先 `stage_resources`，然后直接调用，不必生成 Python 脚本：

```json
{
  "id": "archive.rpa3",
  "operation": "extract",
  "archive": "resources/game/scripts.rpa",
  "output": "work/rpa-scripts",
  "extensions": [".rpy"]
}
```

输出目录必须尚未存在。完整成员名、长度、SHA256 保存在 `work/rpa-scripts/manifest.json`，模型只收到最多 12 个成员的摘要。选中的脚本位于 `files/`，可以直接交给 `try_builtin_adapter`。`members` 可按精确成员名选择，`extensions` 按小写扩展名选择；省略时提取全部成员。所有成员都会先检查路径冲突和越界，即使没有被选中。

回填使用 `operation: "rebuild"`，`replacements` 是“精确成员名 → work/ 下替换文件”的对象。输出为 `rebuilt/<原封包文件名>`。未知成员会报错；组件重读生成的封包，对比所有替换和未替换成员。没有实际修改时保留原封包字节，XP3 有修改时使用标准 v1 布局重建。

输入封包上限 512 MiB，单成员 64 MiB，解码总量 256 MiB，最多 10,000 个成员，成员名合计最多 2 MiB。超限是当前组件预算限制；模块缺失或接口变化是共享工具版本问题，均不应解释为整个引擎不兼容。源码及解释器版本与哈希沿用现有共享依赖固定机制；后续发布的适配器不会自动换用新版本。

支持的封包与脚本组合可用 [组件组合模板](templates.md) 自动生成完整协议入口和发布依赖，省去逐次编写连接代码；仍须通过当前游戏的 `test_adapter` 验收。
