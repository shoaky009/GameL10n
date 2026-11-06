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
async fn project_list_uses_index_even_when_the_project_directory_is_missing() {
    let temporary = tempfile::tempdir().unwrap();
    write_json(&temporary.path().join("games/index.json"), &json!({"schemaVersion":1,"projects":[{
        "id":"1234567890abcdef12345678","gameDirectory":"C:\\Games\\Missing","name":"Saved project"
    }]})).unwrap();
    let app = Application::open(temporary.path().into(), &ToolPaths::new("missing"))
        .await
        .unwrap();
    assert_eq!(app.list_projects("saved").await.len(), 1);
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
    assert_eq!(saved.extra, project.extra);
    assert!(!saved.is_removed);
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
