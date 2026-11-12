use crate::{
    Context, UiSession, install_actions,
    view::{App, MainWindow},
};
use gamel10n_core::{Application, StoredAnalysis, ToolPaths, settings::ApiProfile};
use gamel10n_engines::protocol::ExtractionResult;
use gamel10n_translation::{TranslationUnit, files::write_json};
use slint::{ComponentHandle, Model};
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::sync::{RwLock, oneshot};

#[test]
fn workbench_gives_extra_window_height_to_content_instead_of_page_controls() {
    slint::slint! {
        import { Button } from "std-widgets.slint";
        import { Workbench } from "../ui/workbench.slint";
        import { App, TextRow } from "../ui/state.slint";
        export { App, TextRow }
        export component WorkbenchLayoutCheck inherits Window {
            default-font-family: "Microsoft YaHei UI";
            default-font-size: 14px;
            out property <length> action-height: actions.height;
            out property <length> tab-height: tabs.height;
            out property <length> content-height: workbench.height;
            out property <length> content-width: workbench.width;
            VerticalLayout {
                padding: 20px;
                spacing: 12px;
                Text { text: "游戏项目"; font-size: 26px; }
                actions := HorizontalLayout {
                    Button { text: "开始全量翻译"; }
                    Rectangle { horizontal-stretch: 1; }
                }
                tabs := HorizontalLayout {
                    Button { text: "识别与适配"; }
                    Button { text: "翻译与校对"; }
                    Rectangle { horizontal-stretch: 1; }
                }
                workbench := Workbench {}
            }
        }
    }
    i_slint_backend_testing::init_no_event_loop();
    let ui = WorkbenchLayoutCheck::new().unwrap();
    ui.show().unwrap();
    for loading in [true, false] {
        let app = ui.global::<App>();
        app.set_loading(loading);
        if !loading {
            app.set_texts(slint::ModelRc::new(slint::VecModel::from(vec![TextRow {
                id: "line-1".into(),
                location: "story.ks:1".into(),
                source: "原文".into(),
                translation: "译文".into(),
                ..Default::default()
            }])));
        }
        for (width, height) in [(1420, 900), (2560, 1440), (1920, 1080), (1420, 900)] {
            ui.window()
                .set_size(slint::PhysicalSize::new(width, height));
            assert!(
                ui.get_action_height() < 50.0 && ui.get_tab_height() < 50.0,
                "page controls stretched at {width}x{height}, loading={loading}: actions={}, tabs={}",
                ui.get_action_height(),
                ui.get_tab_height()
            );
            assert!(
                ui.get_content_height() > height as f32 - 200.0
                    && ui.get_content_width() > width as f32 - 50.0,
                "workbench did not fill the remaining area at {width}x{height}, loading={loading}: {}x{}",
                ui.get_content_width(),
                ui.get_content_height()
            );
        }
    }
}

async fn invoke_action(ui: &MainWindow, ctx: &Context, action: &str, argument: &str) {
    ui.global::<App>()
        .invoke_action(action.into(), argument.into());
    wait_for_actions(ui, ctx, action).await;
}

async fn wait_for_actions(ui: &MainWindow, ctx: &Context, action: &str) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if ctx.active_actions.load(Ordering::Acquire) == 0 {
                // Actions enqueue their UI updates before dropping PendingAction.
                // Cross the queue once more to include updates that start another action.
                let (sender, receiver) = oneshot::channel();
                slint::invoke_from_event_loop(move || {
                    let _ = sender.send(());
                })
                .unwrap();
                receiver.await.unwrap();
                if ctx.active_actions.load(Ordering::Acquire) == 0 {
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("workbench action {action:?} did not finish"));
    assert!(
        ui.global::<App>().get_error().is_empty(),
        "workbench action {action:?} failed: {}",
        ui.global::<App>().get_error()
    );
}

#[test]
fn project_actions_keep_settings_selection_and_saved_edits_in_sync_across_tabs() {
    i_slint_backend_testing::init_integration_test_with_system_time();
    let root = tempfile::tempdir().unwrap();
    let game = root.path().join("game");
    std::fs::create_dir(&game).unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let tools = ToolPaths::new(root.path().join("tools"));
    let host = runtime
        .block_on(Application::open(root.path().join("data"), &tools))
        .unwrap();
    let project = runtime.block_on(host.add_project(&game)).unwrap();
    let other_game = root.path().join("other-game");
    std::fs::create_dir(&other_game).unwrap();
    let other_project = runtime.block_on(host.add_project(&other_game)).unwrap();
    let units = (0..221)
        .map(|index| TranslationUnit {
            id: format!("line-{index:03}"),
            resource_path: if index < 120 { "story-a" } else { "story-b" }.into(),
            locator: index.to_string(),
            source_text: format!("Source line {index:03}"),
            context: "Local proofreading fixture".into(),
            speaker: None,
            kind: "dialogue".into(),
            protected_tokens: None,
        })
        .collect();
    write_json(
        &host
            .project_directory(&project.id)
            .unwrap()
            .join("extraction/native.json"),
        &StoredAnalysis {
            schema_version: 1,
            format: "test".into(),
            source_directory: game,
            resource_files: vec!["story-a".into(), "story-b".into()],
            source_hashes: BTreeMap::new(),
            extraction: ExtractionResult {
                units,
                warnings: Vec::new(),
            },
            adapter: None,
            probe: None,
        },
    )
    .unwrap();

    let ui = MainWindow::new().unwrap();
    let ctx = Context {
        host: Arc::new(RwLock::new(host)),
        window: ui.as_weak(),
        session: Arc::new(Mutex::new(UiSession {
            requested_project: project.id.clone(),
            ..Default::default()
        })),
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
    install_actions(&ui, &ctx);
    ui.global::<App>().on_task_api_for(|operation, summary| {
        crate::recognition_ui::task_api(&operation, &summary).into()
    });
    ui.global::<App>().set_project_id(project.id.clone().into());
    ui.global::<App>().set_page(0);
    ui.global::<App>().set_tab(1);

    let _runtime_guard = runtime.enter();
    slint::spawn_local(async move {
        let app = ui.global::<App>();
        invoke_action(&ui, &ctx, "texts", "0").await;
        assert_eq!(app.get_text_total(), 221);
        assert_eq!(app.get_texts().row_count(), 100);

        invoke_action(&ui, &ctx, "select-page", "").await;
        assert_eq!(app.get_selected_texts(), 100);
        assert!(app.get_texts().iter().all(|row| row.selected));

        invoke_action(&ui, &ctx, "texts", "next").await;
        assert_eq!(app.get_offset(), 100);
        assert!(app.get_texts().iter().all(|row| !row.selected));
        invoke_action(&ui, &ctx, "toggle-text", "line-100").await;
        assert_eq!(app.get_selected_texts(), 101);
        assert!(app.get_texts().row_data(0).unwrap().selected);
        assert!(app.get_texts().iter().skip(1).all(|row| !row.selected));
        invoke_action(&ui, &ctx, "texts", "previous").await;
        assert_eq!(app.get_selected_texts(), 101);
        assert!(app.get_texts().iter().all(|row| row.selected));

        let resource = app
            .get_resources()
            .iter()
            .position(|resource| resource.as_str() == "story-b")
            .unwrap();
        app.set_resource_filter(resource as i32);
        invoke_action(&ui, &ctx, "texts", "0").await;
        assert_eq!(app.get_text_total(), 101);
        assert!(app.get_texts().iter().all(|row| !row.selected));
        invoke_action(&ui, &ctx, "select-matching", "").await;
        assert_eq!(app.get_selected_texts(), 202);
        assert!(app.get_texts().iter().all(|row| row.selected));
        invoke_action(&ui, &ctx, "texts", "next").await;
        assert_eq!(app.get_offset(), 100);
        let last = app.get_texts().row_data(0).unwrap();
        assert_eq!(last.id.as_str(), "line-220");
        assert!(last.selected);
        let persisted = ctx
            .app()
            .await
            .load_text_selection(&project.id)
            .await
            .unwrap();
        assert_eq!(persisted.len(), 202);
        assert!(persisted.iter().any(|id| id == "line-000"));
        assert!(persisted.iter().any(|id| id == "line-100"));
        assert!(persisted.iter().any(|id| id == "line-220"));
        assert!(!persisted.iter().any(|id| id == "line-101"));

        app.set_only_selected(true);
        invoke_action(&ui, &ctx, "texts", "0").await;
        assert_eq!(app.get_text_total(), 101);
        assert!(app.get_texts().iter().all(|row| row.selected));
        invoke_action(&ui, &ctx, "select-clear", "").await;
        assert_eq!(app.get_selected_texts(), 0);
        assert_eq!(app.get_text_total(), 0);
        assert_eq!(app.get_texts().row_count(), 0);
        assert_eq!(app.get_offset(), 0);
        assert!(app.get_editor_id().is_empty());
        assert!(app.get_editor_source().is_empty());

        app.set_only_selected(false);
        app.set_resource_filter(0);
        invoke_action(&ui, &ctx, "texts", "0").await;
        invoke_action(&ui, &ctx, "select-page", "").await;
        app.set_only_selected(true);
        invoke_action(&ui, &ctx, "texts", "0").await;
        invoke_action(&ui, &ctx, "toggle-text", "line-000").await;
        assert_eq!(app.get_selected_texts(), 99);
        assert_eq!(app.get_text_total(), 99);
        assert_eq!(app.get_texts().row_count(), 99);
        assert!(
            app.get_texts()
                .iter()
                .all(|row| row.selected && row.id.as_str() != "line-000")
        );
        invoke_action(&ui, &ctx, "select-clear", "").await;
        assert_eq!(app.get_texts().row_count(), 0);

        app.set_only_selected(false);
        invoke_action(&ui, &ctx, "texts", "0").await;
        invoke_action(&ui, &ctx, "edit-text", "line-099").await;
        assert_eq!(app.get_editor_id().as_str(), "line-099");
        app.set_editor_text("保存后切换到下一页".into());
        app.set_editor_dirty(true);
        app.invoke_action("edit-changed".into(), "".into());
        app.invoke_action("editor-step".into(), "1".into());
        // A previous save can finish while the next-page editor is being loaded.
        ctx.refresh_texts(&project.id);
        wait_for_actions(&ui, &ctx, "editor-step with background refresh").await;
        assert_eq!(app.get_editor_id().as_str(), "line-100");
        assert_eq!(app.get_editor_source().as_str(), "Source line 100");
        assert_eq!(app.get_offset(), 100);
        assert_eq!(app.get_texts().row_data(0).unwrap().id.as_str(), "line-100");
        assert!(!app.get_editor_dirty());

        invoke_action(&ui, &ctx, "editor-step", "-1").await;
        assert_eq!(app.get_offset(), 0);
        assert_eq!(app.get_editor_id().as_str(), "line-099");
        assert_eq!(app.get_editor_text().as_str(), "保存后切换到下一页");
        assert_eq!(
            app.get_texts().row_data(99).unwrap().translation.as_str(),
            "保存后切换到下一页"
        );
        app.set_editor_text("确认校对后切换到下一页".into());
        app.set_editor_dirty(true);
        invoke_action(&ui, &ctx, "review-next", "").await;
        assert_eq!(app.get_offset(), 100);
        assert_eq!(app.get_editor_id().as_str(), "line-100");
        assert!(!app.get_editor_dirty());
        let reviewed = ctx
            .app()
            .await
            .workbench_row(&project.id, "line-099")
            .await
            .unwrap();
        assert!(reviewed.reviewed);
        assert_eq!(reviewed.text.as_deref(), Some("确认校对后切换到下一页"));

        invoke_action(&ui, &ctx, "editor-step", "-1").await;
        assert_eq!(app.get_offset(), 0);
        assert_eq!(app.get_editor_id().as_str(), "line-099");
        assert!(app.get_editor_reviewed());
        assert_eq!(app.get_editor_text().as_str(), "确认校对后切换到下一页");
        let row = app.get_texts().row_data(99).unwrap();
        assert_eq!(row.translation.as_str(), "确认校对后切换到下一页");
        assert!(row.metadata.contains("已校对"));

        // Saving an unchanged reviewed text must not revoke its review.
        invoke_action(&ui, &ctx, "save-edit", "").await;
        assert!(app.get_editor_reviewed());
        invoke_action(&ui, &ctx, "unreview", "").await;
        app.set_status_filter(2);
        invoke_action(&ui, &ctx, "texts", "0").await;
        assert_eq!(app.get_text_total(), 1);
        assert_eq!(app.get_editor_id().as_str(), "line-099");
        invoke_action(&ui, &ctx, "review-next", "").await;
        assert_eq!(app.get_text_total(), 0);
        assert!(app.get_editor_id().is_empty());
        assert!(app.get_editor_text().is_empty());
        assert!(app.get_editor_status().is_empty());
        assert!(!app.get_editor_reviewed());

        // Returning to a previous filter may restore an editor only from that result set.
        app.set_status_filter(0);
        app.set_text_query("line 100".into());
        invoke_action(&ui, &ctx, "texts", "0").await;
        assert_eq!(app.get_text_total(), 1);
        assert_eq!(app.get_editor_id().as_str(), "line-100");
        app.set_editor_text("切换筛选前保存".into());
        app.set_editor_dirty(true);
        app.invoke_action("edit-changed".into(), "".into());
        app.set_text_query("no matching text".into());
        invoke_action(&ui, &ctx, "texts", "0").await;
        assert_eq!(app.get_text_total(), 0);
        assert!(app.get_editor_id().is_empty());
        assert_eq!(
            ctx.app()
                .await
                .workbench_row(&project.id, "line-100")
                .await
                .unwrap()
                .text
                .as_deref(),
            Some("切换筛选前保存")
        );
        assert!(
            ctx.app()
                .await
                .load_text_selection(&project.id)
                .await
                .unwrap()
                .is_empty()
        );

        app.set_text_query("line 100".into());
        invoke_action(&ui, &ctx, "texts", "0").await;
        app.set_editor_text("离开项目前保存".into());
        app.set_editor_dirty(true);
        app.invoke_action("edit-changed".into(), "".into());
        invoke_action(&ui, &ctx, "project", &other_project.id).await;
        ctx.refresh_texts(&project.id);
        wait_for_actions(&ui, &ctx, "previous project refresh").await;
        assert_eq!(app.get_project_id().as_str(), other_project.id);
        assert_eq!(app.get_texts().row_count(), 0);
        assert_eq!(app.get_selected_texts(), 0);
        assert!(app.get_editor_id().is_empty());
        assert!(app.get_editor_text().is_empty());
        assert!(app.get_editor_status().is_empty());
        assert_eq!(app.get_terms().row_count(), 0);
        assert_eq!(
            ctx.app()
                .await
                .workbench_row(&project.id, "line-100")
                .await
                .unwrap()
                .text
                .as_deref(),
            Some("离开项目前保存")
        );

        let host = ctx.app().await;
        for (id, protocol) in [("first", "ChatCompletions"), ("second", "Responses")] {
            host.save_api_profile(
                ApiProfile {
                    id: id.into(),
                    name: id.into(),
                    model: format!("{id}-model"),
                    endpoint: "http://127.0.0.1:1/v1".into(),
                    protocol: protocol.into(),
                    ..Default::default()
                },
                Some(String::new()),
                true,
            )
            .await
            .unwrap();
        }
        app.set_task_operation("adapt".into());
        app.set_task_api_summary(
            host.task_api_summary(&other_project.id)
                .await
                .unwrap()
                .into(),
        );
        for (index, id, protocol) in [(1, "second", "Responses"), (0, "first", "ChatCompletions")] {
            invoke_action(&ui, &ctx, "tab", "3").await;
            app.set_analysis_index(index);
            invoke_action(&ui, &ctx, "analysis-api", "").await;
            invoke_action(&ui, &ctx, "tab", "0").await;
            let (provider, _) = host.analysis_provider(&other_project.id).await.unwrap();
            assert_eq!(provider.model(), format!("{id}-model"));
            assert_eq!(
                app.get_task_api().as_str(),
                format!("{id}（{} · {protocol}）", provider.model())
            );
            assert_eq!(
                host.settings()
                    .await
                    .unwrap()
                    .api_configuration
                    .analysis_profile_id
                    .as_deref(),
                Some("first")
            );
            assert_eq!(app.get_loaded_analysis_index(), index);
        }

        let directory = host.data_directory().join("metrics");
        std::fs::create_dir_all(&directory).unwrap();
        let records = [("first-model", 100, 80), ("second-model", 1000, 900)].into_iter().enumerate().map(|(index, (model, input, cached))| serde_json::json!({
            "id": format!("metrics-{index}"), "projectId": project.id, "projectName": "Metrics fixture",
            "softwareVersion": "0.1.0", "buildGitHash": format!("build-{index}"), "model": model,
            "task": "adapt", "mode": "new", "status": "completed", "startedAt": "2026-11-12T04:00:00Z", "endedAt": "2026-11-12T04:01:00Z", "elapsedMs": 60000,
            "api": {"requests":1,"usageRequests":1,"cacheRequests":1,"inputTokens":input,"outputTokens":20,"cachedTokens":cached,"cacheInputTokens":input,"apiMs":50000}
        }).to_string()).collect::<Vec<_>>().join("\n");
        std::fs::write(directory.join("runs-202611.jsonl"), records).unwrap();
        invoke_action(&ui, &ctx, "navigate", "metrics").await;
        assert_eq!(app.get_page(), 5);
        assert!(app.get_metrics_overview().contains("2 次运行"));
        app.set_metrics_dimension(2);
        app.set_metrics_measure(1);
        invoke_action(&ui, &ctx, "metrics", "refresh").await;
        assert_eq!(app.get_metric_groups().row_count(), 2);
        assert_eq!(app.get_metric_groups().row_data(1).unwrap().fraction, 1.0);
        let model_index = app.get_metrics_models().iter().position(|model| model == "second-model").unwrap();
        app.set_metrics_model_index(model_index as i32);
        invoke_action(&ui, &ctx, "metrics", "refresh").await;
        assert_eq!(app.get_metric_groups().row_count(), 1);
        assert!(app.get_metrics_tokens().contains("1000"));
        app.set_metrics_date_range(4);
        app.set_metrics_from("invalid date".into());
        invoke_action(&ui, &ctx, "metrics", "refresh").await;
        assert!(!app.get_metrics_error().is_empty());
        assert_eq!(app.get_metric_groups().row_count(), 0);
        app.set_metrics_from("2026-11-13".into());
        invoke_action(&ui, &ctx, "metrics", "refresh").await;
        assert!(app.get_metrics_error().is_empty());
        assert_eq!(app.get_metrics_total(), 0);
        app.set_metrics_date_range(0);
        app.set_metrics_model_index(0);
        app.set_metrics_tab(1);
        invoke_action(&ui, &ctx, "metrics", "refresh").await;
        assert_eq!(app.get_consumption_rows().row_count(), 2);
        assert!(app.get_consumption_rows().row_data(0).unwrap().configuration.contains("build-"));

        slint::quit_event_loop().unwrap();
    })
    .unwrap();
    slint::run_event_loop().unwrap();
}
