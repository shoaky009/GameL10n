//! Maintenance of the explicit resource copies used by adapter verification.
use super::*;
use gamel10n_translation::files::FileError;
use std::io;

/// Logical game inputs pinned by the host to extraction snapshots or original backups.
/// Files outside this map still resolve in the game directory unless explicitly excluded.
#[derive(Clone, Debug, Default)]
pub struct AdaptationInputs {
    pub files: BTreeMap<String, PathBuf>,
    /// Digests checked by the host when binding these inputs for the current run.
    pub hashes: BTreeMap<String, String>,
    pub excluded: BTreeSet<String>,
}

impl AdaptationInputs {
    fn bound_file(&self, relative: &str) -> Option<&PathBuf> {
        self.files.get(relative).or_else(|| {
            self.files
                .iter()
                .find(|(path, _)| path_key(path) == path_key(relative))
                .map(|(_, file)| file)
        })
    }

    pub(super) fn expected_hash(&self, relative: &str) -> Option<&str> {
        self.hashes
            .get(relative)
            .or_else(|| {
                self.hashes
                    .iter()
                    .find(|(path, _)| path_key(path) == path_key(relative))
                    .map(|(_, hash)| hash)
            })
            .map(String::as_str)
    }

    pub fn excludes(&self, relative: &str) -> bool {
        let relative = relative.replace('\\', "/");
        self.bound_file(&relative).is_none()
            && (self.excluded.contains(&relative)
                || self
                    .excluded
                    .iter()
                    .any(|path| path_key(path) == path_key(&relative)))
    }

    pub fn file(&self, game: &Path, relative: &str) -> Result<PathBuf, EngineError> {
        let current = resource_path(game, relative)?;
        let relative = relative.replace('\\', "/");
        if let Some(path) = self.bound_file(&relative) {
            return Ok(path.clone());
        }
        if self.excludes(&relative) {
            return Err(EngineError::Rejected(format!(
                "文件不属于制作输入：{relative}"
            )));
        }
        Ok(current)
    }

    pub fn inventory(&self, mut inventory: Inventory) -> Result<Inventory, EngineError> {
        if self.files.is_empty() && self.excluded.is_empty() {
            return Ok(inventory);
        }
        let hidden: BTreeSet<_> = self
            .files
            .keys()
            .chain(&self.excluded)
            .map(|path| path_key(path))
            .collect();
        inventory
            .files
            .retain(|file| !hidden.contains(&path_key(&file.path)));
        for (relative, path) in &self.files {
            let metadata = path
                .metadata()
                .map_err(|error| resource_error(relative, "检查制作输入", error))?;
            if !metadata.is_file() {
                return Err(EngineError::Protocol(format!(
                    "制作输入不是普通文件：{relative}"
                )));
            }
            inventory.files.push(crate::discovery::InventoryFile {
                path: relative.clone(),
                length: metadata.len(),
            });
        }
        inventory
            .files
            .sort_by(|left, right| left.path.cmp(&right.path));
        Ok(inventory)
    }
}

fn cancelled(cancellation: &CancellationToken) -> Result<(), EngineError> {
    if cancellation.is_cancelled() {
        return Err(EngineError::Cancelled);
    }
    Ok(())
}

fn path_key(path: &str) -> String {
    path.replace('\\', "/").to_lowercase()
}

fn resource_path(root: &Path, relative: &str) -> Result<PathBuf, EngineError> {
    resolve_relative(root, relative)
        .map_err(|error| EngineError::Protocol(format!("资源路径无效（{relative}）：{error}")))
}

fn resource_error(relative: &str, action: &str, error: io::Error) -> EngineError {
    EngineError::Io(io::Error::new(
        error.kind(),
        format!("{action}资源 {relative} 失败：{error}"),
    ))
}

fn resource_hash(path: &Path, relative: &str) -> Result<String, EngineError> {
    hash_file(path).map_err(|error| match error {
        FileError::Io { source, .. } => resource_error(relative, "读取", source),
        error => EngineError::Protocol(format!("资源哈希校验失败（{relative}）：{error}")),
    })
}

// Only verification copies are made writable; original inputs keep their permissions.
fn make_staged_writable(path: &Path) -> io::Result<()> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    let mut permissions = metadata.permissions();
    if !permissions.readonly() {
        return Ok(());
    }
    #[cfg(windows)]
    #[expect(
        clippy::permissions_set_readonly_false,
        reason = "Windows clears the read-only attribute; Unix changes only the owner's write bit."
    )]
    permissions.set_readonly(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        permissions.set_mode(permissions.mode() | 0o200);
    }
    fs::set_permissions(path, permissions)
}

fn approved(state: &Session) -> Result<BTreeMap<String, String>, EngineError> {
    Ok(serde_json::from_value(
        state
            .extra
            .get("resourceHashes")
            .cloned()
            .unwrap_or_else(|| json!({})),
    )?)
}

fn invalidate(state: &mut Session) {
    state.verified_package = None;
    state.verified_fingerprint = None;
    state.verification = None;
    for key in [
        "verificationCache",
        "trialVerificationCache",
        "discoveryVerificationCache",
    ] {
        state.extra.remove(key);
    }
}

fn staged_files(
    root: &Path,
    directory: &str,
    cancellation: &CancellationToken,
    files: &mut Vec<String>,
) -> Result<(), EngineError> {
    cancelled(cancellation)?;
    let path = if directory.is_empty() {
        root.to_path_buf()
    } else {
        resource_path(root, directory)?
    };
    let entries = match fs::read_dir(path) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(resource_error(directory, "扫描暂存", error)),
    };
    for entry in entries {
        cancelled(cancellation)?;
        let entry = entry.map_err(|error| resource_error(directory, "扫描暂存", error))?;
        let name = entry.file_name();
        let name = name
            .to_str()
            .ok_or_else(|| EngineError::Protocol("暂存资源文件名不是有效 UTF-8。".into()))?;
        let relative = if directory.is_empty() {
            name.to_string()
        } else {
            format!("{directory}/{name}")
        };
        let path = resource_path(root, &relative)?;
        let metadata = match fs::metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(resource_error(&relative, "检查暂存", error)),
        };
        if metadata.is_dir() {
            staged_files(root, &relative, cancellation, files)?;
        } else if metadata.is_file() {
            files.push(relative);
        } else {
            return Err(EngineError::Protocol(format!(
                "暂存资源不是普通文件：{relative}"
            )));
        }
    }
    Ok(())
}

fn remove_unapproved(
    root: &Path,
    hashes: &BTreeMap<String, String>,
    cancellation: &CancellationToken,
) -> Result<Vec<String>, EngineError> {
    let selected: BTreeSet<_> = hashes.keys().map(|path| path_key(path)).collect();
    let mut files = Vec::new();
    staged_files(root, "", cancellation, &mut files)?;
    let mut removed = Vec::new();
    for relative in files {
        cancelled(cancellation)?;
        if selected.contains(&path_key(&relative)) {
            continue;
        }
        let path = resource_path(root, &relative)?;
        make_staged_writable(&path)
            .map_err(|error| resource_error(&relative, "准备删除暂存", error))?;
        match fs::remove_file(path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(resource_error(&relative, "删除暂存", error)),
        }
        removed.push(relative);
    }
    Ok(removed)
}

#[derive(Default, Serialize)]
pub(super) struct ResourceChanges {
    pub removed: Vec<String>,
    pub rebound: Vec<String>,
}

impl ResourceChanges {
    pub fn is_empty(&self) -> bool {
        self.removed.is_empty() && self.rebound.is_empty()
    }
}

/// Reconciles selected copies with host-bound inputs, preserving source and work files.
pub(super) fn reconcile(
    request: &AdaptationRequest,
    workspace: &Path,
    state: &mut Session,
    cancellation: &CancellationToken,
    progress: ProgressCallback<'_>,
) -> Result<ResourceChanges, EngineError> {
    cancelled(cancellation)?;
    let previous = approved(state)?;
    let root = resource_path(workspace, "resources")?;
    let mut next = previous.clone();
    let mut removed = BTreeMap::new();
    let mut rebound = Vec::new();
    for relative in previous.keys() {
        cancelled(cancellation)?;
        resource_path(&root, relative)?;
        if request.inputs.excludes(relative) {
            next.remove(relative);
            removed.insert(path_key(relative), relative.replace('\\', "/"));
            continue;
        }
        let input = request.inputs.file(&request.game_directory, relative)?;
        match fs::metadata(input) {
            Ok(metadata) if metadata.is_file() => {
                if request
                    .inputs
                    .expected_hash(relative)
                    .is_some_and(|hash| !previous[relative].eq_ignore_ascii_case(hash))
                    || !resource_path(&root, relative)?.is_file()
                {
                    rebound.push(relative.clone());
                }
            }
            Ok(_) => {
                return Err(EngineError::Protocol(format!(
                    "已暂存的游戏资源不是普通文件：{relative}"
                )));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                if request.inputs.bound_file(relative).is_some() {
                    return Err(resource_error(relative, "读取已绑定制作输入", error));
                }
                next.remove(relative);
                removed.insert(path_key(relative), relative.replace('\\', "/"));
            }
            Err(error) => return Err(resource_error(relative, "检查游戏", error)),
        }
    }
    for relative in remove_unapproved(&root, &next, cancellation)? {
        removed.entry(path_key(&relative)).or_insert(relative);
    }
    cancelled(cancellation)?;
    // Commit selection changes and rebind copies together; failed copies remain detectable on retry.
    let mut updated = Session::default();
    updated.extra.insert("resourceHashes".into(), json!(next));
    for files in rebound.chunks(256) {
        stage(
            request,
            workspace,
            &mut updated,
            files,
            false,
            cancellation,
            progress,
        )?;
    }
    if next != previous || !removed.is_empty() || !rebound.is_empty() {
        state.extra.insert(
            "resourceHashes".into(),
            updated
                .extra
                .remove("resourceHashes")
                .unwrap_or_else(|| json!({})),
        );
        invalidate(state);
    }
    Ok(ResourceChanges {
        removed: removed.into_values().collect(),
        rebound,
    })
}

/// Stages selected whole files; replacement also removes every unselected copy.
pub(super) fn stage(
    request: &AdaptationRequest,
    workspace: &Path,
    state: &mut Session,
    files: &[String],
    replace: bool,
    cancellation: &CancellationToken,
    progress: ProgressCallback<'_>,
) -> Result<Value, EngineError> {
    cancelled(cancellation)?;
    if files.len() > 256 || (files.is_empty() && !replace) {
        return Err(EngineError::Protocol(
            "每次选择 1 到 256 个资源文件；replace=true 可用空列表清空。".into(),
        ));
    }
    let previous = approved(state)?;
    let root = resource_path(workspace, "resources")?;
    for relative in previous.keys() {
        resource_path(&request.game_directory, relative)?;
        resource_path(&root, relative)?;
    }
    let files: Vec<_> = files.iter().map(|path| path.replace('\\', "/")).collect();
    let unique: BTreeSet<_> = files.iter().map(|path| path_key(path)).collect();
    if unique.len() != files.len() {
        return Err(EngineError::Protocol("暂存资源路径不能重复。".into()));
    }
    let mut next = if replace {
        BTreeMap::new()
    } else {
        previous.clone()
    };
    let mut changed = false;
    for relative in &files {
        cancelled(cancellation)?;
        let input = request.inputs.file(&request.game_directory, relative)?;
        let output = resource_path(&root, relative)?;
        let metadata =
            fs::metadata(&input).map_err(|error| resource_error(relative, "检查游戏", error))?;
        if !metadata.is_file() {
            return Err(EngineError::Protocol(format!(
                "所选的游戏资源不是普通文件：{relative}"
            )));
        }
        EngineService::report(
            progress,
            "暂存验证资源",
            format!("正在校验资源：{relative}（{} 字节）", metadata.len()),
        );
        let digest = resource_hash(&input, relative)?;
        if request
            .inputs
            .expected_hash(relative)
            .is_some_and(|expected| !digest.eq_ignore_ascii_case(expected))
        {
            return Err(EngineError::Rejected(format!(
                "已绑定的制作输入发生变化：{relative}；已有文本基线保留。"
            )));
        }
        let reused = match fs::metadata(&output) {
            Ok(metadata) if metadata.is_file() => resource_hash(&output, relative)? == digest,
            Ok(_) => {
                return Err(EngineError::Protocol(format!(
                    "暂存资源路径被非文件占用：{relative}"
                )));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => false,
            Err(error) => return Err(resource_error(relative, "检查暂存", error)),
        };
        if !reused {
            let parent = output
                .parent()
                .ok_or_else(|| EngineError::Protocol(format!("资源路径没有父目录：{relative}")))?;
            fs::create_dir_all(parent)
                .map_err(|error| resource_error(relative, "创建暂存目录", error))?;
            let temporary = tempfile::NamedTempFile::new_in(parent)
                .map_err(|error| resource_error(relative, "创建暂存文件", error))?;
            cancelled(cancellation)?;
            EngineService::report(
                progress,
                "暂存验证资源",
                format!("正在复制资源：{relative}（{} 字节）", metadata.len()),
            );
            fs::copy(&input, temporary.path())
                .map_err(|error| resource_error(relative, "复制暂存", error))?;
            make_staged_writable(temporary.path())
                .map_err(|error| resource_error(relative, "设置暂存副本权限", error))?;
            if resource_hash(temporary.path(), relative)? != digest {
                return Err(EngineError::Rejected(format!(
                    "原始资源复制校验失败：{relative}"
                )));
            }
            cancelled(cancellation)?;
            make_staged_writable(&output)
                .map_err(|error| resource_error(relative, "准备替换暂存", error))?;
            temporary
                .persist(&output)
                .map_err(|error| resource_error(relative, "提交暂存", error.error))?;
            changed = true;
        }
        next.retain(|path, _| path_key(path) != path_key(relative));
        next.insert(relative.clone(), digest);
    }
    let mut removed = BTreeMap::new();
    if replace {
        for relative in previous
            .keys()
            .filter(|path| !unique.contains(&path_key(path)))
        {
            removed.insert(path_key(relative), relative.replace('\\', "/"));
        }
        for relative in remove_unapproved(&root, &next, cancellation)? {
            removed.entry(path_key(&relative)).or_insert(relative);
        }
    }
    cancelled(cancellation)?;
    changed |= next != previous || !removed.is_empty();
    state.extra.insert("resourceHashes".into(), json!(next));
    if changed {
        invalidate(state);
    }
    Ok(
        json!({"staged":files,"directory":"resources","removed":removed.into_values().collect::<Vec<_>>(),"replace":replace,"changed":changed}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(root: &Path) -> (AdaptationRequest, PathBuf, Session) {
        let game = root.join("game");
        let workspace = root.join("project/adaptation/current");
        for directory in [
            &game,
            &workspace.join("resources"),
            &workspace.join("src"),
            &workspace.join("work"),
        ] {
            fs::create_dir_all(directory).expect("fixture directory");
        }
        let request = AdaptationRequest {
            game_id: "fixture".into(),
            game_directory: game,
            inputs: AdaptationInputs::default(),
            project_directory: root.join("project"),
            package_directory: root.join("extensions"),
            package_directories: vec![],
            user_instruction: String::new(),
            objective: "Repair".into(),
            improve_existing: true,
            require_model_analysis: false,
            reuse_failures: vec![],
            builtin_probes: vec![],
            discovery_inventory: None,
            limits: AdaptationLimits::default(),
            baseline_units: vec![],
        };
        let mut state = Session {
            verified_package: Some(json!({"id":"fixture"})),
            verified_fingerprint: Some("previous".into()),
            verification: Some(VerificationResult::default()),
            ..Session::default()
        };
        for key in [
            "verificationCache",
            "trialVerificationCache",
            "discoveryVerificationCache",
        ] {
            state.extra.insert(key.into(), json!({"passed":true}));
        }
        (request, workspace, state)
    }

    #[test]
    fn pruning_restored_resources_removes_orphans_and_preserves_other_work() {
        let root = tempfile::tempdir().expect("fixture");
        let (request, workspace, mut state) = fixture(root.path());
        fs::create_dir_all(request.game_directory.join("nested")).expect("game directory");
        fs::create_dir_all(workspace.join("resources/nested")).expect("resource directory");
        fs::write(request.game_directory.join("nested/keep.bin"), b"original").expect("game");
        fs::write(workspace.join("resources/nested/KEEP.bin"), b"original").expect("resource");
        fs::write(workspace.join("resources/old.bin"), b"old patch").expect("stale resource");
        fs::write(workspace.join("resources/orphan.bin"), b"interrupted copy").expect("orphan");
        fs::write(workspace.join("src/adapter.py"), b"source").expect("source");
        fs::write(workspace.join("work/notes.txt"), b"notes").expect("notes");
        state.extra.insert(
            "resourceHashes".into(),
            json!({"nested\\keep.bin":"retained", "old.bin":"missing"}),
        );
        let removed = reconcile(
            &request,
            &workspace,
            &mut state,
            &CancellationToken::new(),
            None,
        )
        .expect("prune");
        assert_eq!(removed.removed, ["old.bin", "orphan.bin"]);
        assert!(workspace.join("resources/nested/KEEP.bin").is_file());
        assert!(!workspace.join("resources/old.bin").exists());
        assert!(!workspace.join("resources/orphan.bin").exists());
        assert_eq!(
            state.extra["resourceHashes"],
            json!({"nested\\keep.bin":"retained"})
        );
        assert_eq!(
            fs::read(workspace.join("src/adapter.py")).expect("source"),
            b"source"
        );
        assert_eq!(
            fs::read(workspace.join("work/notes.txt")).expect("notes"),
            b"notes"
        );
        assert!(
            state.verified_package.is_none()
                && state.verified_fingerprint.is_none()
                && state.verification.is_none()
        );
        assert!(!state.extra.contains_key("trialVerificationCache"));
        assert!(
            reconcile(
                &request,
                &workspace,
                &mut state,
                &CancellationToken::new(),
                None
            )
            .expect("repeat")
            .is_empty()
        );
    }

    #[test]
    fn replacing_staged_selection_shrinks_records_and_copies() {
        let root = tempfile::tempdir().expect("fixture");
        let (request, workspace, mut state) = fixture(root.path());
        for name in ["keep.bin", "extra.bin"] {
            fs::write(request.game_directory.join(name), name).expect("game");
        }
        let token = CancellationToken::new();
        stage(
            &request,
            &workspace,
            &mut state,
            &["keep.bin".into(), "extra.bin".into()],
            false,
            &token,
            None,
        )
        .expect("initial stage");
        let result = stage(
            &request,
            &workspace,
            &mut state,
            &["keep.bin".into()],
            true,
            &token,
            None,
        )
        .expect("replace");
        assert_eq!(result["removed"], json!(["extra.bin"]));
        assert_eq!(
            state.extra["resourceHashes"]
                .as_object()
                .expect("hashes")
                .len(),
            1
        );
        assert!(!workspace.join("resources/extra.bin").exists());
        assert_eq!(
            fs::read(workspace.join("resources/keep.bin")).expect("retained"),
            b"keep.bin"
        );
        assert!(request.game_directory.join("extra.bin").is_file());
    }

    #[test]
    fn read_only_sources_and_old_copies_can_be_rebound_and_removed() {
        let root = tempfile::tempdir().expect("fixture");
        let (mut request, workspace, mut state) = fixture(root.path());
        let original = request.game_directory.join("script.bin");
        let revised = root.path().join("bound-script.bin");
        fs::write(&original, b"original").expect("source");
        fs::write(&revised, b"revised").expect("source");
        let permissions = fs::metadata(&original)
            .expect("source permissions")
            .permissions();
        let mut read_only = permissions.clone();
        read_only.set_readonly(true);
        for path in [&original, &revised] {
            fs::set_permissions(path, read_only.clone()).expect("read-only source");
        }
        let token = CancellationToken::new();
        stage(
            &request,
            &workspace,
            &mut state,
            &["script.bin".into()],
            false,
            &token,
            None,
        )
        .expect("stage read-only source");
        let staged = workspace.join("resources/script.bin");
        assert!(
            !fs::metadata(&staged)
                .expect("copy permissions")
                .permissions()
                .readonly()
        );
        fs::set_permissions(&staged, read_only.clone()).expect("old read-only copy");
        request
            .inputs
            .files
            .insert("script.bin".into(), revised.clone());
        request.inputs.hashes.insert(
            "script.bin".into(),
            resource_hash(&revised, "script.bin").expect("hash"),
        );
        let changes =
            reconcile(&request, &workspace, &mut state, &token, None).expect("rebind copy");
        assert_eq!(changes.rebound, ["script.bin"]);
        assert_eq!(fs::read(&staged).expect("rebound content"), b"revised");
        fs::set_permissions(&staged, read_only).expect("read-only copy");
        stage(&request, &workspace, &mut state, &[], true, &token, None).expect("remove old copy");
        assert!(!staged.exists());
        for path in [&original, &revised] {
            assert!(
                fs::metadata(path)
                    .expect("source permissions")
                    .permissions()
                    .readonly()
            );
            fs::set_permissions(path, permissions.clone()).expect("fixture cleanup");
        }
    }

    #[test]
    fn failed_or_cancelled_stage_does_not_commit_state() {
        let root = tempfile::tempdir().expect("fixture");
        let (request, workspace, mut state) = fixture(root.path());
        fs::write(request.game_directory.join("new.bin"), b"new").expect("game");
        let before = serde_json::to_value(&state).expect("snapshot");
        let error = stage(
            &request,
            &workspace,
            &mut state,
            &["new.bin".into(), "missing.bin".into()],
            true,
            &CancellationToken::new(),
            None,
        )
        .expect_err("missing input");
        assert!(error.to_string().contains("missing.bin"));
        assert_eq!(serde_json::to_value(&state).expect("state"), before);
        let token = CancellationToken::new();
        token.cancel();
        assert!(matches!(
            stage(
                &request,
                &workspace,
                &mut state,
                &["new.bin".into()],
                true,
                &token,
                None
            ),
            Err(EngineError::Cancelled)
        ));
        assert_eq!(serde_json::to_value(&state).expect("state"), before);
        assert_eq!(
            reconcile(
                &request,
                &workspace,
                &mut state,
                &CancellationToken::new(),
                None
            )
            .expect("clean residual copy")
            .removed,
            ["new.bin"]
        );
    }
}
