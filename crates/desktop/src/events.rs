use std::sync::atomic::Ordering;

use gamel10n_core::{AppEvent, TaskState};
use gamel10n_engines::EngineProgress;
use gamel10n_translation::execution::TranslationProgress;
use slint::{ComponentHandle, Model};
use tokio::sync::broadcast::error::RecvError;

use crate::{App, Context, recognition_ui, token_ui};

pub(super) async fn connect(ctx: Context) {
    let app = ctx.app().await;
    let mut events = app.subscribe();
    let epoch = ctx.root_generation.load(Ordering::Relaxed);
    ctx.runtime.clone().spawn(async move {
        if let Err(error) = app.clean_replaced_adapters().await {
            ctx.show_error("", format!("历史适配器暂未清理：{error}"));
        }
        drop(app);
        loop {
            let event = match events.recv().await {
                Ok(event) => event,
                Err(RecvError::Lagged(_)) => {
                    ctx.update(|ui| {
                        ui.global::<App>()
                            .invoke_action("refresh-project".into(), "".into())
                    });
                    continue;
                }
                Err(RecvError::Closed) => break,
            };
            if ctx.root_generation.load(Ordering::Relaxed) != epoch {
                break;
            }
            dispatch(&ctx, event);
        }
    });
}

fn dispatch(ctx: &Context, event: AppEvent) {
    match event {
        AppEvent::EngineLibraryChanged => {
            *ctx.catalog
                .lock()
                .unwrap_or_else(|error| error.into_inner()) = None;
            ctx.update(|ui| {
                let app = ui.global::<App>();
                if app.get_page() == 2 {
                    app.invoke_action("catalog-refresh".into(), "".into());
                }
            });
        }
        AppEvent::ProjectsChanged => ctx.update(|ui| {
            ui.global::<App>()
                .invoke_action("projects".into(), "".into());
        }),
        AppEvent::TaskChanged(state) => {
            let catalog_changed =
                !state.running && matches!(state.operation.as_str(), "adapt" | "recognize");
            if catalog_changed {
                *ctx.catalog
                    .lock()
                    .unwrap_or_else(|error| error.into_inner()) = None;
            }
            ctx.update(move |ui| update_task(&ui.global::<App>(), state, catalog_changed));
        }
        AppEvent::ReviewChanged { project_id, state } => ctx.update(move |ui| {
            let app = ui.global::<App>();
            let projects = app.get_projects();
            if let Some((index, mut row)) = projects
                .iter()
                .enumerate()
                .find(|(_, row)| row.id.as_str() == project_id)
                && row.review_running != state.running
            {
                row.review_running = state.running;
                projects.set_row_data(index, row);
                app.invoke_action("projects".into(), "".into());
            }
            if app.get_project_id().as_str() == project_id {
                update_review(&app, &state);
            }
        }),
        AppEvent::PreviewChanged { project_id } => {
            let session = ctx.session.clone();
            ctx.update(move |ui| {
                let app = ui.global::<App>();
                session
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .text_selection
                    .remove(&project_id);
                app.invoke_action("projects".into(), "".into());
                if app.get_project_id().as_str() == project_id {
                    app.invoke_action("refresh-project".into(), "".into());
                }
            });
        }
        AppEvent::TranslationChanged {
            project_id,
            progress,
            parallel,
        } => ctx.update(move |ui| {
            let app = ui.global::<App>();
            if app.get_project_id().as_str() == project_id {
                update_translation(&app, progress, parallel);
            }
        }),
        AppEvent::EngineProgress {
            project_id,
            progress,
        } => ctx.update(move |ui| {
            let app = ui.global::<App>();
            if app.get_project_id().as_str() == project_id && app.get_busy() {
                update_engine_progress(&app, &progress);
            }
        }),
        AppEvent::Log { project_id, line } => ctx.update(move |ui| {
            let app = ui.global::<App>();
            if app.get_project_id().as_str() != project_id {
                return;
            }
            if line.starts_with('[') {
                app.invoke_action("logs".into(), "".into());
            } else {
                app.set_error(line.into());
            }
        }),
    }
}

fn update_task(app: &App, state: TaskState, catalog_changed: bool) {
    let running_changed = update_project_row(app, &state);
    if catalog_changed && app.get_page() == 2 {
        app.invoke_action("catalog-refresh".into(), "".into());
    }
    if app.get_project_id().as_str() == state.project_id {
        update_current_task(app, state);
    }
    if running_changed {
        app.invoke_action("projects".into(), "".into());
    }
}

fn update_project_row(app: &App, state: &TaskState) -> bool {
    let projects = app.get_projects();
    let Some((index, mut row)) = projects
        .iter()
        .enumerate()
        .find(|(_, row)| row.id.as_str() == state.project_id)
    else {
        return false;
    };
    let running_changed = row.running != state.running;
    row.running = state.running;
    row.status = recognition_ui::task_state_status(state, None, false).into();
    projects.set_row_data(index, row);
    running_changed
}

fn update_current_task(app: &App, state: TaskState) {
    let recognition = recognition_ui::recognition_task(&state.operation);
    if state.running && !app.get_busy() {
        app.set_error("".into());
        app.set_activity("".into());
        app.set_progress(0.0);
        app.set_task_usage(Default::default());
        if recognition {
            recognition_ui::clear_failure(app);
            app.set_adaptation_progress("".into());
            app.set_adaptation_notes("".into());
        }
    }
    app.set_busy(state.running);
    app.set_task_operation(state.operation.clone().into());
    app.set_editor_editable(
        !state.running
            || !matches!(
                state.operation.as_str(),
                "retranslate" | "extract" | "recognize" | "adapt" | "workflow"
            ),
    );
    app.set_status(recognition_ui::task_state_status(&state, None, false).into());
    app.set_task_message(state.message.clone().into());
    app.set_adaptation_running(state.running && state.operation == "adapt");
    app.set_recognition_running(state.running && recognition);
    if app.get_recognition_running() {
        app.set_adaptation_progress(state.message.clone().into());
    }
    if state.running && state.operation == "adapt" && !app.get_adaptation_summary().is_empty() {
        app.set_adaptation_result_title("上次适配结果".into());
    }
    if state.running {
        return;
    }
    app.set_activity("".into());
    app.set_adaptation_progress("".into());
    if state.succeeded == Some(false) {
        if recognition {
            recognition_ui::show_task_failure(app, &state);
        } else {
            app.set_error(state.message.into());
        }
    }
    app.invoke_action("refresh-project".into(), "".into());
}

pub(super) fn update_review(app: &App, state: &gamel10n_core::ReviewState) {
    app.set_review_running(state.running);
    if state.succeeded == Some(true) {
        app.set_review_summary(state.message.as_str().into());
        app.set_review_message("".into());
    } else {
        app.set_review_message(state.message.as_str().into());
    }
    if let Some(progress) = &state.progress {
        let usage = token_ui::analysis_usage(
            progress.usage.input_tokens,
            progress.usage.output_tokens,
            progress.usage.cached_tokens.unwrap_or(0),
            progress.api_requests,
            progress.api_requests_estimated,
        );
        if state.succeeded == Some(true) {
            app.set_review_summary_usage(usage.clone());
        }
        app.set_review_usage(usage);
    } else {
        app.set_review_usage(Default::default());
    }
}

fn update_translation(app: &App, progress: TranslationProgress, parallel: bool) {
    if app.get_tab() == 1 && (!progress.updates.is_empty() || !progress.failure_updates.is_empty())
    {
        app.invoke_action("texts".into(), "refresh".into());
    }
    let fraction = if progress.total == 0 {
        0.0
    } else {
        progress.completed as f32 / progress.total as f32
    };
    if parallel {
        app.set_preview_translation_message(progress.message.as_str().into());
        app.set_preview_translation_progress(fraction);
        app.set_preview_translation_usage(token_ui::usage(
            progress.usage.input_tokens,
            progress.usage.output_tokens,
            progress.usage.cached_tokens.unwrap_or(0),
        ));
        if app.get_task_operation() != "translate" {
            return;
        }
    }
    app.set_progress(fraction);
    app.set_counts(
        format!(
            "完成 {} / {} · 复用 {} · 失败 {}",
            progress.completed, progress.total, progress.cache_hits, progress.failed
        )
        .into(),
    );
    app.set_task_message(progress.message.into());
    app.set_task_usage(token_ui::usage(
        progress.usage.input_tokens,
        progress.usage.output_tokens,
        progress.usage.cached_tokens.unwrap_or(0),
    ));
}

fn update_engine_progress(app: &App, progress: &EngineProgress) {
    let detail = recognition_ui::progress_detail(progress);
    let usage = token_ui::analysis_usage(
        progress.usage.input_tokens,
        progress.usage.output_tokens,
        progress.usage.cached_tokens.unwrap_or(0),
        progress.api_requests,
        progress.api_requests_estimated,
    );
    if app.get_recognition_running()
        && (!progress.progress_note.is_null() || !progress.last_breakthrough.is_null())
    {
        app.set_adaptation_notes(
            recognition_ui::progress_notes(&progress.progress_note, &progress.last_breakthrough)
                .into(),
        );
    }
    if app.get_adaptation_running() {
        app.set_adaptation_usage(usage.clone());
    }
    if app.get_recognition_running() {
        app.set_adaptation_progress(detail.clone().into());
    }
    app.set_task_usage(usage);
    app.set_task_message(detail.into());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parallel_translation_keeps_the_recognition_status_and_respects_workflow_control() {
        i_slint_backend_testing::init_no_event_loop();
        let ui = crate::view::MainWindow::new().unwrap();
        let app = ui.global::<App>();
        app.set_busy(true);
        app.set_task_operation("adapt".into());
        app.set_task_message("正在验证回填".into());
        assert!(!app.get_can_start_translation());
        app.set_translation_preview(true);
        assert!(app.get_can_start_translation());
        app.set_preview_translation_running(true);
        assert!(!app.get_can_start_translation());
        update_translation(
            &app,
            TranslationProgress {
                stage: "翻译".into(),
                message: "已翻译 1 / 2".into(),
                completed: 1,
                total: 2,
                cache_hits: 0,
                failed: 0,
                usage: Default::default(),
                updates: Default::default(),
                failure_updates: Default::default(),
            },
            true,
        );
        assert_eq!(app.get_preview_translation_progress(), 0.5);
        assert_eq!(app.get_task_message(), "正在验证回填");
        app.set_preview_translation_running(false);
        app.set_task_operation("workflow".into());
        assert!(app.get_has_workbench_text());
        assert!(!app.get_can_start_translation());
    }
}
