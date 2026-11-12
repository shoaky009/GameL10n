//! Application services shared by any UI or command-line host.
#![forbid(unsafe_code)]

pub mod adapter_feedback;
mod application;
pub mod catalog;
pub mod cleanup;
pub mod engine_tasks;
pub mod game_directory;
mod legacy;
pub mod logs;
pub mod metrics;
pub mod patching;
mod project_views;
pub mod projects;
pub mod relocation;
pub mod settings;
pub mod translation_tasks;
pub mod workbench;

use thiserror::Error;

pub use application::{AppEvent, Application, StoredAnalysis, TaskState, TextPage};
pub use gamel10n_engines::{CancellationToken, ToolPaths};
pub use gamel10n_translation::TranslationUnit;
pub use project_views::ProjectSnapshot;
pub use projects::{GameProject, ProjectSummary, default_data_directory};

pub fn native_formats() -> &'static [&'static str] {
    gamel10n_engines::vntextpatch::FORMATS
}

#[derive(Debug, Error)]
pub enum CoreError {
    #[error(transparent)]
    File(#[from] gamel10n_translation::files::FileError),
    #[error(transparent)]
    Engine(#[from] gamel10n_engines::EngineError),
    #[error(transparent)]
    Text(#[from] gamel10n_translation::TextError),
    #[error(transparent)]
    Storage(#[from] gamel10n_translation::storage::StorageError),
    #[error("文件操作失败：{0}")]
    Io(#[from] std::io::Error),
    #[error("后台操作失败：{0}")]
    Background(#[from] tokio::task::JoinError),
    #[error("数据无效：{0}")]
    InvalidData(String),
    #[error("找不到项目：{0}")]
    ProjectMissing(String),
    #[error("这个项目已有运行中的任务。")]
    ProjectBusy,
}
