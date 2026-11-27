//! CLI protocol v1 shared by native tools and Python adapters.

use std::{collections::BTreeMap, path::PathBuf, sync::Arc};

use gamel10n_translation::{TranslationUnit, ValidationResult};
use serde::{Deserialize, Serialize};

use crate::EngineError;

/// Runtime contract returned by `adapter_protocol` and retained across model compaction.
pub const ADAPTER_PROTOCOL: &str = include_str!("../prompts/adapter-protocol.md");

pub const PROTOCOL_VERSION: u32 = 1;
pub const VALIDATION_CONTEXT_VERSION: u32 = 1;
pub const MAX_LAUNCH_ARGUMENTS_BYTES: usize = 30_000;
pub const GAME_ROOT_DIR: &str = ".";
pub const EXE_EXTENSION: &str = ".exe";
pub const OP_PROBE: &str = "probe";
pub const OP_EXTRACT: &str = "extract";
pub const OP_BUILD: &str = "build";
pub const OP_VALIDATE: &str = "validate";
pub const RESPONSE_FILE_KEY: &str = "responseFile";

pub(crate) const STAGE_PROBE: &str = "probe";
pub(crate) const STAGE_EXTRACTION: &str = "extraction";
pub(crate) const STAGE_BUILD: &str = "build";
pub(crate) const STAGE_VALIDATION: &str = "validation";
pub(crate) const STAGE_RUNTIME: &str = "runtime";

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdapterRequest {
    pub protocol_version: u32,
    pub operation: String,
    pub source_directory: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_directory: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resource_files: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub translations: Option<Arc<BTreeMap<String, String>>>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

impl AdapterRequest {
    pub fn new(operation: &str, source: impl Into<PathBuf>) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            operation: operation.into(),
            source_directory: source.into(),
            output_directory: None,
            resource_files: None,
            format: None,
            translations: None,
            extra: BTreeMap::new(),
        }
    }

    pub fn set_probe_context(&mut self, probe: &ProbeResult) {
        self.extra.insert(
            "originalExecutable".into(),
            serde_json::json!(probe.original_executable),
        );
        if !probe.build_hints.is_empty() {
            self.extra
                .insert("buildHints".into(), serde_json::json!(probe.build_hints));
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProbeResult {
    pub engine_id: String,
    pub adapter_id: String,
    pub adapter_version: String,
    pub description: String,
    pub required_files: Vec<String>,
    #[serde(default)]
    pub warnings: Vec<String>,
    #[serde(default)]
    pub original_executable: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launch: Option<LaunchConfiguration>,
    /// Per-game build choices derived from bounded discovery, not additional source files.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub build_hints: BTreeMap<String, serde_json::Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtractionResult {
    pub units: Vec<TranslationUnit>,
    #[serde(default)]
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BuildResult {
    pub files: Vec<String>,
    #[serde(default, skip_serializing_if = "BuildMode::is_replace")]
    pub mode: BuildMode,
    #[serde(default)]
    pub warnings: Vec<String>,
    #[serde(default)]
    pub launch_executable: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launch: Option<LaunchTarget>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct LaunchConfiguration {
    pub executable: String,
    #[serde(default)]
    pub arguments: String,
    #[serde(default = "game_directory")]
    pub working_directory: String,
}

fn game_directory() -> String {
    GAME_ROOT_DIR.into()
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum LaunchLocation {
    #[default]
    Game,
    Patch,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct LaunchTarget {
    #[serde(default)]
    pub location: LaunchLocation,
    #[serde(flatten)]
    pub configuration: LaunchConfiguration,
}

impl LaunchConfiguration {
    /// Adapter paths are relative to the game root; arguments are a Windows command-line tail.
    pub fn validate_relative(&self) -> Result<(), EngineError> {
        use std::path::{Component, Path};
        let relative = |value: &str| {
            !value.is_empty()
                && !value.contains(['\\', ':'])
                && !value.chars().any(char::is_control)
                && Path::new(value)
                    .components()
                    .all(|part| matches!(part, Component::Normal(_) | Component::CurDir))
        };
        if !relative(&self.executable)
            || !self.executable.to_ascii_lowercase().ends_with(EXE_EXTENSION)
            || !relative(&self.working_directory)
            || self.arguments.len() > MAX_LAUNCH_ARGUMENTS_BYTES
            || self.arguments.chars().any(|c| c.is_control() && c != '\t')
        {
            return Err(EngineError::Protocol(
                "启动配置需要相对 EXE 路径、有效工作目录和启动参数。".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum BuildMode {
    #[default]
    Replace,
    Overlay,
}

impl BuildMode {
    pub fn is_replace(&self) -> bool {
        *self == Self::Replace
    }
}

impl BuildResult {
    pub fn launch_target(&self) -> Result<Option<LaunchTarget>, EngineError> {
        if let Some(launch) = &self.launch {
            launch.configuration.validate_relative()?;
            if self.launch_executable.as_ref().is_some_and(|executable| {
                launch.location != LaunchLocation::Patch
                    || executable != &launch.configuration.executable
            }) {
                return Err(EngineError::Protocol(
                    "补丁的两份启动入口声明不一致。".into(),
                ));
            }
            return Ok(Some(launch.clone()));
        }
        Ok(self
            .launch_executable
            .as_ref()
            .map(|executable| LaunchTarget {
                location: LaunchLocation::Patch,
                configuration: LaunchConfiguration {
                    executable: executable.clone(),
                    arguments: String::new(),
                    working_directory: GAME_ROOT_DIR.into(),
                },
            }))
    }
    pub fn set_output_context(&self, request: &mut AdapterRequest) {
        request
            .extra
            .insert("buildMode".into(), serde_json::json!(self.mode));
        request
            .extra
            .insert("outputFiles".into(), serde_json::json!(self.files));
    }

    /// Reads the effective game without copying original archives into the patch directory.
    pub fn extraction_request(
        &self,
        source: &std::path::Path,
        output: &std::path::Path,
        resources: &[String],
    ) -> AdapterRequest {
        let mut request = AdapterRequest::new(
            OP_EXTRACT,
            if self.mode == BuildMode::Overlay {
                source
            } else {
                output
            },
        );
        if self.mode == BuildMode::Overlay {
            request.resource_files = Some(resources.to_vec());
            request.output_directory = Some(output.to_path_buf());
            self.set_output_context(&mut request);
        } else {
            request.resource_files = Some(self.files.clone());
        }
        request
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdapterResponse {
    pub protocol_version: u32,
    pub success: bool,
    #[serde(default)]
    pub error: Option<String>,
    /// Explicit infrastructure failures stop recognition; legacy operation failures allow fallback.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_kind: Option<String>,
    #[serde(default)]
    pub probe: Option<ProbeResult>,
    #[serde(default)]
    pub extraction: Option<ExtractionResult>,
    #[serde(default)]
    pub build: Option<BuildResult>,
    #[serde(default)]
    pub validation: Option<ValidationResult>,
    /// Opts the outer adapter into verification-scoped extraction reuse.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub validation_context_version: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inspection: Option<serde_json::Value>,
    #[serde(default)]
    pub probe_reads: Vec<ProbeRead>,
    /// A build or validation can request read-only files from a registered asset source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required_assets: Option<RequiredAssets>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RequiredAssets {
    pub catalog: String,
    pub files: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProbeRead {
    pub path: String,
    pub offset: u64,
    pub length: u64,
}

impl AdapterResponse {
    /// Resource-operation failures make a candidate unusable, including unclassified legacy replies.
    pub fn checked_for_operation(self, operation: &str) -> Result<Self, EngineError> {
        let stage = match operation {
            OP_EXTRACT => Some(STAGE_EXTRACTION),
            OP_BUILD => Some(STAGE_BUILD),
            OP_VALIDATE => Some(STAGE_VALIDATION),
            _ => None,
        };
        let candidate_failure =
            matches!(self.error_kind.as_deref(), None | Some("" | "incompatible"));
        self.checked().map_err(|error| match (error, stage) {
            (EngineError::Rejected(message), Some(stage)) if candidate_failure => {
                EngineError::Incompatible { stage, message }
            }
            (error, _) => error,
        })
    }

    pub fn checked(self) -> Result<Self, EngineError> {
        if self.protocol_version != PROTOCOL_VERSION {
            return Err(EngineError::Protocol("不支持的协议版本。".into()));
        }
        if !self.success {
            return Err(EngineError::Rejected(
                self.error
                    .unwrap_or_else(|| "适配器未报告错误原因。".into()),
            ));
        }
        Ok(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validation_receives_outputs_in_both_build_modes() {
        for (mode, files) in [
            (BuildMode::Replace, vec!["text.dat", "encoding.json"]),
            (BuildMode::Overlay, vec!["text.patch.dat"]),
            (BuildMode::Overlay, vec![]),
        ] {
            let build = BuildResult {
                files: files.into_iter().map(str::to_owned).collect(),
                mode,
                warnings: Vec::new(),
                launch_executable: None,
                launch: None,
            };
            let mut request = AdapterRequest::new("validate", "source");
            request.resource_files = Some(vec!["text.dat".into()]);
            build.set_output_context(&mut request);
            assert_eq!(request.extra["buildMode"], serde_json::json!(mode));
            assert_eq!(request.extra["outputFiles"], serde_json::json!(build.files));
            assert_eq!(request.resource_files.unwrap(), ["text.dat"]);
        }
    }

    #[test]
    fn legacy_resource_errors_allow_fallback_without_hiding_infrastructure_or_protocol_failures() {
        for operation in ["extract", "build", "validate"] {
            for kind in [None, Some(""), Some("incompatible")] {
                let response: AdapterResponse = serde_json::from_value(serde_json::json!({"protocolVersion":1,"success":false,"error":"format not supported","errorKind":kind})).unwrap();
                assert!(matches!(
                    response.checked_for_operation(operation),
                    Err(EngineError::Incompatible { .. })
                ));
            }
        }
        for kind in ["io", "dependency", "cancelled", "unknown-kind"] {
            let response: AdapterResponse = serde_json::from_value(serde_json::json!({"protocolVersion":1,"success":false,"error":"environment failure","errorKind":kind})).unwrap();
            assert!(matches!(
                response.checked_for_operation("extract"),
                Err(EngineError::Rejected(_))
            ));
        }
        let invalid: AdapterResponse = serde_json::from_value(
            serde_json::json!({"protocolVersion":99,"success":false,"error":"invalid protocol"}),
        )
        .unwrap();
        assert!(matches!(
            invalid.checked_for_operation("extract"),
            Err(EngineError::Protocol(_))
        ));
    }

    #[test]
    fn launch_metadata_supports_game_entries_and_rejects_conflicting_declarations() {
        let build: BuildResult = serde_json::from_value(serde_json::json!({
            "files":["story.bin"],"launch":{"location":"game","executable":"game.exe","arguments":"--mode \"中文 test\""}
        })).unwrap();
        let launch = build.launch_target().unwrap().unwrap();
        assert_eq!(launch.location, LaunchLocation::Game);
        assert_eq!(launch.configuration.working_directory, ".");
        assert_eq!(launch.configuration.arguments, "--mode \"中文 test\"");
        let legacy: BuildResult = serde_json::from_value(
            serde_json::json!({"files":["launcher.exe"],"launchExecutable":"launcher.exe"}),
        )
        .unwrap();
        assert_eq!(
            legacy.launch_target().unwrap().unwrap().location,
            LaunchLocation::Patch
        );
        let mut conflict = build;
        conflict.launch_executable = Some("launcher.exe".into());
        assert!(conflict.launch_target().is_err());
    }

    #[test]
    fn adapter_launch_metadata_rejects_paths_outside_the_game_and_invalid_arguments() {
        for executable in [
            "../outside.exe",
            "/outside.exe",
            "C:/outside.exe",
            "folder\\game.exe",
            "launch.cmd",
        ] {
            let configuration = LaunchConfiguration {
                executable: executable.into(),
                arguments: String::new(),
                working_directory: ".".into(),
            };
            assert!(configuration.validate_relative().is_err(), "{executable}");
        }
        let mut configuration = LaunchConfiguration {
            executable: "game.exe".into(),
            arguments: String::new(),
            working_directory: "../outside".into(),
        };
        assert!(configuration.validate_relative().is_err());
        configuration.working_directory = ".".into();
        configuration.arguments = "invalid\0argument".into();
        assert!(configuration.validate_relative().is_err());
    }

    #[test]
    fn legacy_builds_keep_replace_mode_and_optional_probe_hints_are_retained() {
        let build: BuildResult =
            serde_json::from_value(serde_json::json!({"files":["scripts.dat"]}))
                .expect("legacy build");
        assert_eq!(build.mode, BuildMode::Replace);
        assert!(
            serde_json::to_value(&build)
                .expect("build JSON")
                .get("mode")
                .is_none()
        );
        let probe: ProbeResult = serde_json::from_value(serde_json::json!({
            "engineId":"fixture", "adapterId":"engine.fixture", "adapterVersion":"1",
            "description":"fixture", "requiredFiles":["scripts.dat"],
            "buildHints":{"patchPath":"scripts.dat.001"}
        }))
        .expect("probe");
        let mut request = AdapterRequest::new("build", "source");
        request.set_probe_context(&probe);
        assert_eq!(request.extra["buildHints"]["patchPath"], "scripts.dat.001");
    }

    #[test]
    fn inspection_responses_preserve_engine_specific_evidence() {
        let inspection = serde_json::json!({"scenes":[{"path":"Scene.pck","commands":[{"offset":92,"name":"display"}]}],"source":{"content":"parser source"}});
        let response: AdapterResponse = serde_json::from_value(
            serde_json::json!({"protocolVersion":1,"success":true,"inspection":inspection}),
        )
        .expect("inspection response");
        assert_eq!(
            response.checked().expect("valid response").inspection,
            Some(inspection)
        );
    }

    #[test]
    fn runtime_protocol_examples_deserialize_with_the_actual_rust_contracts() {
        let mut request_operations = Vec::new();
        let mut response_kinds = Vec::new();
        let protocol = ADAPTER_PROTOCOL.replace("\r\n", "\n");
        for block in protocol.split("```json\n").skip(1) {
            let example = block.split("```").next().expect("example closing fence");
            let value: serde_json::Value =
                serde_json::from_str(example).expect("valid JSON example");
            if value.get("operation").is_some() {
                let request: AdapterRequest =
                    serde_json::from_value(value).expect("request contract");
                assert_eq!(request.protocol_version, 1);
                request_operations.push(request.operation);
            } else {
                let response: AdapterResponse =
                    serde_json::from_value(value.clone()).expect("response contract");
                assert_eq!(response.protocol_version, 1);
                for kind in [
                    "probe",
                    "probeReads",
                    "extraction",
                    "build",
                    "validation",
                    "error",
                ] {
                    if value.get(kind).is_some() {
                        response_kinds.push(kind);
                    }
                }
            }
        }
        for operation in ["probe", "extract", "build", "validate"] {
            assert!(request_operations.iter().any(|actual| actual == operation));
        }
        for kind in [
            "probe",
            "probeReads",
            "extraction",
            "build",
            "validation",
            "error",
        ] {
            assert!(response_kinds.contains(&kind));
        }
    }

    #[test]
    fn probe_identity_fields_remain_required_instead_of_being_fabricated() {
        let complete = serde_json::json!({
            "engineId":"example-format", "adapterId":"template.example-format",
            "adapterVersion":"1", "description":"已识别脚本格式。", "requiredFiles":["scripts.dat"]
        });
        for required in [
            "engineId",
            "adapterId",
            "adapterVersion",
            "description",
            "requiredFiles",
        ] {
            let mut missing = complete.clone();
            missing
                .as_object_mut()
                .expect("probe object")
                .remove(required);
            let error = serde_json::from_value::<ProbeResult>(missing)
                .expect_err("probe must report all required fields");
            assert!(error.to_string().contains(required), "{error}");
        }
    }
}
