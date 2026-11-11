//! Machine-readable failures from the host's independent resource verifier.
use std::collections::BTreeSet;

use gamel10n_translation::TranslationUnit;
use serde::{Deserialize, Serialize};

use crate::EngineError;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum VerificationIssueCode {
    ByteRoundtrip,
    TextCount,
    MissingUnit,
    TextMismatch,
    IdentityMismatch,
    ProtectedTokens,
    InputChanged,
    AdapterOperation,
    Protocol,
    Timeout,
    Storage,
    ToolMissing,
}

/// Excerpts describe one counterexample; the complete output remains in the verification directory.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct VerificationIssue {
    pub code: VerificationIssueCode,
    pub stage: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resource_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unit_id: Option<String>,
    pub expected: String,
    pub actual: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub field_differences: Vec<VerificationFieldDifference>,
}

/// Bounded values for one changed text-unit field; optional values distinguish null from empty strings.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct VerificationFieldDifference {
    pub field: String,
    pub expected: String,
    pub actual: String,
}

impl VerificationIssue {
    pub(crate) fn new(code: VerificationIssueCode, stage: &str) -> Self {
        Self {
            code,
            stage: stage.into(),
            resource_path: None,
            unit_id: None,
            expected: String::new(),
            actual: String::new(),
            field_differences: Vec::new(),
        }
    }

    pub(crate) fn at(mut self, resource: &str, unit: Option<&str>) -> Self {
        self.resource_path = Some(resource.into());
        self.unit_id = unit.map(str::to_owned);
        self
    }

    pub(crate) fn difference(mut self, expected: &str, actual: &str) -> Self {
        self.expected = expected.chars().take(256).collect();
        self.actual = actual.chars().take(256).collect();
        self
    }

    pub(crate) fn unit_difference(
        mut self,
        expected: &TranslationUnit,
        actual: Option<&TranslationUnit>,
        expected_text: &str,
        check_context: bool,
        check_speaker: bool,
    ) -> Self {
        let Some(actual) = actual else {
            return self.difference("text unit present", "missing");
        };
        let mut record = |field: &str, left: String, right: String| {
            if left != right {
                if self.field_differences.is_empty() {
                    self.expected = left.chars().take(256).collect();
                    self.actual = right.chars().take(256).collect();
                }
                self.field_differences.push(VerificationFieldDifference {
                    field: field.into(),
                    expected: left.chars().take(256).collect(),
                    actual: right.chars().take(256).collect(),
                });
            }
        };
        for (field, left, right) in [
            ("sourceText", expected_text, actual.source_text.as_str()),
            (
                "resourcePath",
                expected.resource_path.as_str(),
                actual.resource_path.as_str(),
            ),
            (
                "locator",
                expected.locator.as_str(),
                actual.locator.as_str(),
            ),
            ("kind", expected.kind.as_str(), actual.kind.as_str()),
        ] {
            record(field, left.into(), right.into());
        }
        if check_context {
            record("context", expected.context.clone(), actual.context.clone());
        }
        if check_speaker {
            record(
                "speaker",
                serde_json::json!(expected.speaker).to_string(),
                serde_json::json!(actual.speaker).to_string(),
            );
        }
        let tokens = |unit: &TranslationUnit| {
            serde_json::json!(
                unit.protected_tokens
                    .iter()
                    .flatten()
                    .map(String::as_str)
                    .collect::<BTreeSet<_>>()
            )
            .to_string()
        };
        record("protectedTokens", tokens(expected), tokens(actual));
        self
    }

    pub(crate) fn from_error(stage: &str, error: &EngineError) -> Self {
        use VerificationIssueCode as Code;
        let code = match error {
            EngineError::Io(_) => Code::Storage,
            EngineError::Timeout => Code::Timeout,
            EngineError::ToolMissing(_) => Code::ToolMissing,
            EngineError::Protocol(_) | EngineError::Json(_) => Code::Protocol,
            _ => Code::AdapterOperation,
        };
        Self::new(code, stage).difference("operation succeeds", &error.to_string())
    }

    pub(crate) fn field_names(&self) -> String {
        if self.field_differences.is_empty() {
            "文本单元缺失".into()
        } else {
            self.field_differences
                .iter()
                .map(|difference| difference.field.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn unit() -> TranslationUnit {
        serde_json::from_value(json!({"id":"line-1","resourcePath":"text.json","locator":"1","sourceText":"original","context":"stable","speaker":"original name","kind":"dialogue","protectedTokens":["\\n"]})).unwrap()
    }

    #[test]
    fn identity_diagnostics_name_changed_fields_when_text_is_unchanged() {
        let expected = unit();
        let mut actual = expected.clone();
        actual.speaker = Some("translated name".into());
        let issue = VerificationIssue::new(
            VerificationIssueCode::IdentityMismatch,
            "partial-translation-re-extraction",
        )
        .unit_difference(&expected, Some(&actual), &expected.source_text, false, true);
        assert_eq!(
            issue.field_differences,
            [VerificationFieldDifference {
                field: "speaker".into(),
                expected: "\"original name\"".into(),
                actual: "\"translated name\"".into(),
            }]
        );
        assert_ne!(issue.expected, issue.actual);
    }

    #[test]
    fn field_differences_preserve_comparison_scope_and_bound_values() {
        let expected = unit();
        let mut actual = expected.clone();
        actual.source_text = "translated".into();
        actual.context = "different context".into();
        actual.speaker = None;
        actual.locator = "位".repeat(1000);
        actual.protected_tokens = Some(vec!["\\n".into(), "\\n".into()]);
        let issue = VerificationIssue::new(VerificationIssueCode::IdentityMismatch, "verification")
            .unit_difference(&expected, Some(&actual), "translated", false, false);
        assert_eq!(issue.field_differences.len(), 1);
        assert_eq!(issue.field_differences[0].field, "locator");
        assert_eq!(issue.field_differences[0].actual.chars().count(), 256);
        let issue = VerificationIssue::new(VerificationIssueCode::IdentityMismatch, "verification")
            .unit_difference(&expected, Some(&actual), "translated", true, true);
        assert!(
            issue
                .field_differences
                .iter()
                .any(|field| field.field == "context")
        );
        assert!(
            issue
                .field_differences
                .iter()
                .any(|field| field.field == "speaker" && field.actual == "null")
        );
    }

    #[test]
    fn legacy_issues_deserialize_without_field_differences() {
        let value = json!({"code":"identity-mismatch","stage":"verification","expected":"one","actual":"two"});
        let issue: VerificationIssue = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(issue).unwrap(), value);
    }
}
