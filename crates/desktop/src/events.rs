use std::sync::atomic::Ordering;

use gamel10n_core::{AppEvent, TaskState};
use gamel10n_engines::EngineProgress;
use gamel10n_translation::execution::TranslationProgress;
use slint::{ComponentHandle, Model};
use tokio::sync::broadcast::error::RecvError;

use crate::{App, Context, recognition_ui, token_ui};

pub(super) async fn subscribe_events(ctx: Context) {
    let app = ctx.app().await;
    let mut events = app.subscribe();
    let epoch = ctx.root_generation.load(Ordering::Relaxed);
    ctx.runtime.clone().spawn(async move {
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
            let catalog_changed = !state.running
                && matches!(state.operation.as_str(), "adapt" | "review" | "recognize");
            if catalog_changed {
                *ctx.catalog
                    .lock()
                    .unwrap_or_else(|error| error.into_inner()) = None;
            }
            ctx.update(move |ui| update_task(&ui.global::<App>(), state, catalog_changed));
        }
        AppEvent::TranslationChanged {
            project_id,
            progress,
        } => ctx.update(move |ui| {
            let app = ui.global::<App>();
            if app.get_project_id().as_str() == project_id {
                update_translation(&app, progress);
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
    row.status = recognition_ui::task_status(
        &state.operation,
        state.running,
        state.succeeded,
        None,
        false,
    )
    .into();
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
                "retranslate" | "extract" | "recognize" | "workflow"
            ),
    );
    app.set_status(
        recognition_ui::task_status(
            &state.operation,
            state.running,
            state.succeeded,
            None,
            false,
        )
        .into(),
    );
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

fn update_translation(app: &App, progress: TranslationProgress) {
    if app.get_tab() == 1 && (!progress.updates.is_empty() || !progress.failure_updates.is_empty())
    {
        app.invoke_action("texts".into(), "refresh".into());
    }
    app.set_progress(if progress.total == 0 {
        0.0
    } else {
        progress.completed as f32 / progress.total as f32
    });
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
