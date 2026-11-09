use std::{fs, path::Path, process::Command};

use gamel10n_core::{Application, ToolPaths, engine_tasks::RecognitionResult};
use gamel10n_engines::{AdapterHandle, CancellationToken, EngineService, protocol::AdapterRequest};
use gamel10n_translation::files::hash_file;
use serde_json::Value;

#[tokio::test]
async fn smil_recognition_edit_patch_and_restart_preserve_source_and_ids() {
    let tools = ToolPaths::development();
    if !tools.python().is_file() {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let game = root.path().join("game");
    let fixture = Command::new(tools.python())
        .args(["-I", "-X", "utf8"])
        .arg(repository.join("crates/engines/python/test_xp3_smil.py"))
        .arg("--fixture-directory")
        .arg(&game)
        .arg("1001")
        .output()
        .unwrap();
    assert!(
        fixture.status.success(),
        "{}",
        String::from_utf8_lossy(&fixture.stderr)
    );
    let original_hash = hash_file(&game.join("data.xp3")).unwrap();
    let data = root.path().join("data");
    let shared = data.join("extensions/engine-tools");
    let pin: Value = serde_json::from_str(include_str!(
        "../../../third_party/engine-tools/current.json"
    ))
    .unwrap();
    let relative = format!(
        "references/{}/library.zip",
        pin["revision"].as_str().unwrap()
    );
    let library = shared.join(&relative);
    fs::create_dir_all(library.parent().unwrap()).unwrap();
    fs::copy(
        repository.join("third_party/engine-tools").join(relative),
        library,
    )
    .unwrap();
    let app = Application::open(data.clone(), &tools).await.unwrap();
    let project = app.add_project(&game).await.unwrap();
    assert!(matches!(
        app.recognize(&project.id).await.unwrap(),
        RecognitionResult::Extracted(1006)
    ));
    let mut before = app.text_page(&project.id, "", 0, 500).await.unwrap();
    assert_eq!(before.total, 1006);
    for offset in (500..before.total).step_by(500) {
        before.units.extend(
            app.text_page(&project.id, "", offset, 500)
                .await
                .unwrap()
                .units,
        );
    }
    let selected = before
        .units
        .iter()
        .find(|u| u.context == "scene.ks.scn" && u.kind == "dialogue")
        .unwrap();
    let translated = format!("中文：{}", selected.source_text);
    app.save_translation_edit(&project.id, &selected.id, translated.clone(), true)
        .await
        .unwrap();
    let patch = app
        .generate_patch(&project.id, Some(vec![selected.id.clone()]), false)
        .await
        .unwrap();
    assert_eq!(hash_file(&game.join("data.xp3")).unwrap(), original_hash);
    let service = EngineService::new(&repository, tools.clone()).with_shared_directory(&shared);
    let mut request = AdapterRequest::new("extract", patch.join("files"));
    request.resource_files = Some(vec!["data.xp3".into()]);
    let result = service
        .execute(
            &AdapterHandle::BuiltinKirikiriSmil,
            &request,
            &CancellationToken::new(),
        )
        .await
        .unwrap()
        .extraction
        .unwrap();
    assert_eq!(result.units.len(), before.units.len());
    for (a, b) in before.units.iter().zip(&result.units) {
        assert_eq!(a.id, b.id);
        assert_eq!(
            &b.source_text,
            if a.id == selected.id {
                &translated
            } else {
                &a.source_text
            }
        );
    }
    app.install_patch(&project.id).await.unwrap();
    assert_ne!(hash_file(&game.join("data.xp3")).unwrap(), original_hash);
    app.restore_game(&project.id).await.unwrap();
    assert_eq!(hash_file(&game.join("data.xp3")).unwrap(), original_hash);
    assert!(
        !app.project_directory(&project.id)
            .unwrap()
            .join("adaptation")
            .exists()
    );
    drop(app);
    let reopened = Application::open(data, &tools).await.unwrap();
    let row = reopened
        .workbench_row(&project.id, &selected.id)
        .await
        .unwrap();
    assert_eq!(row.text.as_deref(), Some(translated.as_str()));
    assert!(row.reviewed);
    assert!(matches!(
        reopened.recognize(&project.id).await.unwrap(),
        RecognitionResult::Extracted(1006)
    ));
    assert_eq!(
        reopened
            .workbench_row(&project.id, &selected.id)
            .await
            .unwrap()
            .text,
        Some(translated)
    );
}
