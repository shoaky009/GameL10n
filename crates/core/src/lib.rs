//! Application services shared by any UI or command-line host.
#![forbid(unsafe_code)]

pub mod adapter_feedback;
mod application;
pub mod catalog;
pub mod cleanup;
mod engine_review;
pub mod engine_tasks;
pub mod game_directory;
pub mod launch;
mod legacy;
pub mod logs;
pub mod metrics;
pub mod patching;
mod project_migrations;
mod project_views;
pub mod projects;
pub mod relocation;
pub mod settings;
pub mod terminology;
pub mod translation_tasks;
pub mod workbench;
pub mod workflow;

use thiserror::Error;

pub use application::{
    AppEvent, Application, LiveTranslationProgress, PreviewTranslationStatus, StoredAnalysis,
    TaskState, TextPage,
};
pub use engine_review::ReviewState;
pub use gamel10n_engines::{CancellationToken, ToolPaths};
pub use gamel10n_translation::TranslationUnit;
pub use project_views::{PatchTextPage, ProjectSnapshot};
pub use projects::{GameProject, ProjectSummary, default_data_directory};

/// Default page size shared by desktop views, CLI pagination, and patch text previews.
pub const DEFAULT_PAGE_SIZE: usize = 100;
/// Maximum page size accepted by text, workbench, and catalog pagination endpoints.
pub const MAX_PAGE_SIZE: usize = 500;

#[cfg(windows)]
pub(crate) const WIN32_FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;

/// Canonical project task operation identifiers shared across core services, metrics, and UI.
pub mod task_op {
    pub const ADAPT: &str = "adapt";
    pub const RECOGNIZE: &str = "recognize";
    pub const EXTRACT: &str = "extract";
    pub const BUILD: &str = "build";
    pub const VALIDATE: &str = "validate";
    pub const TRANSLATE: &str = "translate";
    pub const PREVIEW_TRANSLATE: &str = "preview-translate";
    pub const RETRANSLATE: &str = "retranslate";
    pub const CANDIDATE: &str = "candidate";
    pub const TERMINOLOGY: &str = "terminology";
    pub const REVIEW: &str = "review";
    pub const INSTALL: &str = "install";
    pub const RESTORE: &str = "restore";
    pub const WORKFLOW: &str = "workflow";
    pub const REPAIR: &str = "repair";
    pub const EXPAND: &str = "expand";
    pub const CLEANUP: &str = "cleanup";
    pub const VERIFY: &str = "verify";

    /// Operations that mutate extracted text or retranslation targets and therefore lock the editor.
    pub fn locks_editor(operation: &str) -> bool {
        matches!(
            operation,
            RETRANSLATE | EXTRACT | RECOGNIZE | ADAPT | WORKFLOW
        )
    }
}

pub fn native_formats() -> &'static [&'static str] {
    gamel10n_engines::vntextpatch::FORMATS
}

#[derive(Debug, Error)]
pub enum CoreError {
    #[error("工作台分页缓存损坏：{0}")]
    DamagedWorkbench(String),
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
    #[error("启动配置已更新，请确认是否覆盖。")]
    LaunchConflict { revision: String },
    #[error("已安装文件被其他程序修改：{}", .0.join("、"))]
    ModifiedInstalledFiles(Vec<String>),
    #[error("已恢复 {restored_files} 个原文件，移除 {removed_files} 个补丁文件；以下文件在安装后已被修改，未自动覆盖：{}", conflicts.join("、"))]
    RestoreConflicts {
        restored_files: usize,
        removed_files: usize,
        conflicts: Vec<String>,
    },
    #[error("{0}")]
    WorkflowInput(String),
    #[error(transparent)]
    WithModelOutput(Box<CoreError>),
}
