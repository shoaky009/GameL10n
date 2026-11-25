# Ren’Py 运行时与适配说明

面向使用 Ren’Py 7 / 8（`renpy/script.py`、`renpy/loader.py`）的视觉小说。以下规则基于 Ren’Py 官方加载器行为，具体游戏仍需核对是否修改了加载器或 `config.force_archives`。

## 1. 脚本加载优先级（核心规则）

- **封包内（`.rpa`）只加载 `.rpyc`**：在标准 `renpy/script.py` 的 `scan_script_files` 中，当脚本位于封包内（`dir is None`）时，`.rpy`、`_ren.py` 和 `.rpym` 会被直接跳过，引擎只会加载编译后的 `.rpyc` / `.rpymc`。
- **禁止仅修改封包内的 `.rpy`**：当 `.rpa` 同时打包了成对的 `.rpy` 与 `.rpyc` 时，仅回填封包里的 `.rpy` 虽然能通过提取/回填校验，但游戏实际运行时完全不会读取它。应修改 `.rpyc`（使用 `rpa3-renpy-rpyc` 或 `renpy-mixed` 模板）。
- **磁盘松散文件优先于封包**：`renpy.config.force_archives` 默认为 `False`；`renpy/loader.py` 先扫描磁盘目录再扫描 `.rpa`，磁盘上已存在的同路径 `.rpyc` 会屏蔽封包内的同名 `.rpyc`。
- **同目录松散 `.rpy` 与 `.rpyc` 共存**：磁盘同一目录下若同时存在同名 `.rpy` 和 `.rpyc`，`load_appropriate_file` 会比对 `.rpyc` 末尾的 16 字节源码 MD5，不一致时会重新编译 `.rpy`。因此若以松散 `.rpyc` 覆盖封包脚本，同目录不应放未修改的同名松散 `.rpy`。

## 2. 补丁输出模式（Overlay 与 Replace）

- `rpa3-renpy-rpyc` 与只选中封包的 `renpy-mixed` 默认使用 `build.mode = "overlay"`。从源码模板切换到 RPYC2 时沿用此默认值。松散脚本与封包混合时默认使用 `replace`，以便替换原路径上的脚本。纯封包的默认行为如下。
  - 原文无改动时返回空 `files: []`；
  - 有译文改动时仅将变更过的脚本输出到封包同级逻辑目录（例如 `game/script.rpyc`），避免重复复制或重建数 GB 媒体封包。
- 若目标游戏在 `options.rpy` 或启动代码中显式设置了 `config.force_archives = True` 或自定义 `file_open_callback` 导致松散覆盖不生效，可在 `tools/adapter-template.json` 中设置 `"buildMode": "replace"` 切回流式整包重建。

## 3. 多语言目录（`tl/<lang>/`）

- `game/tl/<lang>/`（除 `tl/None/` 外，如 `tl/russian/`、`tl/espanol/`）存放其他语种的翻译脚本。默认模板在提取封包时会跳过这些非默认语种目录，避免将多国语言译文混入主剧本文本。

## 4. 中文字体检查与替换

- 西文或日文 Ren’Py 游戏常用的 `gui.text_font`、`gui.name_text_font`、`gui.interface_text_font`（如 Philosopher、Cinzel、DejaVuSans 等）通常缺少简体中文汉字字形，直接写中文会显示方块字或错位。
- 同一目录的封包在 `overlay` 和 `replace` 模式下共用字体检查。写入中文译文且 GUI 字体缺少常用简体中文字形时，两种补丁都会在封包旁附带 `zz_gamel10n_font.rpy`。验收会核对脚本的清单、文件和内容。若游戏自定义加载器屏蔽松散脚本，需根据该加载器另行适配字体。
- 若游戏使用自定义字体配置或需要指定随包开源字体，可在 `tools/adapter-template.json` 关闭 `"fontFallback": false` 并自定义字体脚本；无需修改 EXE 或注入 Hook。

## 5. 界面与 Python 文本

- `renpy_rpyc` 与内置 `Renpy` 解析器覆盖对白、旁白、菜单和 `TranslateString.new`。
- `screens.rpy` 中的 `textbutton`、`text _("...")` 等界面静态文本可优先通过 Ren’Py 原生 `translate None strings:` 机制覆盖，避免破坏复杂的 screen 布局或 Python 表达式。
