//! User-selected disposal of finished projects' production materials.
use std::{
    fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::{Application, CancellationToken, CoreError, patching::check_path};

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum CleanupCategory {
    Adaptation,
    SourceResources,
    HistoricalPatches,
    Diagnostics,
}

impl CleanupCategory {
    pub const ALL: [Self; 4] = [
        Self::Adaptation,
        Self::SourceResources,
        Self::HistoricalPatches,
        Self::Diagnostics,
    ];

    pub fn id(self) -> &'static str {
        match self {
            Self::Adaptation => "adaptation",
            Self::SourceResources => "sourceResources",
            Self::HistoricalPatches => "historicalPatches",
            Self::Diagnostics => "diagnostics",
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Self::Adaptation => "适配开发资料",
            Self::SourceResources => "原文工作资源",
            Self::HistoricalPatches => "历史补丁",
            Self::Diagnostics => "过程日志与异常记录",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::Adaptation => {
                "适配会话、源码工作副本、解包资源和构建验收文件；清理后无法续接本次分析或重新总结。"
            }
            Self::SourceResources => {
                "制作补丁使用的原始资源副本；清理后重新生成补丁需要重新识别，已提取文本和译文保留。"
            }
            Self::HistoricalPatches => {
                "清理旧补丁、试译补丁及其构建记录，保留当前补丁和对应的译文快照、验证记录。"
            }
            Self::Diagnostics => {
                "详细运行日志和 API 异常记录；任务结果、Token 汇总及已有经验总结保留。"
            }
        }
    }
}

#[derive(Clone, Debug)]
pub struct CleanupItem {
    pub category: CleanupCategory,
    pub bytes: u64,
}

#[derive(Clone, Debug)]
pub struct CleanupPreview {
    pub items: Vec<CleanupItem>,
    pub retained_patch: Option<PathBuf>,
}

impl Application {
    /// Counts file lengths only, on demand. Never parses or hashes resource contents.
    pub async fn preview_material_cleanup(&self, id: &str) -> Result<CleanupPreview, CoreError> {
        let project = self.open_project(id).await?;
        if self.task_state(id).await.is_some_and(|task| task.running) {
            return Err(CoreError::ProjectBusy);
        }
        let root = self.project_directory(id)?;
        let patch = self
            .latest_patch(id)
            .await?
            .map(|patch| patch.patch_directory);
        tokio::task::spawn_blocking(move || {
            validate_root(&root, &project.game_directory)?;
            let retained_patch = retained_patch(&root, patch.as_deref())?;
            let cancellation = CancellationToken::new();
            let items = CleanupCategory::ALL
                .into_iter()
                .map(|category| {
                    let paths = category_paths(&root, category, retained_patch.as_deref())?;
                    let bytes = process_paths(&paths, false, &cancellation)?;
                    Ok(CleanupItem { category, bytes })
                })
                .collect::<Result<_, CoreError>>()?;
            Ok(CleanupPreview {
                items,
                retained_patch,
            })
        })
        .await?
    }

    /// Keeps extraction, paid translations, edits, installation records and the current patch.
    pub async fn cleanup_materials(
        &self,
        id: &str,
        categories: &[CleanupCategory],
        expected_patch: Option<&Path>,
    ) -> Result<u64, CoreError> {
        if categories.is_empty() {
            return Err(CoreError::InvalidData("请至少选择一类项目文件。".into()));
        }
        let cancellation = self.begin(id, "cleanup", "正在清理所选项目文件。").await?;
        let result = self
            .cleanup_materials_in_task(id, categories, expected_patch, &cancellation)
            .await;
        self.finish(
            id,
            "cleanup",
            result
                .as_ref()
                .map(|_| "所选项目文件已清理，项目、译文和当前补丁已保留。".into())
                .map_err(ToString::to_string),
        )
        .await?;
        result
    }

    async fn cleanup_materials_in_task(
        &self,
        id: &str,
        categories: &[CleanupCategory],
        expected_patch: Option<&Path>,
        cancellation: &CancellationToken,
    ) -> Result<u64, CoreError> {
        let root = self.project_directory(id)?;
        let project = self.store.get(id).await?;
        validate_root(&root, &project.game_directory)?;
        let current = self
            .latest_patch(id)
            .await?
            .map(|patch| patch.patch_directory);
        let retained = retained_patch(&root, current.as_deref())?;
        if categories.contains(&CleanupCategory::HistoricalPatches)
            && retained.as_deref() != expected_patch
        {
            return Err(CoreError::InvalidData(
                "当前补丁已变化，请重新打开清理窗口确认。".into(),
            ));
        }
        let mut bytes = 0;
        for category in CleanupCategory::ALL
            .into_iter()
            .filter(|category| categories.contains(category))
        {
            if cancellation.is_cancelled() {
                return Err(gamel10n_engines::EngineError::Cancelled.into());
            }
            self.progress(id, "cleanup", &format!("正在清理{}。", category.title()))
                .await;
            let paths = category_paths(&root, category, retained.as_deref())?;
            for path in paths {
                // Only log deletion excludes log readers/writers; resource and response deletion must not block other projects' logs.
                let _log_guard = if path == root.join("logs") {
                    Some(self.log_gate.lock().await)
                } else {
                    None
                };
                let cancellation = cancellation.clone();
                bytes += tokio::task::spawn_blocking(move || {
                    process_paths(&[path], true, &cancellation)
                })
                .await??;
            }
        }
        let mut project = self.store.get(id).await?;
        let source_resources_cleared = categories.contains(&CleanupCategory::SourceResources)
            || project
                .extra
                .get("materialsCleanup")
                .is_some_and(|value| value["sourceResourcesCleared"] == true);
        project.extra.insert(
            "materialsCleanup".into(),
            json!({
                "completedAt": chrono::Utc::now().to_rfc3339(),
                "categories": categories,
                "sourceResourcesCleared": source_resources_cleared,
                "releasedBytes": bytes,
            }),
        );
        self.store.save(&project).await?;
        let _ = self.events.send(crate::AppEvent::ProjectsChanged);
        Ok(bytes)
    }
}

fn validate_root(root: &Path, game: &Path) -> Result<(), CoreError> {
    check_path(root)?;
    if crate::projects::paths_overlap(root, game)
        || fs::canonicalize(game)
            .ok()
            .is_some_and(|game| crate::projects::paths_overlap(root, &game))
    {
        return Err(CoreError::InvalidData(
            "项目目录与游戏目录重叠，不能清理项目文件。".into(),
        ));
    }
    Ok(())
}

fn retained_patch(root: &Path, patch: Option<&Path>) -> Result<Option<PathBuf>, CoreError> {
    let Some(patch) = patch else {
        return Ok(None);
    };
    check_path(patch)?;
    if patch.parent() != Some(root.join("patches").as_path()) {
        return Err(CoreError::InvalidData(
            "当前补丁不在项目的补丁目录中，未清理任何补丁。".into(),
        ));
    }
    Ok(patch.is_dir().then(|| patch.to_path_buf()))
}

fn category_paths(
    root: &Path,
    category: CleanupCategory,
    retained: Option<&Path>,
) -> Result<Vec<PathBuf>, CoreError> {
    let names: &[&str] = match category {
        CleanupCategory::Adaptation => &["adaptation", "validation"],
        CleanupCategory::SourceResources => &["sources"],
        CleanupCategory::Diagnostics => &["logs", "responses"],
        CleanupCategory::HistoricalPatches => {
            // Without an identified final result, there is no basis for deleting old patches.
            let Some(retained) = retained else {
                return Ok(Vec::new());
            };
            let directory = root.join("patches");
            check_path(&directory)?;
            let name = retained.file_name().unwrap_or_default().to_string_lossy();
            let companion_prefix = format!("{name}.").to_lowercase();
            let mut paths = Vec::new();
            for entry in fs::read_dir(directory)? {
                let entry = entry?;
                let entry_name = entry.file_name();
                let entry_name = entry_name.to_string_lossy();
                if !entry_name.eq_ignore_ascii_case(&name)
                    && !entry_name.to_lowercase().starts_with(&companion_prefix)
                {
                    paths.push(entry.path());
                }
            }
            return Ok(paths);
        }
    };
    Ok(names.iter().map(|name| root.join(name)).collect())
}

fn process_paths(
    paths: &[PathBuf],
    remove: bool,
    cancellation: &CancellationToken,
) -> Result<u64, CoreError> {
    let mut bytes = 0;
    for path in paths {
        check_path(path)?;
        bytes += process_tree(path, remove, cancellation)?;
    }
    Ok(bytes)
}

fn process_tree(
    path: &Path,
    remove: bool,
    cancellation: &CancellationToken,
) -> Result<u64, CoreError> {
    if cancellation.is_cancelled() {
        return Err(gamel10n_engines::EngineError::Cancelled.into());
    }
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(file_error(path, error)),
    };
    let linked = metadata.file_type().is_symlink();
    #[cfg(windows)]
    let linked = {
        use std::os::windows::fs::MetadataExt;
        linked || metadata.file_attributes() & 0x400 != 0
    };
    if linked {
        return Err(CoreError::InvalidData(format!(
            "项目文件包含链接，未访问其目标：{}",
            path.display()
        )));
    }
    let mut bytes = 0;
    if metadata.is_dir() {
        for entry in fs::read_dir(path).map_err(|error| file_error(path, error))? {
            bytes += process_tree(&entry?.path(), remove, cancellation)?;
        }
    } else {
        bytes = metadata.len();
    }
    if remove {
        #[cfg(windows)]
        if metadata.permissions().readonly() {
            let mut permissions = metadata.permissions();
            #[expect(
                clippy::permissions_set_readonly_false,
                reason = "Windows-only removal of the read-only attribute; Unix permissions are not changed."
            )]
            permissions.set_readonly(false);
            fs::set_permissions(path, permissions).map_err(|error| file_error(path, error))?;
        }
        let result = if metadata.is_dir() {
            fs::remove_dir(path)
        } else {
            fs::remove_file(path)
        };
        if let Err(error) = result
            && error.kind() != std::io::ErrorKind::NotFound
        {
            return Err(file_error(path, error));
        }
    }
    Ok(bytes)
}

fn file_error(path: &Path, error: std::io::Error) -> CoreError {
    CoreError::InvalidData(format!("无法处理项目文件 {}：{error}", path.display()))
}

pub(crate) fn require_source_materials(source: &Path) -> Result<(), CoreError> {
    if !source.is_dir() {
        return Err(CoreError::InvalidData("原文工作资源已清理或不存在。重新生成补丁需要重新识别；已保存文本、译文和现有补丁仍可使用。".into()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{StoredAnalysis, ToolPaths, engine_tasks::PatchResult};
    use gamel10n_engines::protocol::ExtractionResult;
    use gamel10n_translation::files::write_json;
    use std::collections::BTreeMap;

    fn write(root: &Path, relative: &str, text: &str) {
        let path = root.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    #[tokio::test]
    async fn selected_cleanup_preserves_final_patch_texts_and_recovery_records() {
        let temporary = tempfile::tempdir().unwrap();
        let game = temporary.path().join("game");
        fs::create_dir(&game).unwrap();
        let data = temporary.path().join("data");
        let app = Application::open(data.clone(), &ToolPaths::new("missing"))
            .await
            .unwrap();
        let project = app.add_project(&game).await.unwrap();
        let root = app.project_directory(&project.id).unwrap();
        for (relative, text) in [
            ("adaptation/current/session.json", "session"),
            ("adaptation/current/work/unpacked.dat", "unpacked"),
            ("validation/old/result.dat", "validation"),
            ("sources/original/script.dat", "original"),
            ("memory/00.json", "translations"),
            ("translation-edits.json", "edits"),
            ("installation.json", "installation"),
            ("engine-review-result.json", "summary"),
            ("adaptation-result.json", "conclusion and usage"),
            ("tasks/translation.json", "cost totals"),
            ("patches/final/game.dat", "final patch"),
            ("patches/final/runtime-test.json", "runtime passed"),
            ("patches/final.result.json", "final result"),
            ("patches/final.texts.json", "final translations"),
            ("patches/old/game.dat", "old patch"),
            ("patches/old.result.json", "old result"),
            ("patches/old.texts.json", "old translations"),
            ("responses/error.json", "error"),
        ] {
            write(&root, relative, text);
        }
        write(&game, "game.dat", "installed game");
        write(&game, "game.dat.bak", "original game");
        write(
            &data,
            "extensions/packages/engine.fixture/v1/src/adapter.py",
            "shared adapter",
        );
        write_json(
            &root.join("latest-result.json"),
            &PatchResult {
                patch_directory: PathBuf::from("patches/final"),
                launch_executable: None,
                total_units: 0,
                translated_units: 0,
                is_trial: false,
                validation: json!({}),
                warnings: vec![],
                source_fingerprint: String::new(),
                extra: BTreeMap::new(),
            },
        )
        .unwrap();
        app.commit_analysis(
            &project.id,
            StoredAnalysis {
                schema_version: 1,
                format: "fixture".into(),
                source_directory: root.join("sources/original"),
                resource_files: vec!["script.dat".into()],
                source_hashes: BTreeMap::new(),
                extraction: ExtractionResult {
                    units: vec![gamel10n_translation::TranslationUnit {
                        id: "script:1".into(),
                        resource_path: "script.dat".into(),
                        locator: "1".into(),
                        source_text: "Original dialogue".into(),
                        context: String::new(),
                        speaker: None,
                        kind: "dialogue".into(),
                        protected_tokens: None,
                    }],
                    warnings: vec![],
                },
                adapter: None,
                probe: None,
            },
        )
        .await
        .unwrap();
        let preview = app.preview_material_cleanup(&project.id).await.unwrap();
        assert_eq!(
            preview
                .items
                .iter()
                .find(|item| item.category == CleanupCategory::SourceResources)
                .unwrap()
                .bytes,
            8
        );
        assert_eq!(
            preview
                .items
                .iter()
                .find(|item| item.category == CleanupCategory::HistoricalPatches)
                .unwrap()
                .bytes,
            ("old patch".len() + "old result".len() + "old translations".len()) as u64
        );
        app.cleanup_materials(
            &project.id,
            &[CleanupCategory::SourceResources],
            preview.retained_patch.as_deref(),
        )
        .await
        .unwrap();
        assert!(!root.join("sources").exists());
        assert!(root.join("adaptation/current/session.json").is_file());
        assert!(root.join("patches/old/game.dat").is_file());
        drop(app);

        let reopened = Application::open(data.clone(), &ToolPaths::new("missing"))
            .await
            .unwrap();
        assert_eq!(
            reopened
                .cached_analysis(&project.id)
                .await
                .unwrap()
                .unwrap()
                .extraction()
                .units[0]
                .source_text,
            "Original dialogue"
        );
        let preview = reopened
            .preview_material_cleanup(&project.id)
            .await
            .unwrap();
        reopened
            .cleanup_materials(
                &project.id,
                &CleanupCategory::ALL,
                preview.retained_patch.as_deref(),
            )
            .await
            .unwrap();
        for relative in [
            "adaptation",
            "validation",
            "sources",
            "patches/old",
            "patches/old.result.json",
            "patches/old.texts.json",
            "responses",
        ] {
            assert!(!root.join(relative).exists(), "must remove {relative}");
        }
        for relative in [
            "extraction/native.json",
            "memory/00.json",
            "translation-edits.json",
            "installation.json",
            "engine-review-result.json",
            "adaptation-result.json",
            "tasks/translation.json",
            "latest-result.json",
            "patches/final/game.dat",
            "patches/final/runtime-test.json",
            "patches/final.result.json",
            "patches/final.texts.json",
        ] {
            assert!(root.join(relative).is_file(), "must keep {relative}");
        }
        assert_eq!(
            fs::read_to_string(game.join("game.dat.bak")).unwrap(),
            "original game"
        );
        assert!(
            data.join("extensions/packages/engine.fixture/v1/src/adapter.py")
                .is_file()
        );
        assert_eq!(
            reopened.open_project(&project.id).await.unwrap().extra["materialsCleanup"]["sourceResourcesCleared"],
            true
        );
    }

    #[tokio::test]
    async fn cleanup_requires_an_idle_project_and_reconfirms_changed_final_patch() {
        let temporary = tempfile::tempdir().unwrap();
        let game = temporary.path().join("game");
        fs::create_dir(&game).unwrap();
        let app = Application::open(temporary.path().join("data"), &ToolPaths::new("missing"))
            .await
            .unwrap();
        let project = app.add_project(&game).await.unwrap();
        let root = app.project_directory(&project.id).unwrap();
        write(&root, "adaptation/current/work/data", "keep");
        app.begin(&project.id, "translate", "translating")
            .await
            .unwrap();
        assert!(matches!(
            app.cleanup_materials(&project.id, &CleanupCategory::ALL, None)
                .await,
            Err(CoreError::ProjectBusy)
        ));
        app.finish(&project.id, "translate", Ok("done".into()))
            .await
            .unwrap();
        let result = app
            .cleanup_materials(
                &project.id,
                &CleanupCategory::ALL,
                Some(&root.join("patches/previous")),
            )
            .await;
        assert!(result.unwrap_err().to_string().contains("当前补丁已变化"));
        assert!(root.join("adaptation/current/work/data").is_file());
    }

    #[test]
    fn unidentified_patches_are_preserved_and_cancelled_cleanup_can_be_retried() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path();
        write(root, "patches/unknown/game.dat", "unidentified patch");
        assert!(
            category_paths(root, CleanupCategory::HistoricalPatches, None)
                .unwrap()
                .is_empty()
        );
        write(root, "sources/remaining/script.dat", "remaining");
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        assert!(process_paths(&[root.join("sources")], true, &cancellation).is_err());
        process_paths(&[root.join("sources")], true, &CancellationToken::new()).unwrap();
        assert_eq!(
            process_paths(&[root.join("sources")], true, &CancellationToken::new()).unwrap(),
            0
        );
        assert!(root.join("patches/unknown/game.dat").is_file());
    }

    #[cfg(windows)]
    #[test]
    fn cleanup_resumes_remaining_files_after_a_sharing_violation() {
        use std::os::windows::fs::OpenOptionsExt;

        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path();
        write(root, "first.dat", "first");
        write(root, "locked.dat", "remaining");
        let paths = [root.join("first.dat"), root.join("locked.dat")];
        let locked = fs::OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(&paths[1])
            .unwrap();
        let cancellation = CancellationToken::new();
        assert!(process_paths(&paths, true, &cancellation).is_err());
        assert!(!paths[0].exists());
        assert_eq!(fs::read_to_string(&paths[1]).unwrap(), "remaining");
        drop(locked);

        assert_eq!(process_paths(&paths, true, &cancellation).unwrap(), 9);
        assert!(!paths[1].exists());
    }
}
