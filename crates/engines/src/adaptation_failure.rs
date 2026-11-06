use super::*;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum AdaptationFailureCategory {
    Api,
    Tool,
    Storage,
    Adapter,
    Data,
    Research,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdaptationFailure {
    pub category: AdaptationFailureCategory,
    pub code: String,
    pub message: String,
    pub next_action: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http_status: Option<u16>,
}

impl AdaptationFailure {
    pub(super) fn from_error(error: &EngineError) -> Self {
        use AdaptationFailureCategory as Category;
        let (category, code, message, action, http_status) = match error {
            EngineError::Api(error) => {
                let code = error.diagnostic.code.as_str();
                let reason = match code {
                    "output-token-limit" => "模型单次输出达到长度上限，响应被截断".into(),
                    "model-refusal" => "模型拒绝处理本次分析请求".into(),
                    "request-size-limit" => "分析请求超过接口大小限制".into(),
                    _ => error.failure_summary(),
                };
                let action = match (error.status_code, code) {
                    (Some(401 | 403), _) | (_, "configuration" | "http-client" | "scheduler") => {
                        "检查分析 API 的地址、密钥、模型及访问权限后，点击“继续适配”。"
                    }
                    (Some(429), _) => {
                        "稍后点击“继续适配”；若持续限流，请检查 API 配额或调整分析 API。"
                    }
                    (Some(408 | 500..=599), _)
                    | (_, "network-error" | "network-interrupted" | "request-timeout") => {
                        "连接或服务恢复后点击“继续适配”，也可以更换分析 API。"
                    }
                    (_, "model-refusal") => {
                        "更换能够处理当前分析任务的 API 或模型后，点击“继续适配”。"
                    }
                    (_, "output-token-limit") => {
                        "点击“继续适配”，让模型缩小每次输出；持续截断时可更换分析模型。"
                    }
                    _ => "检查分析 API 的接口配置及模型工具调用支持后，点击“继续适配”。",
                };
                (
                    Category::Api,
                    code,
                    format!("分析 API 请求失败：{reason}。"),
                    action,
                    error.status_code,
                )
            }
            EngineError::ToolMissing(_) => (
                Category::Tool,
                "tool-missing",
                error.to_string(),
                "检查工具文件或使用完整发行包后，点击“继续适配”。",
                None,
            ),
            EngineError::Timeout => (
                Category::Tool,
                "tool-timeout",
                error.to_string(),
                "检查本地工具的执行记录和资源占用后，点击“继续适配”。",
                None,
            ),
            EngineError::Io(_) => (
                Category::Storage,
                "io-error",
                error.to_string(),
                "检查相关文件的访问权限、占用和磁盘空间后，点击“继续适配”。",
                None,
            ),
            EngineError::Json(_) => (
                Category::Data,
                "invalid-json",
                error.to_string(),
                "根据错误检查相关数据或工具输出后，点击“继续适配”。",
                None,
            ),
            EngineError::Rejected(_)
            | EngineError::Protocol(_)
            | EngineError::Incompatible { .. } => (
                Category::Adapter,
                "adapter-error",
                error.to_string(),
                "根据诊断修正适配输入或实现后，点击“继续适配”。",
                None,
            ),
            EngineError::Research(_) => (
                Category::Research,
                "research-request",
                error.to_string(),
                "检查资料地址和网络连接后，点击“继续适配”。",
                None,
            ),
            EngineError::Cancelled => (
                Category::Tool,
                "cancelled",
                error.to_string(),
                "点击“继续适配”恢复任务。",
                None,
            ),
        };
        Self {
            category,
            code: code.into(),
            message,
            next_action: format!("{action}已有源码和已提交步骤会继续使用。"),
            http_status,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gamel10n_translation::provider::{ProviderDiagnostic, ProviderError};

    #[test]
    fn api_failures_keep_their_cause_and_preserve_adapter_evidence() {
        for (code, status, reason) in [
            ("http-error", Some(502), "服务商异常"),
            ("http-error", Some(429), "限流"),
            ("http-error", Some(401), "身份验证"),
            ("request-timeout", None, "请求超时"),
            ("api-json-syntax", None, "返回格式不正确"),
            ("output-token-limit", None, "单次输出"),
            ("model-refusal", None, "拒绝处理本次分析"),
        ] {
            let error = EngineError::Api(Box::new(ProviderError {
                message: "provider message".into(),
                outcome_unknown: false,
                status_code: status,
                usage: TokenUsage::default(),
                diagnostic: Box::new(ProviderDiagnostic {
                    code: code.into(),
                    detail: None,
                    response_preview: None,
                    response_truncated: false,
                    request_id: None,
                }),
                retry_after: None,
            }));
            let mut session = Session {
                verification: Some(VerificationResult {
                    passed: false,
                    original_roundtrip_passed: true,
                    runtime_tested: false,
                    diagnostics: vec!["中文回填尚未通过".into()],
                    units: 12,
                    ..VerificationResult::default()
                }),
                ..Session::default()
            };
            let completed = failed_result(
                &mut session,
                &error,
                "适配未完成",
                vec!["非致命执行诊断".into()],
            );
            let failure = completed.failure.as_ref().unwrap();
            assert_eq!(completed.status, AdaptationStatus::Failed);
            assert_eq!(failure.category, AdaptationFailureCategory::Api);
            assert_eq!((failure.code.as_str(), failure.http_status), (code, status));
            assert!(failure.message.contains(reason), "{}", failure.message);
            assert_eq!(completed.diagnostics, ["非致命执行诊断"]);
            assert_eq!(completed.verification["diagnostics"][0], "中文回填尚未通过");
            assert_eq!(completed.next_action, session.next_action);
            assert!(completed.next_action.contains("继续适配"));
        }
    }
}
