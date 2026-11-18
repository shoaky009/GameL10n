# KiriKiri SMIL / SmileCrypt / PSB SCN

内置 `engine.kirikiri-smil-smile` 适用于带 `File/smil` 映射、成员解密校验通过的独立 XP3。其他过滤器和保护布局需要单独适配。

## 范围与回填

- 支持 PSB v2/v3 SCN 正文、显示姓名、选项和标题，KS 直接正文，以及 TJS 源码中 `caption`、`hint`、`title` 的静态文字。
- SCN 跳过未确认的控制符和派生字段，保留相应原文。动态界面、编译 TJS 的显示字段和图片内文字需要单独调查。
- 文本身份按原始成员别名和字段位置确定。回填按原路径替换封包，保留受支持的保护字段与未修改成员，更新映射校验。

游戏字体、排版、签名检查和原有启动环境须实测。[识别与验收](../automatic-adaptation.md#验证与产物) · [内置源码](../../../adapters/kirikiri) · [SCN 组件](../components.md#scn-文本语义)
