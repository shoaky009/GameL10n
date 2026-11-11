use gamel10n_core::engine_tasks::RecognitionResult;
use gamel10n_core::{AppEvent, Application, ToolPaths, workbench::WorkbenchFilter};
use gamel10n_translation::files::hash_file;
use std::fs;

#[tokio::test]
async fn recognition_reports_candidates_and_stops_when_a_required_tool_is_missing() {
    let root = tempfile::tempdir().unwrap();
    let game = root.path().join("game");
    fs::create_dir(&game).unwrap();
    fs::write(game.join("script.rpy"), "label start:\n    \"Hello\"\n").unwrap();
    let app = Application::open(
        root.path().join("data"),
        &ToolPaths::new(root.path().join("missing-tools")),
    )
    .await
    .unwrap();
    let project = app.add_project(&game).await.unwrap();
    let mut events = app.subscribe();
    let error = app.recognize(&project.id).await.unwrap_err();
    let mut running_messages = Vec::new();
    let mut stages = Vec::new();
    while let Ok(event) = events.try_recv() {
        if let AppEvent::TaskChanged(state) = event
            && state.project_id == project.id
        {
            stages.push((state.operation.clone(), state.running));
            if state.operation == "recognize" && state.running {
                running_messages.push(state.message);
            }
        }
    }
    assert!(
        running_messages
            .iter()
            .any(|message| message.contains("正在检查 Renpy")),
        "candidate feedback was lost: {running_messages:?}"
    );
    let logs = app.log_tail(&project.id).await.unwrap();
    assert!(
        logs.contains("已扫描 1 个文件") && logs.contains("找不到工具"),
        "{logs}"
    );
    assert!(matches!(
        error,
        gamel10n_core::CoreError::Engine(gamel10n_engines::EngineError::ToolMissing(_))
    ));
    assert_eq!(stages.first(), Some(&("recognize".into(), true)));
    assert_eq!(stages.last(), Some(&("recognize".into(), false)));
    assert!(
        stages[..stages.len() - 1]
            .iter()
            .all(|(_, running)| *running)
    );
    assert!(stages.iter().all(|(operation, _)| operation != "adapt"));
    assert!(
        !app.project_directory(&project.id)
            .unwrap()
            .join("adaptation")
            .exists()
    );
}

#[tokio::test]
async fn cancelled_recognition_does_not_start_intelligent_adaptation() {
    let root = tempfile::tempdir().unwrap();
    let game = root.path().join("game");
    fs::create_dir(&game).unwrap();
    let app = Application::open(
        root.path().join("data"),
        &ToolPaths::new(root.path().join("tools")),
    )
    .await
    .unwrap();
    let project = app.add_project(&game).await.unwrap();
    let mut events = app.subscribe();
    let recognition = app.recognize(&project.id);
    tokio::pin!(recognition);
    let mut cancelled = false;
    let result = loop {
        tokio::select! {
            biased;
            event = events.recv() => {
                if let AppEvent::TaskChanged(state) = event.unwrap() {
                    assert_ne!(state.operation, "adapt");
                    if state.running && !cancelled {
                        app.cancel(&project.id).await;
                        cancelled = true;
                    }
                }
            }
            result = &mut recognition => break result,
        }
    };
    assert!(cancelled);
    assert!(matches!(
        result,
        Err(gamel10n_core::CoreError::Engine(
            gamel10n_engines::EngineError::Cancelled
        ))
    ));
    let state = app.task_state(&project.id).await.unwrap();
    assert!(!state.running && state.succeeded.is_none());
    assert!(
        !app.project_directory(&project.id)
            .unwrap()
            .join("adaptation")
            .exists()
    );
}

#[tokio::test]
async fn recognition_scan_error_does_not_fall_back_to_an_api_error() {
    let root = tempfile::tempdir().unwrap();
    let game = root.path().join("game");
    fs::create_dir(&game).unwrap();
    let app = Application::open(
        root.path().join("data"),
        &ToolPaths::new(root.path().join("tools")),
    )
    .await
    .unwrap();
    let project = app.add_project(&game).await.unwrap();
    fs::remove_dir(&game).unwrap();
    let error = app.recognize(&project.id).await.unwrap_err();
    assert!(error.to_string().contains("游戏目录不存在"), "{error}");
    let state = app.task_state(&project.id).await.unwrap();
    assert_eq!(state.operation, "recognize");
    assert_eq!(state.succeeded, Some(false));
    assert!(
        !app.project_directory(&project.id)
            .unwrap()
            .join("adaptation")
            .exists()
    );
}

#[tokio::test]
#[ignore = "requires portable Python tools; API responses are served locally"]
async fn unmatched_recognition_uses_the_configured_api_and_resumes_its_session() {
    use gamel10n_core::settings::ApiProfile;
    use gamel10n_translation::files::read_json;
    use serde_json::{Value, json};
    use std::io::{BufRead, Read, Write};

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        let mut requests = Vec::<Value>::new();
        for _ in 0..2 {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(15)))
                .unwrap();
            let mut reader = std::io::BufReader::new(&mut stream);
            let mut length = 0;
            loop {
                let mut line = String::new();
                assert!(reader.read_line(&mut line).unwrap() > 0);
                if line == "\r\n" {
                    break;
                }
                if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = value.trim().parse().unwrap();
                }
            }
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            requests.push(serde_json::from_slice(&body).unwrap());
            let response = json!({
                "choices": [{"finish_reason":"stop", "message":{
                    "role":"assistant", "content":"仍在探索：正在核对资源结构。"
                }}],
                "usage":{"prompt_tokens":100,"completion_tokens":20}
            })
            .to_string();
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", response.len(), response).unwrap();
        }
        requests
    });
    let root = tempfile::tempdir().unwrap();
    let game = root.path().join("game");
    fs::create_dir(&game).unwrap();
    fs::write(game.join("unknown.bin"), b"unrecognized resource fixture").unwrap();
    let tools = ToolPaths::development();
    let app = Application::open(root.path().join("data"), &tools)
        .await
        .unwrap();
    let project = app.add_project(&game).await.unwrap();
    app.save_api_profile(
        ApiProfile {
            id: "local-fixture".into(),
            name: "Local fixture".into(),
            endpoint,
            model: "fixture".into(),
            protocol: "ChatCompletions".into(),
            ..ApiProfile::default()
        },
        None,
        false,
    )
    .await
    .unwrap();
    app.save_project_settings(&project.id, json!({"adaptationMaximumTurns":1}))
        .await
        .unwrap();
    let session_path = app
        .project_directory(&project.id)
        .unwrap()
        .join("adaptation/current/session.json");
    for turn in 1..=2 {
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            app.recognize(&project.id),
        )
        .await
        .unwrap()
        .unwrap();
        let RecognitionResult::Adapted(result) = result else {
            panic!("unknown resource used a regular adapter")
        };
        assert_eq!(
            result.status,
            gamel10n_engines::AdaptationStatus::BudgetExhausted
        );
        assert_eq!(result.api_requests, Some(turn));
        let session: Value = read_json(&session_path).unwrap();
        assert_eq!(session["totalTurns"], turn);
        assert_eq!(session["usage"]["inputTokens"], turn * 100);
    }
    let requests = server.join().unwrap();
    assert!(
        requests[1]["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|message| {
                message["role"] == "assistant"
                    && message["content"] == "仍在探索：正在核对资源结构。"
            })
    );
}

#[tokio::test]
async fn workbench_before_extraction_is_empty_but_corrupt_extraction_still_fails() {
    let root = tempfile::tempdir().unwrap();
    let game = root.path().join("game");
    fs::create_dir(&game).unwrap();
    let tools = ToolPaths::new(root.path().join("tools"));
    let app = Application::open(root.path().join("data"), &tools)
        .await
        .unwrap();
    let project = app.add_project(&game).await.unwrap();
    let filter = WorkbenchFilter::default();
    let page = app
        .workbench_page(&project.id, &filter, 100, 100)
        .await
        .unwrap();
    assert!(page.rows.is_empty());
    assert_eq!((page.total, page.offset), (0, 0));
    let extraction = app
        .project_directory(&project.id)
        .unwrap()
        .join("extraction");
    fs::create_dir(&extraction).unwrap();
    fs::write(extraction.join("native.json"), b"invalid json").unwrap();
    assert!(
        app.workbench_page(&project.id, &filter, 0, 100)
            .await
            .is_err()
    );
}

#[tokio::test]
#[ignore = "requires prepared NativeAOT and portable Python tools"]
async fn selected_edit_patch_install_restore_and_restart_preserve_originals() {
    let root = tempfile::tempdir().unwrap();
    let game = root.path().join("game");
    fs::create_dir(&game).unwrap();
    let script = game.join("script.rpy");
    // Keep the patch workflow independent of the small-extraction smart check.
    fs::write(
        &script,
        format!(
            "label start:\n    e \"Hello there!\"\n    \"A quiet morning.\"\n{}    return\n",
            "    \"Fixture dialogue.\"\n".repeat(998)
        ),
    )
    .unwrap();
    let original_hash = hash_file(&script).unwrap();
    let original_time = fs::metadata(&script).unwrap().modified().unwrap();
    let tools = ToolPaths::development();
    let data = root.path().join("data");
    let app = Application::open(data.clone(), &tools).await.unwrap();
    let project = app.add_project(&game).await.unwrap();
    assert!(matches!(
        app.recognize(&project.id).await.unwrap(),
        RecognitionResult::Extracted(1_000)
    ));
    assert!(
        !app.project_directory(&project.id)
            .unwrap()
            .join("adaptation")
            .exists()
    );
    let page = app
        .workbench_page(&project.id, &WorkbenchFilter::default(), 0, 100)
        .await
        .unwrap();
    let selected = page.rows[0].unit.id.clone();
    app.save_translation_edit(&project.id, &selected, "你好！".into(), true)
        .await
        .unwrap();
    app.save_text_selection(&project.id, std::slice::from_ref(&selected))
        .await
        .unwrap();
    let patch = app
        .generate_patch(&project.id, Some(vec![selected.clone()]), false)
        .await
        .unwrap();
    let payload = patch.join(gamel10n_core::patching::PAYLOAD_DIRECTORY);
    let text = fs::read_to_string(payload.join("script.rpy")).unwrap();
    assert!(text.contains("你好！"));
    assert!(text.contains("A quiet morning."));
    assert_eq!(hash_file(&script).unwrap(), original_hash);
    assert_eq!(
        app.generate_patch(&project.id, Some(vec![selected.clone()]), false)
            .await
            .unwrap(),
        patch
    );
    app.save_api_profile(
        gamel10n_core::settings::ApiProfile {
            id: "unused".into(),
            name: "Unused provider".into(),
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
    let mut events = app.subscribe();
    for resume in [false, true] {
        let result = if resume {
            app.resume_translation(&project.id).await
        } else {
            app.translate_project(
                &project.id,
                gamel10n_translation::execution::TranslationOptions {
                    unit_ids: Some(vec![selected.clone()]),
                    build_patch_after: true,
                    ..Default::default()
                },
            )
            .await
        }
        .unwrap();
        assert_eq!(result.cache_hits, 1);
        assert!(
            app.translation_scope(&project.id)
                .await
                .unwrap()
                .unwrap()
                .completed
        );
        let mut finished = Vec::new();
        while let Ok(event) = events.try_recv() {
            if let AppEvent::TaskChanged(state) = event
                && !state.running
            {
                finished.push((state.operation, state.succeeded));
            }
        }
        assert_eq!(finished, vec![("build".into(), Some(true))]);
    }
    app.install_patch(&project.id).await.unwrap();
    assert_eq!(
        hash_file(&script).unwrap(),
        hash_file(&payload.join("script.rpy")).unwrap()
    );
    app.restore_game(&project.id).await.unwrap();
    assert_eq!(hash_file(&script).unwrap(), original_hash);
    assert_eq!(
        fs::metadata(&script).unwrap().modified().unwrap(),
        original_time
    );
    assert!(
        !fs::read_dir(&game)
            .unwrap()
            .filter_map(Result::ok)
            .any(|entry| entry.file_name().to_string_lossy().ends_with(".bak"))
    );
    drop(app);
    let reopened = Application::open(data, &tools).await.unwrap();
    assert_eq!(
        reopened.load_text_selection(&project.id).await.unwrap(),
        vec![selected.clone()]
    );
    let row = reopened
        .workbench_row(&project.id, &selected)
        .await
        .unwrap();
    assert_eq!(row.text.as_deref(), Some("你好！"));
    assert!(row.reviewed);
    let logs = reopened.log_tail(&project.id).await.unwrap();
    assert!(logs.contains("补丁已安装"));
    assert!(logs.contains("已恢复"));
}
