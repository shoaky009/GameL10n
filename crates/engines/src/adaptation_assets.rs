//! Bundled parser source references and the original adapter capability catalog.
use crate::{
    AdaptationRequest, CancellationToken, EngineError, EngineService,
    adaptation::{Session, text},
    adaptation_context::{BSSCRIPT_GUIDANCE, builtin_catalog},
    discovery::{glob_match, resolve_relative},
    vntextpatch::BUILTIN_FORMATS,
};
use gamel10n_translation::files::hash_file;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Read,
    path::{Path, PathBuf},
};

const HELPER: &str = include_str!("../../../adapters/components/gamel10n_host.py");
const KIRIKIRI_REFERENCE: &str = include_str!("../references/Kirikiri_text_encoding.md");
const KAGUYA_REFERENCE: &str = include_str!("../references/Kaguya_MESSAGE_v4_0_runtime.md");
const YURIS_REFERENCE: &str = include_str!("../references/Yuris_text_encoding.md");
const ARTEMIS_REFERENCE: &str = include_str!("../references/Artemis_pfs_overlay.md");
const RENPY_REFERENCE: &str = include_str!("../references/Renpy_runtime.md");
const KAGUYA_WARNING: &str = "KaGuYa MESSAGE v4.0 内置适配器只提供 CP932 脚本提取和回填；可提取不代表已支持中文运行。CP932 无法表示的译文会报错，中文编码、字节遍历和字体绘制仍需验证。";
const GRAPHICS_SHA256: &str = "E1292E3D47D1C65F4AE52A30B1159A476137422BD7A9272A5A2E0129F3CB0684";
pub(crate) const ARTEMIS_PFS_TEMPLATE: &str = "artemis-pfs8-ast";
pub(crate) const QLIE_PACK_TEMPLATE: &str = "qlie-pack3-imos";
pub(crate) const QLIE_PACK31_TEMPLATE: &str = "qlie-pack31-imos";
pub(crate) const FLASH_POOL_TEMPLATE: &str = "flash-avm1-pool";
pub(crate) const SYSTEM4_AIN_TEMPLATE: &str = "system4-ain-msg1";
const DEFAULT_TEMPLATE_LIMITATIONS: &str = "只连接已声明的脚本与封包组件。RPYC2 保留非文本对象及源码摘要；其他脚本编码沿用解析器。不生成未经当前游戏验证的字体、转区或运行时补丁。";
const DEFAULT_PREPARED_NEXT_ACTION: &str = "沿用宿主已验收的资源和源码，核对独立文字覆盖、卷加载顺序及游戏显示";
const QLIE_TEMPLATE_SOURCES: &[&str] = &[
    "qlie_archive.py",
    "qlie_bitmap.py",
    "qlie_pack31.py",
    "qlie_samples.py",
    "qlie_script.py",
    "qlie_systemfont.py",
    "qlie_sfnt.py",
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ProbeReportKind {
    Archive,
    Movie,
    Ain,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CompanionRequirement {
    Optional,
    Required,
    Skip,
}

struct CompanionPolicy {
    format: &'static str,
    required_magic: Option<&'static [u8]>,
}

const COMPANION_POLICIES: &[CompanionPolicy] = &[
    CompanionPolicy {
        format: "Softpal",
        required_magic: None,
    },
    CompanionPolicy {
        format: "Yuris",
        required_magic: Some(b"YSTB"),
    },
];

pub(crate) fn companion_requirement(
    format: &str,
    path: &Path,
) -> Result<CompanionRequirement, EngineError> {
    let Some(policy) = COMPANION_POLICIES
        .iter()
        .find(|policy| policy.format == format)
    else {
        return Ok(CompanionRequirement::Optional);
    };
    let Some(magic) = policy.required_magic else {
        return Ok(CompanionRequirement::Required);
    };
    let mut header = vec![0_u8; magic.len()];
    let matched = fs::File::open(path)?.read(&mut header)? == magic.len() && header == magic;
    Ok(if matched {
        CompanionRequirement::Required
    } else {
        CompanionRequirement::Skip
    })
}

pub(crate) struct ArchiveMemberRetrySpec {
    pub(crate) extension: &'static str,
    pub(crate) format: &'static str,
    pub(crate) fixture_file: &'static str,
}

const ARCHIVE_MEMBER_RETRY_SPECS: &[ArchiveMemberRetrySpec] = &[ArchiveMemberRetrySpec {
    extension: ".pfs",
    format: "ArtemisAst",
    fixture_file: "fixture.pfs",
}];

pub(crate) fn archive_member_retry_spec(resource: &str) -> Option<&'static ArchiveMemberRetrySpec> {
    let lower = resource.to_ascii_lowercase();
    ARCHIVE_MEMBER_RETRY_SPECS
        .iter()
        .find(|spec| lower.ends_with(spec.extension))
}

pub(crate) struct ArchiveSampleSpec {
    pub(crate) format: &'static str,
    pub(crate) scan_sibling_executables: bool,
    pub(crate) fallback_sample_name: &'static str,
    pub(crate) sample_component: Option<&'static str>,
    pub(crate) passed_version: Option<u64>,
    pub(crate) passed_next_action: Option<&'static str>,
}

const DEFAULT_ARCHIVE_SAMPLE_SPEC: ArchiveSampleSpec = ArchiveSampleSpec {
    format: "",
    scan_sibling_executables: false,
    fallback_sample_name: "sample.dat",
    sample_component: None,
    passed_version: None,
    passed_next_action: None,
};

const ARCHIVE_SAMPLE_SPECS: &[ArchiveSampleSpec] = &[
    ArchiveSampleSpec {
        format: "ArtemisAst",
        scan_sibling_executables: false,
        fallback_sample_name: "sample.ast",
        sample_component: None,
        passed_version: None,
        passed_next_action: None,
    },
    ArchiveSampleSpec {
        format: "CatSystem",
        scan_sibling_executables: true,
        fallback_sample_name: "sample.dat",
        sample_component: None,
        passed_version: None,
        passed_next_action: None,
    },
    ArchiveSampleSpec {
        format: "eagls-script",
        scan_sibling_executables: false,
        fallback_sample_name: "sample.dat",
        sample_component: Some("eagls-script"),
        passed_version: None,
        passed_next_action: None,
    },
    ArchiveSampleSpec {
        format: "willplus-wsc",
        scan_sibling_executables: false,
        fallback_sample_name: "sample.dat",
        sample_component: Some("willplus-wsc"),
        passed_version: None,
        passed_next_action: Some(
            "复制 willplus-wsc 组件，复用对白、姓名、选项和跳转回填，以及 archive.will-v1 的封包读写。有回想标题等独立界面文本时，取含空格的标题与带控制符的对白试写，核对扩展汉字和字体后执行整卷验收。",
        ),
    },
    ArchiveSampleSpec {
        format: "Yuris",
        scan_sibling_executables: false,
        fallback_sample_name: "sample.dat",
        sample_component: None,
        passed_version: Some(474),
        passed_next_action: Some(
            "复制 yuris-ypf474 组件复用该布局的封包读写，脚本沿用内置 Yuris 解析器。用 includeYurisSources 在小样中核对对白、选项、菜单及内部路径和字体，再扩大到全包。",
        ),
    },
    ArchiveSampleSpec {
        format: "Ethornell",
        scan_sibling_executables: false,
        fallback_sample_name: "sample.dat",
        sample_component: None,
        passed_version: None,
        passed_next_action: Some(
            "沿用已通过原文往返的 Ethornell 解析。带头 V1 脚本可用 archive.bgi 的 rebuild 回填到可逐字节复现布局的原卷，保留其余成员。继续验收译文编码与游戏加载",
        ),
    },
];

pub(crate) fn archive_sample_spec(format: &str) -> &'static ArchiveSampleSpec {
    ARCHIVE_SAMPLE_SPECS
        .iter()
        .find(|spec| spec.format == format)
        .unwrap_or(&DEFAULT_ARCHIVE_SAMPLE_SPEC)
}

pub(crate) fn archive_sample_next_action(
    format: &str,
    report: &Value,
    sample_passed: bool,
) -> Option<String> {
    let spec = archive_sample_spec(format);
    let mut action = if sample_passed
        && spec
            .passed_version
            .is_none_or(|version| report["version"].as_u64() == Some(version))
    {
        spec.passed_next_action.map(str::to_owned)
    } else {
        None
    };
    if let Some(executable) = report["keyExecutable"].as_str() {
        let base = action
            .as_deref()
            .or_else(|| report["nextAction"].as_str())
            .unwrap_or("用已验证或保留的 CST 小样继续检查脚本回填")
            .trim_end_matches(['。', '.']);
        action = Some(format!(
            "已用 {executable} 验证加密 INT 的成员名。暂存对应 INT 和 EXE 后，用 archive.kif 的 keyExecutable 参数按成员提取。{base}"
        ));
    }
    action
}

pub(crate) struct ComponentPathArg {
    pub(crate) arg: &'static str,
    pub(crate) allow_resources: bool,
    pub(crate) allow_work: bool,
    pub(crate) extension: Option<&'static str>,
    pub(crate) scope_error: &'static str,
    pub(crate) non_overlap_error: Option<&'static str>,
    pub(crate) wrong_component_error: &'static str,
}

pub(crate) struct ArchiveComponentSpec {
    pub(crate) id: &'static str,
    pub(crate) max_input_bytes: Option<u64>,
    pub(crate) required_filter: Option<(&'static str, &'static str)>,
    pub(crate) extra_inputs: &'static [ComponentPathArg],
    pub(crate) output_path_fields: &'static [(&'static str, &'static str)],
}

const KIF_EXTRA_INPUTS: &[ComponentPathArg] = &[ComponentPathArg {
    arg: "keyExecutable",
    allow_resources: true,
    allow_work: true,
    extension: Some("exe"),
    scope_error: "keyExecutable 仅用于 archive.kif，须为 resources/ 或 work/ 下的 EXE",
    non_overlap_error: None,
    wrong_component_error: "keyExecutable 仅用于 archive.kif，须为 resources/ 或 work/ 下的 EXE",
}];

const WHALE_EXTRA_INPUTS: &[ComponentPathArg] = &[ComponentPathArg {
    arg: "filenameManifest",
    allow_resources: false,
    allow_work: true,
    extension: None,
    scope_error: "文件名清单必须位于 work/。",
    non_overlap_error: Some("文件名清单不能位于组件输出目录。"),
    wrong_component_error: "filenameManifest 仅用于 Whale DAT。",
}];

const WHALE_OUTPUT_FIELDS: &[(&str, &str)] = &[("filenameManifest", "filenames.json")];

pub(crate) const ARCHIVE_COMPONENT_SPECS: &[ArchiveComponentSpec] = &[
    ArchiveComponentSpec {
        id: "archive.rpa3",
        max_input_bytes: None,
        required_filter: None,
        extra_inputs: &[],
        output_path_fields: &[],
    },
    ArchiveComponentSpec {
        id: "archive.xp3",
        max_input_bytes: Some(512 << 20),
        required_filter: Some(("none", "请先确认 XP3 未使用过滤器，再明确指定 filter=none。")),
        extra_inputs: &[],
        output_path_fields: &[],
    },
    ArchiveComponentSpec {
        id: "archive.bgi",
        max_input_bytes: None,
        required_filter: None,
        extra_inputs: &[],
        output_path_fields: &[],
    },
    ArchiveComponentSpec {
        id: "archive.kif",
        max_input_bytes: None,
        required_filter: None,
        extra_inputs: KIF_EXTRA_INPUTS,
        output_path_fields: &[],
    },
    ArchiveComponentSpec {
        id: "archive.ypf474",
        max_input_bytes: None,
        required_filter: None,
        extra_inputs: &[],
        output_path_fields: &[],
    },
    ArchiveComponentSpec {
        id: "archive.ypf482",
        max_input_bytes: None,
        required_filter: None,
        extra_inputs: &[],
        output_path_fields: &[],
    },
    ArchiveComponentSpec {
        id: "archive.will-v1",
        max_input_bytes: None,
        required_filter: None,
        extra_inputs: &[],
        output_path_fields: &[],
    },
    ArchiveComponentSpec {
        id: "archive.pfs8",
        max_input_bytes: None,
        required_filter: None,
        extra_inputs: &[],
        output_path_fields: &[],
    },
    ArchiveComponentSpec {
        id: "archive.nekopack4a",
        max_input_bytes: None,
        required_filter: None,
        extra_inputs: &[],
        output_path_fields: &[],
    },
    ArchiveComponentSpec {
        id: "archive.whale-dat",
        max_input_bytes: None,
        required_filter: None,
        extra_inputs: WHALE_EXTRA_INPUTS,
        output_path_fields: WHALE_OUTPUT_FIELDS,
    },
];

pub(crate) fn archive_component_spec(id: &str) -> Option<&'static ArchiveComponentSpec> {
    ARCHIVE_COMPONENT_SPECS.iter().find(|spec| spec.id == id)
}

pub(crate) fn declared_component_extra_args() -> impl Iterator<Item = &'static ComponentPathArg> {
    ARCHIVE_COMPONENT_SPECS
        .iter()
        .flat_map(|spec| spec.extra_inputs.iter())
}

/// An archive-specific adapter can qualify its own input before common verification.
pub(crate) struct ArchiveTemplate {
    pub(crate) id: &'static str,
    pub(crate) format: &'static str,
    pub(crate) archive_component: Option<&'static str>,
    pub(crate) extensions: &'static [&'static str],
    pub(crate) patterns: &'static [&'static str],
    pub(crate) name: &'static str,
    pub(crate) module: &'static str,
    pub(crate) pass_template_id_to_main: bool,
    pub(crate) bind_vntextpatch_reference: bool,
    pub(crate) extra_sources: &'static [&'static str],
    pub(crate) optional_components: &'static [&'static str],
    pub(crate) discovery_key: Option<&'static str>,
    pub(crate) sample_component: Option<&'static str>,
    pub(crate) prepare_for_model: bool,
    pub(crate) exclusive_format_candidate: bool,
    pub(crate) candidate_limit: usize,
    pub(crate) allow_uncovered_resources: bool,
    pub(crate) filter_qlie_ancillary: bool,
    pub(crate) probe_report_kind: ProbeReportKind,
    pub(crate) extra_runtime_checks: &'static [&'static str],
    pub(crate) prepared_next_action: &'static str,
    distinct_volume_names: bool,
    pub(crate) limitations: &'static str,
}

pub(crate) const ARCHIVE_TEMPLATES: &[ArchiveTemplate] = &[
    ArchiveTemplate {
        id: SYSTEM4_AIN_TEMPLATE,
        format: "System4Ain",
        archive_component: None,
        extensions: &[".ain"],
        patterns: &["**/*.ain"],
        name: "AliceSoft System4 AIN 对话",
        module: "system4_ain_adapter",
        pass_template_id_to_main: false,
        bind_vntextpatch_reference: false,
        extra_sources: &["system4_ain.py"],
        optional_components: &[],
        discovery_key: Some("ainScripts"),
        sample_component: None,
        prepare_for_model: true,
        exclusive_format_candidate: false,
        candidate_limit: 64,
        allow_uncovered_resources: true,
        filter_qlie_ancillary: false,
        probe_report_kind: ProbeReportKind::Ain,
        extra_runtime_checks: &["核对 MSG1 之外的 STR0、EX 字段，以及游戏实际使用的编码和字体。"],
        prepared_next_action: "沿用已验收的 MSG1 读写，只调查其他文字覆盖、字库和显示路径",
        distinct_volume_names: false,
        limitations: "AI2 容器内带 VERS、CODE、FUNC 和 MAIN 边界的 MSG1 消息表，支持变长回填。按完整表验证原文编码。界面、EX、字库及实际显示单独确认",
    },
    ArchiveTemplate {
        id: FLASH_POOL_TEMPLATE,
        format: "FlashAvm1",
        archive_component: None,
        extensions: &[".swf"],
        patterns: &["**/*.swf"],
        name: "Flash AVM1 常量池文本",
        module: "flash_avm1_adapter",
        pass_template_id_to_main: false,
        bind_vntextpatch_reference: false,
        extra_sources: &["flash_avm1.py"],
        optional_components: &[],
        discovery_key: Some("flashMovies"),
        sample_component: None,
        prepare_for_model: true,
        exclusive_format_candidate: true,
        candidate_limit: 1024,
        allow_uncovered_resources: false,
        filter_qlie_ancillary: false,
        probe_report_kind: ProbeReportKind::Movie,
        extra_runtime_checks: &["确认 AVM1 外层脚本以外的界面、嵌套影片和启动程序内文字覆盖。"],
        prepared_next_action: DEFAULT_PREPARED_NEXT_ACTION,
        distinct_volume_names: false,
        limitations: "识别 FWS/CWS 顶层 AVM1 动作中的 UTF-8 常量池。嵌套影片、独立界面与中文运行显示需要另行确认",
    },
    ArchiveTemplate {
        id: ARTEMIS_PFS_TEMPLATE,
        format: "ArtemisAst",
        archive_component: Some("archive.pfs8"),
        extensions: &[".ast"],
        patterns: &["**/*.pfs", "**/*.pfs.*"],
        name: "Artemis PFS v8 + AST 2.0",
        module: "artemis_pfs_adapter",
        pass_template_id_to_main: false,
        bind_vntextpatch_reference: false,
        extra_sources: &[],
        optional_components: &[],
        discovery_key: Some("pfsArchives"),
        sample_component: None,
        prepare_for_model: false,
        exclusive_format_candidate: false,
        candidate_limit: 64,
        allow_uncovered_resources: false,
        filter_qlie_ancillary: false,
        probe_report_kind: ProbeReportKind::Archive,
        extra_runtime_checks: &[],
        prepared_next_action: DEFAULT_PREPARED_NEXT_ACTION,
        distinct_volume_names: false,
        limitations: "UTF-8 AST 2.0 的日文显示字段，完整重建含脚本的 PFS v8 卷。存在重名脚本、未知布局或需覆盖其他脚本时继续调查。游戏内加载与显示需要试译确认",
    },
    ArchiveTemplate {
        id: "nekopack4a-advscript2",
        format: "NekoSDK",
        archive_component: Some("archive.nekopack4a"),
        extensions: &[".txt"],
        patterns: &["**/*.pak"],
        name: "NEKOPACK4A + NekoSDK ADVSCRIPT2",
        module: "nekosdk_adapter",
        pass_template_id_to_main: false,
        bind_vntextpatch_reference: false,
        extra_sources: &["nekosdk_script.py"],
        optional_components: &[],
        discovery_key: None,
        sample_component: Some("nekosdk-script"),
        prepare_for_model: true,
        exclusive_format_candidate: false,
        candidate_limit: 64,
        allow_uncovered_resources: false,
        filter_qlie_ancillary: false,
        probe_report_kind: ProbeReportKind::Archive,
        extra_runtime_checks: &[],
        prepared_next_action: DEFAULT_PREPARED_NEXT_ACTION,
        distinct_volume_names: true,
        limitations: "NEKOPACK4A 内带长度的 ADVSCRIPT2 对白、人名和日志字段，按 CP932 回填并复提。选择项、界面资源和中文运行需按游戏验证",
    },
    ArchiveTemplate {
        id: QLIE_PACK_TEMPLATE,
        format: "Qlie",
        archive_component: Some("python.archives.qlie"),
        extensions: &[".s"],
        patterns: &[
            "GameData/*.pack",
            "DLL/key.fkey",
            "*.exe",
            "*.exe.le.config",
            "GameData/System/config.txt",
            "qlie-l10n/*",
        ],
        name: "QLIE PACK 3.0 + ImoScripter FormatType=1",
        module: "qlie_pack_adapter",
        pass_template_id_to_main: true,
        bind_vntextpatch_reference: true,
        extra_sources: QLIE_TEMPLATE_SOURCES,
        optional_components: &["qlie-japanese-font-launcher"],
        discovery_key: None,
        sample_component: None,
        prepare_for_model: true,
        exclusive_format_candidate: false,
        candidate_limit: 64,
        allow_uncovered_resources: false,
        filter_qlie_ancillary: false,
        probe_report_kind: ProbeReportKind::Archive,
        extra_runtime_checks: &[],
        prepared_next_action: DEFAULT_PREPARED_NEXT_ACTION,
        distinct_volume_names: false,
        limitations: "需要同一启动目录的 EXE 图标密钥、外部与各卷内部密钥、HashVer1.3/1.4 和一致的脚本配置。匹配 ImoScripter 替换表、第 6 号外字槽和 OF 字库时将中文写入游戏位图，沿用原启动方式。其余系统字体布局可生成配套字体，游戏内显示需单独验收",
    },
    ArchiveTemplate {
        id: QLIE_PACK31_TEMPLATE,
        format: "Qlie",
        archive_component: Some("qlie_pack31"),
        extensions: &[".s"],
        patterns: &[
            "GameData/*.pack",
            "*.exe",
            "*.exe.le.config",
            "GameData/System/config.txt",
            "qlie-l10n/*",
        ],
        name: "QLIE PACK 3.1 + ImoScripter FormatType=1",
        module: "qlie_pack_adapter",
        pass_template_id_to_main: true,
        bind_vntextpatch_reference: true,
        extra_sources: QLIE_TEMPLATE_SOURCES,
        optional_components: &["qlie-japanese-font-launcher"],
        discovery_key: None,
        sample_component: None,
        prepare_for_model: true,
        exclusive_format_candidate: false,
        candidate_limit: 64,
        allow_uncovered_resources: false,
        filter_qlie_ancillary: true,
        probe_report_kind: ProbeReportKind::Archive,
        extra_runtime_checks: &[],
        prepared_next_action: DEFAULT_PREPARED_NEXT_ACTION,
        distinct_volume_names: false,
        limitations: "支持带首项卷内密钥的 FilePack 3.1 和 HashVer1.4。确认原生 Unicode 对白和启动字体表时使用 UTF-16 和微软雅黑。其他布局按替换表、外字槽及系统字体资源选择显示方案。系统文本、卷覆盖顺序和游戏显示需单独验收",
    },
];

pub(crate) struct BundledTemplate {
    pub(crate) id: &'static str,
    pub(crate) format: &'static str,
    pub(crate) archive_component: Option<&'static str>,
    pub(crate) extensions: &'static [&'static str],
    pub(crate) patterns: &'static [&'static str],
    pub(crate) name: &'static str,
    pub(crate) module: &'static str,
    pub(crate) component_files: &'static [&'static str],
    pub(crate) required_filter: Option<(&'static str, &'static str)>,
}

const SCRIPT_TEMPLATE_COMPONENTS: &[&str] = &["engine_archive.py"];
const RENPY_COMPILED_COMPONENTS: &[&str] = &[
    "engine_archive.py",
    "renpy_font.py",
    "renpy_rpyc_adapter.py",
    "renpy_mixed_adapter.py",
];

const BUNDLED_TEMPLATES: &[BundledTemplate] = &[
    BundledTemplate {
        id: "loose-renpy",
        format: "Renpy",
        archive_component: None,
        extensions: &[".rpy"],
        patterns: &["**/*.rpy"],
        name: "Ren’Py 源脚本",
        module: "script_template",
        component_files: SCRIPT_TEMPLATE_COMPONENTS,
        required_filter: None,
    },
    BundledTemplate {
        id: "loose-kirikiri-ks",
        format: "KirikiriKs",
        archive_component: None,
        extensions: &[".ks"],
        patterns: &["**/*.ks"],
        name: "Kirikiri KS 源脚本",
        module: "script_template",
        component_files: SCRIPT_TEMPLATE_COMPONENTS,
        required_filter: None,
    },
    BundledTemplate {
        id: "rpa3-renpy",
        format: "Renpy",
        archive_component: Some("archive.rpa3"),
        extensions: &[".rpy"],
        patterns: &["**/*.rpa"],
        name: "RPA3 + Ren’Py 源脚本",
        module: "script_template",
        component_files: SCRIPT_TEMPLATE_COMPONENTS,
        required_filter: None,
    },
    BundledTemplate {
        id: "loose-renpy-rpyc",
        format: "Renpy",
        archive_component: None,
        extensions: &[".rpyc"],
        patterns: &["**/*.rpyc"],
        name: "Ren’Py RPYC2 编译脚本",
        module: "renpy_rpyc_adapter",
        component_files: RENPY_COMPILED_COMPONENTS,
        required_filter: None,
    },
    BundledTemplate {
        id: "rpa3-renpy-rpyc",
        format: "Renpy",
        archive_component: Some("archive.rpa3"),
        extensions: &[".rpyc"],
        patterns: &["**/*.rpa"],
        name: "RPA3 + Ren’Py RPYC2 编译脚本",
        module: "renpy_rpyc_adapter",
        component_files: RENPY_COMPILED_COMPONENTS,
        required_filter: None,
    },
    BundledTemplate {
        id: "renpy-mixed",
        format: "Renpy",
        archive_component: Some("archive.rpa3"),
        extensions: &[".rpy", ".rpyc"],
        patterns: &["**/*.rpy", "**/*.rpyc", "**/*.rpa"],
        name: "Ren’Py 无冲突混合资源",
        module: "renpy_mixed_adapter",
        component_files: RENPY_COMPILED_COMPONENTS,
        required_filter: None,
    },
    BundledTemplate {
        id: "xp3-kirikiri-ks",
        format: "KirikiriKs",
        archive_component: Some("archive.xp3"),
        extensions: &[".ks"],
        patterns: &["**/*.xp3"],
        name: "无过滤器 XP3 + Kirikiri KS",
        module: "script_template",
        component_files: SCRIPT_TEMPLATE_COMPONENTS,
        required_filter: Some(("none", "XP3 模板要求已独立确认 filter=none。")),
    },
];

pub(crate) struct TemplateComposition {
    pub(crate) module: &'static str,
    pub(crate) pass_template_id_to_main: bool,
    pub(crate) writes_template_config: bool,
    pub(crate) embedded_script_template: bool,
    pub(crate) bind_vntextpatch_reference: bool,
    pub(crate) required_filter: Option<(&'static str, &'static str)>,
    pub(crate) component_files: Vec<String>,
    pub(crate) optional_components: &'static [&'static str],
    pub(crate) limitations: &'static str,
}

pub(crate) fn template_composition(id: &str) -> Option<TemplateComposition> {
    if let Some(template) = ARCHIVE_TEMPLATES.iter().find(|template| template.id == id) {
        let mut component_files = Vec::new();
        if template.archive_component.is_some() {
            component_files.push("engine_archive.py".to_owned());
        }
        component_files.push(format!("{}.py", template.module));
        component_files.extend(template.extra_sources.iter().map(|source| (*source).to_owned()));
        return Some(TemplateComposition {
            module: template.module,
            pass_template_id_to_main: template.pass_template_id_to_main,
            writes_template_config: false,
            embedded_script_template: false,
            bind_vntextpatch_reference: template.bind_vntextpatch_reference,
            required_filter: None,
            component_files,
            optional_components: template.optional_components,
            limitations: template.limitations,
        });
    }
    let template = BUNDLED_TEMPLATES.iter().find(|template| template.id == id)?;
    Some(TemplateComposition {
        module: template.module,
        pass_template_id_to_main: false,
        writes_template_config: true,
        embedded_script_template: template.module == "script_template",
        bind_vntextpatch_reference: true,
        required_filter: template.required_filter,
        component_files: template
            .component_files
            .iter()
            .map(|file| (*file).to_owned())
            .collect(),
        optional_components: &[],
        limitations: DEFAULT_TEMPLATE_LIMITATIONS,
    })
}

pub(crate) fn check_reused_archive_scope(
    adapter_id: &str,
    required_files: &[String],
) -> Result<(), EngineError> {
    // Check old published versions whose probe predates the template's scope check.
    let Some(template) = adapter_id.strip_prefix("template.").and_then(|id| {
        ARCHIVE_TEMPLATES
            .iter()
            .find(|template| template.id == id && template.distinct_volume_names)
    }) else {
        return Ok(());
    };
    let mut by_name = BTreeMap::new();
    for path in required_files.iter().filter(|path| {
        template
            .patterns
            .iter()
            .any(|pattern| glob_match(pattern, path))
    }) {
        let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
        if let Some(other) = by_name.insert(name.to_lowercase(), path) {
            return Err(EngineError::Incompatible {
                stage: "probe",
                message: format!(
                    "不同目录的同名脚本卷可能属于不同启动范围，请先选择当前游戏加载的卷：{other}、{path}"
                ),
            });
        }
    }
    Ok(())
}

pub(crate) fn template_catalog() -> Vec<Value> {
    let bundled = BUNDLED_TEMPLATES.iter().map(|template| {
        (
            template.id,
            template.format,
            template.archive_component,
            template.extensions.to_vec(),
            template.patterns.to_vec(),
            template.name,
        )
    });
    let archives = ARCHIVE_TEMPLATES.iter().map(|template| {
        (
            template.id,
            template.format,
            template.archive_component,
            template.extensions.to_vec(),
            template.patterns.to_vec(),
            template.name,
        )
    });
    bundled.into_iter().chain(archives).map(|(id, format, archive, extensions, patterns, name)| {
        let mut template = json!({"id":id,"version":1,"format":format,"archiveComponent":archive,
            "scriptExtensions":extensions,"resourcePatterns":patterns,
            "family":{"kind":"template","key":id,"name":name,"adapterId":format!("template.{id}")}});
        let required = match id {
            QLIE_PACK_TEMPLATE | QLIE_PACK31_TEMPLATE => json!({"patterns":["GameData/*.pack"]}),
            SYSTEM4_AIN_TEMPLATE => json!({"patterns":patterns,"signatures":[{"offset":0,"hex":"41493200"}]}),
            ARTEMIS_PFS_TEMPLATE => json!({"patterns":patterns,"signatures":[{"offset":0,"hex":"706638"}]}),
            _ => Value::Null,
        };
        if !required.is_null() {
            template["recognition"] = json!({"schemaVersion":1,"required":[required]});
        }
        if id == FLASH_POOL_TEMPLATE {
            template["scriptComponent"] = json!("flash-avm1");
        } else if id == SYSTEM4_AIN_TEMPLATE {
            template["scriptComponent"] = json!("system4-ain");
        }
        template
    }).collect()
}

pub(crate) fn engine_components() -> Vec<Value> {
    vec![
        json!({"id":"archive.rpa3","version":2,"format":"RPA/ARC 3.0 single-chunk","module":"python.archives.rpa","operations":["inspect","extract","rebuild"],"limitations":["RPA 2/4, multi-chunk entries and scrambled archives are unsupported; compiled scripts require the RPYC2 template.","Only a data-only pickle index is accepted; executable pickle operations are rejected."]}),
        json!({"id":"archive.xp3","version":1,"format":"Standard XP3 v1, filter=none","module":"python.archives.xp3","operations":["inspect","extract","rebuild"],"standaloneBuild":{"call":"python.archives.xp3.build([(name, bytes), ...], filter_name='none')","condition":"Use only with established unfiltered loading and override precedence; the writer does not establish either. Call the pinned API from Python, not run_engine_component.","fallback":"Keep replace output when these facts are unavailable; no extra research solely for patch size."},"limitations":["Explicit filter=none is required from independent engine evidence; zero flags do not establish this.","Encrypted/filtered, chained, obfuscated and executable-overlay XP3 are unsupported.","Changed archives use standard v1 layout; game patch precedence and runtime compatibility require verification."]}),
        json!({"id":"archive.bgi","version":1,"format":"PackFile / BURIKO ARC20","module":"python.archives.bgi","operations":["inspect","extract","rebuild"],"limitations":["Extract decodes raw and DSC members.", "Rebuild accepts decoded headered BGI V1 script replacements in archives up to 64 MiB with a byte-reproducible layout and preserves the original raw/DSC mode.", "Pass selected .bgi or extensionless scripts to Ethornell; confirm CP932 and runtime separately."]}),
        json!({"id":"archive.kif","version":1,"format":"CatSystem2 KIF / INT","module":"python.archives.catsystem2_int","operations":["inspect","extract"],"limitations":["Encrypted script volumes require a staged keyExecutable with a filename key verified against CatScene members.", "Selected .cst members can be tried with CatSystem."]}),
        json!({"id":"archive.ypf474","version":1,"format":"YU-RIS YPF 474, 32-bit offsets, FF name XOR, CRC32 names and Adler32 payloads","module":"engine_archive","operations":["inspect","extract","rebuild"],"limitations":["Rebuild script volumes up to 64 MiB using decoded replacement members. Pass each YSTB sample with its same-directory ysc.ybn."]}),
        json!({"id":"archive.ypf482","version":1,"format":"YU-RIS YPF 482 with Murmur2 and FF name XOR","module":"python.archives.yuris_482","operations":["inspect","extract"],"limitations":["Use a proven scheme for other YPF versions or layouts.", "Pass YSTB scripts with their same-directory ysc.ybn; encrypted YBN needs separate evidence."]}),
        json!({"id":"archive.will-v1","version":1,"format":"Will ARC v1, extension groups with 13-byte base names","module":"engine_archive","operations":["inspect","extract","rebuild"],"limitations":["Member payloads retain their stored rotation. Use willplus-wsc on complete stored WSC members. Rebuild volumes up to 64 MiB."]}),
        json!({"id":"archive.pfs8","version":1,"format":"Artemis PFS v8","module":"python.archives.pfs","operations":["inspect","extract"],"limitations":["Member reads use the index-derived key; verify the script layout and load order separately."]}),
        json!({"id":"archive.nekopack4a","version":1,"format":"NekoSDK NEKOPACK4A","module":"engine_archive","operations":["inspect","extract"],"limitations":["Inspect reads only the index. Select .txt members and verify ADVSCRIPT2 with nekosdk-script. Combine with the nekopack4a-advscript2 template for CP932 script replacement."]}),
        json!({"id":"archive.whale-dat","version":1,"format":"Whale DAT v2 with CRC64 index","module":"whale_dat","operations":["inspect","extract","rebuild"],"limitations":["Provide exact gameTitle for compressed scripts. Extracted scripts can produce filenameManifest, a candidate list for matching asset hashes in other DAT volumes. Unresolved names are counted in inspection. Rebuild supports script-only DAT volumes."]}),
    ]
}

pub(crate) fn siglus_description() -> Value {
    json!({"id":"engine.siglus","version":"1.0.0","name":"Siglus / Scene.pck","upstream":"SiglusSceneScriptUtility 0.5.7 (Unlicense OR 0BSD)",
        "capabilities":["Scene.pck 解包及重封包","内嵌源码查询","字节码参数字符串引用","变长 UTF-16 回填","GDI 字体检查及 FKDS 图片字库补字","独立中文启动入口"],
        "templates":["标准 CD_TEXT/CD_NAME","FKDS 气泡对白"],
        "sources":["gamel10n_siglus.py","siglus_binary.py","siglus_font.py","siglus_ssu/pck.py","siglus_ssu/dat.py","siglus_ssu/g00.py","siglus_ssu/disam.py","siglus_ssu/GEI.py"],
        "usage":concat!("inspect_siglus_game 直接分析当前游戏，返回结构、已识别文本和待处理调用；source/query 按需读取内嵌源码。",
            "copy_adapter id=engine.siglus/version=1.0.0 生成固定依赖和 JSON 调用桥。规则放在 src/tools/siglus-rules.json，",
            "commandRules:[{command:'精确函数名',argument:1,kind:'dialogue'}] 的 argument 从 0 开始。",
            "仅选择源码或已验证语义确认的显示参数；不要按字符串内容猜测引用。test_adapter 仍须通过。"),
        "limits":"引擎版本、显示模板和 EXE 补丁特征分别检查；未识别的调用不自动翻译，游戏实际显示需试译确认。"})
}

fn namespace(format: &str) -> Option<&'static str> {
    match format {
        "AdvHd" => Some("AdvHd"),
        "Age" => Some("ArcGameEngine"),
        "ArtemisAsb" | "ArtemisAst" | "ArtemisTxt" => Some("Artemis"),
        "Ethornell" => Some("Ethornell"),
        "KirikiriKs" | "KirikiriSoc" | "KirikiriTjs" => Some("Kirikiri"),
        "Majiro" => Some("Majiro"),
        "Mware" => Some("Mware"),
        "Propeller" => Some("Propeller"),
        "RealLive" => Some("RealLive"),
        "ShSystem" => Some("ShSystem"),
        "SilkysMap" | "SilkysMes" => Some("Silkys"),
        "Softpal" => Some("Softpal"),
        "SystemNnnDev" | "SystemNnnRelease" => Some("SystemNnn"),
        "TmrHiroAdvSystemCode" | "TmrHiroAdvSystemText" => Some("TmrHiroAdvSystem"),
        "Yuris" => Some("Yuris"),
        _ => None,
    }
}

fn reference(format: &str) -> Option<(&'static str, &'static str)> {
    match format {
        "ArtemisAsb" | "ArtemisAst" | "ArtemisTxt" => {
            Some(("reference/Artemis_pfs_overlay.md", ARTEMIS_REFERENCE))
        }
        "Kaguya" => Some(("reference/Kaguya_MESSAGE_v4_0_runtime.md", KAGUYA_REFERENCE)),
        "Yuris" => Some(("reference/Yuris_text_encoding.md", YURIS_REFERENCE)),
        "KirikiriKs" | "KirikiriSoc" => {
            Some(("reference/Kirikiri_text_encoding.md", KIRIKIRI_REFERENCE))
        }
        "Renpy" => Some(("reference/Renpy_runtime.md", RENPY_REFERENCE)),
        _ => None,
    }
}

fn page_source(
    format: &str,
    path: &str,
    content: &str,
    args: &Value,
) -> Result<Value, EngineError> {
    let number = |key: &str, default: u64, min: u64, max: u64| -> Result<usize, EngineError> {
        let value = match args.get(key) {
            Some(value) => value
                .as_u64()
                .ok_or_else(|| EngineError::Protocol(format!("{key} 必须为整数。")))?,
            None => default,
        };
        if !(min..=max).contains(&value) {
            return Err(EngineError::Protocol(format!("{key} 超出允许范围。")));
        }
        Ok(value as usize)
    };
    let units: Vec<_> = content.encode_utf16().collect();
    let start = number("offset", 0, 0, i32::MAX as u64)?.min(units.len());
    let end = (start + number("length", 12000, 1, 24000)?).min(units.len());
    Ok(
        json!({"format":format,"path":path,"offset":start,"length":end-start,"totalLength":units.len(),"nextOffset":end,"truncated":end<units.len(),"content":String::from_utf16_lossy(&units[start..end])}),
    )
}

async fn read_siglus_source(
    service: &EngineService,
    prepared: &Value,
    path: &str,
    args: &Value,
    cancellation: &CancellationToken,
) -> Result<Value, EngineError> {
    let library = Path::new(text(prepared, "library")?);
    let mut params = args.clone();
    params["path"] = json!(path);
    if library.is_file() {
        params["operation"] = json!("library");
        params["library"] = json!(library);
    } else {
        params["operation"] = json!("read_file");
        params["root"] = json!(library);
        params["encoding"] = json!("utf8");
    }
    service.helper(&params, cancellation).await
}

pub(crate) async fn inspect_builtin(
    service: &EngineService,
    request: &AdaptationRequest<'_>,
    state: &Session,
    args: &Value,
    cancellation: &CancellationToken,
) -> Result<Value, EngineError> {
    let key = text(args, "format")?;
    if key.eq_ignore_ascii_case("Siglus") || key == "engine.siglus" {
        if let Some(path) = args.get("path").and_then(Value::as_str) {
            let prepared = service.siglus(cancellation).await?;
            return read_siglus_source(service, &prepared, path, args, cancellation).await;
        }
        return Ok(siglus_description());
    }
    if key.eq_ignore_ascii_case("NekoSDK") {
        return Ok(
            json!({"format":key,"status":"component","engine":"nekosdk","archiveComponent":"archive.nekopack4a","scriptComponent":"nekosdk-script","template":"nekopack4a-advscript2","usage":"先复用 automaticArchiveTrials 或对完整 ADVSCRIPT2 成员试用 nekosdk-script。组合模板按 CP932 提取、回填和复提；按游戏确认选项、界面和中文运行。"}),
        );
    }
    if key.eq_ignore_ascii_case("BSScriptRev6") || key == "reference.bsscript-rev6" {
        return Ok(
            json!({"format":"BSScriptRev6","status":"reference-only","usage":BSSCRIPT_GUIDANCE}),
        );
    }
    let format = BUILTIN_FORMATS
        .iter()
        .find(|format| format.key.eq_ignore_ascii_case(key))
        .ok_or_else(|| EngineError::Protocol(format!("未知内置脚本格式：{key}")))?;
    let key = format.key;
    let parser_type = format!(
        "VNTextPatch.Shared.Scripts.{}{key}Script",
        namespace(key)
            .map(|namespace| format!("{namespace}."))
            .unwrap_or_default()
    );
    let binary = service.tools.vntextpatch();
    let bundle = binary
        .parent()
        .unwrap_or(Path::new("."))
        .join("sources.zip");
    if let Some(path) = args.get("path") {
        let path = path
            .as_str()
            .ok_or_else(|| EngineError::Protocol("path 必须为字符串。".into()))?
            .replace('\\', "/");
        if path == "tools/gamel10n_host.py" {
            return page_source(key, &path, HELPER, args);
        }
        if let Some((reference, content)) =
            reference(key).filter(|(reference, _)| *reference == path)
        {
            return page_source(key, reference, content, args);
        }
        let mut params = args.clone();
        params["operation"] = json!("builtin_sources");
        params["format"] = json!(key);
        params["path"] = json!(path);
        params["bundle"] = json!(bundle);
        params["parserType"] = json!(parser_type);
        return service.helper(&params, cancellation).await;
    }
    let source = service
        .helper(
            &json!({"operation":"builtin_sources","bundle":bundle,"parserType":parser_type}),
            cancellation,
        )
        .await;
    if matches!(source, Err(EngineError::Cancelled)) {
        return Err(EngineError::Cancelled);
    }
    let mut files = source
        .as_ref()
        .ok()
        .and_then(|source| source["sourceFiles"].as_array())
        .cloned()
        .unwrap_or_default();
    files.push(json!({"path":"tools/gamel10n_host.py","bytes":HELPER.len()}));
    if let Some((path, content)) = reference(key) {
        files.push(json!({"path":path,"bytes":content.len()}));
    }
    files.sort_by(|a, b| a["path"].as_str().cmp(&b["path"].as_str()));
    let issue=source.as_ref().err().map(|error|json!({"code":if bundle.is_file(){"corrupt"}else{"missing"},"asset":"sources.zip","message":error.to_string(),"recovery":"源码包故障不代表引擎不受支持。binaryAvailable=true 时，继续用 try_builtin_adapter 验证兼容性，并以 copy_adapter 复用固定解析器；恢复源码包后可读取结构参考，新增逻辑使用 Python。"}));
    let mut limitations = vec![
        "仅提供已解包脚本的提取、回填和复提校验；资源包的解包/重打包、运行时编码补丁、启动代理及字体支持需要由适配器实现并验证。",
        if matches!(key, "KirikiriKs" | "KirikiriSoc") {
            "KS/SOC 支持读取 CP932、带 BOM 的 UTF-8/UTF-16，修改后写 UTF-16；须验证游戏实际脚本加载路径接受该编码，原文无修改仍保留原字节。"
        } else if key == "Yuris" {
            "默认读写严格 CP932，包括上游名称为 SjisTunnelEncoding 的默认实现。显式 yurisEncoding 可使用固定 sjis-tunnel 映射读写；不自动生成映射、代理或字体，不证明游戏运行兼容。"
        } else {
            "保留内置解析器的原编码约束；Shift-JIS 无法表示的中文会明确报错，不生成未验证的 sjis_ext.bin。"
        },
        "生成适配器的 probe 只获得最多 65536 字节文件头，必须由 Python 按签名、长度和版本识别；调用桥禁止直接使用整文件解析器执行 probe。",
        "解析器源码仅供了解格式；发行包不含 C# 编译器，需要修改的适配逻辑使用 Python，固定解析器通过预编译组件调用。",
    ];
    limitations.push(match key {
        "Kaguya"=>KAGUYA_WARNING,"Renpy"=>"Ren’Py 仅处理单行对白、旁白和菜单；Python、界面及动态表达式不在此解析器范围。",
        "Yuris"=>"YSTB 的指令、属性描述、值、行号区分别重置 XOR 游标；属性描述为 12 字节，类型位于 +2，值长度 +4，值区相对偏移 +8。无属性的脚本不含可提取文本，也无法用首属性推断密钥，应保留原字节。命令和属性含义以当前 ysc.ybn 为准；清点文本请复用 YurisCommandList/YurisScenarioScript，勿把资源路径或变量标识作为显示文字。YPF 容器和中文运行支持仍需单独验证。",
        "KirikiriTjs"|"Mware"=>"编译脚本沿用上游文本识别规则并排除明显资源路径，试译前需要复核变量和表达式。",
        "TmrHiroAdvSystemText"=>"文本表没有引擎签名，只验证记录和文本结构；不能仅凭兼容记录认定游戏引擎。",_=>"解析器版本兼容性与游戏实际加载行为仍须针对当前游戏验证。",
    });
    let guidance = if key == "Kaguya" {
        kaguya_guidance(kaguya_inspection(
            &request.game_directory,
            state,
            cancellation,
        )?)
    } else if matches!(key, "KirikiriKs" | "KirikiriSoc") {
        kirikiri_guidance()
    } else if matches!(key, "ArtemisAsb" | "ArtemisAst" | "ArtemisTxt") {
        json!({
            "patchArchives":"现有证据已确认独立 PFS 覆盖卷与加载优先级时，复用 python.archives.pfs.build 写入修改成员及必要伴随资源，build.mode='overlay'；按原资源＋补丁复提，保留公共文本身份。原文无修改可返回空 files。接入见 reference/Artemis_pfs_overlay.md。未知规则保留 replace，不为体积增加研究、下载、Hook 开发或验收阶段。",
            "naming":"bounded probe 从候选文件名选取同族最大数字卷之后的路径，放入 buildHints，后续构建稳定复用。媒体卷只需名称或索引时不加入 requiredFiles，不复制整包。"
        })
    } else if key == "Yuris" {
        json!({
            "textSources":"需要区分显示文字与内部字段时，调用 parser(request, 'Yuris', operation='extract', includeYurisSources=True)。响应每条 unit 的 yurisSource 提供 groupIndex、role，以及 attributes 中的 commandIndex/command/attributeIndex/attributeId/attribute/attributeType；索引从 0 起，attributeIndex 为脚本属性描述符序号。姓名与正文共享 groupIndex，YSCF 标题为 role=caption、attributes=[]。默认提取省略此字段，ID、locator、context 和文本数量不变。",
            "selection":"先在小样中按当前 ysc.ybn 核对显示字段的正反例。GOSUB 的可提取参数附带 subroutine，可区分 ES.SEL.SET 选项与 ES.CHAR.NAME 姓名；核对 MENU.NAME、DIALOG.STR/CAPTION 和实际引用这些显示位置的赋值。路径与字体名不因含日文而入选，LET/STR 也不能一律排除。复用同次提取的来源信息，保留原 ID、locator、context 与分组。",
            "roundtrip":"原生回填依据原姓名/正文分组保留边界，支持括号独白的前后缀及整句改写。出现复提差异时，先按诊断中的脚本和 string:N 做局部回填、复提对比，再执行完整 test_adapter；原文逐字节一致、译文准确、未修改项不变均须验证。",
            "encoding":"将原始解包脚本直接交给固定解析器，不准备 Shadow 副本。有修改且需隧道时，先用 sjis-tunnel 组件统一规划映射，再传 yurisEncoding={'source':'cp932','output':'sjis-tunnel','mapping':''.join(tunnel.chars)}；原生不新增槽位，按 output 复提核对。已有隧道输入用 source='sjis-tunnel' 及原映射。接入示例见 reference/Yuris_text_encoding.md。",
            "unchangedBuild":"先校验译文 ID，再剔除等于原文的条目。无实际修改时，直接调用原生 build，source/output 均沿用输入编码及已有映射；跳过新映射规划和全脚本转换，保留原字节与既有伴随文件，不安装新运行组件。封包无成员修改时复制原包，仍执行所需验收。复用一次提取的 yurisSource，勿逐条重新解密脚本。",
            "dependency":"新适配器由 copy_adapter 登记当前解析器；已有适配器继续保留原 pin。旧版本缺少接口时，保留 src、ID 和译文并报告能力缺口，不重建适配器、不重置源码或身份、不手改依赖 hash；仅经受支持的显式依赖更新并重新验收后使用新接口。",
            "runtime":"YPF 容器、字符遍历、中文字体与代理加载仍需验证。inspect_adapter_component(id='vntextproxy-runtime') 提供固定官方代理包、成员、字体规则和准备入口；它不代表当前游戏运行通过。映射必须跨脚本共用且保留已有槽位顺序，不能逐文件重建。"
        })
    } else {
        Value::Null
    };
    let catalog = builtin_catalog()
        .into_iter()
        .find(|format| format["format"] == key)
        .unwrap();
    let usage = format!(
        "在 src/adapter.py 实现协议入口。from tools.gamel10n_host import parser, write_response；parser(request, '{key}') 调用固定解析器并返回响应字典。"
    ) + "先解包到独立工作目录，再传入该目录及相对 resourceFiles；build 的 outputDirectory 必须与 sourceDirectory 分开，随后按已确认的加载规则重封包或生成增量覆盖包，声明所有最终输出。"
        + "仅登记固定版本的共享解析工具，由预编译宿主通过进程协议调用，不复制到项目。解析器不支持的逻辑使用 Python 实现。"
        + "最终响应用 write_response(request, response) 写入 responseFile；诊断写 stderr。probe 由入口独立实现，不能用整文件解析器猜测截断输入。";
    let example = format!(
        "from tools.gamel10n_host import parser, write_response\n# Implement bounded probe separately; this full parser handles extract/build/validate.\nresponse = parser(request, \"{key}\", sourceDirectory=unpacked_scripts_directory,\n                  resourceFiles=script_relative_paths, responseFile=None)\n# Build also needs outputDirectory and translationsFile (or translations).\n# Repack into the game's resource format and return final resource paths before write_response."
    );
    Ok(
        json!({"format":key,"name":format.name,"parserType":parser_type,"upstreamCommit":"e68e174f3db6181e03fc7fabfa7570e1b293d148","extensions":catalog["extensions"],"companionFileNames":catalog["companions"],"operations":["extract","build","validate"],"binaryAvailable":binary.is_file(),"sourceAvailable":source.is_ok(),"sourceBundleIssue":issue,"sourceFiles":files,"limitations":limitations,"usage":usage,"example":example,"engineGuidance":guidance}),
    )
}

fn kirikiri_guidance() -> Value {
    json!({"parserReuse":crate::adaptation_context::KIRIKIRI_SUMMARY,
        "scn":"PSB / KAGEX SCN 使用现有 kirikiri-scn-text 组件：inspect_adapter_component(id='kirikiri-scn-text')，复制后对已解密样本运行其 verify 入口。PSB 头或头部校验通过不能证明全文已解密；先依据解析器的具体错误定位过滤器或布局缺口，不重新实现字符串池、偏移表及场景语义。KS/SOC/TJS 则分别试用对应内置解析器。",
        "smilProfile":"File/smil 索引先用 xp3-smil 恢复名称并保留原始别名；同名启动成员可能含保护用伪条目。SMIL 不代表某个固定过滤器，须对多个实际成员验证参数及明文校验，再区分 TJS2100、TJS 源码、KS 和 PSB SCN。内置 SMIL/SmileCrypt 配置只接受其有界样本验证通过的变体。",
        "archiveSupport":"XP3 容器、脚本语义、中文编码和字体是不同能力。现有适配器失败时先检查可复制的源码；容器组件拒绝过滤标记或超出预算，不代表 KS 解析器不可复用。TJS 源码与 TJS2100 编译脚本分别处理。",
        "patchArchives":"若现有初始化脚本或已验证的同类运行时资料已明确 patch 卷或裸文件覆盖及搜索优先级，优先复用已有 writer 与加载路径输出修改脚本、配置和必要字体。已掌握过滤器时沿用，不能为做小包改成无过滤器或另加 DLL；仅已确认无过滤器时可用 python.archives.xp3.build。规则或现成能力不明时保留 replace，不为体积另开研究、下载、Hook 开发或额外验收。",
        "minimalTrial":"先选一个短 KS 和含姓名/选项的代表样本：原文往返、少量中文及保留符号（如 ♪、・）、局部修改后复提。比较实际译文、ID、位置和所有未修改项；封包重建另核对修改与未修改条目的清单及内容。小样通过后再完整 test_adapter。",
        "runtimeEvidence":"确认脚本字节缓冲实际到达的解码函数及编码选择条件。导入表、宿主 ACP、几种指令字节未命中或自定义 encoder/decoder 往返，都不能证明游戏的脚本编码或显示支持。",
        "fontLayout":"Unicode 脚本可读写不等于原日文字体覆盖中文。检查实际 userFace/defaultFace、System.addFont、mapPrerenderedFont 及 font/reset/restore 路径；先用 windows-font-metrics 在同高度/宽度/charset 下比较原字体和覆盖中文的候选字体。缺字时 getTextWidth 与 drawText 可能使用不同回退或度量，造成个别字重叠；需让测宽和绘制使用同一套已覆盖的字形。优先使用引擎已有字体配置或集中字体选择点，保留原字号、风格及正常日文/符号/ASCII；不要插空格、改译文、全局加 pitch、缩小全文或套 QLIE 专用度量。核对 chDefaultFace 等系统设置及读档恢复是否覆盖新配置，不删除用户存档；必要时只迁移已确认的原字体值。原文构建仍须逐字节一致；中文构建才按已确认条件修改字体配置，并独立核对配置已写入实际生效资源。预渲染字库/GDI 以外后端须单独检查；独立测量通过仍需试译确认游戏内排版。",
        "referenceSource":"reference/Kirikiri_text_encoding.md"})
}

fn kaguya_guidance(inspection: Value) -> Value {
    json!({"scriptFormat":"[SCR-MESSAGE]ver4.0","parserReuse":"优先复用现有 KaguyaScript / BuiltinScriptParser 的提取、回填和复提校验，不重写 MESSAGE 解析器。仅确认版本或结构不兼容时才编辑源码。",
        "translationMemory":"沿用原提取结果的 UnitId（协议 id）、ResourcePath、Locator、Context、Speaker 和 Kind；包装适配器不得重新编号或按显示类型重新分类，以免已有翻译记忆失效。","runtimeSupport":KAGUYA_WARNING,
        "nextSteps":["在补丁生成时对实际输出的 Unicode 文本统计 CP932 不可编码字符和可用码位；没有译文时先用少量代表中文验证方案，不要求用户先全量翻译。数量随译文变化，不能固化样本缺字数，也不能把映射后的占位字符存回翻译记忆。","F040/F041 在已调查运行库中是玩家姓名控制码；不要分配给中文字形。其他 PUA 码位仍须验证字节遍历、控制码冲突和绘制路径。","先确认实际运行库和 GDI 导入，再协调编码、TextOutA 字节解释与字体方案；仅换字体或把 CP932 改成 CP936 不能证明游戏已支持中文。","未知运行库须沿导入和 TextInitialize/绘制调用链重新定位；静态线索和独立 GDI 字宽测量都不等于游戏内验收。"],
        "runtimeInspection":inspection,"referenceSource":"reference/Kaguya_MESSAGE_v4_0_runtime.md"})
}

fn kaguya_inspection(
    game: &Path,
    state: &Session,
    cancellation: &CancellationToken,
) -> Result<Value, EngineError> {
    let candidates = state
        .extra
        .get("builtinDiscovery")
        .and_then(|discovery| discovery["candidates"].as_array())
        .into_iter()
        .flatten()
        .filter(|candidate| {
            candidate["format"]
                .as_str()
                .is_some_and(|format| format.eq_ignore_ascii_case("Kaguya"))
        })
        .flat_map(|candidate| candidate["sampleFiles"].as_array().into_iter().flatten());
    let probes = state
        .extra
        .get("builtinProbes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|probe| {
            probe["format"]
                .as_str()
                .is_some_and(|format| format.eq_ignore_ascii_case("Kaguya"))
        })
        .flat_map(|probe| probe["resources"].as_array().into_iter().flatten());
    let mut seen = BTreeSet::new();
    let resources: Vec<_> = candidates
        .chain(probes)
        .filter_map(Value::as_str)
        .filter(|path| seen.insert(path.to_lowercase()))
        .take(8)
        .collect();
    let mut messages = Vec::new();
    let mut directories = Vec::<PathBuf>::new();
    let mut diagnostics = Vec::new();
    let root = game.canonicalize()?;
    for resource in resources {
        if cancellation.is_cancelled() {
            return Err(EngineError::Cancelled);
        }
        let result = (|| -> Result<(), EngineError> {
            let path = resolve_relative(&root, resource)?;
            let mut file = fs::File::open(&path)?;
            let mut header = [0; b"[SCR-MESSAGE]ver4.0".len()];
            if file.metadata()?.len() < header.len() as u64 {
                return Ok(());
            }
            file.read_exact(&mut header)?;
            if header != *b"[SCR-MESSAGE]ver4.0" {
                return Ok(());
            }
            messages.push(resource.replace('\\', "/"));
            for directory in path.parent().into_iter().flat_map(Path::ancestors) {
                if !directories.iter().any(|known| {
                    known
                        .to_string_lossy()
                        .eq_ignore_ascii_case(&directory.to_string_lossy())
                }) {
                    directories.push(directory.into());
                }
                if directory == root || directories.len() >= 24 {
                    break;
                }
            }
            Ok(())
        })();
        if let Err(error) = result
            && diagnostics.len() < 3
        {
            diagnostics.push(format!("无法检查 MESSAGE 样本：{error}"));
        }
    }
    let mut libraries = Vec::new();
    let mut known = Value::Null;
    let mut seen = BTreeSet::new();
    for directory in directories.iter().take(24) {
        for suffix in ["Graphics.dll", "Lib/Graphics.dll"] {
            if cancellation.is_cancelled() {
                return Err(EngineError::Cancelled);
            }
            let candidate = directory.join(suffix);
            if !seen.insert(candidate.to_string_lossy().to_lowercase()) {
                continue;
            }
            let result = (|| -> Result<(), EngineError> {
                let relative = candidate
                    .strip_prefix(&root)
                    .map_err(|error| EngineError::Protocol(error.to_string()))?
                    .to_string_lossy()
                    .replace('\\', "/");
                let path = resolve_relative(&root, &relative)?;
                if !path.is_file() {
                    return Ok(());
                }
                if path.metadata()?.len() > 16 * 1024 * 1024 {
                    libraries.push(json!({"path":relative,"status":"size-limit","sha256":null}));
                    return Ok(());
                }
                let hash = hash_file(&path)
                    .map_err(|error| EngineError::Protocol(error.to_string()))?
                    .to_uppercase();
                let matched = !messages.is_empty() && hash == GRAPHICS_SHA256;
                libraries.push(json!({"path":relative,"status":if matched{"known-runtime"}else{"unknown-runtime"},"sha256":hash}));
                if matched {
                    known = json!({"profile":"MESSAGE-v4.0/Graphics-sha256-E1292E3D","graphicsSha256":GRAPHICS_SHA256,"textInitializeChainRva":["TextInitialize","0x3A8E0","0x3AC20"],"crtIsmbbleadCallRva":"0x3AD8A","originalCrtIsmbbleadTargetRva":"0x41248","playerNameControls":["0xF040","0xF041"],"application":"以上 RVA 仅是该原始 SHA256 对应运行库的静态调查线索，不能作为可直接应用的补丁。修改前核对原指令和调用关系；其他 SHA256 必须重新定位。","runtimeStatus":"not-tested"});
                }
                Ok(())
            })();
            if let Err(error) = result
                && diagnostics.len() < 3
            {
                diagnostics.push(format!("无法检查相邻 Graphics.dll：{error}"));
            }
        }
    }
    Ok(
        json!({"status":if !known.is_null(){"known-runtime-static-evidence"}else if !messages.is_empty(){"message40-only"}else{"no-confirmed-message40"},"messageFiles":messages,"graphicsLibraries":libraries,"knownRuntime":known,"diagnostics":diagnostics,"runtimeStatus":"not-tested","scope":"只读取已发现的最多 8 个 Kaguya 样本文件头；在最多 24 个相邻/祖先目录中仅检查 Graphics.dll 和 Lib/Graphics.dll 两个固定路径，不递归搜索，不修改游戏。"}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kaguya_inspects_only_discovered_samples_and_neighbor_libraries() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("script")).unwrap();
        fs::write(
            root.path().join("script/message.dat"),
            b"[SCR-MESSAGE]ver4.0",
        )
        .unwrap();
        fs::write(root.path().join("Graphics.dll"), b"unknown runtime").unwrap();
        fs::create_dir_all(root.path().join("unrelated")).unwrap();
        fs::write(
            root.path().join("unrelated/Graphics.dll"),
            b"must not inspect",
        )
        .unwrap();
        let mut state = Session::default();
        state.extra.insert(
            "builtinDiscovery".into(),
            json!({"candidates":[{"format":"Kaguya","sampleFiles":["script/message.dat"]}]}),
        );
        let result = kaguya_inspection(root.path(), &state, &CancellationToken::new()).unwrap();
        assert_eq!(result["status"], "message40-only");
        assert_eq!(result["graphicsLibraries"].as_array().unwrap().len(), 1);
        assert_eq!(result["graphicsLibraries"][0]["path"], "Graphics.dll");
        assert_eq!(result["knownRuntime"], Value::Null);
        assert_eq!(
            fs::read(root.path().join("Graphics.dll")).unwrap(),
            b"unknown runtime"
        );
    }

    #[test]
    fn archive_component_specs_match_catalog_and_bound_only_in_memory_archives() {
        let catalog_ids: BTreeSet<_> = engine_components()
            .into_iter()
            .map(|entry| entry["id"].as_str().unwrap().to_owned())
            .collect();
        let spec_ids: BTreeSet<_> = ARCHIVE_COMPONENT_SPECS
            .iter()
            .map(|spec| spec.id.to_owned())
            .collect();
        assert_eq!(catalog_ids, spec_ids);
        let rpa = archive_component_spec("archive.rpa3").unwrap();
        assert_eq!(rpa.max_input_bytes, None);
        assert_eq!(rpa.required_filter, None);
        let xp3 = archive_component_spec("archive.xp3").unwrap();
        assert_eq!(xp3.max_input_bytes, Some(512 << 20));
        assert_eq!(xp3.required_filter.map(|(value, _)| value), Some("none"));
        let kif = archive_component_spec("archive.kif").unwrap();
        assert_eq!(kif.extra_inputs.len(), 1);
        assert_eq!(kif.extra_inputs[0].arg, "keyExecutable");
        let whale = archive_component_spec("archive.whale-dat").unwrap();
        assert_eq!(whale.extra_inputs.len(), 1);
        assert_eq!(whale.extra_inputs[0].arg, "filenameManifest");
    }

    #[test]
    fn template_composition_covers_catalog_and_preserves_entry_contracts() {
        for template in template_catalog() {
            let id = template["id"].as_str().unwrap();
            let composition = template_composition(id).unwrap_or_else(|| panic!("missing spec for {id}"));
            assert!(!composition.module.is_empty());
        }
        let check_entry = |id: &str, expected: &str| {
            let composition = template_composition(id).unwrap();
            let args = if composition.pass_template_id_to_main {
                format!("{id:?}")
            } else {
                String::new()
            };
            let entry = format!(
                "import sys\nfrom tools.{} import main\nsys.exit(main({args}))\n",
                composition.module
            );
            assert_eq!(entry, expected, "entry mismatch for {id}");
        };
        check_entry(
            "loose-renpy",
            "import sys\nfrom tools.script_template import main\nsys.exit(main())\n",
        );
        check_entry(
            "rpa3-renpy-rpyc",
            "import sys\nfrom tools.renpy_rpyc_adapter import main\nsys.exit(main())\n",
        );
        check_entry(
            "renpy-mixed",
            "import sys\nfrom tools.renpy_mixed_adapter import main\nsys.exit(main())\n",
        );
        check_entry(
            "qlie-pack31-imos",
            "import sys\nfrom tools.qlie_pack_adapter import main\nsys.exit(main(\"qlie-pack31-imos\"))\n",
        );
        assert_eq!(
            template_composition("xp3-kirikiri-ks")
                .unwrap()
                .required_filter
                .map(|(value, _)| value),
            Some("none")
        );
    }

    #[test]
    fn shared_format_and_sample_specs_preserve_engine_policies() {
        let root = tempfile::tempdir().unwrap();
        let ystb = root.path().join("scene.ybn");
        let other = root.path().join("other.ybn");
        fs::write(&ystb, b"YSTB\x00\x00").unwrap();
        fs::write(&other, b"YSCM\x00\x00").unwrap();
        assert_eq!(
            companion_requirement("Softpal", &other).unwrap(),
            CompanionRequirement::Required
        );
        assert_eq!(
            companion_requirement("Yuris", &ystb).unwrap(),
            CompanionRequirement::Required
        );
        assert_eq!(
            companion_requirement("Yuris", &other).unwrap(),
            CompanionRequirement::Skip
        );
        assert_eq!(
            companion_requirement("Renpy", &other).unwrap(),
            CompanionRequirement::Optional
        );

        let retry = archive_member_retry_spec("root.PFS").unwrap();
        assert_eq!(retry.format, "ArtemisAst");
        assert_eq!(retry.fixture_file, "fixture.pfs");
        assert!(archive_member_retry_spec("archive.rpa").is_none());

        assert!(archive_sample_spec("CatSystem").scan_sibling_executables);
        assert_eq!(archive_sample_spec("ArtemisAst").fallback_sample_name, "sample.ast");
        assert_eq!(archive_sample_spec("eagls-script").sample_component, Some("eagls-script"));
        assert_eq!(archive_sample_spec("willplus-wsc").sample_component, Some("willplus-wsc"));
        assert!(archive_sample_next_action("Yuris", &json!({"version": 482}), true).is_none());
        assert!(
            archive_sample_next_action("Yuris", &json!({"version": 474}), true)
                .unwrap()
                .contains("yuris-ypf474")
        );
        assert!(
            archive_sample_next_action("CatSystem", &json!({"keyExecutable": "game.exe"}), false)
                .unwrap()
                .contains("archive.kif")
        );
        let (path, content) = reference("Renpy").expect("renpy runtime reference");
        assert_eq!(path, "reference/Renpy_runtime.md");
        assert!(content.contains("force_archives"));
    }
}
