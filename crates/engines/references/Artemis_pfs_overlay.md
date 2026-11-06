# Artemis PFS 增量覆盖

适用于已确认采用独立 PFS 覆盖卷的 Artemis 运行时。`.pfs.NNN` 各自是完整 PFS，
不是物理切片；在已确认的加载规则中，主包先加载，同族数字卷按序加载，后卷同路径成员覆盖前卷。
不得仅凭 `.pfs` 后缀推断当前运行时支持此规则。已有引擎资料和兼容实现可作为依据；
版本或挂载规则不明时保留完整重建，不为体积优化单独展开逆向。

## 复用与输出

通过 `inspect_engine_reference(engine='artemis')` 查看固定库版本，
`use_engine_reference` 登记共享 Python 依赖。`python.archives.pfs` 已提供：

- `read_index(path_or_stream)`：只读取索引，返回 `ArchiveIndex`。
- `read_member(path_or_stream, index, entry).data`：按需读取单个解密成员。
- `volume_paths(path)`：枚举同族卷名；只反映当前目录，不能从暂存目录推断真实游戏全部卷名。
- `resolve_volumes([(path, index), ...])`：按传入顺序解析覆盖关系；调用者提供已确认的加载顺序。
- `build([(member_name, decoded_bytes, unknown), ...], version=8, name_encoding='utf-8')`：构建独立小卷。

保留原逻辑路径、大小写、必要的未知字段和目录规则。只写实际修改的 AST/ASB/TXT 及必要字库、配置等新增伴随资源，
不要复制未改 CG、音频和视频。检查所固定版本的 API；不下载、重写整套封包库。
源脚本没有实际修改时返回 `build:{mode:'overlay',files:[]}`。

## 卷名与文本身份

资源发现规则涵盖主 `.pfs` 及数字卷，使 bounded probe 能看到候选名称和索引。
从 `probeFiles`／`resourceFiles` 枚举同目录、同前缀的数字卷，按当前最大序号加一选择输出名，
以至少三位数字表示。无数字卷时从 000 开始；不可固定为 001、猜测最大后缀或写死游戏名。
通过 probe 的 `buildHints` 返回每个主包到新增卷的映射，例如 `overlayPaths`，
完整操作从 `request.buildHints` 取回。宿主保存真实游戏的选择，后续同项目生成和安装复用同一路径。
无需把只用于名称或索引判断的大型媒体卷纳入 `requiredFiles`；其中确有待翻译文本时才作为完整输入。

提取原文时，按已确认顺序合并必要源卷；overlay 复提时，再将 `outputFiles` 中的补丁卷加入有效命名空间。
若原始文本来自主包，其 `resourcePath`、ID、locator、context 在补丁复提中仍指向主包；
覆盖卷是交付路径，不能改变原有译文和校对数据的身份。选中试译之外的文本继续从原资源读取。

## 验收

沿用宿主的原文、部分中文、完整中文验收。校验器检查覆盖卷索引、成员数据、覆盖优先级，
确认未改成员沿用原字节；有 `validationContextFile` 时复用本次实际提取结果，避免再解析所有脚本。
保持源包与补丁只读，无需构建物理合并目录，也无需复制其他媒体包。
静态验收不能证明游戏确实加载新增卷、中文字体或换行正确，运行状态仍为 `not-tested`。
