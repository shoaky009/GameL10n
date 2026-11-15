use std::{collections::BTreeMap, fs};

use gamel10n_core::{Application, GameProject, ToolPaths};
use gamel10n_translation::files::{read_json, write_json};
use serde_json::json;

#[tokio::test]
async fn opening_a_project_does_not_require_tools_or_start_recognition() {
    let temporary = tempfile::tempdir().unwrap();
    let game = temporary.path().join("game");
    fs::create_dir(&game).unwrap();
    let app = Application::open(
        temporary.path().join("data"),
        &ToolPaths::new("tools-do-not-exist"),
    )
    .await
    .unwrap();
    let project = app.add_project(&game).await.unwrap();
    app.open_project(&project.id).await.unwrap();
    assert!(app.task_state(&project.id).await.is_none());
}

#[tokio::test]
async fn indexed_projects_remain_usable_without_their_metadata() {
    for missing_directory in [false, true] {
        let temporary = tempfile::tempdir().unwrap();
        let game = temporary.path().join("game");
        fs::create_dir(&game).unwrap();
        let data = temporary.path().join("data");
        let app = Application::open(data.clone(), &ToolPaths::new("missing"))
            .await
            .unwrap();
        let project = app.add_project(&game).await.unwrap();
        let settings =
            json!({"analysisProfileId":"analysis", "translationProfileIds":["translation"]});
        app.save_project_settings(&project.id, settings.clone())
            .await
            .unwrap();
        let directory = app.project_directory(&project.id).unwrap();
        if missing_directory {
            fs::remove_dir_all(&directory).unwrap();
        } else {
            fs::remove_file(directory.join("game.json")).unwrap();
        }
        drop(app);
        let app = Application::open(data, &ToolPaths::new("missing"))
            .await
            .unwrap();
        let opened = app.project_snapshot(&project.id).await.unwrap().project;
        assert_eq!(opened.id, project.id);
        assert_eq!(opened.game_directory, game);
        assert_eq!(opened.settings, settings);
        assert!(!directory.join("game.json").exists());
        if missing_directory {
            assert!(!directory.exists());
        }
        let added = tokio::time::timeout(std::time::Duration::from_secs(2), app.add_project(&game))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(added.id, project.id);
        assert_eq!(app.list_projects("").await.len(), 1);
        app.save_project_settings(&project.id, json!({"targetLanguage":"zh-CN"}))
            .await
            .unwrap();
        let saved: GameProject = read_json(&directory.join("game.json")).unwrap();
        assert_eq!(saved.settings["analysisProfileId"], "analysis");
        assert_eq!(saved.settings["targetLanguage"], "zh-CN");
    }
}

#[tokio::test]
async fn removal_does_not_require_complete_project_files() {
    for delete_data in [false, true] {
        for missing in ["directory", "metadata", "invalid metadata"] {
            let temporary = tempfile::tempdir().unwrap();
            let game = temporary.path().join("game");
            fs::create_dir(&game).unwrap();
            fs::write(game.join("original.bin"), "original").unwrap();
            let data = temporary.path().join("data");
            let app = Application::open(data.clone(), &ToolPaths::new("missing"))
                .await
                .unwrap();
            let project = app.add_project(&game).await.unwrap();
            let directory = app.project_directory(&project.id).unwrap();
            fs::write(
                directory.join("translations.jsonl"),
                "retained translations",
            )
            .unwrap();
            match missing {
                "directory" => fs::remove_dir_all(&directory).unwrap(),
                "metadata" => fs::remove_file(directory.join("game.json")).unwrap(),
                _ => fs::write(directory.join("game.json"), "invalid json").unwrap(),
            }
            for _ in 0..2 {
                app.remove_projects(std::slice::from_ref(&project.id), delete_data)
                    .await
                    .unwrap();
                assert!(app.list_projects("").await.is_empty());
            }
            assert_eq!(directory.exists(), !delete_data && missing != "directory");
            if directory.exists() {
                assert_eq!(
                    fs::read_to_string(directory.join("translations.jsonl")).unwrap(),
                    "retained translations"
                );
            }
            assert_eq!(
                fs::read_to_string(game.join("original.bin")).unwrap(),
                "original"
            );
            drop(app);
            let reopened = Application::open(data, &ToolPaths::new("missing"))
                .await
                .unwrap();
            assert!(reopened.list_projects("").await.is_empty());
        }
    }
}

#[tokio::test]
#[cfg(windows)]
async fn index_removal_does_not_require_writable_project_metadata() {
    use std::os::windows::fs::OpenOptionsExt;

    let temporary = tempfile::tempdir().unwrap();
    let game = temporary.path().join("game");
    fs::create_dir(&game).unwrap();
    let app = Application::open(temporary.path().join("data"), &ToolPaths::new("missing"))
        .await
        .unwrap();
    let project = app.add_project(&game).await.unwrap();
    let path = app
        .project_directory(&project.id)
        .unwrap()
        .join("game.json");
    let held = fs::OpenOptions::new()
        .read(true)
        .share_mode(1)
        .open(&path)
        .unwrap();
    app.remove_projects(std::slice::from_ref(&project.id), false)
        .await
        .unwrap();
    assert!(app.list_projects("").await.is_empty());
    assert!(path.is_file());
    drop(held);
}

#[tokio::test]
async fn project_list_sorts_saved_summaries_without_opening_project_directories() {
    let temporary = tempfile::tempdir().unwrap();
    let projects = [
        (
            "000000000000000000000001",
            "A",
            Some("2026-11-06T10:00:00+08:00"),
        ),
        (
            "000000000000000000000003",
            "beta",
            Some("2026-11-06T03:00:00Z"),
        ),
        (
            "000000000000000000000004",
            "alpha",
            Some("2026-11-06T03:00:00Z"),
        ),
        (
            "000000000000000000000002",
            "Alpha",
            Some("2026-11-06T03:00:00Z"),
        ),
        ("000000000000000000000005", "Without a date", None),
    ]
    .map(|(id, name, updated)| {
        let mut project = json!({"id":id,"name":name,"gameDirectory":"C:\\Games\\Missing"});
        if let Some(updated) = updated {
            project["updatedAt"] = json!(updated);
        }
        project
    });
    write_json(
        &temporary.path().join("games/index.json"),
        &json!({"schemaVersion":1,"projects":projects}),
    )
    .unwrap();
    let app = Application::open(temporary.path().into(), &ToolPaths::new("missing"))
        .await
        .unwrap();
    assert_eq!(
        app.list_projects("  ")
            .await
            .into_iter()
            .map(|project| project.id)
            .collect::<Vec<_>>(),
        [
            "000000000000000000000002",
            "000000000000000000000004",
            "000000000000000000000003",
            "000000000000000000000001",
            "000000000000000000000005",
        ]
    );
}

#[test]
fn project_metadata_roundtrip_preserves_settings_and_unknown_fields() {
    let original = json!({"schemaVersion":2,"id":"1234567890abcdef12345678","gameDirectory":"C:\\Games\\Example",
        "name":"Example","settings":{"analysisProfileId":"existing","futureOption":{"enabled":true}},
        "isRemoved":false,"updatedAt":"2026-11-06T00:00:00Z","futureMetadata":{"preserve":42}});
    let project: GameProject = serde_json::from_value(original.clone()).unwrap();
    assert_eq!(serde_json::to_value(project).unwrap(), original);
}

#[tokio::test]
async fn reopening_a_removed_project_preserves_its_existing_settings() {
    let temporary = tempfile::tempdir().unwrap();
    let game = temporary.path().join("game");
    fs::create_dir(&game).unwrap();
    let data = temporary.path().join("data");
    let app = Application::open(data.clone(), &ToolPaths::new("missing"))
        .await
        .unwrap();
    let mut project = app.add_project(&game).await.unwrap();
    project.is_removed = true;
    project.settings = json!({"translationProfileIds":["existing"]});
    project.extra = BTreeMap::from([("customData".into(), json!({"value":1}))]);
    write_json(
        &data.join("games").join(&project.id).join("game.json"),
        &project,
    )
    .unwrap();
    write_json(
        &data.join("games/index.json"),
        &json!({"schemaVersion":1,"projects":[]}),
    )
    .unwrap();
    drop(app);
    let app = Application::open(data.clone(), &ToolPaths::new("missing"))
        .await
        .unwrap();
    app.add_project(&game).await.unwrap();
    let saved: GameProject =
        read_json(&data.join("games").join(&project.id).join("game.json")).unwrap();
    assert_eq!(saved.settings, project.settings);
    assert_eq!(saved.extra["customData"], project.extra["customData"]);
    assert!(
        saved
            .extra
            .get("updatedAt")
            .is_some_and(|value| value.is_string())
    );
    assert!(!saved.is_removed);
}

#[tokio::test]
async fn settings_changes_update_project_order_but_opening_and_noop_saves_do_not() {
    let temporary = tempfile::tempdir().unwrap();
    let data = temporary.path().join("data");
    let app = Application::open(data.clone(), &ToolPaths::new("missing"))
        .await
        .unwrap();
    let mut ids = Vec::new();
    for name in ["earlier", "later"] {
        let game = temporary.path().join(name);
        fs::create_dir(&game).unwrap();
        ids.push(app.add_project(&game).await.unwrap().id);
    }
    let original = app.open_project(&ids[0]).await.unwrap();
    app.save_project_settings(&ids[0], json!({})).await.unwrap();
    assert_eq!(app.list_projects("").await[0].id, ids[1]);
    assert_eq!(
        app.open_project(&ids[0]).await.unwrap().extra["updatedAt"],
        original.extra["updatedAt"]
    );

    app.save_project_settings(&ids[0], json!({"targetLanguage":"zh-CN"}))
        .await
        .unwrap();
    let changed = app.open_project(&ids[0]).await.unwrap();
    app.save_project_settings(&ids[0], json!({"targetLanguage":"zh-CN"}))
        .await
        .unwrap();
    assert_eq!(
        app.open_project(&ids[0]).await.unwrap().extra["updatedAt"],
        changed.extra["updatedAt"]
    );
    drop(app);
    let reopened = Application::open(data, &ToolPaths::new("missing"))
        .await
        .unwrap();
    assert_eq!(reopened.list_projects("").await[0].id, ids[0]);
}

#[tokio::test]
async fn another_host_can_read_but_cannot_write_the_same_data_directory() {
    let temporary = tempfile::tempdir().unwrap();
    let game = temporary.path().join("game");
    fs::create_dir(&game).unwrap();
    let data = temporary.path().join("data");
    let first = Application::open(data.clone(), &ToolPaths::new("missing"))
        .await
        .unwrap();
    first.add_project(&game).await.unwrap();
    let second = Application::open(data, &ToolPaths::new("missing"))
        .await
        .unwrap();
    assert_eq!(second.list_projects("").await.len(), 1);
    assert!(second.add_project(&game).await.is_err());
}

#[tokio::test]
async fn first_write_refreshes_the_index_after_another_writer_exits() {
    let temporary = tempfile::tempdir().unwrap();
    let first_game = temporary.path().join("first-game");
    let second_game = temporary.path().join("second-game");
    fs::create_dir(&first_game).unwrap();
    fs::create_dir(&second_game).unwrap();
    let data = temporary.path().join("data");
    let reader = Application::open(data.clone(), &ToolPaths::new("missing"))
        .await
        .unwrap();
    let writer = Application::open(data, &ToolPaths::new("missing"))
        .await
        .unwrap();
    writer.add_project(&first_game).await.unwrap();
    drop(writer);
    reader.add_project(&second_game).await.unwrap();
    assert_eq!(reader.list_projects("").await.len(), 2);
}
