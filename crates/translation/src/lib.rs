//! Shared text contracts and file primitives, independent of engines and desktop UI.
#![forbid(unsafe_code)]

pub mod compatibility;
pub mod edits;
pub mod execution;
pub mod files;
pub mod provider;
pub mod storage;
pub mod text;

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Text identity and context shared by extraction, translation, and proofreading.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TranslationUnit {
    pub id: String,
    pub resource_path: String,
    pub locator: String,
    pub source_text: String,
    pub context: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speaker: Option<String>,
    #[serde(default = "dialogue")]
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protected_tokens: Option<Vec<String>>,
}

fn dialogue() -> String {
    "dialogue".into()
}

/// Provider-reported cache tokens are part of input tokens, never additional usage.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    #[serde(default)]
    pub estimated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_tokens: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ValidationIssue {
    pub code: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ValidationResult {
    pub passed: bool,
    pub issues: Vec<ValidationIssue>,
    pub runtime_status: String,
}

#[derive(Debug, Error)]
pub enum TextError {
    #[error("提取结果没有可处理的文本。")]
    EmptyExtraction,
    #[error("提取结果包含空文本或缺少文本 ID：{0}")]
    EmptyUnit(String),
    #[error("提取结果包含重复的文本 ID：{0}")]
    DuplicateId(String),
}

/// Rejects ambiguous identities before any translation or result is committed.
pub fn validate_units(units: &[TranslationUnit]) -> Result<(), TextError> {
    if units.is_empty() {
        return Err(TextError::EmptyExtraction);
    }
    let mut ids = HashSet::with_capacity(units.len());
    for unit in units {
        if unit.id.trim().is_empty() || unit.source_text.trim().is_empty() {
            return Err(TextError::EmptyUnit(unit.locator.clone()));
        }
        if !ids.insert(unit.id.as_str()) {
            return Err(TextError::DuplicateId(unit.id.clone()));
        }
    }
    Ok(())
}
