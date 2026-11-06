# 封包组件

组件使用固定共享 Python 与引擎工具库，负责封包读写；文本由对应脚本解析器处理。

| ID / 版本 | 支持范围 | 操作 |
| --- | --- | --- |
| `archive.rpa3` / 2 | RPA/ARC 3.0，单分块、安全数据型 pickle 索引 | inspect、extract、rebuild |
| `archive.xp3` / 1 | 标准 XP3 v1，已独立确认无过滤器 | inspect、extract、rebuild |

RPA 不支持多分块与混淆，编译脚本见 [RPYC2](engines/renpy-rpyc.md)。XP3 不支持私有过滤器、加密、链式索引或 EXE 内嵌包；调用须提供 `filter: "none"`，零标记不足以证明无过滤器。

## 输入、输出与校验

输入为已暂存的真实封包，按精确成员名或扩展名选择提取。所有成员检查越界和路径冲突；输出目录包含 `manifest.json`、提取的 `files/` 和 `rebuilt/<原封包名>`。

RPA `inspect` 读取头部和完整索引，`extract` 只读取所选成员。未读取成员的 `sha256` 及快速操作的 `inputSha256` 为 `null`；`verifyContents: true` 流式校验全部成员。`inputBytes`、`selectedBytes`、`readBytes`、`allContentsVerified` 记录规模和校验范围。

重建按精确成员名替换，未知成员报错，重读后比较全部替换与未替换内容。无实际修改时保留原字节；XP3 修改后采用标准 v1 布局。

| 预算 | RPA | XP3 |
| --- | --- | --- |
| 封包大小 | 输入、输出及成员逻辑总量分别最多 2 GiB | 输入最多 512 MiB |
| 单成员 | 所选成员及替换文件最多 64 MiB；未选媒体可更大 | 最多 64 MiB |
| 内容总量 | 选中及替换内容分别最多 256 MiB | 解码总量最多 256 MiB |
| 索引 | 最多 10,000 项，成员名合计 2 MiB | 同左 |

超限属于组件预算，工具模块缺失属于依赖问题。版本按哈希固定；可通过[组合模板](templates.md)接入，并完成当前资源验收。
