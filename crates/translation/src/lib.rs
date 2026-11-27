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

pub const DEFAULT_UNIT_KIND: &str = "dialogue";

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
    DEFAULT_UNIT_KIND.into()
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

impl ValidationResult {
    /// Summarizes validation failures while retaining the complete issues in the report.
    pub fn issue_summary(&self) -> String {
        const SAMPLE_COUNT: usize = 3;
        const MESSAGE_CHARS: usize = 240;
        if self.issues.is_empty() {
            return "未提供具体失败原因。".into();
        }
        let samples = self
            .issues
            .iter()
            .take(SAMPLE_COUNT)
            .map(|issue| {
                let mut chars = issue.message.chars();
                let mut sample: String = chars
                    .by_ref()
                    .take(MESSAGE_CHARS)
                    .map(|c| if c.is_control() { ' ' } else { c })
                    .collect();
                if chars.next().is_some() {
                    sample.push('…');
                }
                sample
            })
            .collect::<Vec<_>>()
            .join("；");
        let truncated = if self.issues.len() > SAMPLE_COUNT {
            format!("（仅列前 {SAMPLE_COUNT} 项）")
        } else {
            String::new()
        };
        format!(
            "共 {} 项问题{truncated}：{samples}",
            self.issues.len(),
        )
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validation_issue_summary_bounds_individual_messages() {
        let validation = ValidationResult {
            passed: false,
            issues: vec![ValidationIssue {
                code: "invalid-data".into(),
                message: "错误\n".repeat(10000),
                unit_id: None,
            }],
            runtime_status: "not-tested".into(),
        };
        let summary = validation.issue_summary();
        assert!(summary.starts_with("共 1 项问题："));
        assert!(summary.ends_with('…'));
        assert!(summary.chars().count() < 300);
        assert_eq!(summary.lines().count(), 1);
        assert_eq!(validation.issues[0].message.chars().count(), 30000);
    }

    #[test]
    fn failed_validation_without_issues_has_a_reason() {
        let validation = ValidationResult {
            passed: false,
            issues: Vec::new(),
            runtime_status: "not-tested".into(),
        };
        assert_eq!(validation.issue_summary(), "未提供具体失败原因。");
    }
}
