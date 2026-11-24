//! Patch observations and retirement of historical versions after user confirmation.
use std::{collections::BTreeMap, path::Path};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    Application, CoreError,
    catalog::{AdapterVersion, EngineCatalog, VersionFeedback},
    patching::PatchInstaller,
};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct Retirement {
    previous: AdapterVersion,
    replacement: AdapterVersion,
}

pub struct PatchFeedbackResult {
    pub message: String,
    pub can_repair_current: bool,
}

pub(crate) fn current_adapter(extra: &BTreeMap<String, Value>) -> Option<AdapterVersion> {
    let analysis = extra.get("analysis");
    Some(AdapterVersion {
        adapter_id: analysis
            .and_then(|value| value["adapterId"].as_str())
            .or_else(|| extra.get("adapterId")?.as_str())?
            .into(),
        version: analysis
            .and_then(|value| value["adapterVersion"].as_str())
            .or_else(|| extra.get("adapterVersion")?.as_str())?
            .into(),
    })
}

fn saved<T: serde::de::DeserializeOwned>(
    extra: &BTreeMap<String, Value>,
    key: &str,
) -> Result<Option<T>, CoreError> {
    extra
        .get(key)
        .filter(|value| !value.is_null())
        .map(|value| {
            serde_json::from_value(value.clone())
                .map_err(|error| CoreError::InvalidData(format!("版本关系记录无效：{error}")))
        })
        .transpose()
}

impl Application {
    pub async fn record_patch_feedback(
        &self,
        id: &str,
        expected_patch: &Path,
        status: &str,
        notes: &str,
    ) -> Result<PatchFeedbackResult, CoreError> {
        let _project = self.lock_project(id).await?;
        let _lifecycle = self.project_lifecycle.lock().await;
        if self.task_state(id).await.is_some_and(|state| state.running) {
            return Err(CoreError::ProjectBusy);
        }
        self.ensure_writable().await?;
        if !matches!(status, "passed" | "failed") {
            return Err(CoreError::InvalidData("请选择效果正常或存在问题。".into()));
        }
        if status == "failed" && notes.trim().is_empty() {
            return Err(CoreError::InvalidData(
                "请先填写遇到的问题，例如乱码、字体重叠或遗漏文字。".into(),
            ));
        }
        let patch = self
            .latest_patch(id)
            .await?
            .ok_or_else(|| CoreError::InvalidData("尚未生成补丁。".into()))?;
        if patch.patch_directory != expected_patch {
            return Err(CoreError::InvalidData(
                "当前补丁已变化，请刷新后确认实际试用的补丁。".into(),
            ));
        }
        let mut project = self.store.get(id).await?;
        let adapter: Option<AdapterVersion> = saved(&patch.extra, "adapterVersionRef")?;
        let directory = self.project_directory(id)?;
        PatchInstaller::new(directory).record_runtime_test(
            &patch.patch_directory,
            status,
            notes,
        )?;
        let Some(adapter) = adapter else {
            return Ok(PatchFeedbackResult {
                message: "已保存补丁运行反馈。这份补丁未记录适配器版本；重新生成一次补丁后可关联引擎验证和自动替换。".into(),
                can_repair_current: false,
            });
        };
        let is_current = current_adapter(&project.extra).as_ref() == Some(&adapter);
        let catalog = EngineCatalog::open(self.data_directory())?;
        let registered = catalog.record_version_feedback(
            &adapter,
            id,
            VersionFeedback {
                status: status.into(),
                notes: notes.trim().into(),
                patch_id: patch
                    .patch_directory
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into(),
                updated_at: chrono::Utc::now().to_rfc3339(),
            },
        )?;
        if is_current {
            if status == "failed" {
                project.extra.insert("adapterRepair".into(), json!(adapter));
                project
                    .extra
                    .insert("adaptationObjective".into(), json!("Repair"));
                project
                    .extra
                    .insert("adapterRepairNotes".into(), json!(notes.trim()));
            } else if saved::<AdapterVersion>(&project.extra, "adapterRepair")?.as_ref()
                == Some(&adapter)
            {
                project.extra.remove("adapterRepair");
                project.extra.remove("adapterRepairNotes");
            }
            if registered
                && status == "passed"
                && let Some(lifecycle) = catalog.version_lifecycle(&adapter)?
                && let Some(predecessors) = lifecycle.replaces.get(id)
                && !predecessors.is_empty()
            {
                let mut retirements: Vec<Retirement> =
                    saved(&project.extra, "adapterRetirements")?.unwrap_or_default();
                for previous in predecessors {
                    let retirement = Retirement {
                        previous: previous.clone(),
                        replacement: adapter.clone(),
                    };
                    if !retirements.contains(&retirement) {
                        retirements.push(retirement);
                    }
                }
                project
                    .extra
                    .insert("adapterRetirements".into(), json!(retirements));
            }
            self.store.save(&project).await?;
        }
        let message = if !registered {
            "已保存运行反馈；此版本不在全局引擎库中，未重新创建记录。"
        } else if !is_current {
            "已记录这份补丁对应旧版本的运行反馈。项目已切换版本，请生成并试用当前版本的补丁。"
        } else if status == "passed" {
            if !project.extra.contains_key("adapterRetirements") {
                "已记录当前游戏效果正常，此版本可供其他项目匹配复用。"
            } else if self.has_running_tasks().await {
                "已记录当前游戏效果正常。其他任务结束后，会自动清理本游戏已替换的版本，保留其他游戏使用的版本。"
            } else {
                "已记录当前游戏效果正常。将自动清理本游戏已替换的版本，保留其他游戏使用的版本。"
            }
        } else {
            "已记录问题。修复并确认新版效果正常后，会自动清理未被其他项目使用的历史版本。"
        }
        .to_string();
        self.log(id, &message).await?;
        drop(_lifecycle);
        drop(_project);
        if registered
            && is_current
            && status == "passed"
            && let Err(error) = self.clean_replaced_adapters().await
        {
            self.log_task_message(
                id,
                &format!("效果正常的反馈已保存，历史版本暂未清理，将在任务结束后重试：{error}"),
            )
            .await;
        }
        let _ = self.events.send(crate::AppEvent::ProjectsChanged);
        let _ = self.events.send(crate::AppEvent::EngineLibraryChanged);
        Ok(PatchFeedbackResult {
            message,
            can_repair_current: is_current,
        })
    }

    pub(crate) async fn repair_predecessor(
        &self,
        id: &str,
    ) -> Result<Option<AdapterVersion>, CoreError> {
        let project = self.store.get(id).await?;
        let previous: Option<AdapterVersion> = saved(&project.extra, "adapterRepair")?;
        let catalog = EngineCatalog::open(self.data_directory())?;
        let Some(previous) = previous else {
            return Ok(None);
        };
        Ok(catalog
            .version_lifecycle(&previous)?
            .is_some_and(|lifecycle| lifecycle.failed_for(id))
            .then_some(previous))
    }

    pub(crate) async fn remember_adapter_replacement(
        &self,
        id: &str,
        previous: &AdapterVersion,
    ) -> Result<(), CoreError> {
        let _project = self.lock_project(id).await?;
        let mut project = self.store.get(id).await?;
        let Some(replacement) =
            current_adapter(&project.extra).filter(|current| current != previous)
        else {
            return Ok(());
        };
        let mut retirements: Vec<Retirement> =
            saved(&project.extra, "adapterRetirements")?.unwrap_or_default();
        let retirement = Retirement {
            previous: previous.clone(),
            replacement,
        };
        if !retirements.contains(&retirement) {
            retirements.push(retirement);
        }
        project
            .extra
            .insert("adapterRetirements".into(), json!(retirements));
        project.extra.remove("adapterRepair");
        project.extra.remove("adapterRepairNotes");
        self.store.save(&project).await
    }

    /// Retries confirmed retirements after a task finishes or the desktop reopens the library.
    pub async fn clean_replaced_adapters(&self) -> Result<(), CoreError> {
        for id in self.store.pending_adapter_retirements().await {
            // Refreshing a project after a task finishes races this cleanup. Wait for its
            // short project lock before taking the lifecycle lock, just as task startup does.
            let _project = self.lock_project(&id).await?;
            let _lifecycle = self.project_lifecycle.lock().await;
            if self.has_running_tasks().await {
                return Ok(());
            }
            self.ensure_writable().await?;
            let projects = self.store.list("").await;
            let Some(summary) = projects.iter().find(|p| p.id == id) else {
                continue;
            };
            let catalog = EngineCatalog::open(self.data_directory())?;
            let mut retirements =
                saved::<Vec<Retirement>>(&summary.extra, "adapterRetirements")?.unwrap_or_default();
            // Recover the small commit window between saving the extracted version
            // and saving its retirement. Registry lineage proves publication succeeded.
            let mut recovered = false;
            if let Some(previous) = saved::<AdapterVersion>(&summary.extra, "adapterRepair")?
                && let Some(replacement) =
                    current_adapter(&summary.extra).filter(|current| *current != previous)
                && catalog
                    .version_lifecycle(&replacement)?
                    .is_some_and(|lifecycle| lifecycle.replaces_for(&summary.id, &previous))
            {
                let retirement = Retirement {
                    previous,
                    replacement,
                };
                if !retirements.contains(&retirement) {
                    retirements.push(retirement);
                }
                recovered = true;
            }
            if retirements.is_empty() {
                continue;
            }
            let mut remaining = Vec::new();
            for mut retirement in retirements {
                if catalog.version_lifecycle(&retirement.previous)?.is_none() {
                    continue;
                }
                // Deferred cleanup must survive consecutive repairs, including
                // deletion of an intermediate replacement.
                if let Some(current) = current_adapter(&summary.extra)
                    && catalog
                        .version_lifecycle(&current)?
                        .is_some_and(|lifecycle| {
                            lifecycle.replaces_for(&summary.id, &retirement.previous)
                        })
                {
                    retirement.replacement = current;
                }
                let replacement = catalog.version_lifecycle(&retirement.replacement)?;
                if !replacement.is_some_and(|lifecycle| {
                    lifecycle.passed_for(&summary.id)
                        && lifecycle.replaces_for(&summary.id, &retirement.previous)
                }) {
                    remaining.push(retirement);
                    continue;
                }
                let in_use = projects.iter().any(|project| {
                    current_adapter(&project.extra).as_ref() == Some(&retirement.previous)
                });
                let game_id = summary.id.clone();
                let retired = retirement.previous.clone();
                let library = catalog.clone();
                match tokio::task::spawn_blocking(move || {
                    library.retire_project_version(&retired, &game_id, in_use)
                })
                .await?
                {
                    Ok(None) => continue,
                    Ok(Some(result)) if result.deleted => {
                        let _ = self.events.send(crate::AppEvent::EngineLibraryChanged);
                        self.log(
                            &summary.id,
                            &format!(
                                "已清理历史引擎版本 {} · {}。{}",
                                retirement.previous.adapter_id,
                                retirement.previous.version,
                                if result.diagnostics.is_empty() {
                                    String::new()
                                } else {
                                    format!("\n{}", result.diagnostics.join("\n"))
                                }
                            ),
                        )
                        .await?;
                    }
                    Ok(_) => remaining.push(retirement),
                    Err(error) => {
                        self.log(
                            &summary.id,
                            &format!("历史版本清理未完成，将在任务结束后重试：{error}"),
                        )
                        .await?;
                        remaining.push(retirement);
                    }
                }
            }
            if !recovered && summary.extra.get("adapterRetirements") == Some(&json!(remaining)) {
                continue;
            }
            let mut project = self.store.get(&summary.id).await?;
            if recovered {
                project.extra.remove("adapterRepair");
                project.extra.remove("adapterRepairNotes");
            }
            if remaining.is_empty() {
                project.extra.remove("adapterRetirements");
            } else {
                project
                    .extra
                    .insert("adapterRetirements".into(), json!(remaining));
            }
            self.store.save(&project).await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ToolPaths, catalog::GameCompatibility};
    use gamel10n_translation::files::{hash_text, write_json};
    use std::{fs, path::PathBuf};

    async fn fixture() -> (tempfile::TempDir, Application, String) {
        let temp = tempfile::tempdir().unwrap();
        let game = temp.path().join("game");
        fs::create_dir(&game).unwrap();
        let app = Application::open(temp.path().join("data"), &ToolPaths::new("missing"))
            .await
            .unwrap();
        let id = app.add_project(&game).await.unwrap().id;
        (temp, app, id)
    }

    fn adapter(version: &str) -> AdapterVersion {
        AdapterVersion {
            adapter_id: "engine.fixture".into(),
            version: version.into(),
        }
    }

    fn register(
        app: &Application,
        id: &str,
        version: &str,
        previous: Option<&AdapterVersion>,
    ) -> PathBuf {
        let package = app
            .data_directory()
            .join("extensions/packages/engine.fixture")
            .join(version);
        write_json(
            &package.join("package.json"),
            &json!({
                "id":"engine.fixture", "version":version,
                "family":{"kind":"engine","key":"fixture","name":"Fixture"}
            }),
        )
        .unwrap();
        EngineCatalog::open(app.data_directory())
            .unwrap()
            .register_trial_package(
                &package,
                GameCompatibility {
                    game_id: id.into(),
                    title: "Fixture".into(),
                    variant: "".into(),
                    adapter_version: version.into(),
                    structure_status: "passed".into(),
                    coverage_confirmed: true,
                    runtime_status: "not-tested".into(),
                    updated_at: None,
                },
                previous,
            )
            .unwrap();
        package
    }

    async fn pin(app: &Application, id: &str, version: &str) {
        let mut project = app.store.get(id).await.unwrap();
        project.extra.insert(
            "analysis".into(),
            json!({"adapterId":"engine.fixture","adapterVersion":version}),
        );
        app.store.save(&project).await.unwrap();
    }

    fn patch(app: &Application, id: &str, version: &str) -> PathBuf {
        let root = app.project_directory(id).unwrap();
        let patch = root.join("patches").join(version);
        fs::create_dir_all(&patch).unwrap();
        write_json(
            &root.join("latest-result.json"),
            &json!({"patchDirectory":patch,"adapterVersionRef":adapter(version)}),
        )
        .unwrap();
        patch
    }

    #[tokio::test]
    async fn trial_feedback_survives_reindex_and_blocks_only_the_affected_game() {
        let (_temp, app, id) = fixture().await;
        let package = register(&app, &id, "v1", None);
        pin(&app, &id, "v1").await;
        let patch = patch(&app, &id, "v1");
        let catalog = EngineCatalog::open(app.data_directory()).unwrap();
        assert!(catalog.discovery_candidates(None).unwrap().is_empty());
        assert_eq!(
            catalog
                .discovery_candidates(Some(("engine.fixture", "v1")))
                .unwrap()
                .len(),
            1
        );
        let pin_file = app
            .data_directory()
            .join("extensions/games")
            .join(hash_text(&id))
            .join("active.json");
        write_json(&pin_file, &json!({"id":"engine.fixture","version":"v1"})).unwrap();
        catalog.forget_game(&id).unwrap();
        assert!(
            catalog.discovery_candidates(None).unwrap().is_empty(),
            "removing a project must not promote its trial version"
        );
        app.record_patch_feedback(&id, &patch, "passed", "显示正常")
            .await
            .unwrap();
        fs::remove_file(app.data_directory().join("extensions/library-index.json")).unwrap();
        let catalog = EngineCatalog::open(app.data_directory()).unwrap();
        assert_eq!(catalog.candidate_package_directories().unwrap(), [package]);
        catalog
            .record_version_feedback(
                &adapter("v1"),
                "another-game",
                VersionFeedback {
                    status: "failed".into(),
                    notes: "layout".into(),
                    patch_id: "p1".into(),
                    updated_at: "now".into(),
                },
            )
            .unwrap();
        assert_eq!(
            catalog
                .discovery_candidates_for_game(&id, None)
                .unwrap()
                .len(),
            1
        );
        assert!(
            catalog
                .discovery_candidates_for_game("another-game", Some(("engine.fixture", "v1")))
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn feedback_for_an_old_patch_does_not_mark_the_current_version_for_repair() {
        let (_temp, app, id) = fixture().await;
        register(&app, &id, "v1", None);
        register(&app, &id, "v2", None);
        pin(&app, &id, "v2").await;
        let patch = patch(&app, &id, "v1");
        let result = app
            .record_patch_feedback(&id, &patch, "failed", "文字重叠")
            .await
            .unwrap();
        assert!(!result.can_repair_current);
        assert!(
            !app.store
                .get(&id)
                .await
                .unwrap()
                .extra
                .contains_key("adapterRepair")
        );
        let catalog = EngineCatalog::open(app.data_directory()).unwrap();
        assert!(
            catalog
                .version_lifecycle(&adapter("v1"))
                .unwrap()
                .unwrap()
                .failed_for(&id)
        );
        assert!(
            !catalog
                .version_lifecycle(&adapter("v2"))
                .unwrap()
                .unwrap()
                .failed_for(&id)
        );
    }

    #[tokio::test]
    async fn confirmation_cleans_unreported_history_and_preserves_newer_releases() {
        let (_temp, app, id) = fixture().await;
        let old = register(&app, &id, "v1", None);
        let old_patch = patch(&app, &id, "v1");
        let confirmed = register(&app, &id, "v2", None);
        let newer = register(&app, &id, "v3", None);
        pin(&app, &id, "v2").await;
        let patch = patch(&app, &id, "v2");

        app.record_patch_feedback(&id, &patch, "passed", "显示正常")
            .await
            .unwrap();

        assert!(
            !old.exists(),
            "history does not require a prior failure report"
        );
        assert!(confirmed.is_dir());
        assert!(
            newer.is_dir(),
            "confirmation must not delete newer releases"
        );
        assert!(
            old_patch.is_dir(),
            "generated patches are independent of the library"
        );
        fs::remove_file(app.data_directory().join("extensions/library-index.json")).unwrap();
        let catalog = EngineCatalog::open(app.data_directory()).unwrap();
        assert!(catalog.version_lifecycle(&adapter("v1")).unwrap().is_none());
        assert!(catalog.version_lifecycle(&adapter("v3")).unwrap().is_some());
        app.record_patch_feedback(&id, &patch, "passed", "显示正常")
            .await
            .unwrap();
        assert!(newer.is_dir(), "repeated confirmation must remain safe");
    }

    #[tokio::test]
    async fn confirmation_of_an_old_patch_does_not_clean_current_version_history() {
        let (_temp, app, id) = fixture().await;
        let oldest = register(&app, &id, "v1", None);
        let old = register(&app, &id, "v2", None);
        let current = register(&app, &id, "v3", None);
        pin(&app, &id, "v3").await;
        let patch = patch(&app, &id, "v2");

        app.record_patch_feedback(&id, &patch, "passed", "显示正常")
            .await
            .unwrap();
        app.clean_replaced_adapters().await.unwrap();

        assert!(oldest.is_dir());
        assert!(old.is_dir());
        assert!(current.is_dir());
    }

    #[tokio::test]
    async fn confirmation_preserves_other_games_compatibility_pins_and_runtime_packages() {
        let (_temp, app, id) = fixture().await;
        let shared = register(&app, &id, "shared", None);
        register(&app, "other", "shared", None);
        let pinned = register(&app, &id, "pinned", None);
        let runtime = register(&app, &id, "runtime", None);
        let superseded = register(&app, &id, "superseded", None);
        register(&app, &id, "current", None);
        pin(&app, &id, "current").await;
        let extensions = app.data_directory().join("extensions");
        let pin_file = extensions
            .join("games")
            .join(hash_text("other"))
            .join("active.json");
        let mapping_file = extensions.join("runtime-migrations.json");
        write_json(
            &pin_file,
            &json!({"id":"engine.fixture","version":"pinned"}),
        )
        .unwrap();
        write_json(
            &mapping_file,
            &json!({"schemaVersion":1,"entries":{
                "engine.fixture/legacy":{"target":"engine.fixture/runtime"}
            }}),
        )
        .unwrap();

        app.record_patch_feedback(&id, &patch(&app, &id, "current"), "passed", "")
            .await
            .unwrap();
        assert!(!superseded.exists());
        assert!(shared.is_dir() && pinned.is_dir() && runtime.is_dir());
        assert!(pin_file.is_file());
        let mappings: Value = gamel10n_translation::files::read_json(&mapping_file).unwrap();
        assert_eq!(
            mappings["entries"]["engine.fixture/legacy"]["target"],
            "engine.fixture/runtime"
        );
    }

    #[tokio::test]
    async fn reopening_a_legacy_queue_retires_only_the_confirmed_games_own_releases() {
        let (temp, app, id) = fixture().await;
        let other_game = temp.path().join("other-game");
        fs::create_dir(&other_game).unwrap();
        let other = app.add_project(&other_game).await.unwrap().id;
        let old = register(&app, &id, "v1", None);
        let foreign = register(&app, &other, "other-v1", None);
        let unknown = register(&app, &id, "legacy", None);
        let imported = register(&app, &id, "imported", None);
        let confirmed = register(&app, &id, "v2", None);
        let other_current = register(&app, &other, "other-v2", None);
        pin(&app, &id, "v2").await;
        pin(&app, &other, "other-v2").await;
        let data = app.data_directory().to_owned();
        let state_path = data.join("extensions/library-state.json");
        let read_state = || gamel10n_translation::files::read_json::<Value>(&state_path).unwrap();
        let mut state = read_state();
        let family = &mut state["families"]["engine.fixture"];
        let legacy = family["releases"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|r| r["version"] == "legacy")
            .unwrap();
        legacy.as_object_mut().unwrap().remove("gameId");
        family["versionLifecycle"]["imported"]["requiresConfirmation"] = json!(false);
        write_json(&state_path, &state).unwrap();

        let _task = app.begin(&other, "translate", "running").await.unwrap();
        let patch = patch(&app, &id, "v2");
        app.record_patch_feedback(&id, &patch, "passed", "")
            .await
            .unwrap();
        let mut state = read_state();
        let predecessors =
            &mut state["families"]["engine.fixture"]["versionLifecycle"]["v2"]["replaces"][&id];
        assert_eq!(*predecessors, json!([adapter("v1")]));

        // Older registries inferred all earlier family releases, even without a game ID.
        let history = [
            adapter("v1"),
            adapter("other-v1"),
            adapter("legacy"),
            adapter("imported"),
        ];
        *predecessors = json!(history);
        write_json(&state_path, &state).unwrap();
        let mut project = app.store.get(&id).await.unwrap();
        project.extra.insert(
            "adapterRetirements".into(),
            json!(history.map(|previous| Retirement {
                previous,
                replacement: adapter("v2")
            })),
        );
        app.store.save(&project).await.unwrap();
        let reopened = Application::open(data.clone(), &ToolPaths::new("missing"))
            .await
            .unwrap();
        assert!(reopened.clean_replaced_adapters().await.is_err());
        assert!(old.is_dir());
        std::mem::forget(_task); // The previous writer exited with an active task.
        drop(app);
        reopened.clean_replaced_adapters().await.unwrap();
        assert!(!old.exists());
        for retained in [foreign, unknown, imported, confirmed, other_current, patch] {
            assert!(retained.is_dir(), "{}", retained.display());
        }
        let library = EngineCatalog::open(&data).unwrap();
        assert!(library.version_lifecycle(&adapter("v1")).unwrap().is_none());
        for version in ["other-v1", "legacy", "imported", "v2", "other-v2"] {
            assert!(
                library
                    .version_lifecycle(&adapter(version))
                    .unwrap()
                    .is_some()
            );
        }
        assert!(
            !reopened
                .store
                .get(&id)
                .await
                .unwrap()
                .extra
                .contains_key("adapterRetirements")
        );
    }

    #[tokio::test]
    async fn replacement_waits_for_success_current_references_and_running_tasks() {
        let (temp, app, id) = fixture().await;
        let old = register(&app, &id, "v1", None);
        pin(&app, &id, "v1").await;
        let old_patch = patch(&app, &id, "v1");
        app.record_patch_feedback(&id, &old_patch, "failed", "乱码")
            .await
            .unwrap();
        app.clean_replaced_adapters().await.unwrap();
        assert!(
            old.is_dir(),
            "a failed repair must retain the original version"
        );
        let other_game = temp.path().join("other-game");
        fs::create_dir(&other_game).unwrap();
        let other = app.add_project(&other_game).await.unwrap().id;
        pin(&app, &other, "v1").await;
        let replacement = register(&app, &id, "v2", Some(&adapter("v1")));
        pin(&app, &id, "v2").await;
        // Simulate interruption after publication, before saving retirement metadata.
        app.clean_replaced_adapters().await.unwrap();
        assert!(
            old.is_dir(),
            "a different project's current reference protects the version"
        );
        assert!(
            app.store
                .get(&id)
                .await
                .unwrap()
                .extra
                .contains_key("adapterRetirements")
        );
        pin(&app, &other, "v2").await;
        app.clean_replaced_adapters().await.unwrap();
        assert!(old.is_dir(), "publication alone is not user confirmation");
        let _task = app.begin(&other, "translate", "running").await.unwrap();
        let confirmed_patch = patch(&app, &id, "v2");
        app.record_patch_feedback(&id, &confirmed_patch, "passed", "显示正常")
            .await
            .unwrap();
        app.clean_replaced_adapters().await.unwrap();
        assert!(old.is_dir(), "cleanup must wait for running tasks");
        let third_game = temp.path().join("third-game");
        fs::create_dir(&third_game).unwrap();
        let third = app.add_project(&third_game).await.unwrap().id;
        let _unrelated_reader = app.lock_project(&third).await.unwrap();
        let project_reader = app.lock_project(&id).await.unwrap();
        let mut events = app.subscribe();
        let (finished, ()) = tokio::join!(
            app.finish(&other, "translate", Ok("done".into())),
            async {
                while let Ok(event) = events.recv().await {
                    if matches!(event, crate::AppEvent::TaskChanged(s) if !s.running && s.project_id == other)
                    {
                        break;
                    }
                }
                drop(project_reader);
            }
        );
        finished.unwrap();
        assert!(!old.exists());
        assert!(replacement.is_dir());
        assert!(
            old_patch.is_dir(),
            "previously generated patches remain usable"
        );
        assert!(
            !app.store
                .get(&id)
                .await
                .unwrap()
                .extra
                .contains_key("adapterRetirements")
        );
    }

    #[tokio::test]
    async fn deferred_replacement_survives_deletion_of_intermediate_repair() {
        let (temp, app, id) = fixture().await;
        let old = register(&app, &id, "v1", None);
        pin(&app, &id, "v1").await;
        app.record_patch_feedback(&id, &patch(&app, &id, "v1"), "failed", "乱码")
            .await
            .unwrap();
        let other_game = temp.path().join("other-game");
        fs::create_dir(&other_game).unwrap();
        let other = app.add_project(&other_game).await.unwrap().id;
        pin(&app, &other, "v1").await;
        let middle = register(&app, &id, "v2", Some(&adapter("v1")));
        pin(&app, &id, "v2").await;
        app.remember_adapter_replacement(&id, &adapter("v1"))
            .await
            .unwrap();
        app.record_patch_feedback(&id, &patch(&app, &id, "v2"), "failed", "字体重叠")
            .await
            .unwrap();
        let latest = register(&app, &id, "v3", Some(&adapter("v2")));
        pin(&app, &id, "v3").await;
        app.remember_adapter_replacement(&id, &adapter("v2"))
            .await
            .unwrap();
        app.clean_replaced_adapters().await.unwrap();
        assert!(
            middle.is_dir(),
            "keep intermediate repairs until confirmation"
        );
        app.record_patch_feedback(&id, &patch(&app, &id, "v3"), "passed", "显示正常")
            .await
            .unwrap();
        assert!(old.is_dir());
        assert!(!middle.exists());
        pin(&app, &other, "v3").await;
        app.clean_replaced_adapters().await.unwrap();
        assert!(
            !old.exists(),
            "removing v2 must not orphan the deferred cleanup of v1"
        );
        assert!(latest.is_dir());
    }
}
