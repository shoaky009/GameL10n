use super::*;

#[tokio::test]
async fn filtered_pages_keep_extraction_order_across_edits_and_index_recovery() {
    use gamel10n_translation::files::{read_json, write_json};
    use std::fs;
    let (root, app, id, _) = fixture().await;
    let dir = app.project_directory(&id).unwrap();
    let source = dir.join("extraction/native.json");
    let mut original: crate::StoredAnalysis = read_json(&source).unwrap();
    let template = original.extraction.units[0].clone();
    original.extraction.units = (0..405)
        .map(|i| TranslationUnit {
            id: format!("id-{i}"),
            source_text: if i % 2 == 0 { "Needle" } else { "filler" }.into(),
            resource_path: format!("scene/{}", i / 100),
            ..template.clone()
        })
        .collect();
    write_json(&source, &original).unwrap();
    let query = WorkbenchFilter {
        query: "NEEDLE".into(),
        ..Default::default()
    };
    let visited = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let superseded = app
        .workbench_page_while(&id, &query, 100, 100, move || {
            visited.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 2
        })
        .await;
    assert!(matches!(
        superseded,
        Err(CoreError::Engine(gamel10n_engines::EngineError::Cancelled))
    ));
    let page = app.workbench_page(&id, &query, 100, 100).await.unwrap();
    assert_eq!((page.total, page.offset, page.rows.len()), (203, 100, 100));
    let expected: Vec<_> = (200..400).step_by(2).map(|i| format!("id-{i}")).collect();
    assert_eq!(
        page.rows.into_iter().map(|r| r.unit.id).collect::<Vec<_>>(),
        expected
    );
    let last = app.workbench_page(&id, &query, 200, 100).await.unwrap();
    assert_eq!(
        last.rows.into_iter().map(|r| r.unit.id).collect::<Vec<_>>(),
        ["id-400", "id-402", "id-404"]
    );
    assert_eq!(
        app.adjacent_text(&id, &query, "id-198", 1).await.unwrap(),
        Some(("id-200".into(), 100))
    );
    app.save_translation_edit(&id, "id-1", "needle".into(), false)
        .await
        .unwrap();
    let page = app.workbench_page(&id, &query, 100, 100).await.unwrap();
    assert_eq!((page.total, page.rows[0].unit.id.as_str()), (204, "id-198"));
    app.save_translation_edit(&id, "id-1", "saved elsewhere".into(), true)
        .await
        .unwrap();
    let page = app.workbench_page(&id, &query, 100, 100).await.unwrap();
    assert_eq!((page.total, page.rows[0].unit.id.as_str()), (203, "id-200"));
    let select = WorkbenchFilter {
        query: "needle".into(),
        resource_path: Some("scene/2".into()),
        kind: "dialogue".into(),
        selected_ids: Some(vec!["id-200".into(), "id-300".into(), "id-201".into()]),
        ..Default::default()
    };
    assert_eq!(
        app.matching_text_ids(&id, &select).await.unwrap(),
        ["id-200"]
    );
    drop(app);
    let data = dir.join("workbench-v1/pages.bin");
    let idx = dir.join("workbench-v1/pages.idx");
    let record = dir.join("project-migrations.json");
    let modified = |p: &std::path::Path| fs::metadata(p).unwrap().modified().unwrap();
    let committed = (modified(&data), modified(&idx), modified(&record));
    let reopen = Application::open(root.path().join("data"), &crate::ToolPaths::new("missing"))
        .await
        .unwrap();
    assert_eq!(
        reopen
            .workbench_row(&id, "id-1")
            .await
            .unwrap()
            .text
            .as_deref(),
        Some("saved elsewhere")
    );
    assert_eq!(
        (modified(&data), modified(&idx), modified(&record)),
        committed
    );
    let baseline = fs::read(&idx).unwrap();
    for broken in [None, Some(b"broken".as_slice())] {
        if let Some(bytes) = broken {
            fs::write(&idx, bytes).unwrap();
        } else {
            fs::remove_file(&idx).unwrap();
        }
        assert_eq!(
            reopen
                .workbench_page(&id, &query, 200, 100)
                .await
                .unwrap()
                .rows
                .len(),
            3
        );
        assert_eq!(fs::read(&idx).unwrap(), baseline);
        assert_eq!(
            (modified(&data), modified(&record)),
            (committed.0, committed.2)
        );
    }
    fs::remove_file(&record).unwrap();
    assert_eq!(
        reopen
            .workbench_page(&id, &query, 0, 100)
            .await
            .unwrap()
            .total,
        203
    );
    assert!(record.is_file());
    assert_eq!(modified(&data), committed.0);
    let mut damaged = fs::read(&data).unwrap();
    *damaged.last_mut().unwrap() ^= 1;
    fs::write(&data, damaged).unwrap();
    assert_eq!(
        reopen
            .workbench_page(&id, &query, 200, 100)
            .await
            .unwrap()
            .total,
        203
    );
    fs::remove_file(&data).unwrap();
    let row = reopen.workbench_row(&id, "id-1").await.unwrap();
    assert_eq!(row.text.as_deref(), Some("saved elsewhere"));
    assert!(row.reviewed);
    assert_eq!(
        reopen.matching_text_ids(&id, &query).await.unwrap().len(),
        203
    );
}
#[tokio::test]
async fn browsing_retains_only_the_current_project_pages() {
    let (root, app, id, units) = fixture().await;
    app.workbench_page(&id, &WorkbenchFilter::default(), 0, 1)
        .await
        .unwrap();
    for index in 0..4 {
        let game = root.path().join(format!("game-{index}"));
        std::fs::create_dir(&game).unwrap();
        let project = app.add_project(&game).await.unwrap();
        gamel10n_translation::files::write_json(
            &app.project_directory(&project.id)
                .unwrap()
                .join("extraction/native.json"),
            &crate::StoredAnalysis {
                schema_version: 1,
                coverage_pending: false,
                format: "test".into(),
                source_directory: game,
                resource_files: Vec::new(),
                source_hashes: BTreeMap::new(),
                extraction: gamel10n_engines::protocol::ExtractionResult {
                    units: units.clone(),
                    warnings: Vec::new(),
                },
                adapter: None,
                probe: None,
            },
        )
        .unwrap();
        app.workbench_page(&project.id, &WorkbenchFilter::default(), 0, 1)
            .await
            .unwrap();
        app.workbench_page(&id, &WorkbenchFilter::default(), 0, 1)
            .await
            .unwrap();
    }
    assert!(app.workbenches.lock().await.is_empty());
    assert_eq!(app.paged_workbench.lock().unwrap().as_ref().unwrap().id, id);
}

#[tokio::test]
async fn concurrent_readers_migrate_once_and_refresh_after_new_translations() {
    let (root, app, id, units) = fixture().await;
    let directory = app.project_directory(&id).unwrap();
    let data = directory.join("workbench-v1/pages.bin");
    let record = directory.join("project-migrations.json");
    let interrupted = directory.join("workbench-v1/.workbench-build-interrupted");
    std::fs::create_dir_all(interrupted.parent().unwrap()).unwrap();
    std::fs::write(&interrupted, b"partial").unwrap();
    let other = Application::open(root.path().join("data"), &crate::ToolPaths::new("missing"))
        .await
        .unwrap();
    let (first, second) = tokio::join!(app.workbench_row(&id, "2"), other.workbench_row(&id, "2"));
    for row in [first, second] {
        assert_eq!(row.unwrap().text.as_deref(), Some("再见"));
    }
    let committed = std::fs::metadata(&record).unwrap().modified().unwrap();
    assert!(!interrupted.exists());
    let policy = policy(&app.open_project(&id).await.unwrap().settings);
    let key = memory_key(&units[1], &policy);
    let mut memory = TranslationMemory::load(directory.join("memory"), None).unwrap();
    memory
        .store_batch(vec![(
            key,
            gamel10n_translation::storage::MemoryEntry {
                key: None,
                source_text: units[1].source_text.clone(),
                context: units[1].context.clone(),
                translation: "明天再见".into(),
                provider: "test".into(),
                created_at: gamel10n_translation::storage::now(),
            },
        )])
        .unwrap();
    assert_eq!(
        app.workbench_row(&id, "2").await.unwrap().text.as_deref(),
        Some("明天再见")
    );
    app.save_project_settings(&id, serde_json::json!({"targetLanguage": "English"}))
        .await
        .unwrap();
    assert!(app.workbench_row(&id, "2").await.unwrap().text.is_none());
    assert_eq!(
        std::fs::metadata(&record).unwrap().modified().unwrap(),
        committed
    );

    // Older clients must preserve a later format, including when its index needs repair.
    let bytes = std::fs::read(&data).unwrap();
    std::fs::write(&record, br#"{"version": 2}"#).unwrap();
    std::fs::remove_file(directory.join("workbench-v1/pages.idx")).unwrap();
    assert!(app.workbench_row(&id, "2").await.is_err());
    assert_eq!(std::fs::read(&data).unwrap(), bytes);
    assert_eq!(std::fs::read(&record).unwrap(), br#"{"version": 2}"#);
}

#[tokio::test]
async fn reopening_pages_checks_the_game_markers_that_control_resource_compatibility() {
    use gamel10n_translation::files::{read_json, write_json};
    let (root, app, id, _) = fixture().await;
    let native = app
        .project_directory(&id)
        .unwrap()
        .join("extraction/native.json");
    let mut stored: crate::StoredAnalysis = read_json(&native).unwrap();
    stored.adapter = Some(gamel10n_engines::AdapterHandle::Native {
        format: "KirikiriKs".into(),
    });
    stored.resource_files = vec!["data/story.ks".into(), "other.ks".into()];
    stored.extraction.units[0].resource_path = stored.resource_files[0].clone();
    for unit in &mut stored.extraction.units[1..] {
        unit.resource_path = stored.resource_files[1].clone();
    }
    write_json(&native, &stored).unwrap();
    assert_eq!(
        app.workbench_page(&id, &Default::default(), 0, 100)
            .await
            .unwrap()
            .total,
        3
    );
    drop(app);
    let runtime = root.path().join("game/tyrano");
    std::fs::create_dir_all(runtime.join("plugins/kag")).unwrap();
    std::fs::write(runtime.join("tyrano.js"), []).unwrap();
    std::fs::write(runtime.join("plugins/kag/kag.js"), []).unwrap();
    let reopen = Application::open(root.path().join("data"), &crate::ToolPaths::new("missing"))
        .await
        .unwrap();
    assert_eq!(
        reopen
            .matching_text_ids(&id, &Default::default())
            .await
            .unwrap(),
        ["2", "3"]
    );
    drop(reopen);
    std::fs::remove_file(runtime.join("tyrano.js")).unwrap();
    let reopen = Application::open(root.path().join("data"), &crate::ToolPaths::new("missing"))
        .await
        .unwrap();
    assert_eq!(
        reopen
            .matching_text_ids(&id, &Default::default())
            .await
            .unwrap(),
        ["1", "2", "3"]
    );
}
