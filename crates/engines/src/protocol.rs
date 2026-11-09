//! CLI protocol v1 shared by native tools and Python adapters.

use std::{collections::BTreeMap, path::PathBuf};

use gamel10n_translation::{TranslationUnit, ValidationResult};
use serde::{Deserialize, Serialize};

use crate::EngineError;

/// Runtime contract returned by `adapter_protocol` and retained across model compaction.
pub const ADAPTER_PROTOCOL: &str = include_str!("../prompts/adapter-protocol.md");

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
    pub translations: Option<BTreeMap<String, String>>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

impl AdapterRequest {
    pub fn new(operation: &str, source: impl Into<PathBuf>) -> Self {
        Self {
            protocol_version: 1,
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
    pub fn set_output_context(&self, request: &mut AdapterRequest) {
        if self.mode == BuildMode::Overlay {
            request
                .extra
                .insert("buildMode".into(), serde_json::json!(self.mode));
            request
                .extra
                .insert("outputFiles".into(), serde_json::json!(self.files));
        }
    }

    /// Reads the effective game without copying original archives into the patch directory.
    pub fn extraction_request(
        &self,
        source: &std::path::Path,
        output: &std::path::Path,
        resources: &[String],
    ) -> AdapterRequest {
        let mut request = AdapterRequest::new(
            "extract",
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
    /// Only an explicit resource incompatibility may select another adapter.
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
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProbeRead {
    pub path: String,
    pub offset: u64,
    pub length: u64,
}

impl AdapterResponse {
    pub fn checked(self) -> Result<Self, EngineError> {
        if self.protocol_version != 1 {
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
