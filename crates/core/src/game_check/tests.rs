use super::*;
use crate::{StoredAnalysis, ToolPaths, TranslationUnit};
use gamel10n_engines::protocol::{ExtractionResult, LaunchConfiguration};
use gamel10n_translation::files::write_json;
use std::{collections::BTreeMap, sync::Mutex};

async fn fixture() -> (tempfile::TempDir, Application, String) {
    let root = tempfile::tempdir().unwrap();
    let game = root.path().join("game");
    std::fs::create_dir(&game).unwrap();
    std::fs::write(game.join("Game.exe"), "fixture").unwrap();
    let app = Application::open(
        root.path().join("data"),
        &ToolPaths::new(root.path().join("tools")),
    )
    .await
    .unwrap();
    let id = app.add_project(&game).await.unwrap().id;
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
    (root, app, id)
}

#[test]
fn scene_operations_are_bounded_and_roundtrip_without_shell_commands() {
    let steps = parse_steps("等待 1000\n按键 Enter\n点击 25 75").unwrap();
    assert_eq!(parse_steps(&format_steps(&steps)).unwrap(), steps);
    for invalid in [
        "运行 game.exe",
        "按键 Delete",
        "等待 30001",
        "点击 101 50",
        "等待 1000 2000",
    ] {
        assert!(parse_steps(invalid).is_err(), "{invalid}");
    }
    assert!(parse_steps(&"等待 30000\n".repeat(5)).is_err());
    assert!(
        CheckSettings {
            region: TextRegion {
                left: 60,
                width: 50,
                ..Default::default()
            },
            ..Default::default()
        }
        .validate()
        .is_err()
    );
    assert_eq!(normalize_text("ＡＢＣ １２３\n中文"), "ABC123中文");
    assert_ne!(normalize_text("文本甲"), normalize_text("文本乙"));
}

#[tokio::test]
async fn settings_survive_restart_and_corrupt_optional_settings_do_not_prevent_opening() {
    let (root, app, id) = fixture().await;
    let settings = CheckSettings {
        unit_ids: vec!["line-1".into()],
        steps: parse_steps("按键 Enter").unwrap(),
        ..Default::default()
    };
    app.save_game_check_settings(&id, settings.clone())
        .await
        .unwrap();
    drop(app);
    let app = Application::open(
        root.path().join("data"),
        &ToolPaths::new(root.path().join("tools")),
    )
    .await
    .unwrap();
    assert_eq!(app.game_check_settings(&id).await.unwrap(), settings);
    let mut project = app.store.get(&id).await.unwrap();
    project
        .extra
        .insert("gameCheck".into(), serde_json::json!({"steps":"invalid"}));
    app.store.save(&project).await.unwrap();
    drop(app);
    let app = Application::open(
        root.path().join("data"),
        &ToolPaths::new(root.path().join("tools")),
    )
    .await
    .unwrap();
    assert!(app.open_project(&id).await.is_ok());
    assert!(app.game_check_settings(&id).await.is_err());
    app.save_game_check_settings(&id, CheckSettings::default())
        .await
        .unwrap();
    assert!(app.game_check_settings(&id).await.is_ok());
}

#[tokio::test]
async fn expected_text_uses_saved_edits_and_excludes_unchanged_or_short_translations() {
    let (_root, app, id) = fixture().await;
    let directory = app.project_directory(&id).unwrap();
    write_json(
        &directory.join("extraction/native.json"),
        &StoredAnalysis {
            schema_version: 1,
            coverage_pending: false,
            format: "test".into(),
            source_directory: directory.join("sources"),
            resource_files: vec![],
            source_hashes: BTreeMap::new(),
            adapter: None,
            probe: None,
            extraction: ExtractionResult {
                units: vec![TranslationUnit {
                    id: "line-1".into(),
                    source_text: "Original text".into(),
                    resource_path: "story.txt".into(),
                    locator: "1".into(),
                    context: String::new(),
                    speaker: None,
                    kind: "dialogue".into(),
                    protected_tokens: None,
                }],
                warnings: vec![],
            },
        },
    )
    .unwrap();
    let ids = ["line-1".into()];
    app.save_translation_edit(&id, "line-1", "欢迎来到这里".into(), false)
        .await
        .unwrap();
    assert_eq!(
        app.game_check_expected(&id, &ids).await.unwrap(),
        ["欢迎来到这里"]
    );
    app.save_translation_edit(&id, "line-1", "Original text".into(), false)
        .await
        .unwrap();
    assert!(app.game_check_expected(&id, &ids).await.is_err());
    app.save_translation_edit(&id, "line-1", "你好".into(), false)
        .await
        .unwrap();
    assert!(app.game_check_expected(&id, &ids).await.is_err());
}

#[tokio::test]
async fn unavailable_components_and_cancellation_do_not_accept_patches_or_save_reports() {
    let (_root, app, id) = fixture().await;
    let observations = Mutex::new(Vec::new());
    let result = app
        .check_game(
            &id,
            CheckMode::Launch,
            &crate::CancellationToken::new(),
            |r| observations.lock().unwrap().push(r),
        )
        .await
        .unwrap();
    assert_eq!(result.launch.status, CheckStatus::Inconclusive);
    assert_eq!(result.text.status, CheckStatus::NotChecked);
    assert!(result.launch.message.contains("缺少运行检查组件"));
    assert!(!app.task_state(&id).await.unwrap().running);
    let stopped = crate::CancellationToken::new();
    stopped.cancel();
    let result = app
        .check_game(&id, CheckMode::Launch, &stopped, |_| {})
        .await
        .unwrap();
    assert_eq!(result.launch.status, CheckStatus::Inconclusive);
    assert!(result.launch.message.contains("已停止"));
    let project = app.store.get(&id).await.unwrap();
    assert!(!project.extra.contains_key("adapterRepair"));
    let directory = app.project_directory(&id).unwrap();
    for name in [
        "validation",
        "patches",
        "installation.json",
        "validation-report.json",
    ] {
        assert!(!directory.join(name).exists());
    }
    assert!(!app.data_directory().join("metrics").exists());
    assert!(!observations.lock().unwrap().is_empty());
}

#[tokio::test]
#[ignore = "Requires the native check_driver example in GAMEL10N_GAME_CHECK_DRIVER."]
async fn native_protocol_limits_and_cancellation_release_the_project_task() {
    let (root, app, id) = fixture().await;
    let component = root.path().join("tools/game-check/GameL10n.GameCheck.exe");
    std::fs::create_dir_all(component.parent().unwrap()).unwrap();
    std::fs::copy(
        std::env::var_os("GAMEL10N_GAME_CHECK_DRIVER").expect("Build the check_driver example."),
        &component,
    )
    .unwrap();
    let game = app.store.get(&id).await.unwrap().game_directory;
    for (behavior, expected) in [
        ("", CheckStatus::Passed),
        ("malformed", CheckStatus::Inconclusive),
        ("oversize", CheckStatus::Inconclusive),
    ] {
        std::fs::write(game.join("check-behavior"), behavior).unwrap();
        let result = app
            .check_game(
                &id,
                CheckMode::Launch,
                &crate::CancellationToken::new(),
                |_| {},
            )
            .await
            .unwrap();
        assert_eq!(result.launch.status, expected);
        assert!(!app.task_state(&id).await.unwrap().running);
    }
    std::fs::write(game.join("check-behavior"), "error").unwrap();
    let result = app
        .check_game(
            &id,
            CheckMode::Launch,
            &crate::CancellationToken::new(),
            |_| {},
        )
        .await
        .unwrap();
    assert_eq!(result.launch.status, CheckStatus::Failed);
    assert_eq!(app.game_check_errors.lock().await.len(), 1);
    app.close_game_check_errors().await.unwrap();
    assert!(game.join("check-cleaned").exists());
    assert!(app.game_check_errors.lock().await.is_empty());
    std::fs::write(game.join("check-behavior"), "wait").unwrap();
    std::fs::remove_file(game.join("check-started")).unwrap();
    let stop = crate::CancellationToken::new();
    let worker = {
        let app = app.clone();
        let id = id.clone();
        let stop = stop.clone();
        tokio::spawn(async move { app.check_game(&id, CheckMode::Launch, &stop, |_| {}).await })
    };
    tokio::time::timeout(Duration::from_secs(10), async {
        while !game.join("check-started").exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(matches!(
        app.save_game_check_settings(&id, CheckSettings::default())
            .await,
        Err(CoreError::ProjectBusy)
    ));
    assert!(
        app.check_game(
            &id,
            CheckMode::Launch,
            &crate::CancellationToken::new(),
            |_| {}
        )
        .await
        .is_err()
    );
    stop.cancel();
    let result = tokio::time::timeout(Duration::from_secs(3), worker)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(result.launch.message.contains("已停止"));
    assert!(!app.task_state(&id).await.unwrap().running);
    app.save_game_check_settings(&id, CheckSettings::default())
        .await
        .unwrap();
}
