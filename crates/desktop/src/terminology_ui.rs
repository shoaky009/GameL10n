//! Desktop bindings for terminology extraction, quick term addition, and impact selection.

use crate::{
    Context, UiInput,
    ui_contract::{modal, page_arg, project_tab},
    view::{App, MainWindow, TermCandidateRow},
};
use gamel10n_core::{CancellationToken, CoreError, terminology::TerminologySaveResult};
use slint::{ComponentHandle, Model, ModelRc, VecModel};

pub struct PendingTerminologyExtract(Context);

impl PendingTerminologyExtract {
    pub fn start(ctx: &Context, ui: &MainWindow) -> Option<Self> {
        let mut session = ctx.session.lock().unwrap_or_else(|e| e.into_inner());
        if session.terminology_extract.is_some() {
            return None;
        }
        session.terminology_extract = Some(CancellationToken::new());
        let app = ui.global::<App>();
        app.set_terminology_extracting(true);
        app.set_terminology_extract_message("".into());
        app.set_terminology_candidates(ModelRc::new(VecModel::default()));
        app.set_terminology_selected_count(0);
        app.set_error("".into());
        app.set_modal(modal::TERMINOLOGY_EXTRACT.into());
        Some(Self(ctx.clone()))
    }
}

impl Drop for PendingTerminologyExtract {
    fn drop(&mut self) {
        let session = self.0.session.clone();
        let _ = self.0.window.upgrade_in_event_loop(move |ui| {
            session
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .terminology_extract = None;
            ui.global::<App>().set_terminology_extracting(false);
        });
    }
}

pub fn cancel_extraction(ctx: &Context) {
    if let Some(token) = &ctx
        .session
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .terminology_extract
    {
        token.cancel();
    }
}

pub fn sync_affected_count(ctx: &Context, id: &str) {
    let count = ctx
        .session
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .terminology_affected
        .get(id)
        .map_or(0, Vec::len);
    let id = id.to_owned();
    ctx.update(move |ui| {
        let app = ui.global::<App>();
        if app.get_project_id().as_str() == id {
            app.set_terminology_affected_count(count as i32);
        }
    });
}

pub fn apply_save_result(ctx: &Context, id: &str, saved: TerminologySaveResult) {
    let affected_count = saved.affected_unit_ids.len();
    {
        let mut session = ctx.session.lock().unwrap_or_else(|e| e.into_inner());
        if affected_count > 0 {
            session
                .terminology_affected
                .insert(id.to_owned(), saved.affected_unit_ids);
        } else {
            session.terminology_affected.remove(id);
        }
    }
    let project_id = id.to_owned();
    let terminology = saved.terminology;
    let term_count = saved.term_count as i32;
    let status = if affected_count > 0 {
        format!("术语表已保存（共 {term_count} 条），有 {affected_count} 条已译文本受影响")
    } else {
        format!("术语表已保存（共 {term_count} 条）")
    };
    ctx.update(move |ui| {
        let app = ui.global::<App>();
        if app.get_project_id().as_str() != project_id {
            return;
        }
        app.set_terminology(terminology.into());
        app.set_terminology_count(term_count);
        app.set_terminology_affected_count(affected_count as i32);
        app.set_status(status.into());
        if app.get_tab() == project_tab::WORKBENCH {
            app.invoke_action("texts".into(), page_arg::REFRESH.into());
        }
    });
}

fn update_selected_count(app: &App<'_>) {
    let count = app
        .get_terminology_candidates()
        .iter()
        .filter(|row| row.selected)
        .count();
    app.set_terminology_selected_count(count as i32);
}

pub async fn handle(ctx: &Context, action: &str, input: &UiInput) -> Result<bool, CoreError> {
    match action {
        "terminology-extract-cancel" => {
            cancel_extraction(ctx);
        }
        "terminology-extract" => {
            let token = ctx
                .session
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .terminology_extract
                .clone()
                .unwrap_or_default();
            let id = input.project_id.clone();
            let result = ctx
                .app()
                .await
                .extract_project_terminology(&id, &token)
                .await;
            match result {
                Ok(extracted) => {
                    let rows: Vec<TermCandidateRow> = extracted
                        .candidates
                        .into_iter()
                        .map(|item| TermCandidateRow {
                            source: item.source.into(),
                            target: item.target.into(),
                            category: item.category.into(),
                            count: item.count as i32,
                            existing: item.existing_target.unwrap_or_default().into(),
                            selected: item.selected,
                        })
                        .collect();
                    let selected_count = rows.iter().filter(|row| row.selected).count() as i32;
                    let total = rows.len();
                    let usage = crate::token_ui::usage(
                        extracted.usage.input_tokens,
                        extracted.usage.output_tokens,
                        extracted.usage.cached_tokens.unwrap_or(0),
                    );
                    let message = if total == 0 {
                        "未提取到需要固定译名的新术语，可手动添加。".into()
                    } else if usage.text.is_empty() {
                        format!(
                            "已提取 {total} 条候选术语（{} / {}）。已有术语默认不勾选，可按需修改译文。",
                            extracted.provider_name, extracted.model
                        )
                    } else {
                        format!(
                            "已提取 {total} 条候选术语（{} / {} · {}）。已有术语默认不勾选，可按需修改译文。",
                            extracted.provider_name, extracted.model, usage.text
                        )
                    };
                    ctx.update(move |ui| {
                        let app = ui.global::<App>();
                        if app.get_project_id().as_str() != id
                            || app.get_modal().as_str() != modal::TERMINOLOGY_EXTRACT
                        {
                            return;
                        }
                        app.set_terminology_candidates(ModelRc::new(VecModel::from(rows)));
                        app.set_terminology_selected_count(selected_count);
                        app.set_terminology_extract_message(message.into());
                    });
                }
                Err(error) => {
                    let cancelled = matches!(
                        &error,
                        CoreError::Engine(gamel10n_engines::EngineError::Cancelled)
                    );
                    let message = if cancelled {
                        "已取消术语提取。".into()
                    } else {
                        error.to_string()
                    };
                    ctx.update(move |ui| {
                        let app = ui.global::<App>();
                        if app.get_project_id().as_str() != id
                            || app.get_modal().as_str() != modal::TERMINOLOGY_EXTRACT
                        {
                            return;
                        }
                        app.set_terminology_extract_message(message.clone().into());
                        if !cancelled {
                            app.set_error(message.into());
                        }
                    });
                }
            }
        }
        "terminology-selection-changed" => {
            ctx.update(|ui| update_selected_count(&ui.global::<App>()));
        }
        "terminology-select-new" | "terminology-select-all" | "terminology-select-none" => {
            let mode = action.to_owned();
            ctx.update(move |ui| {
                let app = ui.global::<App>();
                let model = app.get_terminology_candidates();
                for index in 0..model.row_count() {
                    if let Some(mut row) = model.row_data(index) {
                        row.selected = match mode.as_str() {
                            "terminology-select-all" => true,
                            "terminology-select-new" => row.existing.is_empty(),
                            _ => false,
                        };
                        model.set_row_data(index, row);
                    }
                }
                update_selected_count(&app);
            });
        }
        "terminology-apply" => {
            if input.terminology_selected.is_empty() {
                return Err(CoreError::InvalidData("请至少选择一条候选术语。".into()));
            }
            let saved = ctx
                .app()
                .await
                .merge_project_terminology(&input.project_id, &input.terminology_selected)
                .await?;
            apply_save_result(ctx, &input.project_id, saved);
            let id = input.project_id.clone();
            ctx.update(move |ui| {
                let app = ui.global::<App>();
                if app.get_project_id().as_str() == id
                    && app.get_modal().as_str() == modal::TERMINOLOGY_EXTRACT
                {
                    app.set_modal("".into());
                }
            });
        }
        "add-term-dialog" => {
            ctx.update(|ui| {
                let app = ui.global::<App>();
                app.set_term_draft_source("".into());
                app.set_term_draft_target("".into());
                app.set_error("".into());
                app.set_modal(modal::ADD_TERM.into());
            });
        }
        "add-term-save" => {
            let saved = ctx
                .app()
                .await
                .merge_project_terminology(
                    &input.project_id,
                    &[(
                        input.term_draft_source.clone(),
                        input.term_draft_target.clone(),
                    )],
                )
                .await?;
            apply_save_result(ctx, &input.project_id, saved);
            let id = input.project_id.clone();
            ctx.update(move |ui| {
                let app = ui.global::<App>();
                if app.get_project_id().as_str() == id && app.get_modal().as_str() == modal::ADD_TERM
                {
                    app.set_modal("".into());
                }
            });
        }
        "terminology-select-affected" => {
            let ids = ctx
                .session
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .terminology_affected
                .remove(&input.project_id)
                .unwrap_or_default();
            if ids.is_empty() {
                sync_affected_count(ctx, &input.project_id);
                return Ok(true);
            }
            ctx.app()
                .await
                .save_text_selection(&input.project_id, &ids)
                .await?;
            let count = ids.len() as i32;
            ctx.set_selection(&input.project_id, ids);
            let id = input.project_id.clone();
            ctx.update(move |ui| {
                let app = ui.global::<App>();
                if app.get_project_id().as_str() != id {
                    return;
                }
                app.set_terminology_affected_count(0);
                app.set_selected_texts(count);
                app.set_text_query("".into());
                app.set_resource_filter(0);
                app.set_kind_filter(0);
                app.set_status_filter(0);
                app.set_only_selected(true);
                app.set_tab(project_tab::WORKBENCH);
                app.invoke_action("texts".into(), page_arg::FIRST.into());
            });
        }
        "terminology-dismiss-affected" => {
            ctx.session
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .terminology_affected
                .remove(&input.project_id);
            sync_affected_count(ctx, &input.project_id);
        }
        _ => return Ok(false),
    }
    Ok(true)
}
