#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
mod catalog_ui;
mod events;
mod game_check_ui;
mod launch_ui;
mod log_ui;
mod metrics_ui;
mod project_ui;
mod recognition_ui;
mod settings_ui;
mod time_ui;
mod token_ui;
#[cfg(test)]
mod ui_preview_tests;
mod window_redraw;
mod window_state;
#[cfg(test)]
mod workbench_tests;
mod workbench_ui;

use clap::Parser;
use gamel10n_core::{
    Application, CoreError, ToolPaths, catalog::EngineCatalog, default_data_directory,
};
use slint::{ComponentHandle, Model, ModelRc, VecModel};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
};
use tokio::sync::RwLock;

#[expect(
    clippy::todo,
    reason = "Slint generates unused component-embedding stubs."
)]
mod view {
    slint::include_modules!();
}
use view::{App, MainWindow, ProjectRow};

#[derive(Parser)]
struct Arguments {
    #[arg(long)]
    data_directory: Option<PathBuf>,
    #[arg(long)]
    tools_directory: Option<PathBuf>,
}

#[derive(Default)]
pub struct UiSession {
    pub game_check: Option<gamel10n_core::CancellationToken>,
    pub game_check_results: BTreeMap<String, gamel10n_core::game_check::CheckResult>,
    pub api_test: Option<gamel10n_core::CancellationToken>,
    pub engine_selection: BTreeSet<String>,
    pub engine_selection_anchor: Option<String>,
    pub engine_visible_ids: Vec<String>,
    pub requested_project: String,
    pub project_selection: BTreeSet<String>,
    project_list_query: String,
    pub text_selection: BTreeMap<String, Arc<Vec<String>>>,
    pub workbench_views: BTreeMap<String, workbench_ui::WorkbenchView>,
}

#[derive(Clone)]
pub struct Context {
    pub host: Arc<RwLock<Application>>,
    pub window: slint::Weak<MainWindow>,
    pub session: Arc<Mutex<UiSession>>,
    pub catalog: Arc<Mutex<Option<EngineCatalog>>>,
    pub generation: Arc<AtomicU64>,
    pub edit_generation: Arc<AtomicU64>,
    pub root_generation: Arc<AtomicU64>,
    pub log_generation: Arc<AtomicU64>,
    pub switching: Arc<AtomicBool>,
    pub active_actions: Arc<AtomicUsize>,
    pub runtime: tokio::runtime::Handle,
    pub tools: ToolPaths,
}
impl Context {
    pub async fn app(&self) -> Application {
        self.host.read().await.clone()
    }
    pub fn update(&self, update: impl FnOnce(MainWindow) + Send + 'static) {
        let generation = self.root_generation.clone();
        let epoch = generation.load(Ordering::Relaxed);
        let _ = self.window.upgrade_in_event_loop(move |ui| {
            if generation.load(Ordering::Relaxed) == epoch {
                update(ui);
            }
        });
    }
    pub fn selection(&self, id: &str) -> Vec<String> {
        self.session
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .text_selection
            .get(id)
            .map(|ids| ids.to_vec())
            .unwrap_or_default()
    }
    pub fn is_requested_project(&self, id: &str) -> bool {
        self.session
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .requested_project
            == id
    }
    pub fn set_selection(&self, id: &str, ids: Vec<String>) {
        self.session
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .text_selection
            .insert(id.into(), Arc::new(ids));
    }
    pub fn refresh_texts(&self, id: &str) {
        let id = id.to_owned();
        self.update(move |ui| {
            let app = ui.global::<App>();
            if app.get_project_id().as_str() == id && app.get_page() == 0 && app.get_tab() == 1 {
                app.invoke_action("texts".into(), "refresh".into());
            }
        });
    }

    fn prepare_action(&self, ui: &MainWindow, action: &str, input: &mut UiInput) {
        let selection = workbench_ui::selection_action(action);
        let navigation = matches!(action, "project" | "project-reference" | "tab" | "navigate")
            || (action == "open"
                && input.page == 0
                && matches!(input.argument.as_str(), "adapter" | "knowledge"));
        let list_navigation =
            action == "texts" && matches!(input.argument.as_str(), "0" | "next" | "previous");
        // Stamp every action at the click, including actions that only refresh an existing view.
        input.view_revision = if navigation
            || matches!(action, "editor-step" | "review-next")
            || action == "texts" && input.argument != "refresh"
            || action == "metrics" && input.argument != "auto"
        {
            self.generation.fetch_add(1, Ordering::SeqCst) + 1
        } else {
            self.generation.load(Ordering::SeqCst)
        };
        input.editor_revision = if navigation
            || list_navigation
            || (action == "confirm" && input.argument != "install")
            || matches!(
                action,
                "edit-changed"
                    | "edit-text"
                    | "save-edit"
                    | "editor-step"
                    | "review-next"
                    | "unreview"
                    | "accept-candidate"
                    | "discard-candidate"
                    | "undo-edit"
                    | "translate"
                    | "candidate"
                    | "generate-patch"
                    | "game-check-dialog"
                    | "close"
            ) {
            self.edit_generation.fetch_add(1, Ordering::SeqCst) + 1
        } else {
            self.edit_generation.load(Ordering::SeqCst)
        };
        let app = ui.global::<App>();
        if selection {
            app.set_selection_busy(true);
            if action == "select-page" {
                input.visible_text_ids = app
                    .get_texts()
                    .iter()
                    .map(|row| row.id.to_string())
                    .collect();
            }
        }
        if input.page == 0
            && input.tab == 1
            && (matches!(action, "editor-step" | "review-next")
                || action == "texts" && (list_navigation || app.get_texts().row_count() == 0))
        {
            app.set_loading(true);
        }
    }
    pub fn show_error(&self, id: &str, message: String) {
        let id = id.to_string();
        self.update(move |ui| {
            let app = ui.global::<App>();
            if id.is_empty() || app.get_project_id().as_str() == id {
                app.set_error(message.into());
                app.set_loading(false);
                app.set_activity("".into());
            }
        });
    }
}

#[derive(Clone, Default)]
pub struct UiInput {
    pub workflow_finish: i32,
    pub workflow_use_existing_text: bool,
    pub check: game_check_ui::CheckInput,
    pub metrics: metrics_ui::MetricsInput,
    pub view_revision: u64,
    pub editor_revision: u64,
    pub visible_text_ids: Vec<String>,
    pub text_scroll_y: f32,
    pub editor_scroll_y: f32,
    pub argument: String,
    pub project_id: String,
    pub editor_id: String,
    pub editor_text: String,
    pub editor_dirty: bool,
    pub text_query: String,
    pub resource_filter: String,
    pub kind_filter: String,
    pub status_filter: String,
    pub only_selected: bool,
    pub offset: usize,
    pub engine_id: String,
    pub version_id: String,
    pub knowledge_id: String,
    pub catalog_query: String,
    pub compatible_query: String,
    pub catalog_filter: i32,
    pub catalog_enabled_filter: i32,
    pub catalog_sort: i32,
    pub catalog_offset: i32,
    pub knowledge_enabled: bool,
    pub verification_project_id: String,
    pub project_query: String,
    pub page: i32,
    pub tab: i32,
    pub api_id: String,
    pub api_test_draft: bool,
    pub api_name: String,
    pub api_endpoint: String,
    pub api_model: String,
    pub api_key: String,
    pub api_remember_key: bool,
    pub api_protocol: i32,
    pub api_format: i32,
    pub api_reasoning: i32,
    pub api_reasoning_custom: String,
    pub api_output_tokens: String,
    pub analysis_index: i32,
    pub chain_index: i32,
    pub candidate_api_index: i32,
    pub chain: Vec<String>,
    pub loaded_analysis_index: i32,
    pub budget: String,
    pub loaded_budget: String,
    pub language: String,
    pub style: String,
    pub terminology: String,
    pub batch_size: i32,
    pub concurrency: i32,
    pub isolate_llm_execution: bool,
    pub instruction: String,
    pub adaptation_objective: i32,
    pub repair_issue: i32,
    pub force_intelligent: bool,
    pub delete_files: bool,
    pub cleanup_categories: Vec<gamel10n_core::cleanup::CleanupCategory>,
    pub cleanup_patch: Option<PathBuf>,
    pub cleanup_project_id: String,
    pub new_directory: String,
    pub migrate_data: bool,
    pub patch_scope: i32,
    pub patch_path: String,
    pub runtime_note: String,
    pub launch_executable: String,
    pub launch_arguments: String,
    pub launch_directory: String,
    pub launch_revision: String,
    pub launch_conflict_revision: String,
    pub launch_conflict_action: String,
}
impl UiInput {
    fn capture(ui: &MainWindow, argument: String) -> Self {
        let a = ui.global::<App>();
        Self {
            check: game_check_ui::CheckInput::capture(&a),
            metrics: metrics_ui::MetricsInput::capture(&a),
            view_revision: 0,
            editor_revision: 0,
            visible_text_ids: Vec::new(),
            text_scroll_y: a.get_text_scroll_y(),
            editor_scroll_y: a.get_editor_scroll_y(),
            argument,
            project_id: a.get_project_id().into(),
            editor_id: a.get_editor_id().into(),
            editor_text: a.get_editor_text().into(),
            editor_dirty: a.get_editor_dirty(),
            text_query: a.get_text_query().into(),
            resource_filter: if a.get_resource_filter() > 0 {
                a.get_resources()
                    .row_data(a.get_resource_filter() as usize)
                    .unwrap_or_default()
                    .into()
            } else {
                String::new()
            },
            kind_filter: [
                "all", "dialogue", "name", "choice", "ui", "item", "skill", "system",
            ]
            .get(a.get_kind_filter().max(0) as usize)
            .unwrap_or(&"all")
            .to_string(),
            status_filter: [
                "all",
                "missing",
                "pending",
                "reviewed",
                "edited",
                "invalid",
                "failed",
                "candidate",
                "automatic",
                "manual",
                "unknown",
            ]
            .get(a.get_status_filter().max(0) as usize)
            .unwrap_or(&"all")
            .to_string(),
            only_selected: a.get_only_selected(),
            offset: a.get_offset().max(0) as usize,
            engine_id: a.get_engine_id().into(),
            version_id: a.get_version_id().into(),
            knowledge_id: a.get_knowledge_id().into(),
            catalog_query: a.get_catalog_query().into(),
            compatible_query: a.get_compatible_query().into(),
            catalog_filter: a.get_catalog_filter(),
            catalog_enabled_filter: a.get_catalog_enabled_filter(),
            catalog_sort: a.get_catalog_sort(),
            catalog_offset: a.get_catalog_offset(),
            knowledge_enabled: a.get_knowledge_enabled(),
            verification_project_id: a
                .get_verification_project_ids()
                .row_data(a.get_verification_project_index().max(0) as usize)
                .unwrap_or_default()
                .into(),
            project_query: a.get_project_query().into(),
            page: a.get_page(),
            tab: a.get_tab(),
            api_id: a.get_api_id().into(),
            api_test_draft: a.get_modal() == "api",
            api_name: a.get_api_name().into(),
            api_endpoint: a.get_api_endpoint().into(),
            api_model: a.get_api_model().into(),
            api_key: a.get_api_key().into(),
            api_remember_key: a.get_api_remember_key(),
            api_protocol: a.get_api_protocol(),
            api_format: a.get_api_format(),
            api_reasoning: a.get_api_reasoning(),
            api_reasoning_custom: a.get_api_reasoning_custom().into(),
            api_output_tokens: a.get_api_output_tokens().into(),
            analysis_index: a.get_analysis_index(),
            chain_index: a.get_chain_index(),
            candidate_api_index: a.get_candidate_api_index(),
            chain: a.get_chain().iter().map(|r| r.id.to_string()).collect(),
            loaded_analysis_index: a.get_loaded_analysis_index(),
            budget: a.get_budget().into(),
            loaded_budget: a.get_loaded_budget().into(),
            language: a.get_language().into(),
            style: a.get_style().into(),
            terminology: a.get_terminology().into(),
            batch_size: a.get_batch_size(),
            concurrency: a.get_concurrency(),
            isolate_llm_execution: a.get_isolate_llm_execution(),
            instruction: a.get_instruction().into(),
            adaptation_objective: a.get_adaptation_objective(),
            repair_issue: a.get_repair_issue(),
            force_intelligent: a.get_force_intelligent(),
            delete_files: a.get_delete_files(),
            cleanup_categories: a
                .get_cleanup_items()
                .iter()
                .filter(|row| row.selected)
                .filter_map(|row| {
                    gamel10n_core::cleanup::CleanupCategory::ALL
                        .into_iter()
                        .find(|category| category.id() == row.id.as_str())
                })
                .collect(),
            cleanup_patch: (!a.get_cleanup_patch().is_empty())
                .then(|| PathBuf::from(a.get_cleanup_patch().as_str())),
            cleanup_project_id: a.get_cleanup_project_id().into(),
            new_directory: a.get_new_directory().into(),
            migrate_data: a.get_migrate_data(),
            patch_scope: a.get_patch_scope(),
            patch_path: a.get_patch_path().into(),
            runtime_note: a.get_runtime_note().into(),
            launch_executable: a.get_launch_executable().into(),
            launch_arguments: a.get_launch_arguments().into(),
            launch_directory: a.get_launch_directory().into(),
            workflow_finish: a.get_workflow_finish(),
            workflow_use_existing_text: a.get_workflow_use_existing_text(),
            launch_revision: a.get_launch_revision().into(),
            launch_conflict_revision: a.get_launch_conflict_revision().into(),
            launch_conflict_action: a.get_launch_conflict_action().into(),
        }
    }
}

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        let _ = rfd::MessageDialog::new()
            .set_title("GameL10n 启动失败")
            .set_description(error.to_string())
            .show();
        std::process::exit(1);
    }
}
fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args = Arguments::parse();
    let runtime = tokio::runtime::Runtime::new()?;
    let data = args.data_directory.unwrap_or_else(default_data_directory);
    let tools = match args.tools_directory {
        Some(root) => ToolPaths::new(root),
        None => ToolPaths::for_application()?,
    };
    let host = runtime.block_on(Application::open(data.clone(), &tools))?;
    let ui = MainWindow::new()?;
    let window_state = window_state::WindowState::install(ui.window());
    ui.global::<App>()
        .set_software_version(gamel10n_core::metrics::SOFTWARE_VERSION.into());
    ui.global::<App>()
        .set_build_git_hash(gamel10n_core::metrics::BUILD_GIT_HASH.into());
    ui.global::<App>()
        .set_repository_url(env!("CARGO_PKG_REPOSITORY").into());
    ui.global::<App>()
        .set_software_license(env!("CARGO_PKG_LICENSE").into());
    ui.global::<App>()
        .on_format_token_budget(|input| token_ui::budget(&input));
    ui.global::<App>()
        .on_concise_text(|input| recognition_ui::concise_text(&input).into());
    ui.global::<App>().on_task_api_for(|operation, summary| {
        recognition_ui::task_api(&operation, &summary).into()
    });
    let context = Context {
        host: Arc::new(RwLock::new(host)),
        window: ui.as_weak(),
        session: Arc::new(Mutex::new(UiSession::default())),
        catalog: Arc::new(Mutex::new(None)),
        generation: Arc::new(AtomicU64::new(0)),
        edit_generation: Arc::new(AtomicU64::new(0)),
        root_generation: Arc::new(AtomicU64::new(0)),
        log_generation: Arc::new(AtomicU64::new(0)),
        switching: Arc::new(AtomicBool::new(false)),
        active_actions: Arc::new(AtomicUsize::new(0)),
        runtime: runtime.handle().clone(),
        tools,
    };
    ui.global::<App>()
        .set_data_directory(data.display().to_string().into());
    ui.global::<App>().set_ready(true);
    install_actions(&ui, &context);
    {
        let ctx = context.clone();
        runtime.spawn(async move {
            events::connect(ctx.clone()).await;
            if let Err(error) = refresh_projects(&ctx, "").await {
                ctx.show_error("", error.to_string());
            }
            if let Err(error) = settings_ui::refresh(&ctx, None).await {
                ctx.show_error("", error.to_string());
            }
            if let Ok(settings) = ctx.app().await.settings().await
                && let Some(id) = settings.last_game_id
            {
                ctx.update(move |ui| {
                    ui.global::<App>()
                        .invoke_action("project".into(), id.into())
                });
            }
        });
    }
    {
        let ctx = context.clone();
        let window_state = window_state.clone();
        ui.window().on_close_requested(move || {
            if ctx.switching.swap(true, Ordering::AcqRel) {
                return slint::CloseRequestResponse::KeepWindowShown;
            }
            let Some(ui) = ctx.window.upgrade() else {
                return slint::CloseRequestResponse::HideWindow;
            };
            window_state.capture(ui.window());
            let mut input = UiInput::capture(&ui, String::new());
            settings_ui::cancel_api_test(&ctx);
            game_check_ui::cancel(&ctx);
            ctx.prepare_action(&ui, "close", &mut input);
            ui.global::<App>().set_ready(false);
            ui.global::<App>()
                .set_activity("正在保存编辑并暂停后台任务…".into());
            let ctx = ctx.clone();
            ctx.runtime.clone().spawn(async move {
                let app = ctx.app().await;
                if let Err(error) = workbench_ui::flush_editor(&ctx, &input).await {
                    ctx.switching.store(false, Ordering::Release);
                    ctx.update(|ui| ui.global::<App>().set_ready(true));
                    ctx.show_error(&input.project_id, error.to_string());
                    return;
                }
                let stopped = tokio::time::timeout(std::time::Duration::from_secs(60), async {
                    loop {
                        if !app.cancel_all_tasks().await
                            && ctx.active_actions.load(Ordering::Acquire) == 0
                        {
                            break;
                        }
                        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    }
                })
                .await;
                if stopped.is_err() {
                    ctx.switching.store(false, Ordering::Release);
                    ctx.update(|ui| ui.global::<App>().set_ready(true));
                    ctx.show_error(
                        &input.project_id,
                        "后台操作仍在退出，程序暂未关闭。请稍后重试。".into(),
                    );
                    return;
                }
                if let Err(error) = app.close_game_check_errors().await {
                    eprintln!("无法关闭运行检查的错误窗口：{error}");
                }
                let _ = slint::quit_event_loop();
            });
            slint::CloseRequestResponse::KeepWindowShown
        });
    }
    ui.run()?;
    if let Err(error) = window_state.save() {
        eprintln!("无法保存窗口状态：{error}");
    }
    runtime.shutdown_timeout(std::time::Duration::from_secs(3));
    Ok(())
}

fn install_actions(ui: &MainWindow, context: &Context) {
    let ctx = context.clone();
    ui.global::<App>().on_action(move |action, argument| {
        if ctx.switching.load(Ordering::Acquire) {
            return;
        }
        let Some(ui) = ctx.window.upgrade() else {
            return;
        };
        let mut input = UiInput::capture(&ui, argument.to_string());
        let action = action.to_string();
        // A background save or translation result must not supersede pending user navigation.
        if action == "texts" && input.argument == "refresh" && ui.global::<App>().get_loading() {
            return;
        }
        if matches!(action.as_str(), "adapt" | "resume-adaptation") {
            recognition_ui::clear_failure(&ui.global::<App>());
        }
        if action == "project" {
            let mut session = ctx.session.lock().unwrap_or_else(|e| e.into_inner());
            if input.page == 0
                && input.project_id == input.argument
                && session.requested_project == input.argument
            {
                return;
            }
            session.requested_project = input.argument.clone();
        }
        if workbench_ui::selection_action(&action) && ui.global::<App>().get_selection_busy() {
            return;
        }
        let pending_addition = if action == "browse" {
            let Some(pending) = PendingGameAddition::start(&ui) else {
                return;
            };
            Some(pending)
        } else {
            None
        };
        let pending_api_test = if action == "api-test" {
            let Some(pending) = settings_ui::PendingApiTest::start(&ctx, &ui) else {
                return;
            };
            Some(pending)
        } else {
            None
        };
        let pending_game_check = if action == "game-check-run" {
            let Some(pending) = game_check_ui::PendingGameCheck::start(&ctx, &ui) else {
                return;
            };
            Some(pending)
        } else {
            None
        };
        ctx.prepare_action(&ui, &action, &mut input);
        let ctx = ctx.clone();
        ctx.active_actions.fetch_add(1, Ordering::AcqRel);
        ctx.runtime.clone().spawn(async move {
            let _pending = PendingAction(ctx.active_actions.clone());
            let _addition = pending_addition;
            let _api_test = pending_api_test;
            let _game_check = pending_game_check;
            if let Err(error) = dispatch(&ctx, &action, &input).await {
                let app = ctx.app().await;
                let message = error.to_string();
                let state = app.task_state(&input.project_id).await;
                if matches!(
                    &error,
                    CoreError::Engine(gamel10n_engines::EngineError::Cancelled)
                ) || state
                    .as_ref()
                    .is_some_and(|s| !s.running && s.succeeded.is_none())
                    && message.contains("暂停")
                {
                    return;
                }
                if !state.as_ref().is_some_and(|s| s.message == message) {
                    let _ = app
                        .log_error(
                            if input.page == 0 {
                                &input.project_id
                            } else {
                                ""
                            },
                            &error,
                        )
                        .await;
                }
                if !state.as_ref().is_some_and(|state| {
                    !state.running
                        && state.succeeded == Some(false)
                        && recognition_ui::recognition_task(&state.operation)
                        && state.message == message
                }) {
                    ctx.show_error(&input.project_id, message);
                }
            }
        });
    });
}

struct PendingGameAddition(slint::Weak<MainWindow>);

impl PendingGameAddition {
    fn start(ui: &MainWindow) -> Option<Self> {
        let app = ui.global::<App>();
        if app.get_adding_game() {
            return None;
        }
        app.set_adding_game(true);
        Some(Self(ui.as_weak()))
    }
}

impl Drop for PendingGameAddition {
    fn drop(&mut self) {
        let _ = self.0.upgrade_in_event_loop(|ui| {
            ui.global::<App>().set_adding_game(false);
        });
    }
}

struct PendingAction(Arc<AtomicUsize>);
impl Drop for PendingAction {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

async fn dispatch(ctx: &Context, action: &str, input: &UiInput) -> Result<(), CoreError> {
    // Resolve the confirmation before awaiting a handler so it shares the same dispatcher frame.
    let mut confirmed;
    let (action, input) = if action == "confirm" {
        confirmed = input.clone();
        let action = match input.argument.as_str() {
            "api" => "api-save",
            "launch" => {
                if !input.launch_conflict_revision.is_empty() {
                    confirmed
                        .launch_revision
                        .clone_from(&input.launch_conflict_revision);
                    if input.launch_conflict_action == "launch-reset" {
                        "launch-reset"
                    } else {
                        "launch-save"
                    }
                } else {
                    "launch-save"
                }
            }
            "adapt" => "adapt",
            "workflow-settings" => "workflow-save",
            "remove" => "remove",
            "patch" => "generate-patch",
            "directory" => "switch-directory",
            "retranslate" => {
                confirmed.argument = "retranslate".into();
                "translate"
            }
            "install" => "install",
            "restore" => "restore",
            "cleanup" => "cleanup",
            "api-delete" => "api-delete",
            "version-delete" => "version-delete",
            "verify-version" => "verify-version-confirm",
            "bulk-review" => "bulk-review-confirm",
            "resume-translation" => {
                confirmed.argument = "resume".into();
                "translate"
            }
            _ => return Ok(()),
        };
        if !matches!(input.argument.as_str(), "launch" | "workflow-settings") {
            ctx.update(|ui| ui.global::<App>().set_modal("".into()));
        }
        (action, &confirmed)
    } else {
        (action, input)
    };
    if action == "switch-directory" {
        settings_ui::cancel_api_test(ctx);
    }
    if metrics_ui::handle(ctx, action, input).await?
        || game_check_ui::handle(ctx, action, input).await?
        || launch_ui::handle(ctx, action, input).await?
        || settings_ui::handle(ctx, action, input).await?
        || catalog_ui::handle(ctx, action, input).await?
        || workbench_ui::handle(ctx, action, input).await?
        || project_ui::handle(ctx, action, input).await?
    {
        return Ok(());
    }
    Err(CoreError::InvalidData(format!("未知界面操作：{action}")))
}

pub async fn refresh_projects(ctx: &Context, query: &str) -> Result<(), CoreError> {
    let app = ctx.app().await;
    let query = query.to_owned();
    let mut rows = Vec::new();
    let mut states = app.task_states().await;
    for p in app.list_projects(&query).await {
        let state = states.remove(&p.id);
        let status = recognition_ui::project_status(state.as_ref(), &p.extra);
        rows.push(ProjectRow {
            id: p.id.clone().into(),
            name: p.name.into(),
            path: p.game_directory.display().to_string().into(),
            running: state.as_ref().is_some_and(|s| s.running),
            review_running: app.review_running(&p.id),
            status: status.into(),
            selected: false,
        });
    }
    let session = ctx.session.clone();
    ctx.update(move |ui| {
        let a = ui.global::<App>();
        if a.get_project_query().as_str() == query {
            let (preserve_position, selected_projects) = {
                let mut session = session.lock().unwrap_or_else(|e| e.into_inner());
                let preserve_position = session.project_list_query == query;
                session.project_list_query = query;
                for row in &mut rows {
                    row.selected = session.project_selection.contains(row.id.as_str());
                }
                (preserve_position, session.project_selection.len())
            };
            let current = a.get_projects();
            let same_order = current.row_count() == rows.len()
                && current.iter().zip(&rows).all(|(old, new)| old.id == new.id);
            if same_order {
                for (index, row) in rows.into_iter().enumerate() {
                    if current.row_data(index).as_ref() != Some(&row) {
                        current.set_row_data(index, row);
                    }
                }
            } else {
                let offset = a.get_project_scroll_y();
                let row_height = a.get_project_row_height();
                let first = (-offset / row_height).floor().max(0.0) as usize;
                let anchor = (preserve_position && offset < 0.0)
                    .then(|| current.row_data(first))
                    .flatten()
                    .and_then(|row| rows.iter().position(|new| new.id == row.id));
                let next_offset = if !preserve_position {
                    0.0
                } else if let Some(index) = anchor {
                    offset + (first as f32 - index as f32) * row_height
                } else {
                    offset
                };
                a.set_projects(ModelRc::new(VecModel::from(rows)));
                a.set_project_scroll_y(next_offset);
            }
            if !preserve_position {
                a.set_project_scroll_y(0.0);
            }
            a.set_selected_projects(selected_projects as i32);
        }
    });
    Ok(())
}

pub fn show_dialog(ctx: &Context, kind: &str, title: &str, message: String) {
    let kind = kind.to_owned();
    let title = title.to_owned();
    ctx.update(move |ui| {
        let a = ui.global::<App>();
        a.set_modal_title(title.into());
        a.set_modal_message(message.into());
        a.set_modal(kind.into());
    });
}
pub fn open_path(path: PathBuf) -> Result<(), CoreError> {
    if !path.exists() {
        return Err(CoreError::InvalidData(format!(
            "文件或目录不存在：{}",
            path.display()
        )));
    }
    #[cfg(windows)]
    let command = "explorer.exe";
    #[cfg(not(windows))]
    let command = "xdg-open";
    std::process::Command::new(command).arg(path).spawn()?;
    Ok(())
}
