//! Small recovery snapshots and append-only history; archived messages are read on demand.
use super::*;
use gamel10n_translation::files::{JsonLines, append_bytes, hash_bytes};

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct HistoryStorage {
    pub bytes: u64,
    pub count: usize,
    pub base: usize,
    pub offset: u64,
    pub digest: String,
}

pub(super) fn restore(path: &Path, state: &mut Session) -> Result<(), EngineError> {
    state.history_file = path.with_file_name("session-history.jsonl");
    if let Some(storage) = &state.history_storage {
        if storage.bytes == 0 && storage.count == 0 && storage.base == 0 {
            return Ok(());
        }
        if storage.base > storage.count || storage.offset > storage.bytes {
            return Err(EngineError::Protocol(
                "会话历史位置无效，已有文件已保留。".into(),
            ));
        }
        let mut lines =
            JsonLines::<Value>::range(&state.history_file, storage.offset, storage.bytes)
                .map_err(file_error)?;
        let mut offset = storage.offset;
        state.messages.clear();
        while let Some(message) = lines.next() {
            state.messages.push(message.map_err(file_error)?);
            state.history_offsets.push(offset);
            offset = lines.valid_bytes;
        }
        if offset != storage.bytes || state.messages.len() != storage.count - storage.base {
            return Err(EngineError::Protocol(
                "会话历史与恢复快照不一致，已有文件已保留。".into(),
            ));
        }
    }
    Ok(())
}

pub(super) async fn save(path: &Path, state: &mut Session) -> Result<(), EngineError> {
    let previous = state.history_storage.clone().unwrap_or_default();
    let persisted = previous.count.saturating_sub(previous.base);
    if persisted > state.messages.len() || state.history_start > state.messages.len() {
        return Err(EngineError::Protocol(
            "会话历史位置不一致，不能覆盖已有记录。".into(),
        ));
    }
    let added = state.messages[persisted..].to_vec();
    let mut offsets = state.history_offsets.clone();
    let keep_from = state.history_start;
    let mut snapshot = serde_json::to_value(&*state)?;
    let path = path.to_owned();
    let history_file = path.with_file_name("session-history.jsonl");
    let saved_history_file = history_file.clone();
    let (storage, offsets) = tokio::task::spawn_blocking(move || -> Result<_, EngineError> {
        let mut bytes = Vec::new();
        for message in &added {
            offsets.push(previous.bytes + bytes.len() as u64);
            serde_json::to_writer(&mut bytes, message)?;
            bytes.push(b'\n');
        }
        let end = if !bytes.is_empty() {
            append_bytes(&history_file, previous.bytes, &bytes).map_err(file_error)?
        } else {
            previous.bytes
        };
        let digest = if bytes.is_empty() {
            previous.digest.clone()
        } else {
            let mut content = previous.digest.as_bytes().to_vec();
            content.extend_from_slice(&bytes);
            hash_bytes(&content)
        };
        let storage = HistoryStorage {
            bytes: end,
            count: previous.count + added.len(),
            base: previous.base + keep_from,
            offset: offsets.get(keep_from).copied().unwrap_or(end),
            digest,
        };
        snapshot["historyStorage"] = serde_json::to_value(&storage)?;
        snapshot["historyStart"] = json!(0);
        write_json(&path, &snapshot).map_err(file_error)?;
        offsets.drain(..keep_from);
        Ok((storage, offsets))
    })
    .await
    .map_err(file_error)??;
    state.messages.drain(..keep_from);
    state.history_start = 0;
    state.history_storage = Some(storage);
    state.history_offsets = offsets;
    state.history_file = saved_history_file;
    Ok(())
}

pub(super) fn base(state: &Session) -> usize {
    state
        .history_storage
        .as_ref()
        .map_or(0, |storage| storage.base)
}

/// Visits the archive without retaining it, then includes the current in-memory messages.
pub(super) fn visit(
    state: &Session,
    mut visitor: impl FnMut(usize, &Value) -> Result<bool, EngineError>,
) -> Result<(), EngineError> {
    let base = base(state);
    if base > 0 {
        let storage = state
            .history_storage
            .as_ref()
            .ok_or_else(|| EngineError::Protocol("会话历史位置缺失。".into()))?;
        let mut lines = JsonLines::<Value>::range(&state.history_file, 0, storage.offset)
            .map_err(file_error)?;
        for index in 0..base {
            let message = lines
                .next()
                .ok_or_else(|| EngineError::Protocol("会话归档不完整。".into()))?
                .map_err(file_error)?;
            if !visitor(index, &message)? {
                return Ok(());
            }
        }
    }
    for (index, message) in state.messages.iter().enumerate() {
        if !visitor(base + index, message)? {
            break;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn archived_history_restores_with_stable_indices_and_discards_uncommitted_tail() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("session.json");
        let mut state = Session {
            messages: vec![
                json!({"role":"tool","tool_call_id":"old","content":"original result"}),
                json!({"role":"user","content":"active"}),
            ],
            ..Default::default()
        };
        save(&path, &mut state).await.unwrap();
        state.history_start = 1;
        save(&path, &mut state).await.unwrap();
        assert_eq!(state.messages.len(), 1);
        let committed = state.history_storage.as_ref().unwrap().bytes;
        append_bytes(
            &state.history_file,
            committed,
            b"{\"role\":\"user\",\"content\":\"uncommitted\"}\n{partial",
        )
        .unwrap();
        let mut restored = load_session(&path).unwrap();
        assert_eq!(restored.messages.len(), 1);
        assert_eq!(
            read_session_history(&restored, &json!({"messageIndex":0})).unwrap()["messageIndex"],
            0
        );
        assert!(
            read_tool_result(root.path(), &restored, &json!({"toolCallId":"old"}))
                .unwrap()
                .to_string()
                .contains("original result")
        );
        restored
            .messages
            .push(json!({"role":"user","content":"new"}));
        save(&path, &mut restored).await.unwrap();
        assert!(
            !fs::read_to_string(&restored.history_file)
                .unwrap()
                .contains("uncommitted")
        );
        let again = load_session(&path).unwrap();
        assert_eq!(again.messages.len(), 2);
        assert_eq!(
            read_session_history(&again, &json!({"messageIndex":2})).unwrap()["messageIndex"],
            2
        );
        let bytes = fs::metadata(&restored.history_file).unwrap().len();
        save(&path, &mut restored).await.unwrap();
        assert_eq!(fs::metadata(&restored.history_file).unwrap().len(), bytes);
    }

    #[tokio::test]
    async fn legacy_session_is_retained_until_history_and_snapshot_commit() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("session.json");
        write_json(&path, &json!({"messages":[{"role":"user","content":"legacy"}],"historyStart":0,"customField":"retained"})).unwrap();
        let mut state = load_session(&path).unwrap();
        save(&path, &mut state).await.unwrap();
        let restored = load_session(&path).unwrap();
        assert_eq!(restored.messages[0]["content"], "legacy");
        assert_eq!(restored.extra["customField"], "retained");
        fs::OpenOptions::new()
            .write(true)
            .open(&restored.history_file)
            .unwrap()
            .set_len(1)
            .unwrap();
        assert!(load_session(&path).is_err());
    }

    #[tokio::test]
    async fn missing_history_stops_adaptation_without_replacing_snapshot() {
        use gamel10n_translation::provider::ProviderSettings;

        let root = tempfile::tempdir().unwrap();
        let game = root.path().join("game");
        fs::create_dir(&game).unwrap();
        let path = root.path().join("adaptation/current/session.json");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut state = Session {
            game_id: "fixture".into(),
            game_directory: game.clone(),
            messages: vec![json!({"role":"user","content":"saved progress"})],
            ..Default::default()
        };
        save(&path, &mut state).await.unwrap();
        let original = fs::read(&path).unwrap();
        fs::remove_file(&state.history_file).unwrap();
        let provider = OpenAiProvider::new(ProviderSettings {
            endpoint: "http://127.0.0.1:1/v1".into(),
            api_key: String::new(),
            model: "fixture".into(),
            protocol: Default::default(),
            translation_format: Default::default(),
            analysis_reasoning_effort: None,
        })
        .unwrap();
        let service = EngineService::new(
            root.path(),
            crate::ToolPaths::new(root.path().join("tools")),
        );
        let request = AdaptationRequest {
            game_id: state.game_id.clone(),
            game_directory: game,
            inputs: AdaptationInputs::default(),
            project_directory: root.path().into(),
            package_directory: root.path().join("extensions"),
            package_directories: vec![],
            user_instruction: String::new(),
            objective: "Develop".into(),
            improve_existing: false,
            require_model_analysis: false,
            reuse_failures: vec![],
            builtin_probes: vec![],
            discovery_inventory: None,
            limits: AdaptationLimits::default(),
            baseline_units: vec![],
        };
        let error = service
            .adapt(&request, &provider, &CancellationToken::new(), None)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("无法安全恢复"), "{error}");
        assert_eq!(provider.request_count(), 0);
        assert_eq!(fs::read(path).unwrap(), original);
    }
}
