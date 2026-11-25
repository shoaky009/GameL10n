# Ren’Py RPYC2 编译脚本

支持松散 `.rpyc`、RPA/ARC 3.0 中的 RPYC2，以及路径无冲突的源码混合资源。可用模板直接处理。[封包组件与模板](../templates.md)

## 适用条件

- `RENPY RPC2` 容器，源码 AST 槽与可选的转换槽内容和文本身份一致。
- 支持已知 AST 中的对白、旁白、菜单和 `TranslateString.new`。静态角色名作为上下文，Python 和 screen 文字需要单独调查。
- 读取受支持的 Pickle 数据结构，回填同步更新各槽并保留源码摘要、控制符及非文本数据。
- 按 Ren’Py 标准加载规则处理封包：忽略封包内与 `.rpyc` 成对存在的 `.rpy`，默认跳过 `tl/<其他语言>/` 翻译目录；含封包资源的模板默认以松散覆盖（`overlay`）输出变更脚本，游戏开启 `force_archives` 时可切换为整包重建（`replace`）。

旧版 RPYC、加扰、未知槽、中文字体缺字和自定义模块需进一步适配。自动处理要求统一加载根，松散脚本与封包间路径无覆盖冲突。[混合资源规则](../templates.md#renpy-混合资源) · [适配器实现](../../../adapters/components/renpy_rpyc_adapter.py)
