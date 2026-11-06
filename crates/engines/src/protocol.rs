//! CLI protocol v1 shared by native tools and Python adapters.

use std::{collections::BTreeMap, path::PathBuf};

use gamel10n_translation::{TranslationUnit, ValidationResult};
use serde::{Deserialize, Serialize};

use crate::EngineError;

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
    #[serde(default)]
    pub warnings: Vec<String>,
    #[serde(default)]
    pub launch_executable: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdapterResponse {
    pub protocol_version: u32,
    pub success: bool,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub probe: Option<ProbeResult>,
    #[serde(default)]
    pub extraction: Option<ExtractionResult>,
    #[serde(default)]
    pub build: Option<BuildResult>,
    #[serde(default)]
    pub validation: Option<ValidationResult>,
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
