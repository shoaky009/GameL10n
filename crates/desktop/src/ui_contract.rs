//! Shared numeric indices and identifiers exchanged between Rust and Slint UI bindings.

pub const NO_SELECTION: i32 = -1;

pub mod page {
    pub const PROJECT: i32 = 0;
    pub const APIS: i32 = 1;
    pub const CATALOG: i32 = 2;
    pub const SETTINGS: i32 = 3;
    pub const ABOUT: i32 = 4;
    pub const METRICS: i32 = 5;
}

pub mod project_tab {
    pub const RECOGNITION: i32 = 0;
    pub const WORKBENCH: i32 = 1;
    pub const PATCH: i32 = 2;
    pub const SETTINGS: i32 = 3;
}

pub mod page_arg {
    pub const FIRST: &str = "0";
    pub const NEXT: &str = "next";
    pub const PREVIOUS: &str = "previous";
    pub const CURRENT: &str = "current";
    pub const REFRESH: &str = "refresh";
}

pub mod modal {
    pub const API: &str = "api";
    pub const API_DELETE: &str = "api-delete";
    pub const LAUNCH: &str = "launch";
    pub const ADAPT: &str = "adapt";
    pub const PROJECT_ADAPTER: &str = "project-adapter";
    pub const WORKFLOW_SETTINGS: &str = "workflow-settings";
    pub const ADD_TERM: &str = "add-term";
    pub const TERMINOLOGY_EXTRACT: &str = "terminology-extract";
    pub const REMOVE: &str = "remove";
    pub const PATCH: &str = "patch";
    pub const PATCH_TEXTS: &str = "patch-texts";
    pub const DIRECTORY: &str = "directory";
    pub const RETRANSLATE: &str = "retranslate";
    pub const INSTALL: &str = "install";
    pub const INSTALL_OVERWRITE: &str = "install-overwrite";
    pub const RESTORE: &str = "restore";
    pub const RESTORE_OVERWRITE: &str = "restore-overwrite";
    pub const CLEANUP: &str = "cleanup";
    pub const ENGINE_DELETE: &str = "engine-delete";
    pub const VERSION_DELETE: &str = "version-delete";
    pub const VERIFY_VERSION: &str = "verify-version";
    pub const BULK_REVIEW: &str = "bulk-review";
    pub const RESUME_TRANSLATION: &str = "resume-translation";
}
