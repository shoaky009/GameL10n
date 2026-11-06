//! Verified patch manifests and recoverable installation with originals kept beside game files.

use std::{
    collections::{BTreeMap, HashSet},
    fs::{self, File, FileTimes, OpenOptions},
    io::{self, Write},
    path::{Component, Path, PathBuf},
};

use chrono::{DateTime, Utc};
use gamel10n_engines::protocol::BuildMode;
use gamel10n_translation::files::{hash_file, hash_text, read_json, write_json};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tempfile::NamedTempFile;

use crate::{CoreError, logs::FileProgress};

pub const MANIFEST_FILENAME: &str = "patch-manifest.json";
pub const PAYLOAD_DIRECTORY: &str = "files";

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PatchManifest {
    pub schema_version: u32,
    pub files: Vec<PatchFile>,
    #[serde(default, skip_serializing_if = "BuildMode::is_replace")]
    pub mode: BuildMode,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub required_sources: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PatchFile {
    pub relative_path: String,
    pub sha256: String,
    /// Build-time source identity; replacement installs back up the actual current file.
    #[serde(default)]
    pub expected_original_sha256: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PatchInstallResult {
    pub installed_files: usize,
    pub unchanged_files: usize,
    pub backup_files: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PatchRestoreResult {
    pub restored_files: usize,
    pub removed_files: usize,
    pub conflicts: Vec<String>,
}

/// A user observation, kept separate from structural validation results.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeTestRecord {
    pub schema_version: u32,
    pub tested_at: DateTime<Utc>,
    pub patch_directory: PathBuf,
    pub status: String,
    pub notes: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallationJournal {
    pub schema_version: u32,
    #[serde(default)]
    pub game_directory: Option<PathBuf>,
    pub entries: Vec<InstallationEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub created_directories: Vec<String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallationEntry {
    pub relative_path: String,
    #[serde(default = "default_original_existed", skip_serializing_if = "is_true")]
    pub original_existed: bool,
    #[serde(default)]
    pub original_sha256: Option<String>,
    #[serde(default)]
    pub original_read_only: bool,
    #[serde(default)]
    pub original_creation_time_utc: Option<DateTime<Utc>>,
    #[serde(default)]
    pub original_last_write_time_utc: Option<DateTime<Utc>>,
    #[serde(default)]
    pub backup_relative_path: Option<String>,
    #[serde(default)]
    pub installed_sha256: Option<String>,
    #[serde(default)]
    pub pending_sha256: Option<String>,
    #[serde(default)]
    pub restoring: bool,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

fn default_original_existed() -> bool {
    true
}

fn is_true(value: &bool) -> bool {
    *value
}

#[derive(Clone, Debug)]
pub struct PatchInstaller {
    project_directory: PathBuf,
    cancellation: Option<crate::CancellationToken>,
    progress: Option<FileProgress>,
}

struct InstallationLock {
    _current: File,
    _legacy: Option<File>,
}

impl PatchInstaller {
    pub fn new(project_directory: impl Into<PathBuf>) -> Self {
        Self {
            project_directory: project_directory.into(),
            cancellation: None,
            progress: None,
        }
    }

    pub fn journal_path(&self) -> PathBuf {
        self.project_directory.join("installation.json")
    }

    pub fn with_cancellation(mut self, cancellation: crate::CancellationToken) -> Self {
        self.cancellation = Some(cancellation);
        self
    }

    pub(crate) fn with_progress(mut self, progress: FileProgress) -> Self {
        self.progress = Some(progress);
        self
    }

    fn report_file(&self, action: &str, path: &Path, name: &str) {
        if let Some(progress) = &self.progress {
            progress.file(action, path, name);
        }
    }

    fn check_cancelled(&self) -> Result<(), CoreError> {
        if self
            .cancellation
            .as_ref()
            .is_some_and(crate::CancellationToken::is_cancelled)
        {
            return Err(gamel10n_engines::EngineError::Cancelled.into());
        }
        Ok(())
    }

    pub fn read_journal(&self, game_directory: &Path) -> Result<InstallationJournal, CoreError> {
        let game = directory_root(game_directory)?;
        self.load_journal(&game, false)
    }

    /// Validates the complete payload before any game or installation-record write.
    pub fn install(
        &self,
        game_directory: &Path,
        patch_directory: &Path,
    ) -> Result<PatchInstallResult, CoreError> {
        let game = directory_root(game_directory)?;
        let patch = directory_root(patch_directory)?;
        if paths_overlap(&game, &patch) {
            return Err(invalid("游戏目录与补丁目录必须分开。"));
        }
        let manifest = read_manifest(&patch)?;
        let payload = directory_root(&safe_path(&patch, PAYLOAD_DIRECTORY)?)?;
        let targets: HashSet<_> = manifest
            .files
            .iter()
            .map(|file| file.relative_path.to_lowercase())
            .collect();
        for file in &manifest.files {
            self.check_cancelled()?;
            let target = safe_path(&game, &file.relative_path)?;
            if target.is_dir() {
                return Err(invalid(format!(
                    "补丁目标被目录占用：{}",
                    file.relative_path
                )));
            }
            let source = safe_path(&payload, &file.relative_path)?;
            self.report_file("正在校验补丁文件", &source, &file.relative_path);
            ensure_hash(&source, Some(&file.sha256))?;
        }
        let _lock = self.operation_lock(&game)?;
        let mut journal = self.load_journal(&game, true)?;
        let conflicts = self.recover_inner(&game, &mut journal)?;
        if !conflicts.is_empty() {
            return Err(invalid(format!(
                "中断操作与现有游戏文件冲突：{}",
                conflicts.join("、")
            )));
        }
        let mut reserved = targets.clone();
        for entry in &journal.entries {
            if let Some(backup) = &entry.backup_relative_path {
                if targets.contains(&backup.to_lowercase()) {
                    return Err(invalid("补丁会覆盖已保存的原始备份。"));
                }
                reserved.insert(backup.to_lowercase());
            }
        }
        // Protect recorded originals and detect external changes before installing any payload.
        for file in &manifest.files {
            let target = safe_path(&game, &file.relative_path)?;
            if target.is_file() {
                self.report_file("正在校验游戏文件", &target, &file.relative_path);
            }
            let current = hash_if_present(&target)?;
            if current.is_none() || same_hash(current.as_deref(), Some(&file.sha256)) {
                continue;
            }
            let entry = journal
                .entries
                .iter()
                .find(|entry| same_relative(&entry.relative_path, &file.relative_path));
            if let Some(entry) = entry {
                let original_present =
                    same_hash(current.as_deref(), entry.original_sha256.as_deref());
                if !original_present
                    && !same_hash(current.as_deref(), entry.installed_sha256.as_deref())
                {
                    return Err(invalid(format!(
                        "已安装文件被其他程序修改：{}",
                        file.relative_path
                    )));
                }
                if !original_present
                    && entry.original_existed
                    && self.find_original_backup(&game, entry)?.is_none()
                {
                    return Err(missing_original(entry));
                }
            }
        }
        for (relative, expected) in &manifest.required_sources {
            let target = safe_path(&game, relative)?;
            self.report_file("正在校验覆盖包原资源", &target, relative);
            let current = hash_if_present(&target)?;
            if same_hash(current.as_deref(), Some(expected)) {
                continue;
            }
            let previous = journal
                .entries
                .iter()
                .find(|entry| same_relative(&entry.relative_path, relative));
            if !previous.is_some_and(|entry| {
                same_hash(entry.original_sha256.as_deref(), Some(expected))
                    && same_hash(current.as_deref(), entry.installed_sha256.as_deref())
            }) {
                return Err(invalid(format!(
                    "覆盖包所需原资源与当前游戏不一致：{relative}"
                )));
            }
            if let Some(entry) = previous
                && self.find_original_backup(&game, entry)?.is_none()
            {
                return Err(missing_original(entry));
            }
        }
        // A patch verified against originals must not inherit an older replacement or overlay.
        if manifest.mode == BuildMode::Overlay
            || journal.extra.get("patchMode").and_then(Value::as_str) == Some("overlay")
        {
            let restored = self.restore_selected(&game, &mut journal, |entry| {
                !targets.contains(&entry.relative_path.to_lowercase())
            })?;
            if !restored.conflicts.is_empty() {
                return Err(invalid(format!(
                    "旧补丁文件发生变化，未安装新补丁：{}",
                    restored.conflicts.join("、")
                )));
            }
        }
        let previous_mode = journal.extra.remove("patchMode");
        if manifest.mode == BuildMode::Overlay {
            journal
                .extra
                .insert("patchMode".into(), Value::String("overlay".into()));
        }
        if previous_mode.as_ref() != journal.extra.get("patchMode") {
            self.save_journal(&game, &journal)?;
        }
        let mut result = PatchInstallResult {
            installed_files: 0,
            unchanged_files: 0,
            backup_files: Vec::new(),
        };
        for file in &manifest.files {
            self.check_cancelled()?;
            let source = safe_path(&payload, &file.relative_path)?;
            let target = safe_path(&game, &file.relative_path)?;
            self.report_file("正在准备安装文件", &source, &file.relative_path);
            let current = hash_if_present(&target)?;
            let existing = journal
                .entries
                .iter()
                .position(|entry| same_relative(&entry.relative_path, &file.relative_path));
            if same_hash(current.as_deref(), Some(&file.sha256)) {
                result.unchanged_files += 1;
                continue;
            }
            let original_present = existing.is_some_and(|position| {
                same_hash(
                    current.as_deref(),
                    journal.entries[position].original_sha256.as_deref(),
                )
            });
            let mut move_original_to_backup = false;
            let position = if let Some(position) = existing.filter(|&position| {
                !journal.entries[position].original_existed || !original_present
            }) {
                if journal.entries[position].original_existed && current.is_some() {
                    let (backup, recovered) =
                        self.ensure_original_backup(&game, &mut journal, position, &mut reserved)?;
                    if recovered {
                        result.backup_files.push(backup);
                    }
                }
                position
            } else {
                let metadata = if current.is_some() {
                    Some(fs::metadata(&target)?)
                } else {
                    None
                };
                let backup = if current.is_none() {
                    None
                } else if let Some(position) = existing
                    && self
                        .find_original_backup(&game, &journal.entries[position])?
                        .is_some()
                {
                    Some(
                        self.ensure_original_backup(&game, &mut journal, position, &mut reserved)?
                            .0,
                    )
                } else {
                    if let Some(position) = existing
                        && let Some(relative) = &journal.entries[position].backup_relative_path
                    {
                        reserved.remove(&relative.to_lowercase());
                    }
                    move_original_to_backup = true;
                    Some(choose_backup(&game, &file.relative_path, &reserved)?)
                };
                if let Some(backup) = &backup {
                    reserved.insert(backup.to_lowercase());
                }
                // A reinstalled original starts a fresh backup cycle, including its current file dates.
                let entry = InstallationEntry {
                    relative_path: file.relative_path.clone(),
                    original_existed: current.is_some(),
                    original_sha256: current.clone(),
                    original_read_only: metadata
                        .as_ref()
                        .is_some_and(|metadata| metadata.permissions().readonly()),
                    original_creation_time_utc: metadata
                        .as_ref()
                        .and_then(|metadata| metadata.created().ok())
                        .map(DateTime::from),
                    original_last_write_time_utc: metadata
                        .as_ref()
                        .and_then(|metadata| metadata.modified().ok())
                        .map(DateTime::from),
                    backup_relative_path: backup,
                    installed_sha256: None,
                    pending_sha256: None,
                    restoring: false,
                    extra: BTreeMap::new(),
                };
                if let Some(position) = existing {
                    journal.entries[position] = entry;
                    position
                } else {
                    journal.entries.push(entry);
                    journal.entries.len() - 1
                }
            };
            self.record_missing_directories(&game, &target, &mut journal)?;
            journal.entries[position].pending_sha256 = Some(file.sha256.clone());
            self.save_journal(&game, &journal)?;
            let entry = &journal.entries[position];
            let mut expected_current = current.as_deref();
            if move_original_to_backup && let Some(relative) = &entry.backup_relative_path {
                let backup = safe_path(&game, relative)?;
                self.report_file(
                    "正在移动并校验原文件",
                    &target,
                    &format!("{} → {relative}", file.relative_path),
                );
                ensure_hash(&target, entry.original_sha256.as_deref())?;
                if backup.exists() {
                    return Err(invalid("原始备份位置已被占用。"));
                }
                // Rename preserves creation/modification dates and attributes, without duplicating original bytes.
                fs::rename(&target, &backup)?;
                ensure_hash(&backup, entry.original_sha256.as_deref())?;
                expected_current = None;
                result.backup_files.push(relative.clone());
            }
            self.report_file("正在复制补丁到游戏目录", &source, &file.relative_path);
            atomic_copy(&source, &target, &file.sha256, expected_current)?;
            journal.entries[position].installed_sha256 = Some(file.sha256.clone());
            journal.entries[position].pending_sha256 = None;
            self.save_journal(&game, &journal)?;
            result.installed_files += 1;
        }
        Ok(result)
    }

    pub fn restore(&self, game_directory: &Path) -> Result<PatchRestoreResult, CoreError> {
        let game = directory_root(game_directory)?;
        let _lock = self.operation_lock(&game)?;
        let mut journal = self.load_journal(&game, true)?;
        self.restore_selected(&game, &mut journal, |_| true)
    }

    fn restore_selected(
        &self,
        game: &Path,
        journal: &mut InstallationJournal,
        include: impl Fn(&InstallationEntry) -> bool,
    ) -> Result<PatchRestoreResult, CoreError> {
        let mut result = PatchRestoreResult {
            restored_files: 0,
            removed_files: 0,
            conflicts: self.recover_inner(game, journal)?,
        };
        let mut reserved: HashSet<_> = journal
            .entries
            .iter()
            .flat_map(|entry| {
                std::iter::once(entry.relative_path.to_lowercase()).chain(
                    entry
                        .backup_relative_path
                        .as_ref()
                        .map(|path| path.to_lowercase()),
                )
            })
            .collect();
        for position in (0..journal.entries.len()).rev() {
            self.check_cancelled()?;
            let entry = &journal.entries[position];
            if !include(entry) {
                continue;
            }
            if result
                .conflicts
                .iter()
                .any(|path| same_relative(path, &entry.relative_path))
            {
                continue;
            }
            let target = safe_path(game, &entry.relative_path)?;
            self.report_file("正在校验待恢复文件", &target, &entry.relative_path);
            let current = hash_if_present(&target)?;
            if same_hash(current.as_deref(), entry.original_sha256.as_deref()) {
                if !self.complete_restore(game, journal, position)? {
                    result
                        .conflicts
                        .push(journal.entries[position].relative_path.clone());
                }
                continue;
            }
            if !same_hash(current.as_deref(), entry.installed_sha256.as_deref()) {
                result.conflicts.push(entry.relative_path.clone());
                continue;
            }
            let backup = if entry.original_existed {
                if self.find_original_backup(game, entry)?.is_none() {
                    return Err(missing_original(entry));
                }
                Some(
                    self.ensure_original_backup(game, journal, position, &mut reserved)?
                        .0,
                )
            } else {
                None
            };
            journal.entries[position].restoring = true;
            self.save_journal(game, journal)?;
            let entry = &journal.entries[position];
            ensure_hash(&target, entry.installed_sha256.as_deref())?;
            if let Some(backup) = backup {
                self.restore_original(&safe_path(game, &backup)?, &target, entry)?;
                result.restored_files += 1;
            } else {
                remove_file(&target)?;
                result.removed_files += 1;
            }
            if !self.complete_restore(game, journal, position)? {
                result
                    .conflicts
                    .push(journal.entries[position].relative_path.clone());
            }
        }
        self.remove_created_directories(game, journal)?;
        Ok(result)
    }

    /// Resolves journal interruptions and returns paths requiring the user's external conflict resolution.
    pub fn recover(&self, game_directory: &Path) -> Result<Vec<String>, CoreError> {
        let game = directory_root(game_directory)?;
        let _lock = self.operation_lock(&game)?;
        let mut journal = self.load_journal(&game, true)?;
        self.recover_inner(&game, &mut journal)
    }

    /// Finds original source bytes without changing the game, journal, backups, or snapshots.
    pub fn resolve_original_file(
        &self,
        game_directory: &Path,
        relative: &str,
    ) -> Result<PathBuf, CoreError> {
        let game = directory_root(game_directory)?;
        let relative = normalize_relative(relative)?;
        let target = safe_path(&game, &relative)?;
        self.report_file("正在查找并校验原始资源", &target, &relative);
        let journal = self.load_journal(&game, false)?;
        let current = hash_if_present(&target)?;
        let Some(entry) = journal
            .entries
            .iter()
            .find(|entry| same_relative(&entry.relative_path, &relative))
        else {
            return current
                .map(|_| target)
                .ok_or_else(|| invalid("所需游戏资源不存在。"));
        };
        if current.is_some() && same_hash(current.as_deref(), entry.original_sha256.as_deref()) {
            return Ok(target);
        }
        if current.is_some()
            && !same_hash(current.as_deref(), entry.installed_sha256.as_deref())
            && !same_hash(current.as_deref(), entry.pending_sha256.as_deref())
        {
            return Ok(target);
        }
        if !entry.original_existed {
            return Err(invalid("所需文件由补丁新增，没有原始资源。"));
        }
        self.find_original_backup(&game, entry)?
            .ok_or_else(|| missing_original(entry))
    }

    pub fn record_runtime_test(
        &self,
        patch_directory: &Path,
        status: &str,
        notes: &str,
    ) -> Result<RuntimeTestRecord, CoreError> {
        if !matches!(status, "passed" | "failed" | "not-tested") {
            return Err(invalid("实机验证状态无效。"));
        }
        let patch_directory = directory_root(patch_directory)?;
        let project = std::path::absolute(&self.project_directory)?;
        if !patch_directory.starts_with(project.join("patches")) {
            return Err(invalid("实机记录必须属于当前项目的补丁。"));
        }
        let record = RuntimeTestRecord {
            schema_version: 1,
            tested_at: Utc::now(),
            patch_directory,
            status: status.into(),
            notes: notes.into(),
        };
        write_json(
            &safe_path(&record.patch_directory, "runtime-test.json")?,
            &record,
        )?;
        Ok(record)
    }

    pub fn runtime_test(
        &self,
        patch_directory: &Path,
    ) -> Result<Option<RuntimeTestRecord>, CoreError> {
        let path = safe_path(patch_directory, "runtime-test.json")?;
        if path.is_file() {
            Ok(Some(read_json(&path)?))
        } else {
            Ok(None)
        }
    }

    fn operation_lock(&self, game: &Path) -> Result<InstallationLock, CoreError> {
        self.check_cancelled()?;
        let project = std::path::absolute(&self.project_directory)?;
        check_path(&project)?;
        if paths_overlap(game, &project) {
            return Err(invalid("项目数据目录与游戏目录必须分开。"));
        }
        ensure_game_closed(game)?;
        let identity = if cfg!(windows) {
            game.to_string_lossy()
                .chars()
                .map(|character| {
                    let mut upper = character.to_uppercase();
                    let first = upper.next().unwrap_or(character);
                    if upper.next().is_none() {
                        first
                    } else {
                        character
                    }
                })
                .collect()
        } else {
            game.to_string_lossy().into_owned()
        };
        let path = std::env::temp_dir().join(format!(
            "AvgTranslator-install-{}.lock",
            hash_text(&identity)
        ));
        check_path(&path)?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        lock.try_lock()
            .map_err(|error| invalid(format!("另一个进程正在安装或恢复此游戏：{error}")))?;
        let legacy_path = game.join(".avg-translator/install.lock");
        check_path(&legacy_path)?;
        let legacy = if legacy_path.is_file() {
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .open(legacy_path)?;
            file.try_lock()
                .map_err(|error| invalid(format!("旧版安装进程仍在使用游戏：{error}")))?;
            Some(file)
        } else {
            None
        };
        Ok(InstallationLock {
            _current: lock,
            _legacy: legacy,
        })
    }

    fn load_journal(&self, game: &Path, migrate: bool) -> Result<InstallationJournal, CoreError> {
        let current = self.journal_path();
        check_path(&current)?;
        let legacy = game.join(".avg-translator/patch-journal.json");
        check_path(&legacy)?;
        let mut journal: Option<InstallationJournal> = if current.is_file() {
            Some(read_installation_journal(&current)?)
        } else {
            None
        };
        if legacy.is_file() {
            let old = read_installation_journal(&legacy)?;
            if let Some(current) = &journal {
                let left =
                    serde_json::to_value(current).map_err(|error| invalid(error.to_string()))?;
                let right =
                    serde_json::to_value(&old).map_err(|error| invalid(error.to_string()))?;
                if left != right {
                    return Err(invalid("项目与游戏目录的安装记录不一致，已保留两份记录。"));
                }
            } else {
                journal = Some(old);
            }
        }
        let mut journal = journal.unwrap_or_else(|| InstallationJournal {
            schema_version: 1,
            game_directory: Some(game.to_path_buf()),
            entries: Vec::new(),
            created_directories: Vec::new(),
            extra: BTreeMap::new(),
        });
        validate_journal(&journal, game)?;
        for entry in &mut journal.entries {
            entry.relative_path = normalize_relative(&entry.relative_path)?;
            if let Some(backup) = &mut entry.backup_relative_path {
                *backup = normalize_relative(backup)?;
            }
        }
        for directory in &mut journal.created_directories {
            *directory = normalize_relative(directory)?;
        }
        journal.game_directory = Some(game.to_path_buf());
        if migrate && legacy.is_file() {
            self.save_journal(game, &journal)?;
            fs::remove_file(&legacy)?;
        }
        Ok(journal)
    }

    fn save_journal(&self, game: &Path, journal: &InstallationJournal) -> Result<(), CoreError> {
        validate_journal(journal, game)?;
        check_path(&self.journal_path())?;
        write_json(&self.journal_path(), journal)?;
        Ok(())
    }

    fn recover_inner(
        &self,
        game: &Path,
        journal: &mut InstallationJournal,
    ) -> Result<Vec<String>, CoreError> {
        let mut conflicts = Vec::new();
        for position in (0..journal.entries.len()).rev() {
            self.check_cancelled()?;
            let entry = journal.entries[position].clone();
            if entry.pending_sha256.is_none() && !entry.restoring {
                continue;
            }
            let target = safe_path(game, &entry.relative_path)?;
            self.report_file("正在检查中断文件", &target, &entry.relative_path);
            let current = hash_if_present(&target)?;
            if !entry.original_existed && current.is_none() {
                if !self.complete_restore(game, journal, position)? {
                    conflicts.push(journal.entries[position].relative_path.clone());
                }
                continue;
            }
            if entry.restoring {
                if same_hash(current.as_deref(), entry.original_sha256.as_deref()) {
                    if !self.complete_restore(game, journal, position)? {
                        conflicts.push(journal.entries[position].relative_path.clone());
                    }
                    continue;
                }
                if same_hash(current.as_deref(), entry.installed_sha256.as_deref()) {
                    journal.entries[position].restoring = false;
                } else if current.is_none() && entry.original_existed {
                    if self.recover_missing_target(game, journal, position)? {
                        continue;
                    }
                    conflicts.push(entry.relative_path.clone());
                    continue;
                } else {
                    conflicts.push(entry.relative_path.clone());
                    continue;
                }
            } else if same_hash(current.as_deref(), entry.pending_sha256.as_deref()) {
                journal.entries[position].installed_sha256 = entry.pending_sha256.clone();
                journal.entries[position].pending_sha256 = None;
            } else if (entry.installed_sha256.is_some()
                && same_hash(current.as_deref(), entry.installed_sha256.as_deref()))
                || (entry.installed_sha256.is_none()
                    && same_hash(current.as_deref(), entry.original_sha256.as_deref()))
            {
                if entry.installed_sha256.is_none() {
                    if !self.complete_restore(game, journal, position)? {
                        conflicts.push(journal.entries[position].relative_path.clone());
                    }
                    continue;
                }
                journal.entries[position].pending_sha256 = None;
            } else if current.is_none()
                && entry.installed_sha256.is_none()
                && entry.original_existed
            {
                if self.recover_missing_target(game, journal, position)? {
                    continue;
                }
                conflicts.push(entry.relative_path.clone());
                continue;
            } else {
                conflicts.push(entry.relative_path.clone());
                continue;
            }
            self.save_journal(game, journal)?;
        }
        Ok(conflicts)
    }

    fn recover_missing_target(
        &self,
        game: &Path,
        journal: &mut InstallationJournal,
        position: usize,
    ) -> Result<bool, CoreError> {
        if self
            .find_original_backup(game, &journal.entries[position])?
            .is_none()
        {
            return Ok(false);
        }
        let mut reserved = journal
            .entries
            .iter()
            .flat_map(|entry| {
                std::iter::once(entry.relative_path.to_lowercase()).chain(
                    entry
                        .backup_relative_path
                        .as_ref()
                        .map(|path| path.to_lowercase()),
                )
            })
            .collect();
        let backup = self
            .ensure_original_backup(game, journal, position, &mut reserved)?
            .0;
        let entry = &journal.entries[position];
        self.restore_original(
            &safe_path(game, &backup)?,
            &safe_path(game, &entry.relative_path)?,
            entry,
        )?;
        self.complete_restore(game, journal, position)
    }

    fn find_original_backup(
        &self,
        game: &Path,
        entry: &InstallationEntry,
    ) -> Result<Option<PathBuf>, CoreError> {
        if !entry.original_existed {
            return Ok(None);
        }
        let Some(expected) = &entry.original_sha256 else {
            return Ok(None);
        };
        if let Some(relative) = &entry.backup_relative_path {
            let path = safe_path(game, relative)?;
            if same_hash(hash_if_present(&path)?.as_deref(), Some(expected)) {
                return Ok(Some(path));
            }
        }
        let target = safe_path(game, &entry.relative_path)?;
        let parent = target
            .parent()
            .ok_or_else(|| invalid("原始资源路径无父目录。"))?;
        if parent.is_dir() {
            let prefix = target
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_lowercase()
                + ".";
            let mut candidates: Vec<_> = fs::read_dir(parent)?.collect::<Result<Vec<_>, _>>()?;
            candidates.sort_by_key(|entry| entry.file_name());
            for candidate in candidates {
                let name = candidate.file_name().to_string_lossy().to_lowercase();
                if name.starts_with(&prefix) && name.ends_with(".bak") {
                    check_path(&candidate.path())?;
                    if same_hash(
                        hash_if_present(&candidate.path())?.as_deref(),
                        Some(expected),
                    ) {
                        return Ok(Some(candidate.path()));
                    }
                }
            }
        }
        Ok(None)
    }

    fn ensure_original_backup(
        &self,
        game: &Path,
        journal: &mut InstallationJournal,
        position: usize,
        reserved: &mut HashSet<String>,
    ) -> Result<(String, bool), CoreError> {
        let entry = &journal.entries[position];
        let source = self
            .find_original_backup(game, entry)?
            .ok_or_else(|| missing_original(entry))?;
        let recorded = entry
            .backup_relative_path
            .as_ref()
            .ok_or_else(|| invalid("原始文件缺少备份位置。"))?;
        if same_path(&source, &safe_path(game, recorded)?) {
            return Ok((recorded.clone(), false));
        }
        let relative = source
            .strip_prefix(game)
            .map_err(|_| invalid("原始备份必须位于游戏目录内。"))?
            .to_string_lossy()
            .replace('\\', "/");
        if reserved.contains(&relative.to_lowercase()) {
            return Err(invalid(format!("原始备份已被其他文件占用：{relative}")));
        }
        journal.entries[position].backup_relative_path = Some(relative.clone());
        reserved.insert(relative.to_lowercase());
        self.save_journal(game, journal)?;
        Ok((relative, true))
    }

    fn complete_restore(
        &self,
        game: &Path,
        journal: &mut InstallationJournal,
        position: usize,
    ) -> Result<bool, CoreError> {
        let entry = &journal.entries[position];
        let target = safe_path(game, &entry.relative_path)?;
        if target.is_file() {
            self.report_file("正在校验恢复结果", &target, &entry.relative_path);
        }
        ensure_hash(&target, entry.original_sha256.as_deref())?;
        if let Some(relative) = &entry.backup_relative_path {
            let backup = safe_path(game, relative)?;
            if backup.is_file() {
                if !same_hash(
                    hash_if_present(&backup)?.as_deref(),
                    entry.original_sha256.as_deref(),
                ) {
                    return Ok(false);
                }
                // Consume even old copy-style backups by moving them back, preserving their original file dates.
                self.restore_original(&backup, &target, entry)?;
            }
        }
        if entry.original_existed {
            let metadata = fs::metadata(&target)?;
            let mut times = FileTimes::new();
            let mut changed = false;
            if let Some(modified) = entry.original_last_write_time_utc {
                let expected: std::time::SystemTime = modified.into();
                if metadata.modified()? != expected {
                    times = times.set_modified(expected);
                    changed = true;
                }
            }
            #[cfg(windows)]
            {
                use std::os::windows::fs::FileTimesExt;
                if let Some(created) = entry.original_creation_time_utc {
                    let expected: std::time::SystemTime = created.into();
                    if metadata.created()? != expected {
                        times = times.set_created(expected);
                        changed = true;
                    }
                }
            }
            if changed {
                if metadata.permissions().readonly() {
                    make_writable(&target)?;
                }
                OpenOptions::new()
                    .write(true)
                    .open(&target)?
                    .set_times(times)?;
            }
            if entry.original_read_only {
                let mut permissions = fs::metadata(&target)?.permissions();
                permissions.set_readonly(true);
                fs::set_permissions(&target, permissions)?;
            } else if fs::metadata(&target)?.permissions().readonly() {
                make_writable(&target)?;
            }
        }
        journal.entries.remove(position);
        self.save_journal(game, journal)?;
        Ok(true)
    }

    fn restore_original(
        &self,
        backup: &Path,
        target: &Path,
        entry: &InstallationEntry,
    ) -> Result<(), CoreError> {
        self.report_file("正在移动备份恢复原版", backup, &entry.relative_path);
        move_original(backup, target, entry)
    }

    fn record_missing_directories(
        &self,
        game: &Path,
        target: &Path,
        journal: &mut InstallationJournal,
    ) -> Result<(), CoreError> {
        let mut missing = Vec::new();
        let mut parent = target.parent();
        while let Some(directory) = parent.filter(|path| *path != game) {
            if directory.exists() {
                break;
            }
            missing.push(
                directory
                    .strip_prefix(game)
                    .map_err(|_| invalid("补丁目录不属于当前游戏。"))?
                    .to_string_lossy()
                    .replace('\\', "/"),
            );
            parent = directory.parent();
        }
        for directory in missing.into_iter().rev() {
            if !journal
                .created_directories
                .iter()
                .any(|path| same_relative(path, &directory))
            {
                journal.created_directories.push(directory);
            }
        }
        Ok(())
    }

    fn remove_created_directories(
        &self,
        game: &Path,
        journal: &mut InstallationJournal,
    ) -> Result<(), CoreError> {
        if journal.created_directories.is_empty() {
            return Ok(());
        }
        // Remove only directories created by installation, from children to parents.
        let mut directories = journal.created_directories.clone();
        directories.sort_unstable_by_key(|path| std::cmp::Reverse(path.len()));
        for relative in directories {
            self.check_cancelled()?;
            let path = safe_path(game, &relative)?;
            match fs::remove_dir(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) if error.kind() == io::ErrorKind::DirectoryNotEmpty => continue,
                Err(error) => return Err(error.into()),
            }
            journal
                .created_directories
                .retain(|directory| !same_relative(directory, &relative));
        }
        self.save_journal(game, journal)
    }
}

fn read_installation_journal(path: &Path) -> Result<InstallationJournal, CoreError> {
    let mut document: Value = read_json(path)?;
    if let Some(entries) = document.get_mut("entries").and_then(Value::as_array_mut) {
        for entry in entries {
            // Older records explicitly stored a null original hash for patch-added files.
            // Missing metadata alone does not establish that a file can be deleted.
            if let Some(entry) = entry.as_object_mut()
                && !entry.contains_key("originalExisted")
                && entry.get("originalSha256").is_some_and(Value::is_null)
                && entry.get("backupRelativePath").is_none_or(Value::is_null)
            {
                entry.insert("originalExisted".into(), Value::Bool(false));
            }
        }
    }
    serde_json::from_value(document).map_err(|error| invalid(format!("安装记录无效：{error}")))
}

/// Reads metadata only; installation additionally checks the payload bytes.
pub fn read_manifest(patch_directory: &Path) -> Result<PatchManifest, CoreError> {
    let path = safe_path(patch_directory, MANIFEST_FILENAME)?;
    if fs::metadata(&path)?.len() > 16 * 1024 * 1024 {
        return Err(invalid("补丁清单过大。"));
    }
    let mut manifest: PatchManifest = read_json(&path)?;
    validate_manifest(&mut manifest)?;
    Ok(manifest)
}

/// Hashes game-ready files in `files/`, keeping patch records outside the payload.
pub fn build_manifest(
    patch_directory: &Path,
    originals: &BTreeMap<String, String>,
) -> Result<PatchManifest, CoreError> {
    let root = directory_root(patch_directory)?;
    let payload = directory_root(&safe_path(&root, PAYLOAD_DIRECTORY)?)?;
    let mut paths = Vec::new();
    collect_files(&payload, &payload, &mut paths)?;
    paths.sort();
    let mut manifest = PatchManifest {
        schema_version: 2,
        files: Vec::new(),
        mode: BuildMode::Replace,
        required_sources: BTreeMap::new(),
    };
    for relative in paths {
        manifest.files.push(PatchFile {
            sha256: hash_file(&safe_path(&payload, &relative)?)?,
            expected_original_sha256: originals
                .iter()
                .find(|(path, _)| same_relative(path, &relative))
                .map(|(_, hash)| hash.clone()),
            relative_path: relative,
        });
    }
    validate_manifest(&mut manifest)?;
    write_json(&safe_path(&root, MANIFEST_FILENAME)?, &manifest)?;
    Ok(manifest)
}

fn validate_manifest(manifest: &mut PatchManifest) -> Result<(), CoreError> {
    if manifest.schema_version != 2 {
        return Err(invalid("补丁清单版本不支持，请重新生成补丁。"));
    }
    let mut paths = HashSet::new();
    for file in &mut manifest.files {
        file.relative_path = normalize_relative(&file.relative_path)?;
        validate_hash(&file.sha256)?;
        file.sha256.make_ascii_lowercase();
        if let Some(original) = &file.expected_original_sha256 {
            validate_hash(original)?;
        }
        if !paths.insert(file.relative_path.to_lowercase()) {
            return Err(invalid("补丁清单含重复目标。"));
        }
    }
    for path in &paths {
        let mut parent = Path::new(path).parent();
        while let Some(directory) = parent {
            if paths.contains(&directory.to_string_lossy().into_owned()) {
                return Err(invalid("补丁文件同时被用作目录。"));
            }
            parent = directory.parent();
        }
    }
    if manifest.mode == BuildMode::Overlay && manifest.required_sources.is_empty() {
        return Err(invalid("覆盖包缺少原资源版本信息。"));
    }
    if manifest.mode == BuildMode::Replace && !manifest.required_sources.is_empty() {
        return Err(invalid("仅覆盖包使用独立的原资源版本清单。"));
    }
    let mut sources = BTreeMap::new();
    let mut source_paths = HashSet::new();
    for (relative, hash) in std::mem::take(&mut manifest.required_sources) {
        let relative = normalize_relative(&relative)?;
        validate_hash(&hash)?;
        let normalized = relative.to_lowercase();
        if !source_paths.insert(normalized.clone()) || paths.contains(&normalized) {
            return Err(invalid("覆盖包原资源重复或与输出路径重叠。"));
        }
        sources.insert(relative, hash.to_ascii_lowercase());
    }
    manifest.required_sources = sources;
    Ok(())
}

fn validate_journal(journal: &InstallationJournal, game: &Path) -> Result<(), CoreError> {
    if journal.schema_version != 1 {
        return Err(invalid("安装记录版本不支持。"));
    }
    if journal
        .game_directory
        .as_ref()
        .is_some_and(|directory| !same_path(directory, game))
    {
        return Err(invalid(
            "安装记录属于另一个游戏目录，请使用对应的项目数据。",
        ));
    }
    let mut paths = HashSet::new();
    let mut backups = HashSet::new();
    for entry in &journal.entries {
        let relative = normalize_relative(&entry.relative_path)?;
        safe_path(game, &relative)?;
        if !paths.insert(relative.to_lowercase()) {
            return Err(invalid("安装记录含重复目标。"));
        }
        if entry.original_existed {
            let original = entry
                .original_sha256
                .as_ref()
                .ok_or_else(|| invalid("原始文件缺少哈希，不能按新增文件处理。"))?;
            validate_hash(original)?;
            let backup = entry
                .backup_relative_path
                .as_ref()
                .ok_or_else(|| invalid("原始文件没有备份位置。"))?;
            let backup = normalize_relative(backup)?;
            safe_path(game, &backup)?;
            if !backup
                .to_lowercase()
                .starts_with(&(relative.to_lowercase() + "."))
                || !backup.to_lowercase().ends_with(".bak")
                || !backups.insert(backup.to_lowercase())
            {
                return Err(invalid("原始备份位置无效。"));
            }
        } else if entry.original_sha256.is_some() || entry.backup_relative_path.is_some() {
            return Err(invalid("新增文件不能有原始备份。"));
        }
        if let Some(hash) = &entry.installed_sha256 {
            validate_hash(hash)?;
        }
        if let Some(hash) = &entry.pending_sha256 {
            validate_hash(hash)?;
        }
        if (entry.installed_sha256.is_none() && entry.pending_sha256.is_none())
            || (entry.restoring
                && (entry.installed_sha256.is_none() || entry.pending_sha256.is_some()))
        {
            return Err(invalid("安装记录的待完成操作无效。"));
        }
    }
    if !paths.is_disjoint(&backups) {
        return Err(invalid("安装记录会覆盖原始备份。"));
    }
    let mut directories = HashSet::new();
    for directory in &journal.created_directories {
        let directory = normalize_relative(directory)?;
        safe_path(game, &directory)?;
        let key = directory.to_lowercase();
        if !directories.insert(key.clone()) || paths.contains(&key) || backups.contains(&key) {
            return Err(invalid("安装记录的新增目录无效。"));
        }
    }
    Ok(())
}

/// Rejects Windows aliases and traversal consistently on every supported host.
pub(crate) fn normalize_relative(relative: &str) -> Result<String, CoreError> {
    let relative = relative.replace('\\', "/");
    if relative.is_empty() || relative.starts_with('/') {
        return Err(invalid("文件路径必须为非空相对路径。"));
    }
    for part in relative.split('/') {
        if part.is_empty()
            || matches!(part, "." | "..")
            || part.ends_with([' ', '.'])
            || part
                .chars()
                .any(|character| character.is_control() || "<>:\"|?*".contains(character))
        {
            return Err(invalid(format!("不安全的相对路径：{relative}")));
        }
        let stem = part.split('.').next().unwrap_or_default().to_uppercase();
        if matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            || ((stem.starts_with("COM") || stem.starts_with("LPT"))
                && stem.chars().count() == 4
                && stem
                    .chars()
                    .nth(3)
                    .is_some_and(|character| "123456789¹²³".contains(character)))
        {
            return Err(invalid("路径包含保留文件名。"));
        }
    }
    if relative
        .split('/')
        .next()
        .is_some_and(|part| part.eq_ignore_ascii_case(".avg-translator"))
    {
        return Err(invalid("补丁不能修改安装器状态目录。"));
    }
    Ok(relative)
}

pub(crate) fn safe_path(root: &Path, relative: &str) -> Result<PathBuf, CoreError> {
    let relative = normalize_relative(relative)?;
    let root = std::path::absolute(root)?;
    let path = root.join(relative);
    check_path(&path)?;
    Ok(path)
}

/// Every existing ancestor is checked so junctions cannot redirect a mutation or recursive deletion.
pub(crate) fn check_path(path: &Path) -> Result<(), CoreError> {
    let absolute = std::path::absolute(path)?;
    for component in absolute.components() {
        if matches!(component, Component::ParentDir) {
            return Err(invalid("绝对路径包含上级目录。"));
        }
    }
    for ancestor in absolute.ancestors() {
        match fs::symlink_metadata(ancestor) {
            Ok(metadata) => {
                let linked = metadata.file_type().is_symlink();
                #[cfg(windows)]
                let linked = {
                    use std::os::windows::fs::MetadataExt;
                    linked || metadata.file_attributes() & 0x400 != 0
                };
                if linked {
                    return Err(invalid(format!(
                        "路径不允许符号链接或重解析点：{}",
                        ancestor.display()
                    )));
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                ) => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn directory_root(path: &Path) -> Result<PathBuf, CoreError> {
    let path = std::path::absolute(path)?;
    check_path(&path)?;
    if !path.is_dir() {
        return Err(invalid(format!("目录不存在：{}", path.display())));
    }
    Ok(path)
}

fn hash_if_present(path: &Path) -> Result<Option<String>, CoreError> {
    check_path(path)?;
    if path.is_dir() {
        return Err(invalid(format!("文件路径被目录占用：{}", path.display())));
    }
    match fs::metadata(path) {
        Ok(_) => Ok(Some(hash_file(path)?)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn ensure_hash(path: &Path, expected: Option<&str>) -> Result<(), CoreError> {
    if same_hash(hash_if_present(path)?.as_deref(), expected) {
        Ok(())
    } else {
        Err(invalid(format!(
            "文件校验不一致，已保留当前内容：{}",
            path.display()
        )))
    }
}

fn validate_hash(hash: &str) -> Result<(), CoreError> {
    if hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err(invalid("SHA-256 必须为 64 位十六进制字符串。"))
    }
}

fn same_hash(left: Option<&str>, right: Option<&str>) -> bool {
    match (left, right) {
        (None, None) => true,
        (Some(left), Some(right)) => left.eq_ignore_ascii_case(right),
        _ => false,
    }
}
fn same_relative(left: &str, right: &str) -> bool {
    left.replace('\\', "/")
        .eq_ignore_ascii_case(&right.replace('\\', "/"))
}
fn same_path(left: &Path, right: &Path) -> bool {
    if cfg!(windows) {
        left.to_string_lossy()
            .replace('\\', "/")
            .trim_end_matches(['/', '\\'])
            .eq_ignore_ascii_case(
                right
                    .to_string_lossy()
                    .replace('\\', "/")
                    .trim_end_matches(['/', '\\']),
            )
    } else {
        left == right
    }
}
fn paths_overlap(left: &Path, right: &Path) -> bool {
    let key = |path: &Path| {
        let value = path
            .to_string_lossy()
            .replace('\\', "/")
            .trim_end_matches('/')
            .to_owned();
        if cfg!(windows) {
            value.to_lowercase()
        } else {
            value
        }
    };
    let left = key(left);
    let right = key(right);
    left == right || left.starts_with(&(right.clone() + "/")) || right.starts_with(&(left + "/"))
}
fn invalid(message: impl Into<String>) -> CoreError {
    CoreError::InvalidData(message.into())
}
fn missing_original(entry: &InstallationEntry) -> CoreError {
    invalid(format!(
        "无法找到可用的原始备份：{}。游戏目录中的备份缺失或校验未通过；请恢复游戏原文件后重试。已保存的译文仍可继续使用。",
        entry.relative_path
    ))
}

fn choose_backup(
    game: &Path,
    relative: &str,
    reserved: &HashSet<String>,
) -> Result<String, CoreError> {
    let mut candidate = relative.to_owned() + ".bak";
    let mut suffix = 0_u32;
    while reserved.contains(&candidate.to_lowercase()) || safe_path(game, &candidate)?.exists() {
        suffix += 1;
        candidate = format!(
            "{relative}.{}.{suffix}.bak",
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        );
    }
    Ok(candidate)
}

fn atomic_copy(
    source: &Path,
    target: &Path,
    expected_hash: &str,
    expected_current: Option<&str>,
) -> Result<(), CoreError> {
    check_path(source)?;
    check_path(target)?;
    let parent = target.parent().ok_or_else(|| invalid("文件没有父目录。"))?;
    fs::create_dir_all(parent)?;
    check_path(target)?;
    let mut temporary = NamedTempFile::new_in(parent)?;
    let mut input = File::open(source)?;
    io::copy(&mut input, temporary.as_file_mut())?;
    temporary.flush()?;
    temporary.as_file().sync_all()?;
    ensure_hash(temporary.path(), Some(expected_hash))?;
    ensure_hash(target, expected_current)?;
    check_path(target)?;
    let original_permissions = if target.is_file() {
        Some(fs::metadata(target)?.permissions())
    } else {
        None
    };
    if original_permissions
        .as_ref()
        .is_some_and(|permissions| permissions.readonly())
    {
        make_writable(target)?;
    }
    if let Err(error) = temporary.persist(target) {
        if let Some(permissions) = original_permissions
            && target.is_file()
        {
            fs::set_permissions(target, permissions)?;
        }
        return Err(error.error.into());
    }
    Ok(())
}

fn move_original(backup: &Path, target: &Path, entry: &InstallationEntry) -> Result<(), CoreError> {
    ensure_hash(backup, entry.original_sha256.as_deref())?;
    let current = hash_if_present(target)?;
    if !same_hash(current.as_deref(), entry.installed_sha256.as_deref())
        && !same_hash(current.as_deref(), entry.original_sha256.as_deref())
        && current.is_some()
    {
        return Err(invalid("恢复目标已被其他程序修改。"));
    }
    let permissions = if target.is_file() {
        Some(fs::metadata(target)?.permissions())
    } else {
        None
    };
    if permissions
        .as_ref()
        .is_some_and(|permissions| permissions.readonly())
    {
        make_writable(target)?;
    }
    check_path(backup)?;
    check_path(target)?;
    if let Err(error) = fs::rename(backup, target) {
        if let Some(permissions) = permissions
            && target.is_file()
        {
            fs::set_permissions(target, permissions)?;
        }
        return Err(error.into());
    }
    Ok(())
}

fn make_writable(path: &Path) -> Result<(), CoreError> {
    let mut permissions = fs::metadata(path)?.permissions();
    #[cfg(windows)]
    #[expect(
        clippy::permissions_set_readonly_false,
        reason = "This Windows-only branch clears the read-only attribute; Unix sets only the owner's write bit below."
    )]
    permissions.set_readonly(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        permissions.set_mode(permissions.mode() | 0o200);
    }
    fs::set_permissions(path, permissions)?;
    Ok(())
}

fn remove_file(path: &Path) -> Result<(), CoreError> {
    check_path(path)?;
    if path.is_file() {
        if fs::metadata(path)?.permissions().readonly() {
            make_writable(path)?;
        }
        fs::remove_file(path)?;
    }
    Ok(())
}

fn collect_files(root: &Path, directory: &Path, output: &mut Vec<String>) -> Result<(), CoreError> {
    check_path(directory)?;
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        check_path(&entry.path())?;
        if entry.file_type()?.is_dir() {
            collect_files(root, &entry.path(), output)?;
        } else if entry.file_type()?.is_file() {
            output.push(
                entry
                    .path()
                    .strip_prefix(root)
                    .map_err(|error| invalid(error.to_string()))?
                    .to_string_lossy()
                    .replace('\\', "/"),
            );
        }
    }
    Ok(())
}

/// Checks processes only for an operation that can change game files.
pub fn ensure_game_closed(game_directory: &Path) -> Result<(), CoreError> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        use std::process::{Command, Stdio};
        let root = game_directory
            .to_string_lossy()
            .trim_end_matches(['/', '\\'])
            .to_owned()
            + "\\";
        let output = Command::new("powershell.exe").creation_flags(0x08000000).args(["-NoProfile", "-NonInteractive", "-Command", "$ErrorActionPreference='Stop'; $root=[Environment]::GetEnvironmentVariable('GAMEL10N_INSTALL_GAME_ROOT'); Get-CimInstance Win32_Process | Where-Object { $_.ExecutablePath -and $_.ExecutablePath.StartsWith($root,[StringComparison]::OrdinalIgnoreCase) } | Select-Object -ExpandProperty Name"]).env("GAMEL10N_INSTALL_GAME_ROOT", root).stdin(Stdio::null()).output()?;
        if !output.status.success() {
            return Err(invalid("无法检查游戏进程，请确认系统进程查询可用后重试。"));
        }
        let processes = String::from_utf8_lossy(&output.stdout);
        if !processes.trim().is_empty() {
            return Err(invalid(format!(
                "游戏或汉化启动器仍在运行：{}。请关闭后重试。",
                processes.trim()
            )));
        }
    }
    #[cfg(target_os = "linux")]
    {
        for entry in fs::read_dir("/proc")? {
            let entry = entry?;
            if entry
                .file_name()
                .to_string_lossy()
                .bytes()
                .all(|byte| byte.is_ascii_digit())
                && let Ok(executable) = fs::read_link(entry.path().join("exe"))
                && executable.starts_with(game_directory)
            {
                return Err(invalid("游戏或汉化启动器仍在运行，请关闭后重试。"));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture {
        _root: tempfile::TempDir,
        game: PathBuf,
        patch: PathBuf,
        payload: PathBuf,
        installer: PatchInstaller,
    }
    impl Fixture {
        fn new() -> Self {
            let root = tempfile::tempdir().unwrap();
            let game = root.path().join("game");
            let patch = root.path().join("project/patches/fixture");
            let payload = patch.join(PAYLOAD_DIRECTORY);
            fs::create_dir_all(&game).unwrap();
            fs::create_dir_all(&payload).unwrap();
            let installer = PatchInstaller::new(root.path().join("project"));
            Self {
                _root: root,
                game,
                patch,
                payload,
                installer,
            }
        }
        fn prepare(&self) {
            fs::write(self.game.join("data.bin"), b"original").unwrap();
            fs::write(self.payload.join("data.bin"), b"translated").unwrap();
            build_manifest(
                &self.patch,
                &BTreeMap::from([("data.bin".into(), hash_text("original"))]),
            )
            .unwrap();
        }
        fn entry(&self) -> InstallationEntry {
            InstallationEntry {
                relative_path: "data.bin".into(),
                original_existed: true,
                original_sha256: Some(hash_text("original")),
                original_read_only: false,
                original_creation_time_utc: None,
                original_last_write_time_utc: None,
                backup_relative_path: Some("data.bin.bak".into()),
                installed_sha256: None,
                pending_sha256: Some(hash_text("translated")),
                restoring: false,
                extra: BTreeMap::new(),
            }
        }
        fn save_entry(&self, entry: InstallationEntry) {
            self.installer
                .save_journal(
                    &self.game,
                    &InstallationJournal {
                        schema_version: 1,
                        game_directory: Some(self.game.clone()),
                        entries: vec![entry],
                        created_directories: Vec::new(),
                        extra: BTreeMap::new(),
                    },
                )
                .unwrap();
        }
    }

    #[test]
    fn game_files_keep_game_relative_paths_and_exclude_patch_records() {
        let fixture = Fixture::new();
        fixture.prepare();
        fs::create_dir(fixture.payload.join("Data")).unwrap();
        fs::write(fixture.payload.join("Data/scene.bin"), b"translated scene").unwrap();
        fs::write(fixture.patch.join("translation-report.json"), b"report").unwrap();
        fs::write(fixture.patch.join("translation-snapshot.json"), b"snapshot").unwrap();
        fixture
            .installer
            .record_runtime_test(&fixture.patch, "passed", "fixture")
            .unwrap();
        let manifest = build_manifest(&fixture.patch, &BTreeMap::new()).unwrap();
        assert_eq!(manifest.schema_version, 2);
        assert_eq!(
            manifest
                .files
                .iter()
                .map(|file| file.relative_path.as_str())
                .collect::<Vec<_>>(),
            ["Data/scene.bin", "data.bin"]
        );

        let installed = fixture
            .installer
            .install(&fixture.game, &fixture.patch)
            .unwrap();
        assert_eq!(installed.installed_files, 2);
        assert_eq!(
            fs::read(fixture.game.join("data.bin")).unwrap(),
            b"translated"
        );
        assert_eq!(
            fs::read(fixture.game.join("Data/scene.bin")).unwrap(),
            b"translated scene"
        );
        assert_eq!(fs::read_dir(&fixture.game).unwrap().count(), 3);
        assert!(!fixture.game.join(PAYLOAD_DIRECTORY).exists());
        assert!(!fixture.game.join(MANIFEST_FILENAME).exists());
    }

    #[test]
    fn flat_patch_layout_requires_regeneration_without_touching_game_files() {
        let fixture = Fixture::new();
        fixture.prepare();
        let mut manifest = read_manifest(&fixture.patch).unwrap();
        manifest.schema_version = 1;
        fs::rename(
            fixture.payload.join("data.bin"),
            fixture.patch.join("data.bin"),
        )
        .unwrap();
        write_json(&fixture.patch.join(MANIFEST_FILENAME), &manifest).unwrap();

        let error = fixture
            .installer
            .install(&fixture.game, &fixture.patch)
            .unwrap_err();
        assert!(error.to_string().contains("请重新生成补丁"));
        assert_eq!(
            fs::read(fixture.game.join("data.bin")).unwrap(),
            b"original"
        );
        assert!(!fixture.game.join("data.bin.bak").exists());
        assert!(!fixture.installer.journal_path().exists());
    }

    #[test]
    fn install_and_restore_preserve_dates_consume_backups_and_remove_added_files() {
        let fixture = Fixture::new();
        fixture.prepare();
        fs::write(fixture.game.join("unchanged.bin"), b"same").unwrap();
        fs::write(fixture.payload.join("unchanged.bin"), b"same").unwrap();
        fs::write(fixture.payload.join("added.bin"), b"new").unwrap();
        let modified = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_600_000_000);
        OpenOptions::new()
            .write(true)
            .open(fixture.game.join("data.bin"))
            .unwrap()
            .set_times(FileTimes::new().set_modified(modified))
            .unwrap();
        let created = fs::metadata(fixture.game.join("data.bin"))
            .unwrap()
            .created()
            .ok();
        build_manifest(&fixture.patch, &BTreeMap::new()).unwrap();
        let installed = fixture
            .installer
            .install(&fixture.game, &fixture.patch)
            .unwrap();
        assert_eq!(
            (
                installed.installed_files,
                installed.unchanged_files,
                installed.backup_files.len()
            ),
            (2, 1, 1)
        );
        let again = fixture
            .installer
            .install(&fixture.game, &fixture.patch)
            .unwrap();
        assert_eq!((again.installed_files, again.unchanged_files), (0, 3));
        let restored = fixture.installer.restore(&fixture.game).unwrap();
        assert_eq!(
            (
                restored.restored_files,
                restored.removed_files,
                restored.conflicts.len()
            ),
            (1, 1, 0)
        );
        assert_eq!(
            fs::read(fixture.game.join("data.bin")).unwrap(),
            b"original"
        );
        assert_eq!(
            fs::metadata(fixture.game.join("data.bin"))
                .unwrap()
                .modified()
                .unwrap(),
            modified
        );
        assert_eq!(
            fs::metadata(fixture.game.join("data.bin"))
                .unwrap()
                .created()
                .ok(),
            created
        );
        assert!(!fixture.game.join("data.bin.bak").exists());
        assert!(!fixture.game.join("unchanged.bin.bak").exists());
        assert!(!fixture.game.join("added.bin").exists());
        assert!(
            fixture
                .installer
                .read_journal(&fixture.game)
                .unwrap()
                .entries
                .is_empty()
        );
    }

    #[test]
    fn updating_added_files_keeps_their_origin_and_restores_a_clean_game_directory() {
        let fixture = Fixture::new();
        fixture.prepare();
        fs::create_dir(fixture.game.join("existing-empty")).unwrap();
        fs::create_dir_all(fixture.payload.join("patch-data/fonts")).unwrap();
        fs::write(fixture.payload.join("patch-data/fonts/added.bin"), b"first").unwrap();
        build_manifest(&fixture.patch, &BTreeMap::new()).unwrap();
        fixture
            .installer
            .install(&fixture.game, &fixture.patch)
            .unwrap();

        fs::write(
            fixture.payload.join("patch-data/fonts/added.bin"),
            b"updated",
        )
        .unwrap();
        fs::create_dir(fixture.payload.join("existing-empty")).unwrap();
        fs::write(fixture.payload.join("existing-empty/added.bin"), b"new").unwrap();
        build_manifest(&fixture.patch, &BTreeMap::new()).unwrap();
        let installer = PatchInstaller::new(fixture.installer.project_directory.clone());
        let updated = installer.install(&fixture.game, &fixture.patch).unwrap();
        assert!(updated.backup_files.is_empty());
        let journal: Value = read_json(&installer.journal_path()).unwrap();
        let added = journal["entries"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["relativePath"] == "patch-data/fonts/added.bin")
            .unwrap();
        assert_eq!(added["originalExisted"], false);
        assert!(added["originalSha256"].is_null());
        assert!(added["backupRelativePath"].is_null());

        let restored = installer.restore(&fixture.game).unwrap();
        assert!(restored.conflicts.is_empty());
        assert_eq!((restored.restored_files, restored.removed_files), (1, 2));
        assert_eq!(
            fs::read(fixture.game.join("data.bin")).unwrap(),
            b"original"
        );
        assert!(!fixture.game.join("data.bin.bak").exists());
        assert!(!fixture.game.join("patch-data").exists());
        assert!(fixture.game.join("existing-empty").is_dir());
        assert_eq!(fs::read_dir(&fixture.game).unwrap().count(), 2);
    }

    #[test]
    fn missing_targets_ignore_build_baselines_and_remain_added_across_reinstalls() {
        let fixture = Fixture::new();
        fs::write(fixture.payload.join("font.otf"), b"font patch").unwrap();
        let originals = BTreeMap::from([("font.otf".into(), hash_text("previous source"))]);
        build_manifest(&fixture.patch, &originals).unwrap();

        for _ in 0..2 {
            let installed = fixture
                .installer
                .install(&fixture.game, &fixture.patch)
                .unwrap();
            assert_eq!(installed.installed_files, 1);
            assert!(installed.backup_files.is_empty());
            assert!(
                !fixture
                    .installer
                    .read_journal(&fixture.game)
                    .unwrap()
                    .entries[0]
                    .original_existed
            );
            assert_eq!(
                fixture
                    .installer
                    .install(&fixture.game, &fixture.patch)
                    .unwrap()
                    .unchanged_files,
                1
            );
            fs::remove_file(fixture.game.join("font.otf")).unwrap();
        }
        fixture
            .installer
            .install(&fixture.game, &fixture.patch)
            .unwrap();
        fs::write(fixture.payload.join("font.otf"), b"updated font").unwrap();
        build_manifest(&fixture.patch, &originals).unwrap();
        let updated = fixture
            .installer
            .install(&fixture.game, &fixture.patch)
            .unwrap();
        assert!(updated.backup_files.is_empty());
        assert_eq!(
            fs::read(fixture.game.join("font.otf")).unwrap(),
            b"updated font"
        );

        let restored = fixture.installer.restore(&fixture.game).unwrap();
        assert!(restored.conflicts.is_empty());
        assert_eq!(restored.removed_files, 1);
        assert_eq!(fs::read_dir(&fixture.game).unwrap().count(), 0);
    }

    #[test]
    fn missing_installed_targets_can_be_replaced_without_losing_original_backup_records() {
        for keep_backup in [false, true] {
            let fixture = Fixture::new();
            fixture.prepare();
            fixture
                .installer
                .install(&fixture.game, &fixture.patch)
                .unwrap();
            fs::remove_file(fixture.game.join("data.bin")).unwrap();
            if !keep_backup {
                fs::remove_file(fixture.game.join("data.bin.bak")).unwrap();
            }
            let installed = fixture
                .installer
                .install(&fixture.game, &fixture.patch)
                .unwrap();
            assert_eq!(installed.installed_files, 1);
            assert!(installed.backup_files.is_empty());
            let journal = fixture.installer.read_journal(&fixture.game).unwrap();
            assert!(journal.entries[0].original_existed);
            assert_eq!(
                journal.entries[0].original_sha256,
                Some(hash_text("original"))
            );
            assert_eq!(
                fs::read(fixture.game.join("data.bin")).unwrap(),
                b"translated"
            );

            if keep_backup {
                let restored = fixture.installer.restore(&fixture.game).unwrap();
                assert!(restored.conflicts.is_empty());
                assert_eq!(
                    fs::read(fixture.game.join("data.bin")).unwrap(),
                    b"original"
                );
            } else {
                assert!(fixture.installer.restore(&fixture.game).is_err());
                assert_eq!(
                    fs::read(fixture.game.join("data.bin")).unwrap(),
                    b"translated"
                );
            }
        }
    }

    #[test]
    fn interrupted_added_file_updates_and_removals_resume_without_backups() {
        let fixture = Fixture::new();
        fs::create_dir_all(fixture.payload.join("patch-data/fonts")).unwrap();
        fs::write(fixture.payload.join("patch-data/fonts/added.bin"), b"new").unwrap();
        build_manifest(&fixture.patch, &BTreeMap::new()).unwrap();
        fixture
            .installer
            .install(&fixture.game, &fixture.patch)
            .unwrap();
        fs::write(
            fixture.payload.join("patch-data/fonts/added.bin"),
            b"updated",
        )
        .unwrap();
        build_manifest(&fixture.patch, &BTreeMap::new()).unwrap();
        let mut journal = fixture.installer.read_journal(&fixture.game).unwrap();
        journal.entries[0].pending_sha256 = Some(hash_text("updated"));
        fixture
            .installer
            .save_journal(&fixture.game, &journal)
            .unwrap();
        fs::remove_file(fixture.game.join("patch-data/fonts/added.bin")).unwrap();
        let updated = fixture
            .installer
            .install(&fixture.game, &fixture.patch)
            .unwrap();
        assert!(updated.backup_files.is_empty());
        assert_eq!(
            fs::read(fixture.game.join("patch-data/fonts/added.bin")).unwrap(),
            b"updated"
        );

        let mut journal = fixture.installer.read_journal(&fixture.game).unwrap();
        journal.entries[0].restoring = true;
        fixture
            .installer
            .save_journal(&fixture.game, &journal)
            .unwrap();
        fs::remove_file(fixture.game.join("patch-data/fonts/added.bin")).unwrap();
        fs::write(fixture.game.join("patch-data/user.txt"), b"keep").unwrap();

        let restored = fixture.installer.restore(&fixture.game).unwrap();
        assert!(restored.conflicts.is_empty());
        assert!(!fixture.game.join("patch-data/fonts").exists());
        assert_eq!(
            fs::read(fixture.game.join("patch-data/user.txt")).unwrap(),
            b"keep"
        );
        assert!(
            fixture
                .installer
                .read_journal(&fixture.game)
                .unwrap()
                .entries
                .is_empty()
        );
    }

    #[test]
    fn legacy_null_original_hash_still_identifies_a_patch_added_file() {
        let fixture = Fixture::new();
        fs::write(fixture.game.join("added.bin"), b"new").unwrap();
        write_json(
            &fixture.installer.journal_path(),
            &serde_json::json!({
                "schemaVersion":1,
                "entries":[{
                    "relativePath":"added.bin",
                    "originalSha256":null,
                    "backupRelativePath":null,
                    "installedSha256":hash_text("new")
                }]
            }),
        )
        .unwrap();
        let restored = fixture.installer.restore(&fixture.game).unwrap();
        assert!(restored.conflicts.is_empty());
        assert_eq!(restored.removed_files, 1);
        assert_eq!(fs::read_dir(&fixture.game).unwrap().count(), 0);
    }

    #[test]
    fn missing_origin_metadata_does_not_authorize_deleting_an_existing_file() {
        let fixture = Fixture::new();
        fs::write(fixture.game.join("existing.bin"), b"keep").unwrap();
        write_json(
            &fixture.installer.journal_path(),
            &serde_json::json!({
                "schemaVersion":1,
                "entries":[{
                    "relativePath":"existing.bin",
                    "installedSha256":hash_text("keep")
                }]
            }),
        )
        .unwrap();
        assert!(fixture.installer.restore(&fixture.game).is_err());
        assert_eq!(
            fs::read(fixture.game.join("existing.bin")).unwrap(),
            b"keep"
        );
    }

    #[test]
    fn reinstall_after_game_reset_refreshes_backups_and_restores_the_reinstalled_original() {
        for keep_backup in [false, true] {
            let fixture = Fixture::new();
            fixture.prepare();
            fs::write(fixture.payload.join("added.bin"), b"new").unwrap();
            build_manifest(
                &fixture.patch,
                &BTreeMap::from([("data.bin".into(), hash_text("original"))]),
            )
            .unwrap();
            fixture
                .installer
                .install(&fixture.game, &fixture.patch)
                .unwrap();

            // A reinstall replaces patched resources and removes files added by the patch.
            fs::remove_file(fixture.game.join("data.bin")).unwrap();
            fs::remove_file(fixture.game.join("added.bin")).unwrap();
            if !keep_backup {
                fs::remove_file(fixture.game.join("data.bin.bak")).unwrap();
            }
            fs::write(fixture.game.join("data.bin"), b"original").unwrap();
            let modified = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
            OpenOptions::new()
                .write(true)
                .open(fixture.game.join("data.bin"))
                .unwrap()
                .set_times(FileTimes::new().set_modified(modified))
                .unwrap();
            let created = fs::metadata(fixture.game.join("data.bin"))
                .unwrap()
                .created()
                .ok();

            let result = fixture
                .installer
                .install(&fixture.game, &fixture.patch)
                .unwrap();
            assert_eq!(result.installed_files, 2);
            assert_eq!(result.backup_files.len(), usize::from(!keep_backup));
            assert_eq!(
                fs::read(fixture.game.join("data.bin")).unwrap(),
                b"translated"
            );
            assert_eq!(
                fs::read(fixture.game.join("data.bin.bak")).unwrap(),
                b"original"
            );
            assert_eq!(fs::read_dir(&fixture.game).unwrap().count(), 3);
            assert_eq!(
                fixture
                    .installer
                    .install(&fixture.game, &fixture.patch)
                    .unwrap()
                    .unchanged_files,
                2
            );

            let restored = fixture.installer.restore(&fixture.game).unwrap();
            assert!(restored.conflicts.is_empty());
            assert_eq!((restored.restored_files, restored.removed_files), (1, 1));
            assert_eq!(
                fs::read(fixture.game.join("data.bin")).unwrap(),
                b"original"
            );
            let metadata = fs::metadata(fixture.game.join("data.bin")).unwrap();
            assert_eq!(metadata.modified().unwrap(), modified);
            assert_eq!(metadata.created().ok(), created);
            assert_eq!(fs::read_dir(&fixture.game).unwrap().count(), 1);
            assert!(
                fixture
                    .installer
                    .read_journal(&fixture.game)
                    .unwrap()
                    .entries
                    .is_empty()
            );
        }
    }

    #[test]
    fn invalid_payload_does_not_change_game_or_create_backups() {
        let fixture = Fixture::new();
        fixture.prepare();
        fs::write(fixture.payload.join("data.bin"), b"tampered").unwrap();
        assert!(
            fixture
                .installer
                .install(&fixture.game, &fixture.patch)
                .is_err()
        );
        assert_eq!(
            fs::read(fixture.game.join("data.bin")).unwrap(),
            b"original"
        );
        assert!(!fixture.game.join("data.bin.bak").exists());
        assert!(!fixture.installer.journal_path().exists());
    }

    #[test]
    fn replacement_installs_back_up_current_bytes_instead_of_requiring_build_baseline() {
        let fixture = Fixture::new();
        fixture.prepare();
        fs::write(fixture.game.join("data.bin"), b"game update").unwrap();

        let installed = fixture
            .installer
            .install(&fixture.game, &fixture.patch)
            .unwrap();
        assert_eq!(installed.installed_files, 1);
        assert_eq!(installed.backup_files, ["data.bin.bak"]);
        assert_eq!(
            fs::read(fixture.game.join("data.bin")).unwrap(),
            b"translated"
        );
        assert_eq!(
            fs::read(fixture.game.join("data.bin.bak")).unwrap(),
            b"game update"
        );
        assert_eq!(
            fixture
                .installer
                .read_journal(&fixture.game)
                .unwrap()
                .entries[0]
                .original_sha256,
            Some(hash_text("game update"))
        );

        fs::write(fixture.payload.join("data.bin"), b"updated translation").unwrap();
        build_manifest(
            &fixture.patch,
            &BTreeMap::from([("data.bin".into(), hash_text("another source version"))]),
        )
        .unwrap();
        let updated = fixture
            .installer
            .install(&fixture.game, &fixture.patch)
            .unwrap();
        assert_eq!(updated.installed_files, 1);
        assert!(updated.backup_files.is_empty());
        assert_eq!(
            fs::read(fixture.game.join("data.bin")).unwrap(),
            b"updated translation"
        );

        let restored = fixture.installer.restore(&fixture.game).unwrap();
        assert!(restored.conflicts.is_empty());
        assert_eq!(restored.restored_files, 1);
        assert_eq!(
            fs::read(fixture.game.join("data.bin")).unwrap(),
            b"game update"
        );
        assert!(!fixture.game.join("data.bin.bak").exists());
    }

    #[test]
    fn recovery_rolls_back_interruption_after_original_was_moved() {
        let fixture = Fixture::new();
        fixture.prepare();
        fixture.save_entry(fixture.entry());
        fs::rename(
            fixture.game.join("data.bin"),
            fixture.game.join("data.bin.bak"),
        )
        .unwrap();
        assert!(fixture.installer.recover(&fixture.game).unwrap().is_empty());
        assert_eq!(
            fs::read(fixture.game.join("data.bin")).unwrap(),
            b"original"
        );
        assert!(!fixture.game.join("data.bin.bak").exists());
        assert!(
            fixture
                .installer
                .read_journal(&fixture.game)
                .unwrap()
                .entries
                .is_empty()
        );
        assert_eq!(
            fixture
                .installer
                .install(&fixture.game, &fixture.patch)
                .unwrap()
                .installed_files,
            1
        );
    }

    #[test]
    fn retry_commits_pending_payload_and_restore_finishes_already_moved_backup() {
        let fixture = Fixture::new();
        fixture.prepare();
        fixture.save_entry(fixture.entry());
        fs::rename(
            fixture.game.join("data.bin"),
            fixture.game.join("data.bin.bak"),
        )
        .unwrap();
        fs::copy(
            fixture.payload.join("data.bin"),
            fixture.game.join("data.bin"),
        )
        .unwrap();
        assert_eq!(
            fixture
                .installer
                .install(&fixture.game, &fixture.patch)
                .unwrap()
                .unchanged_files,
            1
        );
        let mut journal = fixture.installer.read_journal(&fixture.game).unwrap();
        journal.entries[0].restoring = true;
        fixture
            .installer
            .save_journal(&fixture.game, &journal)
            .unwrap();
        fs::rename(
            fixture.game.join("data.bin.bak"),
            fixture.game.join("data.bin"),
        )
        .unwrap();
        assert!(fixture.installer.recover(&fixture.game).unwrap().is_empty());
        assert!(
            fixture
                .installer
                .read_journal(&fixture.game)
                .unwrap()
                .entries
                .is_empty()
        );
    }

    #[test]
    fn install_and_restore_keep_external_changes_and_report_conflict() {
        let fixture = Fixture::new();
        fixture.prepare();
        fixture
            .installer
            .install(&fixture.game, &fixture.patch)
            .unwrap();
        fs::write(fixture.game.join("data.bin"), b"external change").unwrap();
        let journal = fs::read(fixture.installer.journal_path()).unwrap();
        assert!(
            fixture
                .installer
                .install(&fixture.game, &fixture.patch)
                .unwrap_err()
                .to_string()
                .contains("已安装文件被其他程序修改")
        );
        assert_eq!(fs::read(fixture.installer.journal_path()).unwrap(), journal);
        assert_eq!(
            fixture.installer.restore(&fixture.game).unwrap().conflicts,
            ["data.bin"]
        );
        assert_eq!(
            fs::read(fixture.game.join("data.bin")).unwrap(),
            b"external change"
        );
        assert_eq!(
            fs::read(fixture.game.join("data.bin.bak")).unwrap(),
            b"original"
        );
    }

    #[test]
    fn original_resolver_reads_backup_when_installed_target_is_missing() {
        let fixture = Fixture::new();
        fixture.prepare();
        fixture
            .installer
            .install(&fixture.game, &fixture.patch)
            .unwrap();
        fs::remove_file(fixture.game.join("data.bin")).unwrap();
        let journal = fs::read(fixture.installer.journal_path()).unwrap();
        let backup = fixture.game.join("data.bin.bak");

        assert_eq!(
            fixture
                .installer
                .resolve_original_file(&fixture.game, "data.bin")
                .unwrap(),
            backup
        );
        assert!(!fixture.game.join("data.bin").exists());
        assert_eq!(fs::read(backup).unwrap(), b"original");
        assert_eq!(fs::read(fixture.installer.journal_path()).unwrap(), journal);
    }

    #[test]
    fn missing_or_changed_backup_does_not_restore_from_working_sources() {
        for changed in [false, true] {
            let fixture = Fixture::new();
            fixture.prepare();
            fixture
                .installer
                .install(&fixture.game, &fixture.patch)
                .unwrap();
            let backup = fixture.game.join("data.bin.bak");
            if changed {
                fs::write(&backup, b"external backup").unwrap();
            } else {
                fs::remove_file(&backup).unwrap();
            }
            let source = fixture
                .installer
                .project_directory
                .join("sources")
                .join("a".repeat(64));
            fs::create_dir_all(&source).unwrap();
            fs::write(source.join("data.bin"), b"original").unwrap();
            let journal = fs::read(fixture.installer.journal_path()).unwrap();

            let error = fixture.installer.restore(&fixture.game).unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("游戏目录中的备份缺失或校验未通过")
            );
            assert_eq!(
                fs::read(fixture.game.join("data.bin")).unwrap(),
                b"translated"
            );
            assert_eq!(fs::read(source.join("data.bin")).unwrap(), b"original");
            assert_eq!(fs::read(fixture.installer.journal_path()).unwrap(), journal);
            if changed {
                assert_eq!(fs::read(&backup).unwrap(), b"external backup");
            } else {
                assert!(!backup.exists());
            }
        }
    }

    #[test]
    fn restore_uses_a_verified_alternate_backup_in_the_game_directory() {
        let fixture = Fixture::new();
        fixture.prepare();
        let original_metadata = fs::metadata(fixture.game.join("data.bin")).unwrap();
        let original_created = original_metadata.created().ok();
        let original_modified = original_metadata.modified().unwrap();
        fixture
            .installer
            .install(&fixture.game, &fixture.patch)
            .unwrap();
        let backup = fixture.game.join("data.bin.previous.bak");
        fs::rename(fixture.game.join("data.bin.bak"), &backup).unwrap();
        fs::write(fixture.game.join("data.bin.bak"), b"external backup").unwrap();
        assert_eq!(
            fixture
                .installer
                .restore(&fixture.game)
                .unwrap()
                .restored_files,
            1
        );
        assert_eq!(
            fs::read(fixture.game.join("data.bin")).unwrap(),
            b"original"
        );
        assert_eq!(
            fs::read(fixture.game.join("data.bin.bak")).unwrap(),
            b"external backup"
        );
        assert_eq!(fs::read_dir(&fixture.game).unwrap().count(), 2);
        assert!(!backup.exists());
        assert_eq!(
            fs::metadata(fixture.game.join("data.bin"))
                .unwrap()
                .created()
                .ok(),
            original_created
        );
        assert_eq!(
            fs::metadata(fixture.game.join("data.bin"))
                .unwrap()
                .modified()
                .unwrap(),
            original_modified
        );
    }

    #[test]
    fn path_validation_rejects_traversal_aliases_and_case_duplicates() {
        for path in [
            "../outside",
            "a/../../outside",
            "C:/outside",
            "a\\..\\outside",
            "NUL.txt",
            "a. /b",
            ".avg-translator/state",
        ] {
            assert!(normalize_relative(path).is_err(), "{path}");
        }
        let mut manifest = PatchManifest {
            schema_version: 2,
            mode: BuildMode::Replace,
            required_sources: BTreeMap::new(),
            files: vec![
                PatchFile {
                    relative_path: "Data.bin".into(),
                    sha256: hash_text("a"),
                    expected_original_sha256: None,
                },
                PatchFile {
                    relative_path: "data.bin".into(),
                    sha256: hash_text("b"),
                    expected_original_sha256: None,
                },
            ],
        };
        assert!(validate_manifest(&mut manifest).is_err());
    }

    #[test]
    fn runtime_observations_are_recorded_separately_from_structural_manifest() {
        let fixture = Fixture::new();
        fixture.prepare();
        let before = fs::read(fixture.patch.join(MANIFEST_FILENAME)).unwrap();
        fixture
            .installer
            .record_runtime_test(&fixture.patch, "failed", "菜单正常，场景文字乱码")
            .unwrap();
        assert_eq!(
            fixture
                .installer
                .runtime_test(&fixture.patch)
                .unwrap()
                .unwrap()
                .status,
            "failed"
        );
        assert_eq!(
            fs::read(fixture.patch.join(MANIFEST_FILENAME)).unwrap(),
            before
        );
    }

    #[test]
    fn read_only_original_is_restored_with_its_read_only_attribute() {
        let fixture = Fixture::new();
        fixture.prepare();
        let path = fixture.game.join("data.bin");
        let mut permissions = fs::metadata(&path).unwrap().permissions();
        permissions.set_readonly(true);
        fs::set_permissions(&path, permissions).unwrap();
        fixture
            .installer
            .install(&fixture.game, &fixture.patch)
            .unwrap();
        fixture.installer.restore(&fixture.game).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"original");
        assert!(fs::metadata(&path).unwrap().permissions().readonly());
        assert!(!fixture.game.join("data.bin.bak").exists());
        make_writable(&path).unwrap();
    }

    #[test]
    fn cancelled_install_does_not_move_originals_or_write_a_journal() {
        let fixture = Fixture::new();
        fixture.prepare();
        let cancellation = crate::CancellationToken::new();
        cancellation.cancel();
        assert!(
            fixture
                .installer
                .clone()
                .with_cancellation(cancellation)
                .install(&fixture.game, &fixture.patch)
                .is_err()
        );
        assert_eq!(
            fs::read(fixture.game.join("data.bin")).unwrap(),
            b"original"
        );
        assert!(!fixture.installer.journal_path().exists());
    }
}
