//! Desktop binding for text selection, proofreading and translation tasks.

use crate::{
    Context, UiInput, show_dialog,
    view::{App, TermRow, TextRow},
};
use gamel10n_core::{
    CoreError,
    workbench::{WorkbenchFilter, WorkbenchRow, parse_terminology},
};
use gamel10n_translation::{execution::TranslationOptions, files::hash_text};
use serde_json::Value;
use slint::{ComponentHandle, Model, ModelRc, VecModel};
use std::{
    collections::{BTreeSet, HashSet},
    sync::atomic::Ordering,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const PAGE_SIZE: usize = 100;
static EDIT_WRITES: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
static SELECTION_WRITES: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
// Only the visible workbench needs a read; queued, superseded views must not load another snapshot.
static TEXT_READS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[derive(Clone, Default)]
pub struct WorkbenchView {
    pub query: String,
    pub resource: String,
    pub kind: String,
    pub status: String,
    pub only_selected: bool,
    pub offset: usize,
    pub active_text_id: String,
    pub text_scroll_y: f32,
    pub editor_scroll_y: f32,
}
pub fn capture_view(ctx: &Context, input: &UiInput) {
    if input.project_id.is_empty() {
        return;
    }
    let mut session = ctx
        .session
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let previous = session.workbench_views.get(&input.project_id);
    let active = if input.editor_id.is_empty() {
        previous
            .map(|view| view.active_text_id.clone())
            .unwrap_or_default()
    } else {
        input.editor_id.clone()
    };
    session.workbench_views.insert(
        input.project_id.clone(),
        WorkbenchView {
            query: input.text_query.clone(),
            resource: input.resource_filter.clone(),
            kind: input.kind_filter.clone(),
            status: input.status_filter.clone(),
            only_selected: input.only_selected,
            offset: input.offset,
            active_text_id: active,
            text_scroll_y: input.text_scroll_y,
            editor_scroll_y: input.editor_scroll_y,
        },
    );
}
pub fn saved_view(ctx: &Context, id: &str) -> WorkbenchView {
    ctx.session
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .workbench_views
        .get(id)
        .cloned()
        .unwrap_or_default()
}
pub fn restore_view(app: &App<'_>, view: &WorkbenchView) {
    clear_editor(app);
    app.set_selection_busy(false);
    app.set_text_query(view.query.clone().into());
    app.set_only_selected(view.only_selected);
    app.set_offset(view.offset as i32);
    app.set_kind_filter(
        [
            "all", "dialogue", "name", "choice", "ui", "item", "skill", "system",
        ]
        .iter()
        .position(|kind| *kind == view.kind)
        .unwrap_or(0) as i32,
    );
    app.set_status_filter(
        [
            "all",
            "missing",
            "pending",
            "reviewed",
            "edited",
            "invalid",
            "failed",
            "candidate",
        ]
        .iter()
        .position(|status| *status == view.status)
        .unwrap_or(0) as i32,
    );
    let resources = if view.resource.is_empty() {
        vec!["全部资源".into()]
    } else {
        vec!["全部资源".into(), view.resource.clone().into()]
    };
    app.set_resources(ModelRc::new(VecModel::from(resources)));
    app.set_resource_filter(i32::from(!view.resource.is_empty()));
    app.set_text_scroll_y(view.text_scroll_y);
    app.set_editor_scroll_y(view.editor_scroll_y);
}

fn clear_editor(app: &App<'_>) {
    app.set_editor_id("".into());
    app.set_editor_text("".into());
    app.set_editor_dirty(false);
    app.set_editor_location("".into());
    app.set_editor_source("".into());
    app.set_editor_context("".into());
    app.set_editor_draft("".into());
    app.set_editor_validation("".into());
    app.set_editor_has_issues(false);
    app.set_candidate("".into());
    app.set_editor_status("".into());
    app.set_editor_reviewed(false);
    app.set_terms(ModelRc::default());
}

fn filter(ctx: &Context, input: &UiInput) -> WorkbenchFilter {
    WorkbenchFilter {
        query: input.text_query.clone(),
        status: input.status_filter.clone(),
        kind: input.kind_filter.clone(),
        resource_path: (!input.resource_filter.is_empty()).then(|| input.resource_filter.clone()),
        selected_ids: input
            .only_selected
            .then(|| ctx.selection(&input.project_id)),
        ..Default::default()
    }
}

pub(crate) async fn ensure_selection(ctx: &Context, id: &str) -> Result<Vec<String>, CoreError> {
    if let Some(selection) = ctx
        .session
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .text_selection
        .get(id)
        .cloned()
    {
        return Ok(selection);
    }
    let saved = ctx.app().await.load_text_selection(id).await?;
    let mut session = ctx
        .session
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    Ok(session
        .text_selection
        .entry(id.into())
        .or_insert(saved)
        .clone())
}

async fn save_selection(ctx: &Context, id: &str, selection: Vec<String>) -> Result<(), CoreError> {
    ctx.app().await.save_text_selection(id, &selection).await?;
    ctx.set_selection(id, selection);
    Ok(())
}

async fn change_selection(ctx: &Context, action: &str, input: &UiInput) -> Result<(), CoreError> {
    let _gate = SELECTION_WRITES.lock().await;
    let mut selected: BTreeSet<_> = ensure_selection(ctx, &input.project_id)
        .await?
        .into_iter()
        .collect();
    match action {
        "toggle-text" => {
            if !selected.remove(&input.argument) {
                selected.insert(input.argument.clone());
            }
        }
        "select-clear" => selected.clear(),
        "select-page" => selected.extend(input.visible_text_ids.iter().cloned()),
        _ => selected.extend(
            ctx.app()
                .await
                .matching_text_ids(&input.project_id, &filter(ctx, input))
                .await?,
        ),
    }
    save_selection(ctx, &input.project_id, selected.into_iter().collect()).await
}

pub(crate) fn selection_action(action: &str) -> bool {
    matches!(
        action,
        "toggle-text" | "select-page" | "select-matching" | "select-clear"
    )
}

fn refresh_selection(ctx: &Context, id: &str) {
    let id = id.to_owned();
    let session = ctx.session.clone();
    ctx.update(move |ui| {
        let app = ui.global::<App>();
        if app.get_project_id().as_str() != id {
            return;
        }
        let saved = session.lock().unwrap_or_else(|error| error.into_inner());
        let selection: HashSet<_> = saved
            .text_selection
            .get(&id)
            .into_iter()
            .flatten()
            .map(String::as_str)
            .collect();
        app.set_selected_texts(selection.len() as i32);
        let rows = app.get_texts();
        for (index, mut row) in rows.iter().enumerate() {
            let selected = selection.contains(row.id.as_str());
            row.selected = selected;
            rows.set_row_data(index, row);
        }
        drop(saved);
        app.set_selection_busy(false);
        if app.get_only_selected() && app.get_page() == 0 && app.get_tab() == 1 {
            app.invoke_action("texts".into(), "current".into());
        }
    });
}

async fn save_editor(ctx: &Context, input: &UiInput, reviewed: bool) -> Result<(), CoreError> {
    if input.editor_id.is_empty() {
        return Ok(());
    }
    let _gate = EDIT_WRITES.lock().await;
    let app = ctx.app().await;
    app.save_translation_edit(
        &input.project_id,
        &input.editor_id,
        input.editor_text.clone(),
        reviewed,
    )
    .await?;
    let row = app
        .workbench_row(&input.project_id, &input.editor_id)
        .await?;
    let validation = validation_message(&row);
    let has_issues = row.failure.is_some() || !row.issues.is_empty();
    let id = input.project_id.clone();
    let editor = input.editor_id.clone();
    let text = input.editor_text.clone();
    let generation = ctx.edit_generation.clone();
    let revision = input.editor_revision;
    ctx.update(move |ui| {
        let a = ui.global::<App>();
        if a.get_project_id().as_str() == id
            && a.get_editor_id().as_str() == editor
            && a.get_editor_text().as_str() == text
            && generation.load(Ordering::SeqCst) == revision
        {
            a.set_editor_dirty(false);
            a.set_editor_reviewed(reviewed);
            a.set_editor_validation(validation.into());
            a.set_editor_has_issues(has_issues);
            a.set_editor_status(
                if reviewed {
                    "已校对 · 手工译文受保护"
                } else {
                    "已保存 · 手工译文受保护"
                }
                .into(),
            );
        }
    });
    Ok(())
}

pub async fn flush_editor(ctx: &Context, input: &UiInput) -> Result<(), CoreError> {
    if input.editor_dirty && !input.editor_id.is_empty() {
        save_editor(ctx, input, false).await?;
    }
    Ok(())
}

fn row_metadata(row: &WorkbenchRow) -> String {
    let status = if !row.issues.is_empty() {
        "格式问题"
    } else if row.reviewed {
        "已校对"
    } else if row.edited {
        "手工编辑"
    } else if row.text.is_some() {
        "待校对"
    } else {
        "未翻译"
    };
    format!(
        "{}{} · {}{}{}",
        row.unit
            .speaker
            .as_ref()
            .map(|speaker| format!("{speaker} · "))
            .unwrap_or_default(),
        row.unit.kind,
        status,
        if row.failure.is_some() {
            " · 上次请求失败"
        } else {
            ""
        },
        if row.candidate.is_some() {
            " · 有候选"
        } else {
            ""
        }
    )
}

fn validation_message(row: &WorkbenchRow) -> String {
    let validation = if row.text.is_none() {
        "尚未翻译，可输入译文或使用翻译功能。".into()
    } else if row.issues.is_empty() {
        "文本格式检查通过。确认校对前请检查含义、角色名和控制标记。".into()
    } else {
        format!(
            "{}\n可保留为草稿；修正后才能确认校对或加入补丁。",
            row.issues
                .iter()
                .map(|issue| issue.message.as_str())
                .collect::<Vec<_>>()
                .join("；")
        )
    };
    match &row.failure {
        Some(failure) => format!("{failure}\n{validation}"),
        None => validation,
    }
}

async fn render_editor(ctx: &Context, input: &UiInput, editor_id: &str) -> Result<(), CoreError> {
    let id = &input.project_id;
    let revision = input.editor_revision;
    let view_revision = input.view_revision;
    let app = ctx.app().await;
    let row = app.workbench_row(id, editor_id).await?;
    let mut context = app.workbench_context(id, editor_id).await?;
    let project = app.open_project(id).await?;
    if let Some(previous) = &row.previous_edit {
        context.push_str(&format!(
            "\n\n原文、上下文或目标语言已变化，旧校对未自动采用：\n{previous}"
        ));
    }
    let terminology =
        parse_terminology(project.settings.get("terminology").unwrap_or(&Value::Null));
    let terms: Vec<_> = terminology
        .into_iter()
        .filter(|(source, _)| {
            row.unit.source_text.contains(source)
                || row.unit.context.contains(source)
                || row
                    .unit
                    .speaker
                    .as_ref()
                    .is_some_and(|speaker| speaker.contains(source))
        })
        .collect();
    let id = id.to_owned();
    let generation = ctx.edit_generation.clone();
    let view_generation = ctx.generation.clone();
    let session = ctx.session.clone();
    let previous_editor = input.editor_id.clone();
    ctx.update(move |ui| {
        let a = ui.global::<App>();
        if a.get_project_id().as_str() != id
            || generation.load(Ordering::SeqCst) != revision
            || view_generation.load(Ordering::SeqCst) != view_revision
            || a.get_page() != 0
            || a.get_tab() != 1
            || a.get_editor_id().as_str() != previous_editor
                && a.get_editor_id().as_str() != row.unit.id
            || a.get_editor_dirty()
        {
            return;
        }
        let validation = validation_message(&row);
        let has_issues = row.failure.is_some() || !row.issues.is_empty();
        let status = if row.reviewed {
            "已校对 · 手工译文受保护"
        } else if row.edited {
            "已保存 · 手工译文受保护"
        } else if row.text.is_some() {
            "已保存 · 等待校对"
        } else {
            "尚未翻译"
        };
        a.set_editor_location(
            format!(
                "{}\n位置：{} · {}",
                row.unit.resource_path, row.unit.locator, row.unit.kind
            )
            .into(),
        );
        let mut saved = session.lock().unwrap_or_else(|error| error.into_inner());
        let view = saved.workbench_views.entry(id.clone()).or_default();
        let scroll = if view.active_text_id == row.unit.id {
            view.editor_scroll_y
        } else {
            0.0
        };
        view.active_text_id = row.unit.id.clone();
        view.editor_scroll_y = scroll;
        a.set_editor_scroll_y(scroll);
        drop(saved);
        a.set_editor_source(row.unit.source_text.into());
        a.set_editor_context(context.into());
        a.set_editor_text(row.text.unwrap_or_default().into());
        a.set_editor_draft(
            row.model_text
                .unwrap_or_else(|| "没有模型初稿。".into())
                .into(),
        );
        a.set_editor_status(status.into());
        a.set_editor_validation(validation.into());
        a.set_editor_has_issues(has_issues);
        a.set_editor_reviewed(row.reviewed);
        a.set_editor_dirty(false);
        a.set_editor_id(row.unit.id.into());
        a.set_candidate(row.candidate.unwrap_or_default().into());
        a.set_terms(ModelRc::new(VecModel::from(
            terms
                .into_iter()
                .map(|(source, target)| TermRow {
                    source: source.into(),
                    target: target.into(),
                })
                .collect::<Vec<_>>(),
        )));
    });
    Ok(())
}

async fn texts(ctx: &Context, input: &UiInput) -> Result<(), CoreError> {
    let revision = input.view_revision;
    let is_current = || {
        ctx.generation.load(Ordering::SeqCst) == revision
            && ctx.is_requested_project(&input.project_id)
    };
    if input.project_id.is_empty() || input.page != 0 || input.tab != 1 || !is_current() {
        return Ok(());
    }
    let _read = TEXT_READS.lock().await;
    if !is_current() {
        return Ok(());
    }
    let editor_revision = input.editor_revision;
    if matches!(input.argument.as_str(), "0" | "next" | "previous") {
        flush_editor(ctx, input).await?;
        if !is_current() {
            return Ok(());
        }
    }
    capture_view(ctx, input);
    let saved = saved_view(ctx, &input.project_id);
    let selection = ensure_selection(ctx, &input.project_id).await;
    if !is_current() {
        return Ok(());
    }
    selection?;
    let offset = match input.argument.as_str() {
        "next" => input.offset.saturating_add(PAGE_SIZE),
        "previous" => input.offset.saturating_sub(PAGE_SIZE),
        "0" => 0,
        _ => input.offset,
    };
    let app = ctx.app().await;
    let options = filter(ctx, input);
    let page = app
        .workbench_page(&input.project_id, &options, offset, PAGE_SIZE)
        .await;
    if !is_current() {
        return Ok(());
    }
    let mut page = page?;
    if page.rows.is_empty() && page.total > 0 && offset > 0 {
        page = app
            .workbench_page(
                &input.project_id,
                &options,
                (page.total - 1) / PAGE_SIZE * PAGE_SIZE,
                PAGE_SIZE,
            )
            .await?;
    }
    let preferred = if input.editor_id.is_empty() {
        &saved.active_text_id
    } else {
        &input.editor_id
    };
    let first = page
        .rows
        .iter()
        .find(|row| row.unit.id == *preferred)
        .or_else(|| page.rows.first())
        .map(|row| row.unit.id.clone());
    let clear = first.is_none();
    let id = input.project_id.clone();
    let generation = ctx.generation.clone();
    let session = ctx.session.clone();
    let resource = input.resource_filter.clone();
    let scroll = if matches!(input.argument.as_str(), "0" | "next" | "previous")
        || input.offset != page.offset
    {
        0.0
    } else {
        input.text_scroll_y
    };
    let edit_generation = ctx.edit_generation.clone();
    ctx.update(move |ui| {
        let a = ui.global::<App>();
        if a.get_project_id().as_str() != id
            || a.get_page() != 0
            || a.get_tab() != 1
            || generation.load(Ordering::SeqCst) != revision
        {
            return;
        }
        let mut saved = session.lock().unwrap_or_else(|error| error.into_inner());
        let selected: HashSet<_> = saved
            .text_selection
            .get(&id)
            .into_iter()
            .flatten()
            .map(String::as_str)
            .collect();
        let rows = page
            .rows
            .into_iter()
            .map(|row| {
                let metadata = row_metadata(&row);
                TextRow {
                    id: row.unit.id.clone().into(),
                    location: format!("{} · {}", row.unit.resource_path, row.unit.locator).into(),
                    source: row.unit.source_text.into(),
                    translation: row.text.unwrap_or_default().into(),
                    metadata: metadata.into(),
                    selected: selected.contains(row.unit.id.as_str()),
                }
            })
            .collect::<Vec<_>>();
        a.set_shown(rows.len() as i32);
        a.set_text_total(page.total.min(i32::MAX as usize) as i32);
        a.set_offset(page.offset.min(i32::MAX as usize) as i32);
        a.set_selected_texts(selected.len() as i32);
        a.set_texts(ModelRc::new(VecModel::from(rows)));
        let resource_index = page
            .resources
            .iter()
            .position(|path| *path == resource)
            .map_or(0, |index| index + 1);
        let mut resources = vec!["全部资源".into()];
        resources.extend(page.resources.into_iter().map(Into::into));
        a.set_resources(ModelRc::new(VecModel::from(resources)));
        a.set_resource_filter(resource_index as i32);
        a.set_text_scroll_y(scroll);
        let view = saved.workbench_views.entry(id.clone()).or_default();
        view.offset = page.offset;
        view.text_scroll_y = scroll;
        drop(saved);
        if clear
            && !a.get_editor_dirty()
            && edit_generation.load(Ordering::SeqCst) == editor_revision
        {
            clear_editor(&a);
        }
        a.set_counts(
            format!(
                "{} 条 · 已译 {} · 已校对 {} · 格式问题 {} · 翻译失败 {}",
                page.summary.total,
                page.summary.translated,
                page.summary.reviewed,
                page.summary.invalid,
                page.summary.failed
            )
            .into(),
        );
        a.set_loading(false);
    });
    if let Some(first) = first
        && is_current()
        && ctx.edit_generation.load(Ordering::SeqCst) == editor_revision
    {
        render_editor(ctx, input, &first).await?;
    }
    Ok(())
}

pub async fn handle(ctx: &Context, action: &str, input: &UiInput) -> Result<bool, CoreError> {
    match action {
        "insert-term" => {
            let id = input.project_id.clone();
            let editor = input.editor_id.clone();
            let text: slint::SharedString = input.argument.clone().into();
            ctx.update(move |ui| {
                let a = ui.global::<App>();
                if a.get_project_id().as_str() == id
                    && a.get_editor_id().as_str() == editor
                    && a.get_editor_editable()
                {
                    ui.window()
                        .dispatch_event(slint::platform::WindowEvent::KeyPressed {
                            text: text.clone(),
                        });
                    ui.window()
                        .dispatch_event(slint::platform::WindowEvent::KeyReleased { text });
                }
            });
        }
        "texts" => texts(ctx, input).await?,
        "toggle-text" | "select-page" | "select-matching" | "select-clear" => {
            let result = change_selection(ctx, action, input).await;
            refresh_selection(ctx, &input.project_id);
            result?;
        }
        "edit-text" => {
            flush_editor(ctx, input).await?;
            render_editor(ctx, input, &input.argument).await?;
        }
        "save-edit" => {
            flush_editor(ctx, input).await?;
            ctx.refresh_texts(&input.project_id);
        }
        "edit-changed" => {
            let revision = input.editor_revision;
            let id = input.project_id.clone();
            let editor = input.editor_id.clone();
            let generation = ctx.edit_generation.clone();
            ctx.update(move |ui| {
                let a = ui.global::<App>();
                if a.get_project_id().as_str() == id
                    && a.get_editor_id().as_str() == editor
                    && generation.load(Ordering::SeqCst) == revision
                {
                    a.set_editor_status("等待保存…".into());
                    a.set_editor_reviewed(false);
                }
            });
            tokio::time::sleep(Duration::from_millis(400)).await;
            let _gate = EDIT_WRITES.lock().await;
            if ctx.edit_generation.load(Ordering::SeqCst) != revision {
                return Ok(true);
            }
            let app = ctx.app().await;
            app.save_translation_edit(
                &input.project_id,
                &input.editor_id,
                input.editor_text.clone(),
                false,
            )
            .await?;
            let row = app
                .workbench_row(&input.project_id, &input.editor_id)
                .await?;
            let validation = validation_message(&row);
            let has_issues = row.failure.is_some() || !row.issues.is_empty();
            let id = input.project_id.clone();
            let editor = input.editor_id.clone();
            let text = input.editor_text.clone();
            let generation = ctx.edit_generation.clone();
            ctx.update(move |ui| {
                let a = ui.global::<App>();
                if a.get_project_id().as_str() == id
                    && a.get_editor_id().as_str() == editor
                    && a.get_editor_text().as_str() == text
                    && generation.load(Ordering::SeqCst) == revision
                {
                    a.set_editor_dirty(false);
                    a.set_editor_status("已保存 · 手工译文受保护".into());
                    a.set_editor_validation(validation.into());
                    a.set_editor_has_issues(has_issues);
                }
            });
            drop(_gate);
            ctx.refresh_texts(&input.project_id);
        }
        "editor-step" | "review-next" => {
            if action == "review-next" {
                save_editor(ctx, input, true).await?;
            } else {
                flush_editor(ctx, input).await?;
            }
            let direction = if action == "review-next" {
                1
            } else {
                input.argument.parse::<i32>().unwrap_or(1)
            };
            let mut page = input.clone();
            page.argument = "current".into();
            if let Some((next, index)) = ctx
                .app()
                .await
                .adjacent_text(
                    &input.project_id,
                    &filter(ctx, input),
                    &input.editor_id,
                    direction,
                )
                .await?
            {
                render_editor(ctx, input, &next).await?;
                page.editor_id = next;
                page.offset = index / PAGE_SIZE * PAGE_SIZE;
            }
            texts(ctx, &page).await?;
        }
        "unreview" => {
            flush_editor(ctx, input).await?;
            ctx.app()
                .await
                .mark_reviewed(
                    &input.project_id,
                    std::slice::from_ref(&input.editor_id),
                    false,
                )
                .await?;
            ctx.refresh_texts(&input.project_id);
        }
        "bulk-review" => {
            show_dialog(
                ctx,
                "bulk-review",
                "确认所选译文已校对",
                format!(
                    "请确认已检查所选 {} 条译文的含义和表达。未翻译、有格式问题或已校对的文本会跳过。",
                    ensure_selection(ctx, &input.project_id).await?.len()
                ),
            );
        }
        "bulk-review-confirm" => {
            flush_editor(ctx, input).await?;
            let selection = ensure_selection(ctx, &input.project_id).await?;
            let count = ctx
                .app()
                .await
                .mark_reviewed(&input.project_id, &selection, true)
                .await?;
            let id = input.project_id.clone();
            ctx.update(move |ui| {
                let a = ui.global::<App>();
                if a.get_project_id().as_str() == id {
                    a.set_status(
                        format!("已确认 {count} 条译文校对，跳过未翻译或格式异常的文本。").into(),
                    );
                }
            });
            ctx.refresh_texts(&input.project_id);
        }
        "candidate" => {
            flush_editor(ctx, input).await?;
            let app = ctx.app().await;
            let row = app
                .workbench_row(&input.project_id, &input.editor_id)
                .await?;
            if row.candidate.is_none() {
                let result = app
                    .translate_project(
                        &input.project_id,
                        TranslationOptions {
                            batch_size: 1,
                            unit_ids: Some(vec![input.editor_id.clone()]),
                            candidate_only: true,
                            ..Default::default()
                        },
                    )
                    .await;
                ctx.refresh_texts(&input.project_id);
                result?;
            } else {
                ctx.refresh_texts(&input.project_id);
            }
        }
        "accept-candidate" | "discard-candidate" | "undo-edit" => {
            flush_editor(ctx, input).await?;
            let app = ctx.app().await;
            match action {
                "accept-candidate" => {
                    app.adopt_candidate(&input.project_id, &input.editor_id)
                        .await?
                }
                "discard-candidate" => {
                    app.discard_candidate(&input.project_id, &input.editor_id)
                        .await?
                }
                _ => {
                    app.undo_translation_edit(&input.project_id, &input.editor_id)
                        .await?
                }
            }
            ctx.refresh_texts(&input.project_id);
        }
        "retranslate-dialog" => show_dialog(
            ctx,
            "retranslate",
            "重新翻译所选文本",
            format!(
                "将重新翻译所选 {} 条文本，包括已编辑和已校对的文本。新译文通过检查后替换当前译文并标为待校对；失败文本保留原译文。",
                ensure_selection(ctx, &input.project_id).await?.len()
            ),
        ),
        "translate" => {
            flush_editor(ctx, input).await?;
            let app = ctx.app().await;
            let previous = app
                .translation_scope(&input.project_id)
                .await?
                .filter(|scope| !scope.completed);
            if input.argument == "all"
                && let Some(scope) = &previous
                && scope.unit_ids.is_some()
            {
                show_dialog(
                    ctx,
                    "resume-translation",
                    "继续上次翻译",
                    format!(
                        "上次任务选择了 {} 条文本。继续原选区时只处理尚未成功的文本，已完成结果不会重复请求。",
                        scope.unit_ids.as_ref().map_or(0, Vec::len)
                    ),
                );
                return Ok(true);
            }
            if input.argument == "resume" {
                let result = app.resume_translation(&input.project_id).await;
                ctx.refresh_texts(&input.project_id);
                result?;
                return Ok(true);
            }
            let ids = if matches!(input.argument.as_str(), "all" | "force-all") {
                None
            } else {
                let selection = ensure_selection(ctx, &input.project_id).await?;
                if selection.is_empty() {
                    return Err(CoreError::InvalidData("请先选择要翻译的文本。".into()));
                }
                Some(selection)
            };
            let mut options = TranslationOptions {
                batch_size: input.batch_size.max(1) as usize,
                unit_ids: ids,
                build_patch_after: input.argument == "trial",
                ..Default::default()
            };
            if input.argument == "retranslate" {
                let previous = previous.as_ref().map(|scope| scope.options());
                let same_selection = |previous: &TranslationOptions| {
                    previous.retranslation_id.is_some()
                        && previous
                            .unit_ids
                            .as_ref()
                            .map(|ids| ids.iter().collect::<BTreeSet<_>>())
                            == options
                                .unit_ids
                                .as_ref()
                                .map(|ids| ids.iter().collect::<BTreeSet<_>>())
                };
                options.retranslation_id = previous
                    .filter(same_selection)
                    .and_then(|options| options.retranslation_id)
                    .or_else(|| {
                        Some(
                            hash_text(&format!(
                                "{}{}",
                                input.project_id,
                                SystemTime::now()
                                    .duration_since(UNIX_EPOCH)
                                    .unwrap_or_default()
                                    .as_nanos(),
                            ))[..32]
                                .into(),
                        )
                    });
            }
            let result = app.translate_project(&input.project_id, options).await;
            ctx.refresh_texts(&input.project_id);
            result?;
        }
        _ => return Ok(false),
    }
    Ok(true)
}
