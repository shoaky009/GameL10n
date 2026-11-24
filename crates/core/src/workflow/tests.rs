use super::*;
#[cfg(unix)]
use crate::StoredAnalysis;
use crate::ToolPaths;
#[cfg(unix)]
use gamel10n_engines::protocol::ExtractionResult;
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
        (WorkflowFinish::Trial, true, "check"),
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
                "locate"
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
    let mut legacy = WorkflowDefinition::default();
    legacy
        .steps
        .iter_mut()
        .find(|step| step.id == "input")
        .unwrap()
        .message = LEGACY_WAIT_MESSAGE.into();
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
fn invalid_branches_and_ambiguous_text_require_correction() {
    let mut config = WorkflowDefinition::default();
    config.validate().unwrap();
    config.steps[0].on_success = Some("missing".into());
    assert!(config.validate().is_err());
    let mut units: Vec<TranslationUnit> = serde_json::from_value(serde_json::json!([
        {"id":"menu", "sourceText":"Start game", "kind":"ui", "resourcePath":"text.json", "locator":"0", "context":""},
        {"id":"opening", "sourceText":"[name]Opening dialogue", "kind":"dialogue", "resourcePath":"text.json", "locator":"1", "context":"", "protectedTokens":["[name]"]},
        {"id":"later", "sourceText":"Later dialogue", "kind":"dialogue", "resourcePath":"text.json", "locator":"2", "context":""}
    ])).unwrap();
    assert_eq!(
        match_visible_unit(&units, "Start game\nOpening dialogue\nLater dialogue").unwrap(),
        "opening"
    );
    let mut duplicate = units[1].clone();
    duplicate.id = "other-scene".into();
    units.push(duplicate);
    assert!(matches!(
        match_visible_unit(&units, "Opening dialogue"),
        Err(CoreError::WorkflowInput(_))
    ));
}

#[tokio::test]
async fn branches_waiting_failure_and_restart_release_project_ownership() {
    let (root, app, id) = fixture().await;
    let path = app.workflow_configuration(&id).await.unwrap();
    let config = definition(
        serde_json::json!({"schemaVersion":1,"entry":"locate","steps":[
            {"id":"locate","action":"locateText","onFailure":"input"},
            {"id":"input","action":"waitForInput","message":"请选择开场文本"}
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
    assert_eq!(saved.entry, "locate");
    let config = definition(serde_json::json!({"schemaVersion":1,"entry":"all","steps":[
        {"id":"all","action":"translate","scope":"all"}
    ]}));
    write_json(&path, &config).unwrap();
    let error = app
        .run_workflow(&id, String::new(), RecognitionMode::Automatic)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("尚未通过"));
    assert_eq!(app.task_state(&id).await.unwrap().succeeded, Some(false));
    // A persisted in-flight stage becomes interrupted, without resuming external actions.
    let _task = app.begin(&id, "workflow", "running").await.unwrap();
    app.set_workflow_progress(
        &id,
        WorkflowProgress {
            step_id: "check".into(),
            action: WorkflowAction::CheckGame,
            status: WorkflowStatus::Running,
            target_unit_id: None,
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
    assert!(state.message.contains("check"));
    assert_eq!(state.workflow.unwrap().status, WorkflowStatus::Interrupted);
}

#[cfg(unix)]
async fn pipeline_fixture(behavior: &str) -> (tempfile::TempDir, Application, String) {
    use gamel10n_engines::{AdapterHandle, protocol::LaunchConfiguration};
    use gamel10n_translation::files::hash_file;
    use std::os::unix::fs::{PermissionsExt, symlink};
    let (root, app, id) = fixture().await;
    let game = root.path().join("game");
    std::fs::write(game.join("Game.exe"), "fixture").unwrap();
    std::fs::write(game.join("check-behavior"), behavior).unwrap();
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
    app.save_launch(
        &id,
        Some(LaunchConfiguration {
            executable: "Game.exe".into(),
            arguments: String::new(),
            working_directory: ".".into(),
        }),
        &app.launch_view(&id).await.unwrap().revision,
    )
    .await
    .unwrap();
    let driver = root.path().join("tools/game-check/GameL10n.GameCheck.exe");
    std::fs::create_dir_all(driver.parent().unwrap()).unwrap();
    std::fs::write(&driver, r#"#!/usr/bin/python3
import json, sys, time
from pathlib import Path
r = json.load(sys.stdin)
game = Path(r['gameDirectory'])
behavior = (game / 'check-behavior').read_text()
def emit(kind, status, message):
    print(json.dumps(dict(kind=kind, value=dict(status=status, message=message))), flush=True)
assert r['closeAfter']
(game / ('locating' if r['mode'] == 'locateText' else 'checking')).touch()
if behavior == 'wait' and r['mode'] != 'locateText':
    time.sleep(60)
elif behavior == 'launch-fail' and r['mode'] != 'locateText':
    emit('launch', 'failed', '游戏错误：加载资源失败')
else:
    emit('launch', 'passed', '启动成功')
    if r['mode'] == 'locateText':
        if behavior == 'no-text':
            emit('text', 'inconclusive', '没有稳定文字')
        else:
            print(json.dumps(dict(kind='visibleText',value='Opening dialogue')), flush=True)
            emit('text', 'passed', '已定位')
    else:
        emit('text', 'failed' if behavior == 'text-fail' else 'passed', '目标译文检查')
    emit('closed', 'failed' if behavior == 'close-fail' and r['mode'] != 'locateText' else 'passed', '关闭进程检查')
"#).unwrap();
    std::fs::set_permissions(&driver, std::fs::Permissions::from_mode(0o755)).unwrap();
    let config = WorkflowDefinition {
        entry: "locate".into(),
        ..Default::default()
    };
    write_json(&app.workflow_configuration(&id).await.unwrap(), &config).unwrap();
    (root, app, id)
}

#[cfg(unix)]
#[tokio::test]
async fn trial_verification_controls_full_install_and_cancellation() {
    for behavior in [
        "pass",
        "no-text",
        "launch-fail",
        "text-fail",
        "close-fail",
        "wait",
    ] {
        let (root, app, id) = pipeline_fixture(behavior).await;
        let game = root.path().join("game");
        let app = std::sync::Arc::new(app);
        let task_app = app.clone();
        let task_id = id.clone();
        let task = tokio::spawn(async move {
            task_app
                .run_workflow(&task_id, String::new(), RecognitionMode::Automatic)
                .await
        });
        if behavior == "wait" {
            tokio::time::timeout(std::time::Duration::from_secs(10), async {
                while !game.join("checking").is_file() {
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                }
            })
            .await
            .unwrap();
            app.cancel(&id).await;
        }
        let result = task.await.unwrap();
        let state = app.task_state(&id).await.unwrap();
        assert!(!state.running, "{behavior}");
        if behavior == "pass" {
            result.unwrap();
            assert_eq!(state.succeeded, Some(true));
            let text: Vec<String> = read_json(&game.join("text.json")).unwrap();
            assert_eq!(text, ["开场测试对白", "后续测试对白"]);
        } else {
            assert!(result.is_err(), "{behavior}");
            let progress = state.workflow.unwrap();
            assert_eq!(
                progress.status,
                match behavior {
                    "no-text" => WorkflowStatus::WaitingForInput,
                    "wait" => WorkflowStatus::Interrupted,
                    _ => WorkflowStatus::Failed,
                }
            );
            assert_eq!(
                progress.step_id,
                if behavior == "no-text" {
                    "input"
                } else {
                    "check"
                }
            );
            if behavior != "no-text" {
                let text: Vec<String> = read_json(&game.join("text.json")).unwrap();
                assert_eq!(
                    text,
                    ["开场测试对白", "Later dialogue"],
                    "full install must not run after {behavior}"
                );
            }
            if behavior == "launch-fail" {
                assert!(state.message.contains("加载资源失败"));
            }
        }
    }
}
