//! Machine-readable failures from the host's independent resource verifier.
use crate::EngineError;
use serde::{Deserialize, Serialize};

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
}
