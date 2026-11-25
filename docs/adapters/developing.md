# 适配器开发与兼容性指南

本文面向往 GameL10n 增加或维护引擎适配能力的开发者，覆盖查重、落点选择、提示词与工具边界、测试验收以及宿主升级时的兼容契约。

## 1. 先确认已有能力

新增实现前，按下面 5 层核对项目是否已经覆盖了封包、脚本或运行支持，定位真正的缺口（例如已有读包组件、仅缺脚本回填，或已有脚本解析、仅缺字体/编码支持）：

| 层级 | 查看位置 | 说明 |
| --- | --- | --- |
| 内置脚本解析器 | [VNTextPatch 格式表](engines/vntextpatch.md)、`cargo run -p gamel10n-core -- formats` | 覆盖常见已解包脚本格式，支持提取、原文往返与回填 |
| 仓库内置适配器 | [引擎与格式能力表](README.md#引擎与格式能力表)、`adapters/` | 如 KiriKiri SMIL、Siglus、SFA16 等完整内置引擎适配器 |
| 封包组件与组合模板 | [封包与模板](templates.md) | `engine_archive.py` 中的封包读写器，以及可直接拼装完整适配器的组合模板 |
| 独立文本与运行组件 | [组件注册表](../../crates/engines/prompts/adapter-components.json)、[组件说明](components.md) | 按完整 ID 注册的脚本、编码、字体、代理与启动组件（以注册表为准） |
| 固定外部参考工具库 | 发行包附带的共享参考库（运行时由 `search_engine_references` 查询） | 仅作解析参考与按需复用，不代表当前游戏已验收通过 |

同名引擎常因版本、封包加密或字体机制不同而不通用；判断能否复用以格式证据和往返验收为准。

## 2. 确定落点，以及是否需要改提示词或加 LLM 工具

### 选择新增层级

| 场景 | 推荐落点 | 主要入口 |
| --- | --- | --- |
| 仅针对当前游戏快速交付 | 外置 Python 适配器包（Manifest v4 / CLI v1） | `extensions/packages/`（由智能适配或本地发布生成），遵循 [CLI 协议](protocol.md) 与 [包识别声明](recognition.md) |
| 单个可复用解析、编码、字体或启动能力 | 独立组件 | `adapters/components/` + [组件注册表](../../crates/engines/prompts/adapter-components.json) + `crates/engines/src/adaptation_components.rs` |
| 新的封包索引/解包/重建能力 | 封包组件 | `adapters/components/engine_archive.py`，必要时接入 `crates/engines/src/adaptation_automatic.rs` 首轮小样试用 |
| “封包 + 脚本”已稳定串联、可跨游戏复用 | 组合模板 | `adapters/components/*_adapter.py` + `crates/engines/src/adaptation_assets.rs`（`ARCHIVE_TEMPLATES`） |
| 需要调用 Windows API 或高频原生解析 | 独立 Rust 原生工具 | `tools/` + `scripts/prepare-tools.ps1` / `publish.ps1`（FFI 与 `unsafe` 限定在工具内，不进入四个主 crate） |

交付新引擎或格式能力时，按 [AGENTS.md](../../AGENTS.md) 同步更新 `docs/adapters/engines/` 中对应主题文档及 [引擎与格式能力表](README.md#引擎与格式能力表)；文档说明格式、能力和适用条件，不写具体游戏名。

### 要不要改提示词或给 LLM 增加 tools？

- **绝大多数新引擎/格式：不需要新增 LLM tool，也不要直接往主提示词里塞引擎细节。**
  - 组件、封包组件和组合模板完成注册后，现有的 `list_adapters`、`inspect_adapter_component`、`copy_adapter_components`、`run_engine_component`、`compose_adapter_template` 会自动发现并把接口、适用范围和示例提供给分析模型。
  - 引擎特有的限制、编码注意事项或回填前提，优先写在注册条目的 `scope` / `api` / `limitations` 中，或放入 `crates/engines/references/` 按需提供，避免膨胀主提示词。
- **只有两类情况才改动提示词或工具集：**
  1. 新增**跨引擎、高频的通用调查手段**（例如 PE 导入表/反汇编、元数据查询等无法靠现有通用组件表达的能力），才在 [adaptation-tools.json](../../crates/engines/prompts/adaptation-tools.json) 和 `crates/engines/src/adaptation_tools.rs` 中新增工具定义与实现。
  2. 共享流程或验收规则发生变化（例如新增通用协议能力、新的自动阶段），才同步调整 [adaptation.md](../../crates/engines/prompts/adaptation.md) 或 [adapter-protocol.md](../../crates/engines/prompts/adapter-protocol.md)。
- **注意目录边界**：`crates/engines/prompts/` 是编译进程序的运行时提示词和注册表；仓库根目录的 `llm/` 仅供人工或编程代理按需查阅，**不会**在应用运行时自动加载，修改 `llm/` 不会改变宿主发给模型的请求。

## 3. 测试与验收

建议按“不花模型 Token → 自动化回归 → 隔离与真机”的顺序逐层验证：

### 本地 CLI 协议冒烟

无需启动桌面端或调用 LLM，直接用 `gamel10n-core` 对脚本或发布包执行 CLI v1 请求：

```powershell
# 测试开发中的 Python 适配器入口
cargo run -p gamel10n-core -- tool ./request.json --python-adapter ./src/adapter.py

# 校验发布包清单与固定依赖后执行
cargo run -p gamel10n-core -- tool ./request.json --package ./extensions/packages/<适配器ID>/<版本>
```

`tool` 子命令通过标准输出返回 JSON，请求文件中应省略 `responseFile`。最小 `probe` 请求示例：

```json
{
  "protocolVersion": 1,
  "operation": "probe",
  "sourceDirectory": "D:\\Samples\\ExampleGame",
  "resourceFiles": ["script/main.bin"]
}
```

依次验证 `probe` → `extract` → 原文 `build` → 译文 `build` → `validate`。

### 自动化回归测试

- 在 `crates/engines/tests/` 中用**脱敏的最小合成样本**补齐集成测试（参考现有 `resource_verification.rs`、`artemis_pfs.rs`、`kirikiri_smil.rs`），至少覆盖：
  - `probe` 命中有效资源、对无关或畸形资源返回 `probe: null` 或 `"incompatible"`；
  - 原文逐字节往返（`replace` 保持源字节不变，`overlay` 复提与原文一致）；
  - 部分翻译与全量中文翻译后复提，未译条目原文与既有文本 `id` 保持不变；
  - 越界路径、非法结构与缺失依赖明确报错。
- 涉及便携 Python、VNTextPatch 或 Windows AppContainer 只读隔离的测试，需先在 Windows 运行 `./scripts/prepare-tools.ps1` 准备 `target/runtime/`，再执行 `cargo test --workspace --locked`。[开发与检查](../development.md#检查)

### 桌面端验收与识别收益核对

- **静态与打包验收**：在桌面端“识别与适配 → 手动执行适配器…”或“全局引擎库 → 验证发布包…”用真实游戏目录做无模型验收，并核对试译补丁生成、安装、游戏内显示和“恢复原版”。
- **智能识别收益核对**：改动接入自动试用、模板或提示词后，用 `./scripts/analyze-adaptation-log.ps1` 对比改动前后的请求轮数、Token 用量与耗时，确认真实缩短了调查路径且没有拖慢无关游戏的常规识别。[诊断入口](automatic-adaptation.md#诊断)

## 4. 宿主兼容性与破坏性变更

### 核心约束：已发布适配器包是冻结副本

用户数据目录 `extensions/packages/<适配器ID>/<版本>/` 中的 `src/` 与 `tools/`（包含发布时复制进去的 `gamel10n_host.py`、组件模块和依赖）都受 `package.json` 的 `packageFiles` SHA-256 校验保护，**宿主升级不会自动修改已发布包内的任何文件**。

因此：
- 修改仓库里 `adapters/components/` 中的模板或组件，只影响**后续新发布**的适配器，不会改变既有旧包；
- 修改**宿主侧契约**（CLI v1 请求/响应格式、宿主注入的 `toolPaths` 结构、原生工具子进程协议、共享库/便携 Python 路径布局、隔离权限模型），则可能直接导致所有历史已发布包失效。

### 版本字段与演进规则

| 版本字段 | 位置 | 职责与演进规则 |
| --- | --- | --- |
| `protocolVersion` | CLI 请求与响应、包清单 | 当前为 `1`。新增可选请求字段、`toolPaths` 键或可选响应字段保持为 `1`；修改/移除既有必填字段或破坏既有语义时必须升版。 |
| `manifestVersion` | `package.json` | 当前可执行 Python 包为 `4`（历史 v2/v3 为已淘汰格式）。仅在包结构或校验规则无法向下兼容时升版。 |
| `discoveryIndexVersion` | 引擎库索引 / 包清单 | 当前为 `1`。用于控制静态筛选索引重建，升级时旧包仍能退回常规资源匹配。 |
| `recognition.schemaVersion` | `package.json` 的 `recognition` | 当前为 `1`。未知版本不直接判死，可退回探测。 |
| `validationContextVersion` | 提取响应声明 | 当前为 `1`。用于按能力协商宿主提供的校验上下文，未声明的旧包走默认复提校验。 |

**兼容演进优先**：
1. 宿主扩展能力时优先使用**新增可选字段**或**响应端显式声明能力版本号**（如 `validationContextVersion`），让旧包无需改动即可继续运行；
2. 宿主更新内置原生工具（如 VNTextPatch）时保持子命令与协议向后兼容：旧包运行时直接使用当前安装的原生工具，工作区会自动刷新依赖记录并可按原验收配置复验；
3. 宿主升级前，使用 `cargo run -p gamel10n-core -- --data-directory <目录> audit` 检查既有项目元数据与提取缓存，并在单测中保留旧版包清单/缺可选字段的回归用例。

### 确需破坏性变更或旧适配器失效时的处理

当宿主必须做出不兼容变更（例如淘汰旧运行格式、升级 `protocolVersion` 或 `manifestVersion`）时，须遵守以下底线：

1. **明确拒绝，不静默降级或破坏数据**：宿主在加载清单或执行协议前校验版本号（参见 `crates/engines/src/discovery.rs` 与 `protocol.rs`），不兼容时直接返回带原因和修复指引的错误，保留原包、项目提取文本、译文和校对数据。
2. **提供无损接管路径**：
   - 优先让用户通过“重新识别 / 修复现有问题”或“手动执行适配器…”切换到通过验收的新版本；历史项目只要提取出的文本 `id` 与匹配字段保持一致，既有译文、校对和补丁记录即可无缝沿用。
   - 若需批量替换已固定到旧版本（如历史 Manifest v2/v3）的运行实现，可通过 `extensions/runtime-migrations.json` 登记经完整哈希校验的替代包映射（`PackageManifest::load_runtime`），在不改动项目身份与提取基线的前提下切换到新版 Python 包。
3. **旧适配器失效排查顺序**：先看项目日志与任务详情区分失败类别——① `manifestVersion` / `protocolVersion` 不受支持（需重发或迁移）；② `packageFiles` 或固定工具哈希不匹配（包文件被改写或发行工具不完整，需恢复发行包或重新验证）；③ `errorKind: "dependency"` / `"io"`（共享运行时、素材或隔离句柄访问受阻）；④ 仅在最新宿主下复现的协议字段或工具输出变化（属于宿主侧兼容回归，应优先在宿主侧恢复兼容）。
