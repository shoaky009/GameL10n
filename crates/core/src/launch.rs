//! Game launch configuration and selection from adapter and installation metadata.

use crate::{
    Application, CoreError, GameProject,
    patching::{InstallationJournal, PatchInstaller},
};
use gamel10n_engines::{
    discovery::Inventory,
    protocol::{LaunchConfiguration, ProbeResult},
};
use gamel10n_translation::files::hash_text;
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default)]
struct LaunchPreferences {
    detected: Option<LaunchConfiguration>,
    manual: Option<LaunchConfiguration>,
    patch_override: Option<PatchOverride>,
    candidates: Vec<String>,
    reason: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct PatchOverride {
    patch: String,
    configuration: LaunchConfiguration,
}

pub struct LaunchView {
    pub revision: String,
    pub configuration: Option<LaunchConfiguration>,
    pub source: String,
    pub reason: String,
    pub candidates: Vec<String>,
    pub can_launch: bool,
}

fn revision(saved: &LaunchPreferences, journal: &InstallationJournal) -> Result<String, CoreError> {
    // Ignore candidate ordering and diagnostics: only configuration changes conflict with edits.
    let configuration = (
        &saved.detected,
        &saved.manual,
        &saved.patch_override,
        journal.extra.get("launch"),
        journal.extra.get("launchPatch"),
    );
    let json = serde_json::to_string(&configuration)
        .map_err(|error| CoreError::InvalidData(error.to_string()))?;
    Ok(hash_text(&json))
}

fn preferences(project: &GameProject) -> Result<LaunchPreferences, CoreError> {
    project
        .extra
        .get("launch")
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .map_err(|error| CoreError::InvalidData(format!("启动配置无效：{error}")))
        .map(Option::unwrap_or_default)
}

fn path_from(root: &Path, value: &str) -> Result<PathBuf, CoreError> {
    if value.chars().any(char::is_control) {
        return Err(CoreError::InvalidData("启动路径含无效字符。".into()));
    }
    Ok(if value.is_empty() || value == "." {
        root.to_owned()
    } else if Path::new(value).is_absolute() {
        PathBuf::from(value)
    } else {
        root.join(value)
    })
}

pub(crate) fn validate_configuration(
    root: &Path,
    configuration: &LaunchConfiguration,
) -> Result<(PathBuf, PathBuf), CoreError> {
    if configuration.executable.trim().is_empty() {
        return Err(CoreError::InvalidData("请先选择游戏启动程序。".into()));
    }
    let executable = path_from(root, &configuration.executable)?;
    let directory = path_from(root, &configuration.working_directory)?;
    if !executable.is_file()
        || !configuration
            .executable
            .to_ascii_lowercase()
            .ends_with(".exe")
    {
        return Err(CoreError::InvalidData(format!(
            "启动程序不存在或不是 EXE：{}",
            executable.display()
        )));
    }
    if !directory.is_dir() {
        return Err(CoreError::InvalidData(format!(
            "启动工作目录不存在：{}",
            directory.display()
        )));
    }
    if configuration.arguments.len() > 30000
        || configuration
            .arguments
            .chars()
            .any(|c| c.is_control() && c != '\t')
    {
        return Err(CoreError::InvalidData("启动参数过长或含无效字符。".into()));
    }
    Ok((executable, directory))
}

pub(crate) fn discover(
    project: &mut GameProject,
    probe: Option<&ProbeResult>,
) -> Result<(), CoreError> {
    let mut saved = preferences(project)?;
    let declared = probe.and_then(|probe| {
        probe.launch.clone().or_else(|| {
            probe
                .original_executable
                .as_ref()
                .map(|executable| LaunchConfiguration {
                    executable: executable.clone(),
                    arguments: String::new(),
                    working_directory: ".".into(),
                })
        })
    });
    if let Some(configuration) = declared {
        configuration.validate_relative()?;
        saved.candidates = vec![configuration.executable.clone()];
        let valid = crate::patching::safe_path(&project.game_directory, &configuration.executable)
            .and_then(|_| {
                if configuration.working_directory != "." {
                    crate::patching::safe_path(
                        &project.game_directory,
                        &configuration.working_directory,
                    )?;
                }
                validate_configuration(&project.game_directory, &configuration)
            });
        match valid {
            Ok(_) => {
                saved.detected = Some(configuration);
                saved.reason = "适配器已声明游戏启动入口。".into();
            }
            Err(error) => {
                saved.detected = None;
                saved.reason = error.to_string();
            }
        }
    } else {
        let inventory = Inventory::scan(&project.game_directory)?;
        let executables: Vec<_> = inventory
            .files
            .iter()
            .filter(|file| file.path.to_ascii_lowercase().ends_with(".exe"))
            .map(|file| file.path.clone())
            .collect();
        saved.candidates = executables.iter().take(32).cloned().collect();
        // Resource pairs establish an entry independently of executable order or file size.
        let paired: Vec<_> = executables
            .iter()
            .filter(|executable| {
                if auxiliary(executable) {
                    return false;
                }
                let path = project.game_directory.join(executable);
                let stem = path.file_stem().unwrap_or_default().to_string_lossy();
                let data = Path::new(executable)
                    .with_file_name(format!("{stem}_Data"))
                    .to_string_lossy()
                    .replace('\\', "/")
                    .to_ascii_lowercase();
                let pck = Path::new(executable)
                    .with_extension("pck")
                    .to_string_lossy()
                    .replace('\\', "/")
                    .to_ascii_lowercase();
                let has_data = path.with_file_name(format!("{stem}_Data")).is_dir();
                let has_pck = path.with_extension("pck").is_file();
                probe.is_some_and(|probe| {
                    probe.required_files.iter().any(|resource| {
                        let resource = resource.replace('\\', "/").to_ascii_lowercase();
                        (has_data && resource.starts_with(&format!("{data}/")))
                            || (has_pck && resource == pck)
                    })
                })
            })
            .cloned()
            .collect();
        let possible: Vec<_> = executables
            .iter()
            .filter(|path| !auxiliary(path))
            .cloned()
            .collect();
        let selected = if inventory.complete && paired.len() == 1 {
            paired.first()
        } else if inventory.complete && paired.is_empty() && possible.len() == 1 {
            possible.first()
        } else {
            None
        };
        saved.detected = selected.map(|executable| LaunchConfiguration {
            executable: executable.clone(),
            arguments: String::new(),
            working_directory: ".".into(),
        });
        saved.reason = if saved.detected.is_some() {
            if paired.len() == 1 {
                "已根据配套游戏资源确定启动入口。"
            } else {
                "已找到唯一的游戏启动候选。"
            }
        } else if !inventory.complete {
            "目录检查不完整，请手动选择启动程序。"
        } else if possible.is_empty() {
            "未找到明确的游戏 EXE，请手动选择游戏或启动器。"
        } else {
            "存在多个启动候选，无法明确区分游戏入口，请手动选择。"
        }
        .into();
    }
    if probe.and_then(|probe| probe.launch.as_ref()).is_none()
        && saved.detected.as_ref().is_some_and(|configuration| {
            project
                .game_directory
                .join(format!("{}.le.config", configuration.executable))
                .is_file()
        })
    {
        saved.detected = None;
        saved.reason = "发现此 EXE 的转区配置，请手动选择所需启动器并填写启动参数。".into();
    }
    project.extra.insert(
        "launch".into(),
        serde_json::to_value(saved).map_err(|e| CoreError::InvalidData(e.to_string()))?,
    );
    Ok(())
}

fn auxiliary(path: &str) -> bool {
    let path = path.to_ascii_lowercase();
    let name = path.rsplit('/').next().unwrap_or(&path);
    [
        "unitycrashhandler",
        "unins",
        "crashreport",
        "dxsetup",
        "vcredist",
        "vc_redist",
    ]
    .iter()
    .any(|prefix| name.starts_with(prefix))
        || [
            "setup.exe",
            "uninstall.exe",
            "config.exe",
            "configuration.exe",
            "update.exe",
            "updater.exe",
            "python.exe",
            "pythonw.exe",
        ]
        .contains(&name)
        || path
            .split('/')
            .any(|part| ["_commonredist", "redist", "directx"].contains(&part))
}

impl Application {
    pub async fn launch_view(&self, id: &str) -> Result<LaunchView, CoreError> {
        let project = self.store.get(id).await?;
        let saved = preferences(&project)?;
        let journal = PatchInstaller::new(self.project_directory(id)?)
            .read_journal(&project.game_directory)?;
        let revision = revision(&saved, &journal)?;
        let installed: Option<LaunchConfiguration> = journal
            .extra
            .get("launch")
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .map_err(|e| CoreError::InvalidData(format!("补丁启动配置无效：{e}")))?;
        let identity = journal
            .extra
            .get("launchPatch")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        let manual_patch = saved
            .patch_override
            .as_ref()
            .filter(|saved| saved.patch == identity && installed.is_some());
        let (configuration, source) = if let Some(saved) = manual_patch {
            (Some(saved.configuration.clone()), "手动设置（当前补丁）")
        } else if let Some(installed) = installed {
            (Some(installed), "已安装补丁")
        } else if let Some(manual) = saved.manual {
            (Some(manual), "手动设置")
        } else {
            (saved.detected, "识别结果")
        };
        let pending = journal.extra.get("launchPending") == Some(&serde_json::Value::Bool(true));
        let pending = pending
            || journal
                .entries
                .iter()
                .any(|entry| entry.pending_sha256.is_some() || entry.restoring);
        let reason = if pending {
            "上次补丁安装或恢复未完成，请先完成安装或恢复。".into()
        } else if let Some(configuration) = &configuration {
            validate_configuration(&project.game_directory, configuration)
                .err()
                .map(|e| e.to_string())
                .unwrap_or_default()
        } else if saved.reason.is_empty() {
            "尚未确定启动入口，请检测入口或手动选择。".into()
        } else {
            saved.reason
        };
        let can_launch = configuration.is_some() && reason.is_empty();
        let source = if configuration.is_some() {
            source
        } else {
            "未设置"
        };
        Ok(LaunchView {
            revision,
            configuration,
            source: source.into(),
            reason,
            candidates: saved.candidates,
            can_launch,
        })
    }

    pub async fn detect_launch(&self, id: &str) -> Result<(), CoreError> {
        let _guard = self.lock_project(id).await?;
        let _lifecycle = self.project_lifecycle.lock().await;
        if self.task_state(id).await.is_some_and(|state| {
            state.running
                && (matches!(state.operation.as_str(), "install" | "restore")
                    || state.workflow.as_ref().is_some_and(|workflow| {
                        matches!(
                            workflow.action,
                            crate::workflow::WorkflowAction::InstallPatch
                        )
                    }))
        }) {
            return Err(CoreError::InvalidData(
                "补丁正在安装或恢复，请在操作结束后检测启动入口。".into(),
            ));
        }
        self.ensure_writable().await?;
        let mut project = self.store.get(id).await?;
        let probe = self.cached_analysis(id).await?.and_then(|analysis| {
            analysis
                .native()
                .ok()
                .and_then(|stored| stored.probe.clone())
        });
        project = tokio::task::spawn_blocking(move || {
            discover(&mut project, probe.as_ref())?;
            Ok::<_, CoreError>(project)
        })
        .await??;
        self.store.save(&project).await?;
        Ok(())
    }

    pub async fn save_launch(
        &self,
        id: &str,
        configuration: Option<LaunchConfiguration>,
        expected_revision: &str,
    ) -> Result<(), CoreError> {
        let _guard = self.lock_project(id).await?;
        let _lifecycle = self.project_lifecycle.lock().await;
        if self.task_state(id).await.is_some_and(|state| {
            state.running
                && (matches!(state.operation.as_str(), "install" | "restore")
                    || state.workflow.as_ref().is_some_and(|workflow| {
                        matches!(
                            workflow.action,
                            crate::workflow::WorkflowAction::InstallPatch
                        )
                    }))
        }) {
            return Err(CoreError::InvalidData(
                "补丁正在安装或恢复，请在操作结束后保存启动设置。".into(),
            ));
        }
        self.ensure_writable().await?;
        let mut project = self.store.get(id).await?;
        let mut saved = preferences(&project)?;
        let journal = PatchInstaller::new(self.project_directory(id)?)
            .read_journal(&project.game_directory)?;
        let revision = revision(&saved, &journal)?;
        if revision != expected_revision {
            return Err(CoreError::LaunchConflict { revision });
        }
        if let Some(configuration) = &configuration {
            validate_configuration(&project.game_directory, configuration)?;
        }
        if journal.extra.contains_key("launch") {
            saved.patch_override = configuration.map(|configuration| PatchOverride {
                patch: journal
                    .extra
                    .get("launchPatch")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .into(),
                configuration,
            });
        } else {
            saved.manual = configuration;
        }
        project.extra.insert(
            "launch".into(),
            serde_json::to_value(saved).map_err(|e| CoreError::InvalidData(e.to_string()))?,
        );
        self.store.save(&project).await?;
        Ok(())
    }

    pub async fn launch_game(&self, id: &str) -> Result<u32, CoreError> {
        let _guard = self.lock_project(id).await?;
        if self.task_state(id).await.is_some_and(|state| state.running) {
            return Err(CoreError::ProjectBusy);
        }
        let view = self.launch_view(id).await?;
        if !view.can_launch {
            return Err(CoreError::InvalidData(if view.reason.is_empty() {
                "请先设置游戏启动程序。".into()
            } else {
                view.reason
            }));
        }
        let configuration = view
            .configuration
            .ok_or_else(|| CoreError::InvalidData("请先设置游戏启动程序。".into()))?;
        let project = self.store.get(id).await?;
        let (executable, directory) =
            validate_configuration(&project.game_directory, &configuration)?;
        tokio::task::spawn_blocking(move || {
            let mut command = Command::new(&executable);
            command
                .current_dir(directory)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                command.raw_arg(&configuration.arguments);
            }
            #[cfg(not(windows))]
            if !configuration.arguments.is_empty() {
                return Err(CoreError::InvalidData(
                    "原样启动参数仅支持 Windows。".into(),
                ));
            }
            let child = command.spawn().map_err(|error| {
                CoreError::InvalidData(format!("无法启动 {}：{error}", executable.display()))
            })?;
            let id = child.id();
            #[cfg(not(windows))]
            std::thread::spawn(move || {
                let mut child = child;
                let _ = child.wait();
            });
            Ok(id)
        })
        .await?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::patching::{MANIFEST_FILENAME, PAYLOAD_DIRECTORY, build_manifest};
    use gamel10n_engines::protocol::{LaunchLocation, LaunchTarget};
    use gamel10n_translation::files::write_json;

    async fn fixture() -> (tempfile::TempDir, Application, String) {
        let root = tempfile::tempdir().unwrap();
        let game = root.path().join("game with spaces");
        std::fs::create_dir_all(game.join("bin")).unwrap();
        std::fs::write(game.join("game.exe"), "original exe").unwrap();
        std::fs::write(game.join("story.bin"), "original story").unwrap();
        let app = Application::open(root.path().join("data"), &crate::ToolPaths::new("missing"))
            .await
            .unwrap();
        let id = app.add_project(&game).await.unwrap().id;
        (root, app, id)
    }

    fn configuration(executable: &str, arguments: &str) -> LaunchConfiguration {
        LaunchConfiguration {
            executable: executable.into(),
            arguments: arguments.into(),
            working_directory: ".".into(),
        }
    }

    fn patch(root: &Path, name: &str, launch: Option<LaunchTarget>) -> PathBuf {
        let directory = root.join(name);
        let payload = directory.join(PAYLOAD_DIRECTORY);
        std::fs::create_dir_all(&payload).unwrap();
        std::fs::write(payload.join("story.bin"), "translated story").unwrap();
        if let Some(launch) = &launch
            && launch.location == LaunchLocation::Patch
        {
            std::fs::write(
                payload.join(&launch.configuration.executable),
                "patch launcher",
            )
            .unwrap();
        }
        let mut manifest = build_manifest(&directory, &Default::default()).unwrap();
        manifest.launch = launch;
        write_json(&directory.join(MANIFEST_FILENAME), &manifest).unwrap();
        directory
    }

    #[tokio::test]
    async fn settings_remain_editable_during_recognition_and_launch_changes_require_confirmation() {
        let (_root, app, id) = fixture().await;
        let _task = app.begin(&id, "recognize", "recognizing").await.unwrap();
        app.detect_launch(&id).await.unwrap();
        let initial = app.launch_view(&id).await.unwrap();
        app.save_project_settings(&id, serde_json::json!({"targetLanguage":"en"}))
            .await
            .unwrap();
        let manual = configuration("game.exe", "--manual");
        app.save_launch(&id, Some(manual.clone()), &initial.revision)
            .await
            .unwrap();
        let edited = app.launch_view(&id).await.unwrap();
        let game = app.open_project(&id).await.unwrap().game_directory;
        let stored = crate::StoredAnalysis {
            schema_version: 2,
            coverage_pending: false,
            format: "fixture".into(),
            source_directory: game,
            resource_files: vec![],
            source_hashes: Default::default(),
            extraction: gamel10n_engines::protocol::ExtractionResult { units: vec![], warnings: vec![] },
            adapter: None,
            probe: Some(serde_json::from_value(serde_json::json!({
                "engineId":"fixture", "adapterId":"fixture", "adapterVersion":"1", "description":"fixture",
                "requiredFiles":[], "launch":configuration("game.exe", "--detected"),
            })).unwrap()),
        };
        let guard = app.lock_project(&id).await.unwrap();
        let commit = app.commit_analysis(&id, stored);
        tokio::pin!(commit);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), &mut commit)
                .await
                .is_err()
        );
        drop(guard);
        commit.await.unwrap();
        let project = app.open_project(&id).await.unwrap();
        assert_eq!(project.settings["targetLanguage"], "en");
        assert_eq!(
            app.launch_view(&id).await.unwrap().configuration,
            Some(manual)
        );
        let CoreError::LaunchConflict { revision } = app
            .save_launch(&id, None, &edited.revision)
            .await
            .unwrap_err()
        else {
            panic!("expected launch conflict");
        };
        app.save_launch(&id, None, &revision).await.unwrap();
        assert_eq!(
            app.launch_view(&id).await.unwrap().configuration,
            Some(configuration("game.exe", "--detected"))
        );
    }

    #[tokio::test]
    async fn resource_pairs_select_the_game_among_multiple_executables() {
        let (_root, app, id) = fixture().await;
        let game = app.open_project(&id).await.unwrap().game_directory;
        for executable in ["UnityCrashHandler64.exe", "Config.exe", "another.exe"] {
            std::fs::write(game.join(executable), "fixture").unwrap();
        }
        std::fs::create_dir(game.join("game_Data")).unwrap();
        std::fs::write(game.join("game_Data/resources.assets"), "asset fixture").unwrap();
        let mut project = app.open_project(&id).await.unwrap();
        let probe: ProbeResult = serde_json::from_value(serde_json::json!({"engineId":"unity","adapterId":"fixture","adapterVersion":"1","description":"fixture","requiredFiles":["game_Data/resources.assets"]})).unwrap();
        discover(&mut project, Some(&probe)).unwrap();
        app.store.save(&project).await.unwrap();
        let view = app.launch_view(&id).await.unwrap();
        assert!(view.can_launch);
        assert_eq!(view.configuration.unwrap().executable, "game.exe");
        assert_eq!(view.candidates.len(), 4);
    }

    #[tokio::test]
    async fn unrelated_resource_pairs_and_locale_profiles_do_not_choose_a_launch_route() {
        let (_root, app, id) = fixture().await;
        let game = app.open_project(&id).await.unwrap().game_directory;
        std::fs::create_dir(game.join("demo_Data")).unwrap();
        std::fs::write(game.join("demo.exe"), "demo").unwrap();
        app.detect_launch(&id).await.unwrap();
        assert!(!app.launch_view(&id).await.unwrap().can_launch);
        std::fs::remove_file(game.join("demo.exe")).unwrap();
        std::fs::write(game.join("game.exe.le.config"), "locale profile").unwrap();
        app.detect_launch(&id).await.unwrap();
        let view = app.launch_view(&id).await.unwrap();
        assert!(!view.can_launch);
        assert!(view.reason.contains("转区"));
        app.save_launch(
            &id,
            Some(configuration("game.exe", "--manual-route")),
            &view.revision,
        )
        .await
        .unwrap();
        assert!(app.launch_view(&id).await.unwrap().can_launch);
    }

    #[tokio::test]
    async fn ambiguous_entries_require_selection_and_keep_manual_arguments_after_restart() {
        let (root, app, id) = fixture().await;
        let game = app.open_project(&id).await.unwrap().game_directory;
        std::fs::write(game.join("another.exe"), "fixture").unwrap();
        app.detect_launch(&id).await.unwrap();
        let view = app.launch_view(&id).await.unwrap();
        assert!(!view.can_launch);
        assert!(view.reason.contains("多个"));
        let saved = configuration("another.exe", r#"--name "名字 with spaces" "" --flag"#);
        app.save_launch(&id, Some(saved.clone()), &view.revision)
            .await
            .unwrap();
        app.detect_launch(&id).await.unwrap();
        drop(app);
        let app = Application::open(root.path().join("data"), &crate::ToolPaths::new("missing"))
            .await
            .unwrap();
        let view = app.launch_view(&id).await.unwrap();
        assert_eq!(view.source, "手动设置");
        assert_eq!(view.configuration, Some(saved));
    }

    #[tokio::test]
    async fn adapter_launch_metadata_selects_an_existing_executable_without_resource_copying() {
        let (_root, app, id) = fixture().await;
        let mut project = app.open_project(&id).await.unwrap();
        std::fs::write(project.game_directory.join("another.exe"), "fixture").unwrap();
        let declared = configuration("another.exe", "--original");
        let probe: ProbeResult = serde_json::from_value(serde_json::json!({
            "engineId":"fixture","adapterId":"fixture","adapterVersion":"1","description":"fixture",
            "requiredFiles":["story.bin"],"launch":declared,
        }))
        .unwrap();
        discover(&mut project, Some(&probe)).unwrap();
        app.store.save(&project).await.unwrap();
        assert_eq!(
            app.launch_view(&id).await.unwrap().configuration,
            Some(declared)
        );
    }

    #[tokio::test]
    async fn installation_switches_launch_configuration_and_restore_keeps_original_settings() {
        let (root, app, id) = fixture().await;
        let game = app.open_project(&id).await.unwrap().game_directory;
        let original = configuration("game.exe", "--original");
        app.save_launch(
            &id,
            Some(original.clone()),
            &app.launch_view(&id).await.unwrap().revision,
        )
        .await
        .unwrap();
        let installed = configuration("launcher.exe", "--translated");
        let directory = patch(
            root.path(),
            "patch",
            Some(LaunchTarget {
                location: LaunchLocation::Patch,
                configuration: installed.clone(),
            }),
        );
        let installer = PatchInstaller::new(app.project_directory(&id).unwrap());
        assert_eq!(
            app.launch_view(&id).await.unwrap().configuration,
            Some(original.clone())
        );
        installer.install(&game, &directory).unwrap();
        let view = app.launch_view(&id).await.unwrap();
        assert_eq!(view.source, "已安装补丁");
        assert_eq!(view.configuration, Some(installed));
        let custom = configuration("launcher.exe", "--custom");
        app.save_launch(&id, Some(custom.clone()), &view.revision)
            .await
            .unwrap();
        assert_eq!(
            app.launch_view(&id).await.unwrap().configuration,
            Some(custom)
        );
        installer.restore(&game).unwrap();
        assert!(!game.join("launcher.exe").exists());
        assert_eq!(
            app.launch_view(&id).await.unwrap().configuration,
            Some(original)
        );
    }

    #[tokio::test]
    async fn resource_patch_can_select_a_game_executable_and_later_inherit_the_original_configuration()
     {
        let (root, app, id) = fixture().await;
        let game = app.open_project(&id).await.unwrap().game_directory;
        std::fs::write(game.join("another.exe"), "fixture").unwrap();
        let original = configuration("game.exe", "--original");
        app.save_launch(
            &id,
            Some(original.clone()),
            &app.launch_view(&id).await.unwrap().revision,
        )
        .await
        .unwrap();
        let selected = configuration("another.exe", "--translated");
        let directory = patch(
            root.path(),
            "patch",
            Some(LaunchTarget {
                location: LaunchLocation::Game,
                configuration: selected.clone(),
            }),
        );
        assert_eq!(
            std::fs::read_dir(directory.join(PAYLOAD_DIRECTORY))
                .unwrap()
                .count(),
            1
        );
        let installer = PatchInstaller::new(app.project_directory(&id).unwrap());
        installer.install(&game, &directory).unwrap();
        assert_eq!(
            app.launch_view(&id).await.unwrap().configuration,
            Some(selected)
        );
        let inherited = patch(root.path(), "resource-only", None);
        installer.install(&game, &inherited).unwrap();
        assert_eq!(
            app.launch_view(&id).await.unwrap().configuration,
            Some(original.clone())
        );
        installer.restore(&game).unwrap();
        assert_eq!(
            app.launch_view(&id).await.unwrap().configuration,
            Some(original)
        );
    }

    #[tokio::test]
    async fn invalid_launch_metadata_is_rejected_before_installation() {
        let (root, app, id) = fixture().await;
        let game = app.open_project(&id).await.unwrap().game_directory;
        let directory = patch(
            root.path(),
            "patch",
            Some(LaunchTarget {
                location: LaunchLocation::Game,
                configuration: configuration("../outside.exe", ""),
            }),
        );
        let installer = PatchInstaller::new(app.project_directory(&id).unwrap());
        assert!(installer.install(&game, &directory).is_err());
        assert_eq!(
            std::fs::read_to_string(game.join("story.bin")).unwrap(),
            "original story"
        );
        assert!(!installer.journal_path().exists());
        assert!(
            app.save_launch(
                &id,
                Some(configuration("missing.exe", "")),
                &app.launch_view(&id).await.unwrap().revision
            )
            .await
            .is_err()
        );
    }

    #[tokio::test]
    async fn interrupted_installation_blocks_launch_until_restoration_finishes() {
        let (root, app, id) = fixture().await;
        let game = app.open_project(&id).await.unwrap().game_directory;
        app.save_launch(
            &id,
            Some(configuration("game.exe", "")),
            &app.launch_view(&id).await.unwrap().revision,
        )
        .await
        .unwrap();
        let directory = patch(root.path(), "patch", None);
        let installer = PatchInstaller::new(app.project_directory(&id).unwrap());
        installer.install(&game, &directory).unwrap();
        let mut journal = installer.read_journal(&game).unwrap();
        let entry = &mut journal.entries[0];
        entry.pending_sha256.clone_from(&entry.installed_sha256);
        write_json(&installer.journal_path(), &journal).unwrap();
        assert!(!app.launch_view(&id).await.unwrap().can_launch);
        assert!(
            app.launch_game(&id)
                .await
                .unwrap_err()
                .to_string()
                .contains("未完成")
        );
        installer.restore(&game).unwrap();
        assert!(app.launch_view(&id).await.unwrap().can_launch);
    }

    #[cfg(windows)]
    #[tokio::test]
    #[ignore = "Requires a Windows GUI argument fixture in GAMEL10N_LAUNCH_FIXTURE."]
    async fn launching_preserves_quoted_unicode_empty_arguments_and_working_directory() {
        let (root, app, id) = fixture().await;
        let game = app.open_project(&id).await.unwrap().game_directory;
        let fixture = std::env::var_os("GAMEL10N_LAUNCH_FIXTURE").expect("launch fixture path");
        std::fs::copy(fixture, game.join("bin/launch fixture.exe")).unwrap();
        let output = root.path().join("launch result.txt");
        let arguments = format!(
            "\"{}\" \"空格 参数\" \"\" \"quote\\\"value\" \"tail\\\\\"",
            output.display()
        );
        app.save_launch(
            &id,
            Some(LaunchConfiguration {
                executable: "bin/launch fixture.exe".into(),
                arguments,
                working_directory: "bin".into(),
            }),
            &app.launch_view(&id).await.unwrap().revision,
        )
        .await
        .unwrap();
        assert!(app.launch_game(&id).await.unwrap() > 0);
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if let Ok(result) = std::fs::read_to_string(&output) {
                    break result;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("launch fixture result");
        let (directory, arguments) = result.split_once('\n').unwrap();
        assert_eq!(
            directory.replace("\\\\?\\", ""),
            game.join("bin")
                .display()
                .to_string()
                .replace("\\\\?\\", "")
        );
        assert_eq!(
            arguments,
            format!("{:?}", ["空格 参数", "", "quote\"value", "tail\\"])
        );
    }
}
