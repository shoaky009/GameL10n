//! Engine capabilities and subprocess protocols; no project storage or UI dependency.
#![forbid(unsafe_code)]

mod process;
pub mod protocol;
pub mod python;
pub mod vntextpatch;

use std::path::{Path, PathBuf};

use thiserror::Error;

pub use process::ToolProcess;
pub use tokio_util::sync::CancellationToken;

#[derive(Debug, Error)]
pub enum EngineError {
    #[error("找不到工具：{0}")]
    ToolMissing(PathBuf),
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
        self.root.join("vntextpatch/GameL10n.VnTextPatch.exe")
    }

    pub fn python(&self) -> PathBuf {
        self.root.join("python/python.exe")
    }

    pub fn beside_executable() -> Result<Self, EngineError> {
        let executable = std::env::current_exe()?;
        let parent = executable.parent().unwrap_or_else(|| Path::new("."));
        Ok(Self::new(parent.join("tools")))
    }
}
