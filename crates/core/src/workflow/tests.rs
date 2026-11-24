use super::*;
#[cfg(unix)]
use crate::StoredAnalysis;
use crate::ToolPaths;
#[cfg(unix)]
use gamel10n_engines::protocol::ExtractionResult;
#[cfg(unix)]
use gamel10n_translation::TranslationUnit;
#[cfg(unix)]
use std::collections::BTreeMap;

async fn fixture() -> (tempfile::TempDir, Application, String) {
    let root = tempfile::tempdir().unwrap();
    let game = root.path().join("game");
    std::fs::create_dir(&game).unwrap();
    let app = Application::open(
        root.path().join("data"),
        &ToolPaths::new(root.path().join("tools")),
    )
    .await
    .unwrap();
    let id = app.add_project(&game).await.unwrap().id;
    (root, app, id)
}

fn definition(value: serde_json::Value) -> WorkflowDefinition {
    serde_json::from_value(value).unwrap()
}

#[test]
fn preset_success_path_stops_at_the_requested_goal() {
    for (finish, use_existing_text, expected) in [
        (WorkflowFinish::Trial, true, "review"),
        (WorkflowFinish::Build, true, "buildAll"),
        (WorkflowFinish::Install, false, "installAll"),
    ] {
        let config = WorkflowDefinition::from_preset(WorkflowPreset {
            finish,
            use_existing_text,
        });
        config.validate().unwrap();
        assert_eq!(
            config.entry,
            if use_existing_text {
                "select"
            } else {
                "recognize"
            }
        );
        let mut success_path = config
            .steps
            .iter()
            .skip_while(|step| step.id != config.entry);
        assert_eq!(
            success_path
                .find(|step| step.on_success.as_deref() == Some("done"))
                .unwrap()
                .id,
            expected
        );
    }
}

#[tokio::test]
async fn project_preset_takes_effect_without_replacing_a_custom_definition() {
    let (_root, app, id) = fixture().await;
    let path = app.workflow_configuration(&id).await.unwrap();
    let legacy = serde_json::json!({
        "schemaVersion": 1,
        "entry": "recognize",
        "steps": [
            {"id": "recognize", "action": "recognize"},
            {"id": "locate", "action": "locateText", "onUnavailable": "input"},
            {"id": "trialTranslate", "action": "translate"},
            {"id": "trialBuild", "action": "buildPatch"},
            {"id": "trialInstall", "action": "installPatch"},
            {"id": "check", "action": "checkGame"},
            {"id": "translateAll", "action": "translate", "scope": "all"},
            {"id": "buildAll", "action": "buildPatch", "scope": "all"},
            {"id": "installAll", "action": "installPatch", "scope": "all", "onSuccess": "done"},
            {"id": "input", "action": "waitForInput", "message": LEGACY_WAIT_MESSAGE},
            {"id": "done", "action": "complete"}
        ]
    });
    write_json(&path, &legacy).unwrap();
    assert_eq!(
        app.workflow_preferences(&id).await.unwrap(),
        (WorkflowChoice::Preset(WorkflowPreset::default()), false)
    );
    let custom = definition(serde_json::json!({
        "schemaVersion": 1,
        "entry": "finish",
        "steps": [{"id": "finish", "action": "complete"}]
    }));
    write_json(&path, &custom).unwrap();
    assert_eq!(
        app.workflow_preferences(&id).await.unwrap(),
        (WorkflowChoice::Custom, true)
    );

    let preset = WorkflowChoice::Preset(WorkflowPreset {
        use_existing_text: true,
        finish: WorkflowFinish::Trial,
    });
    app.save_workflow_preferences(&id, preset).await.unwrap();
    assert_eq!(app.workflow_preferences(&id).await.unwrap(), (preset, true));
    let error = app
        .run_workflow(&id, String::new(), RecognitionMode::Automatic)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("请先识别并提取游戏文本"));
    app.run_workflow_with_choice(
        &id,
        String::new(),
        RecognitionMode::Automatic,
        WorkflowChoice::Custom,
    )
    .await
    .unwrap();
    assert_eq!(app.workflow_preferences(&id).await.unwrap(), (preset, true));
    let saved: WorkflowDefinition = read_json(&path).unwrap();
    assert_eq!(saved, custom);

    app.save_workflow_preferences(&id, WorkflowChoice::Custom)
        .await
        .unwrap();
    app.run_workflow(&id, String::new(), RecognitionMode::Automatic)
        .await
        .unwrap();
    std::fs::remove_file(&path).unwrap();
    assert!(
        app.run_workflow(&id, String::new(), RecognitionMode::Automatic)
            .await
            .is_err()
    );
    assert!(!path.exists());
}

#[test]
fn invalid_branches_require_correction() {
    let mut config = WorkflowDefinition::default();
    config.validate().unwrap();
    config.steps[0].on_success = Some("missing".into());
    assert!(config.validate().is_err());
}

#[tokio::test]
async fn branches_waiting_failure_and_restart_release_project_ownership() {
    let (root, app, id) = fixture().await;
    let path = app.workflow_configuration(&id).await.unwrap();
    let config = definition(
        serde_json::json!({"schemaVersion":1,"entry":"select","steps":[
            {"id":"select","action":"selectTrial","onFailure":"input"},
            {"id":"input","action":"waitForInput","message":"请选择试译文本"}
        ]}),
    );
    write_json(&path, &config).unwrap();
    assert!(matches!(
        app.run_workflow(&id, String::new(), RecognitionMode::Automatic)
            .await,
        Err(CoreError::WorkflowInput(_))
    ));
    let state = app.task_state(&id).await.unwrap();
    assert!(!state.running);
    assert_eq!(state.succeeded, None);
    assert_eq!(
        state.workflow.unwrap().status,
        WorkflowStatus::WaitingForInput
    );
    // Configuration is project-owned and must survive opening its edit entry again.
    app.workflow_configuration(&id).await.unwrap();
    let saved: WorkflowDefinition = read_json(&path).unwrap();
    assert_eq!(saved.entry, "select");
    let config = definition(serde_json::json!({"schemaVersion":1,"entry":"all","steps":[
        {"id":"all","action":"translate","scope":"all"}
    ]}));
    write_json(&path, &config).unwrap();
    let error = app
        .run_workflow(&id, String::new(), RecognitionMode::Automatic)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("尚未通过实机确认"));
    assert_eq!(app.task_state(&id).await.unwrap().succeeded, Some(false));
    // A persisted in-flight stage becomes interrupted, without resuming external actions.
    let _task = app.begin(&id, "workflow", "running").await.unwrap();
    app.set_workflow_progress(
        &id,
        WorkflowProgress {
            step_id: "review".into(),
            action: WorkflowAction::ReviewTrial,
            status: WorkflowStatus::Running,
            target_unit_id: None,
            pause_kind: None,
        },
    )
    .await
    .unwrap();
    std::mem::forget(_task); // Simulate a process exit during the persisted stage.
    drop(app);
    let app = Application::open(
        root.path().join("data"),
        &ToolPaths::new(root.path().join("tools")),
    )
    .await
    .unwrap();
    app.open_project(&id).await.unwrap();
    let state = app.task_state(&id).await.unwrap();
    assert!(!state.running);
    assert!(state.message.contains("review"));
    assert_eq!(state.workflow.unwrap().status, WorkflowStatus::Interrupted);
}

#[cfg(unix)]
async fn pipeline_fixture() -> (tempfile::TempDir, Application, String) {
    use gamel10n_engines::AdapterHandle;
    use gamel10n_translation::files::hash_file;
    use std::os::unix::fs::symlink;
    let (root, app, id) = fixture().await;
    let game = root.path().join("game");
    std::fs::write(
        game.join("text.json"),
        r#"["Opening dialogue","Later dialogue"]"#,
    )
    .unwrap();
    let python = root.path().join("tools/python/python.exe");
    std::fs::create_dir_all(python.parent().unwrap()).unwrap();
    symlink("/usr/bin/python3", &python).unwrap();
    let adapter = root.path().join("adapter.py");
    std::fs::write(
        &adapter,
        include_str!("../../../engines/tests/fixtures/json_adapter.py"),
    )
    .unwrap();
    let units: Vec<TranslationUnit> = ["Opening dialogue", "Later dialogue"]
        .into_iter()
        .enumerate()
        .map(|(i, text)| TranslationUnit {
            id: i.to_string(),
            source_text: text.into(),
            resource_path: "text.json".into(),
            locator: i.to_string(),
            context: "stable".into(),
            speaker: None,
            kind: "dialogue".into(),
            protected_tokens: None,
        })
        .collect();
    write_json(
        &app.project_directory(&id)
            .unwrap()
            .join("extraction/native.json"),
        &StoredAnalysis {
            schema_version: 2,
            coverage_pending: false,
            format: "fixture".into(),
            source_directory: game.clone(),
            resource_files: vec!["text.json".into()],
            source_hashes: BTreeMap::from([(
                "text.json".into(),
                hash_file(&game.join("text.json")).unwrap(),
            )]),
            adapter: Some(AdapterHandle::Python {
                entry: adapter,
                package_directory: None,
                id: "engine.fixture".into(),
                version: "1".into(),
            }),
            probe: None,
            extraction: ExtractionResult {
                units,
                warnings: vec![],
            },
        },
    )
    .unwrap();
    app.save_api_profile(
        crate::settings::ApiProfile {
            id: "unused".into(),
            name: "Unused".into(),
            endpoint: "http://127.0.0.1:9/v1".into(),
            model: "unused".into(),
            protocol: "ChatCompletions".into(),
            ..Default::default()
        },
        None,
        false,
    )
    .await
    .unwrap();
    app.save_translation_edit(&id, "0", "开场测试对白".into(), true)
        .await
        .unwrap();
    app.save_translation_edit(&id, "1", "后续测试对白".into(), true)
        .await
        .unwrap();
    (root, app, id)
}

#[cfg(unix)]
#[tokio::test]
async fn selection_pause_and_manual_trial_review_gate_drive_full_install() {
    let (root, app, id) = pipeline_fixture().await;
    let game = root.path().join("game");
    let choice = WorkflowChoice::Preset(WorkflowPreset {
        use_existing_text: true,
        finish: WorkflowFinish::Install,
    });

    // Without pre-selected texts, workflow pauses cleanly for selection.
    app.run_workflow_with_choice(&id, String::new(), RecognitionMode::Automatic, choice)
        .await
        .unwrap();
    let state = app.task_state(&id).await.unwrap();
    assert!(!state.running);
    assert_eq!(state.succeeded, None);
    assert_eq!(
        state.workflow.as_ref().unwrap().status,
        WorkflowStatus::WaitingForInput
    );
    assert_eq!(
        state.workflow.as_ref().unwrap().pause_kind,
        Some(WorkflowPauseKind::SelectTrial)
    );
    let session = app.workflow_session(&id).await.unwrap().unwrap();
    assert_eq!(session.pause_kind, WorkflowPauseKind::SelectTrial);
    assert!(app.resume_workflow_selection(&id).await.is_err());

    // Selecting trial text and resuming builds and installs the trial patch, then pauses for review.
    app.save_text_selection(&id, &["0".into()]).await.unwrap();
    app.resume_workflow_selection(&id).await.unwrap();
    let session = app.workflow_session(&id).await.unwrap().unwrap();
    assert_eq!(session.pause_kind, WorkflowPauseKind::ReviewTrial);
    assert_eq!(session.target_unit_ids, vec!["0"]);
    assert_eq!(session.confirm_label(), "确认正常，继续全量安装");
    let trial_text: Vec<String> = read_json(&game.join("text.json")).unwrap();
    assert_eq!(trial_text, ["开场测试对白", "Later dialogue"]);

    // Persisted review checkpoint survives reopening the application.
    drop(app);
    let app = Application::open(
        root.path().join("data"),
        &ToolPaths::new(root.path().join("tools")),
    )
    .await
    .unwrap();
    let snapshot = app.project_snapshot(&id).await.unwrap();
    assert_eq!(
        snapshot.workflow_session.as_ref().unwrap().pause_kind,
        WorkflowPauseKind::ReviewTrial
    );

    // Confirming the trial patch completes full translation, build, and install.
    app.confirm_workflow_trial(&id).await.unwrap();
    assert!(app.workflow_session(&id).await.unwrap().is_none());
    let state = app.task_state(&id).await.unwrap();
    assert_eq!(state.succeeded, Some(true));
    let full_text: Vec<String> = read_json(&game.join("text.json")).unwrap();
    assert_eq!(full_text, ["开场测试对白", "后续测试对白"]);
}

#[cfg(unix)]
#[tokio::test]
async fn preselected_texts_skip_selection_pause_and_support_trial_build_and_dismiss() {
    let (root, app, id) = pipeline_fixture().await;
    let game = root.path().join("game");
    app.save_text_selection(&id, &["0".into()]).await.unwrap();

    // Trial-only preset installs trial patch and finishes on confirmation without full install.
    app.run_workflow_with_choice(
        &id,
        String::new(),
        RecognitionMode::Automatic,
        WorkflowChoice::Preset(WorkflowPreset {
            use_existing_text: true,
            finish: WorkflowFinish::Trial,
        }),
    )
    .await
    .unwrap();
    let session = app.workflow_session(&id).await.unwrap().unwrap();
    assert_eq!(session.pause_kind, WorkflowPauseKind::ReviewTrial);
    assert_eq!(session.confirm_label(), "确认正常，完成试译");
    app.confirm_workflow_trial(&id).await.unwrap();
    assert!(app.workflow_session(&id).await.unwrap().is_none());
    let trial_only: Vec<String> = read_json(&game.join("text.json")).unwrap();
    assert_eq!(trial_only, ["开场测试对白", "Later dialogue"]);

    // Build-only preset generates full patch while keeping trial patch in game directory.
    app.run_workflow_with_choice(
        &id,
        String::new(),
        RecognitionMode::Automatic,
        WorkflowChoice::Preset(WorkflowPreset {
            use_existing_text: true,
            finish: WorkflowFinish::Build,
        }),
    )
    .await
    .unwrap();
    let session = app.workflow_session(&id).await.unwrap().unwrap();
    assert_eq!(session.confirm_label(), "确认正常，生成全量补丁");
    app.confirm_workflow_trial(&id).await.unwrap();
    let built_only: Vec<String> = read_json(&game.join("text.json")).unwrap();
    assert_eq!(built_only, ["开场测试对白", "Later dialogue"]);
    let patch_texts = app.latest_patch_texts(&id, 0).await.unwrap();
    assert_eq!(patch_texts.total, 2);

    // Dismissing a paused workflow clears the gate without touching project files.
    app.run_workflow_with_choice(
        &id,
        String::new(),
        RecognitionMode::Automatic,
        WorkflowChoice::Preset(WorkflowPreset {
            use_existing_text: true,
            finish: WorkflowFinish::Install,
        }),
    )
    .await
    .unwrap();
    assert!(app.workflow_session(&id).await.unwrap().is_some());
    app.dismiss_workflow(&id).await.unwrap();
    assert!(app.workflow_session(&id).await.unwrap().is_none());
    assert_eq!(
        app.task_state(&id).await.unwrap().workflow.unwrap().status,
        WorkflowStatus::Interrupted
    );
}
