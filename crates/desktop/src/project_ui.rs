use crate::{Context, UiInput, refresh_projects, settings_ui, show_dialog, view::App};
use gamel10n_core::{
    CoreError,
    engine_tasks::{AdaptationObjective, RecognitionMode, RepairIssue},
    patching::PatchInstaller,
    workflow::{WorkflowChoice, WorkflowFinish, WorkflowPauseKind, WorkflowPreset},
};
use serde_json::Value;
use slint::{ComponentHandle, Model, ModelRc};
use std::{future::Future, path::PathBuf, pin::Pin, sync::atomic::Ordering};

fn file_size(bytes: u64) -> String {
    let units = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < units.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.2} {}", units[unit])
    }
}

async fn confirm_game_directory(directory: PathBuf) -> Result<bool, CoreError> {
    tokio::task::spawn_blocking(move || {
        let games = gamel10n_core::game_directory::suspected_games(&directory)?;
        if games.is_empty() {
            return Ok(true);
        }
        let examples = games
            .iter()
            .map(|path| format!("• {}", path.display()))
            .collect::<Vec<_>>()
            .join("\n");
        let message = format!(
            "所选目录可能包含多个游戏，以下子目录分别具有可执行入口和游戏资源特征：\n\n{examples}\n\n这只是初步判断，单个游戏也可能采用这种结构。\n仍要将整个目录作为一个游戏项目继续吗？\n\n{}",
            directory.display()
        );
        Ok(rfd::MessageDialog::new()
            .set_title("请确认游戏目录")
            .set_description(message)
            .set_level(rfd::MessageLevel::Warning)
            .set_buttons(rfd::MessageButtons::OkCancel)
            .show()
            == rfd::MessageDialogResult::Ok)
    })
    .await?
}

fn workflow_choice(input: &UiInput) -> Result<WorkflowChoice, CoreError> {
    let finish = match input.workflow_finish {
        0 => WorkflowFinish::Trial,
        1 => WorkflowFinish::Build,
        2 => WorkflowFinish::Install,
        3 => return Ok(WorkflowChoice::Custom),
        _ => return Err(CoreError::InvalidData("请选择自动汉化的完成目标。".into())),
    };
    Ok(WorkflowChoice::Preset(WorkflowPreset {
        use_existing_text: input.workflow_use_existing_text,
        finish,
    }))
}

fn load_workflow_preferences(app: &App<'_>, choice: WorkflowChoice, has_custom: bool) {
    let (finish, use_existing_text) = match choice {
        WorkflowChoice::Preset(preset) => {
            let finish = match preset.finish {
                WorkflowFinish::Trial => 0,
                WorkflowFinish::Build => 1,
                WorkflowFinish::Install => 2,
            };
            (finish, preset.use_existing_text)
        }
        WorkflowChoice::Custom => (3, false),
    };
    app.set_workflow_has_custom(has_custom);
    app.set_workflow_finish(finish);
    app.set_workflow_use_existing_text(use_existing_text);
}

// Choose the future before polling so adaptation skips the other project action frames.
pub fn handle<'a>(
    ctx: &'a Context,
    action: &'a str,
    input: &'a UiInput,
) -> Pin<Box<dyn Future<Output = Result<bool, CoreError>> + Send + 'a>> {
    if action == "resume-adaptation" {
        Box::pin(async move {
            ctx.app().await.resume_adaptation(&input.project_id).await?;
            Ok(true)
        })
    } else if action == "run-project-adapter" {
        Box::pin(async move {
            let (adapter_id, version) = input
                .argument
                .split_once('/')
                .ok_or_else(|| CoreError::InvalidData("请选择适配器版本。".into()))?;
            let app = ctx.app().await;
            let project = app.open_project(&input.project_id).await?;
            if confirm_game_directory(project.game_directory).await? {
                let selected = gamel10n_core::catalog::AdapterVersion {
                    adapter_id: adapter_id.into(),
                    version: version.into(),
                };
                app.run_project_adapter(&input.project_id, &selected)
                    .await?;
            }
            Ok(true)
        })
    } else if action == "adapt" {
        Box::pin(adapt(ctx, input))
    } else if action == "workflow-resume-selection" {
        Box::pin(resume_workflow_selection(ctx, input))
    } else if action == "workflow-confirm-trial" {
        Box::pin(confirm_workflow_trial(ctx, input))
    } else {
        Box::pin(handle_project_action(ctx, action, input))
    }
}

async fn resume_workflow_selection(ctx: &Context, input: &UiInput) -> Result<bool, CoreError> {
    ctx.app()
        .await
        .resume_workflow_selection(&input.project_id)
        .await?;
    refresh_project(ctx, &input.project_id, false).await?;
    Ok(true)
}

async fn confirm_workflow_trial(ctx: &Context, input: &UiInput) -> Result<bool, CoreError> {
    ctx.app()
        .await
        .confirm_workflow_trial(&input.project_id)
        .await?;
    *ctx.catalog
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = None;
    refresh_project(ctx, &input.project_id, false).await?;
    Ok(true)
}

async fn adapt(ctx: &Context, input: &UiInput) -> Result<bool, CoreError> {
    let app = ctx.app().await;
    let project = app.open_project(&input.project_id).await?;
    let reopen = || {
        let id = input.project_id.clone();
        let context = ctx.clone();
        ctx.update(move |ui| {
            let app = ui.global::<App>();
            if app.get_project_id().as_str() == id && context.is_requested_project(&id) {
                app.set_modal("adapt".into());
            }
        });
    };
    if !confirm_game_directory(project.game_directory).await? {
        reopen();
        return Ok(true);
    }
    let (goal, objective) = match input.adaptation_objective {
        0 => ("recognize", AdaptationObjective::Develop),
        1 => ("repair", AdaptationObjective::Repair),
        2 => ("expandCoverage", AdaptationObjective::ExpandCoverage),
        3 => ("workflow", AdaptationObjective::Develop),
        _ => return Err(CoreError::InvalidData("请选择本次适配目标。".into())),
    };
    let workflow = (input.adaptation_objective == 3)
        .then(|| workflow_choice(input))
        .transpose()?;
    let saved = async {
        let mut updates = match workflow {
            Some(WorkflowChoice::Preset(WorkflowPreset {
                use_existing_text: true,
                ..
            })) => serde_json::json!({}),
            _ => settings_ui::adaptation_budget_updates(input)?,
        };
        updates["recognitionGoal"] = goal.into();
        app.save_project_settings(&input.project_id, updates).await
    }
    .await;
    if let Err(error) = saved {
        reopen();
        return Err(error);
    }
    if objective == AdaptationObjective::Develop {
        let mode = if input.force_intelligent {
            RecognitionMode::Intelligent
        } else {
            RecognitionMode::Automatic
        };
        if let Some(choice) = workflow {
            app.run_workflow_with_choice(
                &input.project_id,
                input.instruction.clone(),
                mode,
                choice,
            )
            .await?;
        } else {
            app.recognize_with_mode(&input.project_id, input.instruction.clone(), mode)
                .await?;
        }
    } else {
        let repair_issue =
            (objective == AdaptationObjective::Repair).then(|| match input.repair_issue {
                0 => RepairIssue::Crash,
                1 => RepairIssue::Font,
                2 => RepairIssue::PatchNotApplied,
                _ => RepairIssue::Other,
            });
        app.adapt_with_issue(
            &input.project_id,
            input.instruction.clone(),
            Some(objective),
            repair_issue,
        )
        .await?;
    }
    Ok(true)
}

async fn handle_project_action(
    ctx: &Context,
    action: &str,
    input: &UiInput,
) -> Result<bool, CoreError> {
    match action {
        "project-link" => open_project_link(input)?,
        "projects" => refresh_projects(ctx, &input.project_query).await?,
        "project-reference" => follow_project_reference(ctx, input).await?,
        "browse" => browse_game(ctx).await?,
        "project" => select_project(ctx, input).await?,
        "refresh-project" => refresh_project(ctx, &input.project_id, false).await?,
        "logs" => crate::log_ui::refresh(ctx, &input.project_id).await?,
        "tab" => switch_project_tab(ctx, input).await?,
        "navigate" => navigate(ctx, input).await?,
        "project-selection" => select_projects(ctx, input),
        "toggle-project" => {
            {
                let mut state = ctx.session.lock().unwrap_or_else(|e| e.into_inner());
                if !state.project_selection.remove(&input.argument) {
                    state.project_selection.insert(input.argument.clone());
                }
            }
            refresh_projects(ctx, &input.project_query).await?;
        }
        "remove-dialog" => {
            ctx.update(|ui| ui.global::<App>().set_delete_files(false));
            show_dialog(
                ctx,
                "remove",
                "移除游戏项目",
                "默认只从列表移除。保留的数据可在重新添加相同游戏目录后继续使用。".into(),
            );
        }
        "remove" => remove_projects(ctx, input).await?,
        "adapt-dialog" => show_adaptation_dialog(ctx, input).await?,
        "adapt-cancel" => {
            let reuse_text = input.adaptation_objective == 3
                && input.workflow_finish != 3
                && input.workflow_use_existing_text;
            if !reuse_text && let Ok(updates) = settings_ui::adaptation_budget_updates(input) {
                ctx.app()
                    .await
                    .save_project_settings(&input.project_id, updates)
                    .await?;
            }
        }
        "workflow-settings" => show_workflow_settings(ctx, input).await?,
        "workflow-save" => {
            let choice = workflow_choice(input)?;
            ctx.app()
                .await
                .save_workflow_preferences(&input.project_id, choice)
                .await?;
            let id = input.project_id.clone();
            ctx.update(move |ui| {
                let app = ui.global::<App>();
                if app.get_project_id().as_str() == id && app.get_modal() == "workflow-settings" {
                    app.set_modal("".into());
                    app.set_status("已保存自动汉化设置".into());
                    app.set_error("".into());
                }
            });
        }
        "workflow-repair" => open_workflow_repair(ctx, input).await?,
        "workflow-dismiss" => {
            ctx.app().await.dismiss_workflow(&input.project_id).await?;
            refresh_project(ctx, &input.project_id, false).await?;
            refresh_projects(ctx, &input.project_query).await?;
        }
        "review" => review_engine(ctx, input).await?,
        "cancel-preview-translation" => {
            ctx.app()
                .await
                .cancel_preview_translation(&input.project_id)
                .await
        }
        "cancel-review" => ctx.app().await.cancel_review(&input.project_id),
        "cancel" => {
            ctx.app().await.cancel(&input.project_id).await;
        }
        "patch-dialog" => show_dialog(
            ctx,
            "patch",
            "生成汉化补丁",
            "使用已保存的有效译文。未包含的文本保留原文，不调用翻译 API。".into(),
        ),
        "generate-patch" => {
            if !crate::workbench_ui::flush_editor(ctx, input).await? {
                return Ok(true);
            }
            let ids = if input.patch_scope == 1 {
                Some(
                    crate::workbench_ui::ensure_selection(ctx, &input.project_id)
                        .await?
                        .to_vec(),
                )
            } else {
                None
            };
            ctx.app()
                .await
                .generate_patch(&input.project_id, ids, input.patch_scope == 2)
                .await?;
            refresh_patch(ctx, &input.project_id).await?;
            ctx.update(|ui| ui.global::<App>().set_tab(2));
        }
        "restore-dialog" => show_dialog(
            ctx,
            "restore",
            "恢复原版",
            "请关闭游戏及汉化启动器。程序会核对原文件哈希，恢复原文件并移除工具添加的补丁文件。"
                .into(),
        ),
        "install" => {
            ctx.app()
                .await
                .install_latest_patch(&input.project_id, Some(input.patch_path.as_ref()))
                .await?;
            crate::launch_ui::refresh(ctx, &input.project_id, false).await?;
        }
        "restore" => {
            ctx.app().await.restore_game(&input.project_id).await?;
            crate::launch_ui::refresh(ctx, &input.project_id, false).await?;
        }
        "cleanup-dialog" => preview_cleanup(ctx, input).await?,
        "cleanup" => clean_project(ctx, input).await?,
        "runtime" => record_runtime_feedback(ctx, input).await?,
        "patch-texts" | "patch-texts-page" => {
            show_patch_texts(ctx, input, action == "patch-texts").await?
        }
        "directory-dialog" => show_dialog(
            ctx,
            "directory",
            "更换数据保存位置",
            "可迁移现有数据，或只切换到另一份数据。切换完成后立即生效。".into(),
        ),
        "browse-data" => {
            if let Some(path) = tokio::task::spawn_blocking(|| {
                rfd::FileDialog::new()
                    .set_title("选择数据保存目录")
                    .pick_folder()
            })
            .await?
            {
                ctx.update(move |ui| {
                    ui.global::<App>()
                        .set_new_directory(path.display().to_string().into())
                });
            }
        }
        "switch-directory" => switch_directory(ctx, input).await?,
        "open" => open_project_target(ctx, input).await?,
        _ => return Ok(false),
    }
    Ok(true)
}

fn open_project_link(input: &UiInput) -> Result<(), CoreError> {
    let path = match input.argument.as_str() {
        "repository" => "",
        "releases" => "/releases",
        "issues" => "/issues",
        "documentation" => "/blob/main/docs/README.md",
        "changelog" => "/blob/main/CHANGELOG.md",
        "license" => "/blob/main/LICENSE",
        "third-party" => "/blob/main/THIRD-PARTY-NOTICES.md",
        _ => return Err(CoreError::InvalidData("未知的项目链接。".into())),
    };
    let url = format!("{}{path}", env!("CARGO_PKG_REPOSITORY"));
    #[cfg(windows)]
    let command = "explorer.exe";
    #[cfg(not(windows))]
    let command = "xdg-open";
    std::process::Command::new(command).arg(url).spawn()?;

    Ok(())
}

async fn follow_project_reference(ctx: &Context, input: &UiInput) -> Result<(), CoreError> {
    let host = ctx.app().await;
    let target = host.open_project(&input.argument).await;
    if ctx.generation.load(Ordering::SeqCst) != input.view_revision {
        return Ok(());
    }
    match target {
        Ok(project) if !project.is_removed => {}
        Ok(_) | Err(CoreError::ProjectMissing(_)) => {
            ctx.show_error("", "对应游戏项目已移除或删除。".into());
            return Ok(());
        }
        Err(error) => return Err(error),
    }
    ctx.update(|ui| ui.global::<App>().set_project_query("".into()));
    refresh_projects(ctx, "").await?;
    let generation = ctx.generation.clone();
    let revision = input.view_revision;
    let id = input.argument.clone();
    ctx.update(move |ui| {
        if generation.load(Ordering::SeqCst) != revision {
            return;
        }
        let app = ui.global::<App>();
        if let Some(position) = app
            .get_projects()
            .iter()
            .position(|project| project.id.as_str() == id)
        {
            app.set_project_scroll_y(-(position as f32) * app.get_project_row_height());
        }
        app.set_tab(0);
        app.invoke_action("project".into(), id.into());
    });

    Ok(())
}

async fn browse_game(ctx: &Context) -> Result<(), CoreError> {
    let folder = tokio::task::spawn_blocking(|| {
        rfd::FileDialog::new()
            .set_title("选择游戏安装目录")
            .pick_folder()
    })
    .await?;
    if let Some(folder) = folder {
        if !confirm_game_directory(folder.clone()).await? {
            return Ok(());
        }
        let project = ctx.app().await.add_project(&folder).await?;
        ctx.update(move |ui| {
            ui.global::<App>()
                .invoke_action("project".into(), project.id.into())
        });
    }

    Ok(())
}

async fn select_project(ctx: &Context, input: &UiInput) -> Result<(), CoreError> {
    crate::workbench_ui::capture_view(ctx, input);
    let current = crate::workbench_ui::flush_editor(ctx, input).await?;
    let id = input.argument.clone();
    let workbench_view = crate::workbench_ui::saved_view(ctx, &id);
    let requested = id.clone();
    if ctx
        .session
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .requested_project
        != id
    {
        return Ok(());
    }
    let session = ctx.session.clone();
    let generation = ctx.edit_generation.clone();
    let revision = input.editor_revision;
    ctx.update(move |ui| {
        let a = ui.global::<App>();
        let mut session = session.lock().unwrap_or_else(|e| e.into_inner());
        if session.requested_project != requested {
            return;
        }
        if generation.load(Ordering::SeqCst) != revision || a.get_editor_dirty() {
            session.requested_project = a.get_project_id().into();
            return;
        }
        drop(session);
        a.set_project_path("".into());
        a.set_project_files_path("".into());
        a.set_patch_path("".into());
        if a.get_project_id().as_str() != requested {
            a.set_instruction("".into());
        }
        if matches!(
            a.get_modal().as_str(),
            "install" | "adapt" | "project-adapter"
        ) {
            a.set_modal("".into());
        }
        a.set_project_id(requested.into());
        a.set_page(0);
        a.set_error("".into());
        a.set_log_text("".into());
        a.set_progress(0.0);
        a.set_task_message("".into());
        a.set_task_operation("".into());
        a.set_task_api_summary("".into());
        a.set_task_usage(Default::default());
        a.set_source_materials_cleared(false);
        a.set_can_resume_adaptation(false);
        a.set_translation_preview(false);
        a.set_preview_translation_running(false);
        a.set_preview_translation_resumable(false);
        crate::recognition_ui::clear(&a);
        crate::workbench_ui::restore_view(&a, &workbench_view);
        a.set_texts(ModelRc::default());
        a.set_selected_texts(0);
        a.set_activity("正在读取保存的项目状态…".into());
    });
    if !current {
        return Ok(());
    }
    refresh_project(ctx, &id, true).await?;
    if ctx
        .session
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .requested_project
        == id
    {
        ctx.app().await.remember_project(&id).await?;
    }

    Ok(())
}

async fn switch_project_tab(ctx: &Context, input: &UiInput) -> Result<(), CoreError> {
    if !crate::workbench_ui::flush_editor(ctx, input).await? {
        return Ok(());
    }
    let tab = input.argument.parse::<i32>().unwrap_or(0);
    let id = input.project_id.clone();
    let editor_generation = ctx.edit_generation.clone();
    let editor_revision = input.editor_revision;
    let view_generation = ctx.generation.clone();
    let view_revision = input.view_revision;
    ctx.update(move |ui| {
        if editor_generation.load(Ordering::SeqCst) != editor_revision
            || view_generation.load(Ordering::SeqCst) != view_revision
        {
            return;
        }
        let a = ui.global::<App>();
        if a.get_project_id().as_str() == id {
            a.set_page(0);
            a.set_tab(tab);
            if tab == 1 {
                a.invoke_action("texts".into(), "current".into());
            }
        }
    });
    if tab == 3 {
        settings_ui::refresh(ctx, Some(&input.project_id)).await?;
    }
    if tab == 2 {
        refresh_patch(ctx, &input.project_id).await?;
    }

    Ok(())
}

async fn navigate(ctx: &Context, input: &UiInput) -> Result<(), CoreError> {
    if !crate::workbench_ui::flush_editor(ctx, input).await? {
        return Ok(());
    }
    let page = match input.argument.as_str() {
        "apis" => 1,
        "catalog" => 2,
        "about" => 4,
        "metrics" => 5,
        _ => 3,
    };
    let editor_generation = ctx.edit_generation.clone();
    let editor_revision = input.editor_revision;
    let view_generation = ctx.generation.clone();
    let view_revision = input.view_revision;
    ctx.update(move |ui| {
        if editor_generation.load(Ordering::SeqCst) != editor_revision
            || view_generation.load(Ordering::SeqCst) != view_revision
        {
            return;
        }
        let a = ui.global::<App>();
        a.set_page(page);
        a.set_activity("".into());
        a.set_error("".into());
        if page == 2 {
            a.invoke_action("catalog".into(), "0".into());
        }
        if page == 5 {
            a.invoke_action("metrics".into(), "refresh".into());
        }
    });
    if matches!(page, 1 | 3) {
        settings_ui::refresh(ctx, None).await?;
    }

    Ok(())
}

fn select_projects(ctx: &Context, input: &UiInput) {
    let selecting = input.argument == "start";
    if !selecting {
        ctx.session
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .project_selection
            .clear();
    }
    ctx.update(move |ui| {
        let app = ui.global::<App>();
        app.set_multi_projects(selecting);
        if !selecting {
            app.set_selected_projects(0);
            let projects = app.get_projects();
            for (index, mut row) in projects.iter().enumerate() {
                row.selected = false;
                projects.set_row_data(index, row);
            }
        }
    });
}

async fn remove_projects(ctx: &Context, input: &UiInput) -> Result<(), CoreError> {
    let mut ids = ctx
        .session
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .project_selection
        .iter()
        .cloned()
        .collect::<Vec<_>>();
    if ids.is_empty() && !input.project_id.is_empty() {
        ids.push(input.project_id.clone());
    }
    if !input.delete_files
        && ids.contains(&input.project_id)
        && !crate::workbench_ui::flush_editor(ctx, input).await?
    {
        return Ok(());
    }
    ctx.app()
        .await
        .remove_projects(&ids, input.delete_files)
        .await?;
    {
        let mut session = ctx.session.lock().unwrap_or_else(|e| e.into_inner());
        session.project_selection.clear();
        if ids.contains(&session.requested_project) {
            session.requested_project.clear();
        }
    }
    ctx.update(move |ui| {
        let a = ui.global::<App>();
        if !ids.iter().any(|id| id == a.get_project_id().as_str()) {
            return;
        }
        crate::workbench_ui::clear_editor(&a);
        a.set_project_id("".into());
        a.set_project_name("".into());
        a.set_project_files_path("".into());
        a.set_log_text("".into());
        a.set_page(0);
        a.set_texts(ModelRc::default());
    });
    refresh_projects(ctx, &input.project_query).await?;

    Ok(())
}

async fn show_adaptation_dialog(ctx: &Context, input: &UiInput) -> Result<(), CoreError> {
    settings_ui::refresh(ctx, Some(&input.project_id)).await?;
    let host = ctx.app().await;
    let project = host.open_project(&input.project_id).await?;
    let goal = match project.next_recognition_goal() {
        Some("recognize") => 0,
        Some("repair") => 1,
        Some("expandCoverage") => 2,
        Some("workflow") => 3,
        _ => match host.recognition_objective(&input.project_id).await? {
            AdaptationObjective::Develop => 0,
            AdaptationObjective::Repair => 1,
            AdaptationObjective::ExpandCoverage => 2,
        },
    };
    let issue = project
        .extra
        .get("repairIssue")
        .and_then(|v| serde_json::from_value::<RepairIssue>(v.clone()).ok());
    let (choice, has_custom) = host.workflow_preferences(&input.project_id).await?;
    let id = input.project_id.clone();
    let context = ctx.clone();
    ctx.update(move |ui| {
        let app = ui.global::<App>();
        if app.get_project_id().as_str() != id || !context.is_requested_project(&id) {
            return;
        }
        app.set_force_intelligent(false);
        app.set_repair_issue(match issue {
            Some(RepairIssue::Crash) => 0,
            Some(RepairIssue::Font) => 1,
            Some(RepairIssue::PatchNotApplied) => 2,
            _ => 3,
        });
        load_workflow_preferences(&app, choice, has_custom);
        app.invoke_select_adaptation_objective(goal);
        app.set_modal_title("识别与适配".into());
        app.set_modal_message(
            "优先复用已有适配器，必要时继续智能适配。也可选择修复或补全已有结果。".into(),
        );
        app.set_modal("adapt".into());
    });

    Ok(())
}

async fn show_workflow_settings(ctx: &Context, input: &UiInput) -> Result<(), CoreError> {
    let (choice, has_custom) = ctx
        .app()
        .await
        .workflow_preferences(&input.project_id)
        .await?;
    let id = input.project_id.clone();
    let context = ctx.clone();
    ctx.update(move |ui| {
        let app = ui.global::<App>();
        if app.get_project_id().as_str() != id || !context.is_requested_project(&id) {
            return;
        }
        load_workflow_preferences(&app, choice, has_custom);
        app.set_error("".into());
        app.set_modal_title("自动汉化设置".into());
        app.set_modal_message("".into());
        app.set_modal("workflow-settings".into());
    });

    Ok(())
}

async fn review_engine(ctx: &Context, input: &UiInput) -> Result<(), CoreError> {
    let host = ctx.app().await;
    if let Err(error) = host.review_engine(&input.project_id).await {
        let message = error.to_string();
        let state = host.review_state(&input.project_id);
        if !state.is_some_and(|state| {
            state.message == message
                || (state.succeeded.is_none()
                    && matches!(
                        error,
                        CoreError::Engine(gamel10n_engines::EngineError::Cancelled)
                    ))
        }) {
            let _ = host.log_error(&input.project_id, &error).await;
            let id = input.project_id.clone();
            ctx.update(move |ui| {
                let app = ui.global::<App>();
                if app.get_project_id().as_str() == id && !app.get_review_running() {
                    app.set_review_message(message.into());
                }
            });
        }
    }

    Ok(())
}

async fn preview_cleanup(ctx: &Context, input: &UiInput) -> Result<(), CoreError> {
    if !crate::workbench_ui::flush_editor(ctx, input).await? {
        return Ok(());
    }
    let id = input.project_id.clone();
    ctx.update(move |ui| {
        let a = ui.global::<App>();
        if a.get_project_id().as_str() != id {
            return;
        }
        a.set_error("".into());
        a.set_cleanup_project_id(id.into());
        a.set_cleanup_loading(true);
        a.set_cleanup_items(ModelRc::default());
        a.set_cleanup_patch("".into());
        a.set_modal_title("清理项目文件".into());
        a.set_modal_message(
            "按类别选择要清理的文件。项目记录、提取文本、译文、校对数据和安装记录始终保留。".into(),
        );
        a.set_modal("cleanup".into());
    });
    let preview = ctx
        .app()
        .await
        .preview_material_cleanup(&input.project_id)
        .await;
    let id = input.project_id.clone();
    let preview = match preview {
        Ok(preview) => preview,
        Err(error) => {
            ctx.update(move |ui| {
                let a = ui.global::<App>();
                if a.get_cleanup_project_id().as_str() == id {
                    a.set_cleanup_loading(false);
                }
            });
            return Err(error);
        }
    };
    let rows = preview
        .items
        .into_iter()
        .map(|item| crate::view::CleanupRow {
            id: item.category.id().into(),
            title: item.category.title().into(),
            description: item.category.description().into(),
            size: file_size(item.bytes).into(),
            selected: true,
        })
        .collect::<Vec<_>>();
    ctx.update(move |ui| {
        let a = ui.global::<App>();
        if a.get_project_id().as_str() != id || a.get_modal() != "cleanup" {
            return;
        }
        let patch = preview
            .retained_patch
            .map(|path| path.display().to_string());
        a.set_modal_message(
            format!(
                "项目记录、提取文本、译文、校对数据和安装记录始终保留。\n{}",
                patch
                    .as_ref()
                    .map(|_| "保留“补丁与验证”中的当前补丁及其译文快照、运行验证记录。".to_owned())
                    .unwrap_or_else(|| "未找到当前补丁，补丁目录将全部保留。".into())
            )
            .into(),
        );
        a.set_cleanup_patch(patch.unwrap_or_default().into());
        a.set_cleanup_items(std::rc::Rc::new(slint::VecModel::from(rows)).into());
        a.set_cleanup_loading(false);
    });

    Ok(())
}

async fn clean_project(ctx: &Context, input: &UiInput) -> Result<(), CoreError> {
    if input.cleanup_project_id != input.project_id {
        return Err(CoreError::InvalidData(
            "当前项目已变化，请重新打开清理窗口。".into(),
        ));
    }
    let bytes = ctx
        .app()
        .await
        .cleanup_materials(
            &input.project_id,
            &input.cleanup_categories,
            input.cleanup_patch.as_deref(),
        )
        .await?;
    let host = ctx.app().await;
    let _ = host
        .log(
            &input.project_id,
            &format!("已清理所选项目文件，共 {}。", file_size(bytes)),
        )
        .await;
    refresh_project(ctx, &input.project_id, false).await?;
    crate::log_ui::refresh(ctx, &input.project_id).await?;

    Ok(())
}

async fn record_runtime_feedback(ctx: &Context, input: &UiInput) -> Result<(), CoreError> {
    let host = ctx.app().await;
    let feedback = host
        .record_patch_feedback(
            &input.project_id,
            std::path::Path::new(&input.patch_path),
            &input.argument,
            &input.runtime_note,
        )
        .await?;
    *ctx.catalog
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = None;
    refresh_patch(ctx, &input.project_id).await?;
    let message = feedback.message;
    ctx.update(move |ui| ui.global::<App>().set_status(message.into()));
    if input.argument == "failed" && feedback.can_repair_current {
        settings_ui::refresh(ctx, Some(&input.project_id)).await?;
        let notes = input.runtime_note.clone();
        let id = input.project_id.clone();
        let context = ctx.clone();
        ctx.update(move |ui| {
        let app = ui.global::<App>();
        if app.get_project_id().as_str() != id || !context.is_requested_project(&id) {
            return;
        }
        app.invoke_select_adaptation_objective(1);
        app.set_instruction(notes.into());
        app.set_error("".into());
        app.set_modal_title("修复游戏显示问题".into());
        app.set_modal_message("修复当前补丁对应的适配器。确认新版补丁效果正常后，会自动清理未被其他项目使用的历史版本。".into());
        app.set_modal("adapt".into());
    });
    }

    Ok(())
}

async fn open_workflow_repair(ctx: &Context, input: &UiInput) -> Result<(), CoreError> {
    settings_ui::refresh(ctx, Some(&input.project_id)).await?;
    let id = input.project_id.clone();
    let context = ctx.clone();
    ctx.update(move |ui| {
        let app = ui.global::<App>();
        if app.get_project_id().as_str() != id || !context.is_requested_project(&id) {
            return;
        }
        app.invoke_select_adaptation_objective(1);
        app.set_error("".into());
        app.set_modal_title("修复试译显示问题".into());
        app.set_modal_message(
            "请描述在游戏中看到的现象（如乱码、字体异常或补丁未生效），确认后会以修复模式调整当前适配器。"
                .into(),
        );
        app.set_modal("adapt".into());
    });
    Ok(())
}

async fn show_patch_texts(ctx: &Context, input: &UiInput, opening: bool) -> Result<(), CoreError> {
    let offset = if opening {
        0
    } else {
        input.argument.parse::<usize>().unwrap_or(0)
    };
    let id = input.project_id.clone();
    let opening_id = id.clone();
    let revision = input.view_revision;
    let opening_generation = ctx.generation.clone();
    ctx.update(move |ui| {
        let app = ui.global::<App>();
        if opening_generation.load(Ordering::SeqCst) != revision
            || app.get_project_id().as_str() != opening_id
        {
            return;
        }
        if opening {
            app.set_patch_text_total(0);
        }
        app.set_patch_text_offset(offset as i32);
        app.set_patch_text_loading(true);
        app.set_modal_message("".into());
        app.set_error("".into());
        app.set_modal("patch-texts".into());
    });
    let host = ctx.app().await;
    let result = host
        .latest_patch_texts(&input.project_id, offset)
        .await
        .and_then(|page| {
            let text = serde_json::to_string_pretty(&page.texts)
                .map_err(|e| CoreError::InvalidData(e.to_string()))?;
            Ok((text, page.total))
        });
    let generation = ctx.generation.clone();
    ctx.update(move |ui| {
        let a = ui.global::<App>();
        if generation.load(Ordering::SeqCst) != revision
            || a.get_project_id().as_str() != id
            || a.get_modal().as_str() != "patch-texts"
            || a.get_patch_text_offset() != offset as i32
        {
            return;
        }
        a.set_patch_text_loading(false);
        match result {
            Ok((text, total)) => {
                a.set_modal_message(text.into());
                a.set_patch_text_total(total.min(i32::MAX as usize) as i32);
            }
            Err(error) => a.set_error(error.to_string().into()),
        }
    });

    Ok(())
}

async fn switch_directory(ctx: &Context, input: &UiInput) -> Result<(), CoreError> {
    if input.new_directory.trim().is_empty() {
        return Err(CoreError::InvalidData("请选择保存位置。".into()));
    }
    if ctx.switching.swap(true, Ordering::AcqRel) {
        return Ok(());
    }
    let switching = Switching(ctx.clone());
    ctx.update(|ui| {
        let a = ui.global::<App>();
        a.set_ready(false);
        a.set_activity("正在暂停任务并保存进度…".into());
    });
    let host = ctx.app().await;
    tokio::time::timeout(std::time::Duration::from_secs(60), async {
        loop {
            if !host.cancel_all_tasks().await && ctx.active_actions.load(Ordering::Acquire) <= 1 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    })
    .await
    .map_err(|_| CoreError::InvalidData("任务仍在退出，保存位置尚未切换。请稍后重试。".into()))?;
    ctx.update(|ui| {
        ui.global::<App>()
            .set_activity("正在迁移并校验项目数据…".into())
    });
    let result = ctx
        .app()
        .await
        .relocate(
            &PathBuf::from(input.new_directory.trim()),
            input.migrate_data,
            &ctx.tools,
        )
        .await;
    ctx.update(|ui| ui.global::<App>().set_ready(true));
    let next = result?;
    let path = next.data_directory().display().to_string();
    ctx.root_generation.fetch_add(1, Ordering::Relaxed);
    *ctx.host.write().await = next;
    *ctx.catalog.lock().unwrap_or_else(|e| e.into_inner()) = None;
    *ctx.session.lock().unwrap_or_else(|e| e.into_inner()) = Default::default();
    crate::events::connect(ctx.clone()).await;
    drop(switching);
    ctx.update(move |ui| {
        let a = ui.global::<App>();
        a.set_data_directory(path.into());
        a.set_activity("保存位置已切换。".into());
        a.set_project_id("".into());
        a.set_project_name("".into());
        a.set_project_files_path("".into());
        a.set_log_text("".into());
        a.invoke_action("projects".into(), "".into());
    });
    settings_ui::refresh(ctx, None).await?;

    Ok(())
}

async fn open_project_target(ctx: &Context, input: &UiInput) -> Result<(), CoreError> {
    let host = ctx.app().await;
    if matches!(input.argument.as_str(), "knowledge" | "adapter") && !input.project_id.is_empty() {
        crate::workbench_ui::capture_view(ctx, input);
        if !crate::workbench_ui::flush_editor(ctx, input).await? {
            return Ok(());
        }
        let snapshot = host.project_snapshot(&input.project_id).await?;
        let project = snapshot.project;
        if ctx.generation.load(Ordering::SeqCst) != input.view_revision {
            return Ok(());
        }
        if input.argument == "knowledge" {
            let review = snapshot.review;
            if let Some(id) = review["entryId"].as_str().filter(|id| !id.is_empty()) {
                crate::catalog_ui::open_knowledge(ctx, id).await?;
                return Ok(());
            }
        }
        if gamel10n_core::projects::extraction_coverage_pending(&project.extra) {
            return Ok(());
        }
        let analysis = project.extra.get("analysis").unwrap_or(&Value::Null);
        let adapter = analysis["adapterId"]
            .as_str()
            .filter(|id| !id.is_empty())
            .or_else(|| project.extra.get("adapterId").and_then(Value::as_str))
            .unwrap_or("");
        let target = if input.argument == "adapter" {
            let version = analysis["adapterVersion"]
                .as_str()
                .filter(|version| !version.is_empty())
                .or_else(|| project.extra.get("adapterVersion").and_then(Value::as_str))
                .unwrap_or("");
            crate::catalog_ui::LibraryTarget::Version(version)
        } else {
            crate::catalog_ui::LibraryTarget::Knowledge(None)
        };
        crate::catalog_ui::open_family(ctx, adapter, target).await?;
        return Ok(());
    }
    let root = host.project_directory(&input.project_id).ok();
    let path = match input.argument.as_str() {
        "project" => root,
        "game" => Some(host.open_project(&input.project_id).await?.game_directory),
        "logs" => Some(
            root.unwrap_or_else(|| host.data_directory().into())
                .join("logs"),
        ),
        "patch" => host.latest_patch(&input.project_id).await?.map(|p| {
            p.patch_directory
                .join(gamel10n_core::patching::PAYLOAD_DIRECTORY)
        }),
        "data" => Some(host.data_directory().into()),
        _ => None,
    }
    .ok_or_else(|| CoreError::InvalidData("对应目录尚不存在。".into()))?;
    if matches!(input.argument.as_str(), "project" | "logs" | "data") {
        std::fs::create_dir_all(&path)?;
    }
    crate::open_path(path)?;

    Ok(())
}

struct Switching(Context);
impl Drop for Switching {
    fn drop(&mut self) {
        self.0.switching.store(false, Ordering::Release);
        self.0.update(|ui| ui.global::<App>().set_ready(true));
    }
}

pub async fn refresh_project(ctx: &Context, id: &str, opening: bool) -> Result<(), CoreError> {
    if id.is_empty() || !ctx.is_requested_project(id) {
        return Ok(());
    }
    let host = ctx.app().await;
    let snapshot = host.project_snapshot(id).await?;
    let project_files_path = host.project_directory(id)?.display().to_string();
    let preview = snapshot.translation_preview;
    let preview_run = snapshot.preview_translation;
    let preview_resumable = snapshot.preview_translation_resumable;

    let project = snapshot.project;
    let state = snapshot.task;
    crate::log_ui::refresh(ctx, id).await?;
    if !ctx.is_requested_project(id) {
        return Ok(());
    }
    let api_summary = host.task_api_summary(id).await?;
    let progress = snapshot.progress;
    let adaptation = snapshot.adaptation;
    let workflow_session = snapshot.workflow_session;
    let review = snapshot.review;
    let recognition = crate::recognition_ui::RecognitionView::new(
        &project.extra,
        state.as_ref(),
        &progress,
        &adaptation,
    );
    let recognition_failed = recognition.has_failure();
    // Restore the persisted selection when the workbench or a selection action first needs it.
    let mut selected_count = {
        let mut session = ctx.session.lock().unwrap_or_else(|e| e.into_inner());
        if opening {
            session.text_selection.clear();
        }
        session.text_selection.get(id).map_or(0, |ids| ids.len())
    };
    if workflow_session
        .as_ref()
        .is_some_and(|session| session.pause_kind == WorkflowPauseKind::SelectTrial)
        && let Ok(saved) = crate::workbench_ui::ensure_selection(ctx, id).await
    {
        selected_count = saved.len();
    }
    let selected_id = id.to_owned();
    crate::launch_ui::refresh(ctx, id, false).await?;
    let session = ctx.session.clone();
    ctx.update(move |ui| {
        let a = ui.global::<App>();
        if a.get_project_id().as_str() != selected_id
            || session
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .requested_project
                != selected_id
        {
            return;
        }
        a.set_project_name(project.name.into());
        a.set_project_files_path(project_files_path.into());
        a.set_source_materials_cleared(
            project
                .extra
                .get("materialsCleanup")
                .is_some_and(|value| value["sourceResourcesCleared"] == true),
        );
        a.set_project_path(project.game_directory.display().to_string().into());
        a.set_translation_preview(preview);
        a.set_preview_translation_running(preview_run.is_some());
        a.set_preview_translation_resumable(preview_resumable);
        if let Some(run) = &preview_run {
            let fraction = if run.total == 0 {
                0.0
            } else {
                run.completed as f32 / run.total as f32
            };
            let usage = crate::token_ui::usage(
                run.usage.input_tokens,
                run.usage.output_tokens,
                run.usage.cached_tokens.unwrap_or(0),
            );
            a.set_preview_translation_message(run.task.message.as_str().into());
            a.set_preview_translation_progress(fraction);
            a.set_preview_translation_usage(usage.clone());
            if state
                .as_ref()
                .is_some_and(|task| task.operation == "translate")
            {
                a.set_task_message(run.task.message.as_str().into());
                a.set_progress(fraction);
                a.set_task_usage(usage);
            }
        }
        a.set_busy(state.as_ref().is_some_and(|s| s.running));
        a.set_task_operation(
            state
                .as_ref()
                .map(|s| s.operation.as_str())
                .unwrap_or("")
                .into(),
        );
        a.set_task_api_summary(api_summary.into());
        if opening
            || state.as_ref().is_some_and(|state| !state.running)
            || a.get_task_message().is_empty()
        {
            a.set_task_message(
                state
                    .as_ref()
                    .map(|state| state.message.as_str())
                    .unwrap_or("")
                    .into(),
            );
        }
        if let Some(state) = state.as_ref().filter(|state| {
            state.succeeded == Some(false)
                && !crate::recognition_ui::recognition_task(&state.operation)
        }) {
            a.set_error(state.message.clone().into());
        } else if opening || recognition_failed || state.as_ref().is_some_and(|state| state.running)
        {
            a.set_error(snapshot.warnings.join("\n").into());
        }
        a.set_editor_editable(state.as_ref().is_none_or(|s| {
            !s.running
                || !matches!(
                    s.operation.as_str(),
                    "retranslate" | "extract" | "recognize" | "adapt" | "workflow"
                )
        }));
        if opening || a.get_page() != 0 || a.get_tab() != 1 {
            a.set_loading(false);
        }
        a.set_activity("".into());
        let summary = project.extra.get("analysis");
        let count = summary
            .and_then(|s| s.get("unitCount"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        a.set_counts(
            if count > 0 {
                format!("共 {count} 条文本")
            } else {
                "尚未提取文本".into()
            }
            .into(),
        );
        recognition.apply(&a);
        a.set_can_resume_adaptation(snapshot.can_resume_adaptation);
        if let Some(session) = &workflow_session {
            let (stage, title) = match session.pause_kind {
                WorkflowPauseKind::SelectTrial => ("select", "自动汉化 · 等待勾选试译文本"),
                WorkflowPauseKind::ReviewTrial => ("review", "自动汉化 · 请进入游戏实机把关"),
            };
            a.set_workflow_pause_stage(stage.into());
            a.set_workflow_pause_title(title.into());
            a.set_workflow_pause_message(session.message.as_str().into());
            a.set_workflow_pause_confirm_label(session.confirm_label().into());
        } else {
            a.set_workflow_pause_stage("".into());
            a.set_workflow_pause_title("".into());
            a.set_workflow_pause_message("".into());
            a.set_workflow_pause_confirm_label("".into());
        }
        a.set_selected_texts(selected_count as i32);
        let matches_operation = |operation: &str| {
            progress["operation"]
                .as_str()
                .map_or(operation == "adapt", |saved| saved == operation)
        };
        let adaptation_usage = crate::token_ui::saved_analysis(
            if state
                .as_ref()
                .is_some_and(|state| state.running && state.operation == "adapt")
            {
                if matches_operation("adapt") {
                    &progress
                } else {
                    &Value::Null
                }
            } else {
                &adaptation
            },
        );
        a.set_adaptation_usage(adaptation_usage.clone());
        if state
            .as_ref()
            .is_some_and(|state| state.operation == "adapt")
        {
            a.set_task_usage(adaptation_usage);
        }
        a.set_review_summary(
            review
                .get("summary")
                .and_then(Value::as_str)
                .unwrap_or("")
                .into(),
        );
        a.set_review_summary_usage(crate::token_ui::saved_analysis(&review));
        if let Some(state) = host.review_state(&selected_id) {
            crate::events::update_review(&a, &state);
        } else {
            a.set_review_running(false);
            a.set_review_message("".into());
            a.set_review_usage(Default::default());
        }
        if a.get_page() == 0 && a.get_tab() == 1 {
            a.invoke_action(
                "texts".into(),
                if opening { "current" } else { "refresh" }.into(),
            );
        }
    });
    if !ctx.is_requested_project(id) {
        return Ok(());
    }
    if opening {
        settings_ui::refresh(ctx, Some(id)).await?;
    }
    refresh_patch(ctx, id).await?;
    Ok(())
}

pub async fn refresh_patch(ctx: &Context, id: &str) -> Result<(), CoreError> {
    if id.is_empty() {
        return Ok(());
    }
    let host = ctx.app().await;
    let result = host.latest_patch(id).await?;
    let id = id.to_owned();
    let runtime = if let Some(p) = &result {
        PatchInstaller::new(host.project_directory(&id)?).runtime_test(&p.patch_directory)?
    } else {
        None
    };
    ctx.update(move |ui| {
        let a = ui.global::<App>();
        if a.get_project_id().as_str() != id {
            return;
        }
        if let Some(p) = result {
            a.set_patch_path(p.patch_directory.display().to_string().into());
            a.set_patch_summary(
                format!(
                    "{} · 使用 {} / {} 条译文 · {}",
                    if p.is_trial {
                        "试译补丁"
                    } else {
                        "汉化补丁"
                    },
                    p.translated_units,
                    p.total_units,
                    if p.validation.get("passed").and_then(Value::as_bool) == Some(true) {
                        "结构验证通过"
                    } else {
                        "结构验证未通过"
                    }
                )
                .into(),
            );
            a.set_patch_launch(
                p.extra
                    .get("launch")
                    .and_then(|value| {
                        serde_json::from_value::<gamel10n_engines::protocol::LaunchTarget>(
                            value.clone(),
                        )
                        .ok()
                    })
                    .map(|launch| {
                        format!(
                            "补丁声明入口：{}{}",
                            launch.configuration.executable,
                            if launch.configuration.arguments.is_empty() {
                                String::new()
                            } else {
                                format!(" {}", launch.configuration.arguments)
                            }
                        )
                    })
                    .or_else(|| {
                        p.launch_executable
                            .map(|path| format!("补丁声明入口：{path}"))
                    })
                    .unwrap_or_else(|| "此补丁沿用项目原游戏的启动配置。".into())
                    .into(),
            );
            a.set_warnings(p.warnings.join("\n").into());
        } else {
            a.set_patch_path("".into());
            a.set_patch_summary("".into());
            a.set_patch_launch("".into());
            a.set_warnings("".into());
        }
        a.set_runtime_check(
            runtime
                .as_ref()
                .map(|r| {
                    format!(
                        "{}\n{}",
                        match r.status.as_str() {
                            "passed" => "已记录效果正常",
                            "failed" => "已记录存在问题",
                            _ => "尚未验证",
                        },
                        r.notes
                    )
                })
                .unwrap_or_default()
                .into(),
        );
        a.set_runtime_note(runtime.map(|r| r.notes).unwrap_or_default().into());
    });
    Ok(())
}
