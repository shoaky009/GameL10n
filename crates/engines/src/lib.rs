//! Engine capabilities and subprocess protocols; no project storage or UI dependency.
#![forbid(unsafe_code)]

pub mod adaptation;
mod adaptation_assets;
mod adaptation_context;
mod adapter_assets;
pub mod discovery;
mod native_tools;
mod process;
pub mod protocol;
pub mod python;
mod research;
pub mod review;
pub mod service;
mod source;
mod tool_activity;
mod tyrano;
mod verification_diagnostic;
pub mod vntextpatch;

use std::path::{Path, PathBuf};

use thiserror::Error;

pub use adaptation::{AdaptationLimits, AdaptationRequest, AdaptationResult, AdaptationStatus};
pub use discovery::{AdapterHandle, PackageManifest};
pub use process::ToolProcess;
pub use service::{
    BuiltinAdapterProbeObservation, DiscoveryResult, EngineProgress, EngineService,
    GeneratedAdapterReuseFailure, ProgressCallback, SharedHelperDir, VerificationResult,
    VerifiedExtraction,
};
pub use source::ReadOnlySource;
pub use tokio_util::sync::CancellationToken;
pub use verification_diagnostic::{
    VerificationFieldDifference, VerificationIssue, VerificationIssueCode,
};

#[derive(Debug, Error)]
pub enum EngineError {
    #[error("找不到工具：{0}")]
    ToolMissing(PathBuf),
    #[error("执行隔离失败：{0}")]
    Isolation(String),
    #[error("工具执行失败：{0}")]
    Io(#[from] std::io::Error),
    #[error("工具返回的 JSON 无效：{0}")]
    Json(#[from] serde_json::Error),
    #[error("工具执行超过时间限制。")]
    Timeout,
    #[error("任务已取消。")]
    Cancelled,
    #[error("适配器处理失败：{0}")]
    Rejected(String),
    #[error("适配器协议无效：{0}")]
    Protocol(String),
    #[error("适配器与当前资源不兼容：{message}")]
    Incompatible {
        stage: &'static str,
        message: String,
    },
    #[error("分析 API 请求失败：{}", analysis_api_summary(.0))]
    Api(Box<gamel10n_translation::provider::ProviderError>),
    #[error("资料获取失败：{0}")]
    Research(String),
}

fn analysis_api_summary(error: &gamel10n_translation::provider::ProviderError) -> String {
    if error.diagnostic.code == gamel10n_translation::provider::diagnostic_codes::OUTPUT_TOKEN_LIMIT {
        "单次分析输出被截断，请拆分本轮生成内容或检查模型配置中的分析输出上限。".into()
    } else {
        error.failure_summary()
    }
}

/// Application-owned shared tools, resolved independently of the current directory.
#[derive(Clone, Debug)]
pub struct ToolPaths {
    pub root: PathBuf,
}

impl ToolPaths {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn vntextpatch(&self) -> PathBuf {
        self.root
            .join(native_tools::VNTEXTPATCH_DIR)
            .join(native_tools::EXECUTABLE)
    }

    pub fn python(&self) -> PathBuf {
        self.root.join("python/python.exe")
    }

    pub fn isolated_runner(&self) -> PathBuf {
        self.root.join("tool-runner/GameL10n.ToolRunner.exe")
    }

    /// Root containing adapters in the workspace or application distribution.
    pub fn asset_root(&self) -> &Path {
        let parent = self.root.parent().unwrap_or_else(|| Path::new("."));
        if self.root.ends_with("target/runtime") {
            parent.parent().unwrap_or(parent)
        } else {
            parent
        }
    }

    pub fn beside_executable() -> Result<Self, EngineError> {
        let executable = std::env::current_exe()?;
        let parent = executable.parent().unwrap_or_else(|| Path::new("."));
        Ok(Self::new(parent.join("tools")))
    }

    /// Prepared tools used when running directly from the workspace.
    pub fn development() -> Self {
        Self::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/runtime"))
    }

    /// Uses bundled tools when available, otherwise the prepared development runtime.
    pub fn for_application() -> Result<Self, EngineError> {
        let bundled = Self::beside_executable()?;
        Ok(if bundled.python().is_file() {
            bundled
        } else {
            Self::development()
        })
    }
}
