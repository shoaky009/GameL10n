# Ren’Py RPYC2 编译脚本

支持松散 `.rpyc`、RPA-3.0 / ARC-3.0 中的 RPYC2，以及无路径冲突的源码混合资源。可通过[组合模板](../templates.md)直接适配，无需反编译为 `.rpy`。

## 支持范围

- `RENPY RPC2` 头，源码 AST 槽 1 与可选转换 AST 槽 2，两槽文本身份和内容须一致。
- `Say`／`TranslateSay` 对白与旁白、`Menu` 选项、`TranslateString.new`。静态角色名仅作上下文；角色声明、查找键 `old`、Python 和 screen 字符串不翻译。
- Pickle 协议 2–5 的受支持子集，保留别名和循环为数据，不执行游戏对象或代码。
- 同步回填各槽，保留节点、控制符、变量、跳转和非文本字段。末尾 16 字节 MD5 描述原源码，保持原值。

槽与摘要定义参考 [Ren’Py 加载实现](https://github.com/renpy/renpy/blob/master/renpy/script.py)。

## 自动处理与限制

自动流程扫描完整目录和索引，要求统一加载根，无同名源码／编译脚本或跨包重复路径。模块 `_ren.py`／`.rpym`／`.rpymc`、未知加载根和自定义覆盖关系交给智能分析；引擎附带脚本可能需要定向排除。路径规则见[混合模板](../templates.md#renpy-混合资源)。

单个 RPYC2 及槽解压总量最多 32 MiB，选中脚本最多 256 MiB。封包预算见[RPA 组件](../engine-components.md)，自动流程预算见[首次自动组合](../adaptation-internals.md#首次自动组合)。旧版 RPYC、加扰、未知槽及异常对象结构报错。

验收从原始对象图重建允许修改并比较产物，未改封包成员保持一致。工具模块缺失时恢复固定版本，发布资格与实机验证见[智能适配](../automatic-adaptation.md#验证与产物)。
