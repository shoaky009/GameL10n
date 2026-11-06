use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use gamel10n_translation::files::{hash_text, read_json, write_json};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::Mutex;

use crate::CoreError;

/// Full project metadata; unrecognized fields are retained when saving.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GameProject {
    pub schema_version: u32,
    pub id: String,
    pub game_directory: PathBuf,
    pub name: String,
    #[serde(default)]
    pub settings: Value,
    #[serde(default)]
    pub is_removed: bool,
    #[serde(default, flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl GameProject {
    pub(crate) fn mark_updated(&mut self) {
        self.extra.insert(
            "updatedAt".into(),
            Value::String(chrono::Utc::now().to_rfc3339()),
        );
    }
}

/// Lightweight project list data. Opening the list never opens extraction payloads.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectSummary {
    pub id: String,
    pub game_directory: PathBuf,
    pub name: String,
    #[serde(default, flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl ProjectSummary {
    pub(crate) fn updated_at(&self) -> Option<chrono::DateTime<chrono::FixedOffset>> {
        self.extra
            .get("updatedAt")
            .and_then(Value::as_str)
            .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
    }

    pub fn extracted_unit_count(&self) -> Option<u64> {
        self.extra
            .get("analysis")
            .and_then(|analysis| analysis.get("unitCount"))
            .and_then(Value::as_u64)
    }
}

impl From<&GameProject> for ProjectSummary {
    fn from(project: &GameProject) -> Self {
        let mut extra = project.extra.clone();
        if let Some(id) = project.settings.get("analysisProfileId") {
            extra.insert("analysisProfileId".into(), id.clone());
        }
        if let Some(ids) = project.settings.get("translationProfileIds") {
            extra.insert("translationProfileIds".into(), ids.clone());
        }
        Self {
            id: project.id.clone(),
            game_directory: project.game_directory.clone(),
            name: project.name.clone(),
            extra,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProjectIndex {
    schema_version: u32,
    projects: Vec<ProjectSummary>,
    #[serde(default, flatten)]
    extra: BTreeMap<String, Value>,
}

pub(crate) struct ProjectStore {
    root: PathBuf,
    index: Mutex<ProjectIndex>,
    write_lease: Mutex<Option<fs::File>>,
}

impl ProjectStore {
    pub async fn open(root: PathBuf) -> Result<Self, CoreError> {
        let root = std::path::absolute(root)?;
        let index_root = root.clone();
        let index = tokio::task::spawn_blocking(move || load_index(&index_root)).await??;
        Ok(Self {
            root,
            index: Mutex::new(index),
            write_lease: Mutex::new(None),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub async fn reload(&self) -> Result<(), CoreError> {
        let root = self.root.clone();
        *self.index.lock().await = tokio::task::spawn_blocking(move || load_index(&root)).await??;
        Ok(())
    }

    pub async fn ensure_writable(&self) -> Result<(), CoreError> {
        let mut lease = self.write_lease.lock().await;
        if lease.is_some() {
            return Ok(());
        }
        let root = self.root.clone();
        let (file, index) =
            tokio::task::spawn_blocking(move || -> Result<(fs::File, ProjectIndex), CoreError> {
                fs::create_dir_all(&root)?;
                let mut options = fs::OpenOptions::new();
                options.read(true).write(true).create(true).truncate(false);
                #[cfg(windows)]
                {
                    use std::os::windows::fs::OpenOptionsExt;
                    options.share_mode(1); // FILE_SHARE_READ: compatible with the existing app's writer exclusion.
                }
                let file = options.open(root.join(".app.lock")).map_err(|error| {
                    if matches!(error.raw_os_error(), Some(32 | 33)) {
                        CoreError::InvalidData(
                            "数据目录正被其他程序写入，请先关闭另一个程序。".into(),
                        )
                    } else {
                        CoreError::Io(error)
                    }
                })?;
                file.try_lock().map_err(|error| {
                    CoreError::InvalidData(format!("无法取得数据目录写入锁：{error}"))
                })?;
                let index = load_index(&root)?;
                if root.join("games/index-pending.json").exists() {
                    persist_index(&root, &index, &pending_index_ids(&root)?)?;
                }
                Ok((file, index))
            })
            .await??;
        // A read-only host may have observed an older index before the previous writer exited.
        *self.index.lock().await = index;
        *lease = Some(file);
        Ok(())
    }

    pub fn directory(&self, id: &str) -> Result<PathBuf, CoreError> {
        validate_id(id)?;
        Ok(self.root.join("games").join(id))
    }

    pub async fn list(&self, query: &str) -> Vec<ProjectSummary> {
        let query = query.to_lowercase();
        self.index
            .lock()
            .await
            .projects
            .iter()
            .filter(|project| {
                query.is_empty()
                    || project.name.to_lowercase().contains(&query)
                    || project
                        .game_directory
                        .to_string_lossy()
                        .to_lowercase()
                        .contains(&query)
            })
            .cloned()
            .collect()
    }

    pub async fn save(&self, project: &GameProject) -> Result<(), CoreError> {
        validate_project(project, &project.id)?;
        self.ensure_writable().await?;
        let mut index = self.index.lock().await;
        self.save_locked(project, &mut index).await
    }

    async fn save_locked(
        &self,
        project: &GameProject,
        index: &mut ProjectIndex,
    ) -> Result<(), CoreError> {
        let mut next = index.clone();
        next.projects.retain(|p| p.id != project.id);
        if !project.is_removed {
            next.projects.push(ProjectSummary::from(project));
        }
        let path = self.directory(&project.id)?.join("game.json");
        let root = self.root.clone();
        let saved = project.clone();
        tokio::task::spawn_blocking(move || -> Result<(), CoreError> {
            mark_index_pending(&root, &saved.id)?;
            write_json(&path, &saved)?;
            Ok(())
        })
        .await??;
        // game.json is authoritative, including when the derived index cannot be replaced.
        *index = next;
        let root = self.root.clone();
        let committed = index.clone();
        let committed_ids = vec![project.id.clone()];
        tokio::task::spawn_blocking(move || persist_index(&root, &committed, &committed_ids))
            .await?
    }

    pub async fn remove(&self, id: &str, delete_data: bool) -> Result<(), CoreError> {
        self.ensure_writable().await?;
        let directory = self.directory(id)?;
        if !delete_data && directory.is_dir() {
            let mut project = self.get(id).await?;
            project.is_removed = true;
            return self.save(&project).await;
        }
        let project = if directory.is_dir() {
            let project = self.get(id).await?;
            if paths_overlap(&directory, &project.game_directory) {
                return Err(CoreError::InvalidData(
                    "项目目录与游戏目录重叠，不能删除；可仅移除记录。".into(),
                ));
            }
            crate::application::resolve_relative(&self.root, &format!("games/{id}"))?;
            Some(project)
        } else {
            None
        };
        {
            let _index = self.index.lock().await;
            let root = self.root.clone();
            let pending_id = id.to_owned();
            tokio::task::spawn_blocking(move || mark_index_pending(&root, &pending_id)).await??;
        }
        if let Some(saved) = project {
            tokio::task::spawn_blocking(move || -> Result<(), CoreError> {
                if let Err(error) = remove_project_directory(&directory) {
                    write_json(&directory.join("game.json"), &saved)?;
                    return Err(error.into());
                }
                Ok(())
            })
            .await??;
        }
        let mut index = self.index.lock().await;
        index.projects.retain(|p| p.id != id);
        let root = self.root.clone();
        let committed = index.clone();
        let committed_ids = vec![id.to_owned()];
        tokio::task::spawn_blocking(move || persist_index(&root, &committed, &committed_ids))
            .await?
    }

    pub async fn get(&self, id: &str) -> Result<GameProject, CoreError> {
        let path = self.directory(id)?.join("game.json");
        if !path.is_file() {
            return Err(CoreError::ProjectMissing(id.into()));
        }
        let expected_id = id.to_owned();
        tokio::task::spawn_blocking(move || {
            let project = read_json(&path)?;
            validate_project(&project, &expected_id)?;
            Ok(project)
        })
        .await?
    }

    pub async fn add(&self, directory: &Path) -> Result<GameProject, CoreError> {
        let directory = crate::game_directory::validate(directory)?;
        if paths_overlap(&directory, &self.root) {
            return Err(CoreError::InvalidData(
                "游戏目录和应用数据目录不能互相包含。".into(),
            ));
        }
        self.ensure_writable().await?;
        let mut index = self.index.lock().await;
        if let Some(existing) = index
            .projects
            .iter()
            .find(|project| same_path(&project.game_directory, &directory))
        {
            return self.get(&existing.id).await;
        }
        let id = project_id(&directory)?;
        let game_path = self.directory(&id)?.join("game.json");
        let mut project = if game_path.exists() {
            let existing: GameProject = read_json(&game_path)?;
            validate_project(&existing, &id)?;
            existing
        } else {
            GameProject {
                schema_version: 2,
                id,
                name: directory
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned(),
                game_directory: directory,
                settings: json!({}),
                is_removed: false,
                extra: BTreeMap::new(),
            }
        };
        project.is_removed = false;
        project.mark_updated();
        self.save_locked(&project, &mut index).await?;
        Ok(project)
    }
}

fn load_index(root: &Path) -> Result<ProjectIndex, CoreError> {
    let path = root.join("games/index.json");
    let pending = pending_index_ids(root)?;
    let mut index = if path.exists() {
        let index: ProjectIndex = read_json(&path)?;
        if index.schema_version != 1 {
            return Err(CoreError::InvalidData("不支持的项目列表版本。".into()));
        }
        for project in &index.projects {
            validate_id(&project.id)?;
        }
        index
    } else {
        let mut projects = Vec::new();
        let games = root.join("games");
        if games.is_dir() {
            for entry in fs::read_dir(games)? {
                let entry = entry?;
                let name = entry.file_name().to_string_lossy().into_owned();
                if validate_id(&name).is_err() || !entry.file_type()?.is_dir() {
                    continue;
                }
                let path = entry.path().join("game.json");
                if !path.exists() && pending.contains(&name) {
                    continue;
                }
                let project: GameProject = read_json(&path)?;
                validate_project(&project, &name)?;
                if !project.is_removed {
                    projects.push(ProjectSummary::from(&project));
                }
            }
        }
        ProjectIndex {
            schema_version: 1,
            projects,
            extra: BTreeMap::new(),
        }
    };
    // A valid index needs only the few projects whose last commit was interrupted.
    for id in pending {
        index.projects.retain(|project| project.id != id);
        let path = root.join("games").join(&id).join("game.json");
        if path.exists() {
            let project: GameProject = read_json(&path)?;
            validate_project(&project, &id)?;
            if !project.is_removed {
                index.projects.push(ProjectSummary::from(&project));
            }
        }
    }
    Ok(index)
}

fn pending_index_ids(root: &Path) -> Result<Vec<String>, CoreError> {
    let path = root.join("games/index-pending.json");
    let ids: Vec<String> = if path.exists() {
        read_json(&path)?
    } else {
        Vec::new()
    };
    for id in &ids {
        validate_id(id)?;
    }
    Ok(ids)
}

fn mark_index_pending(root: &Path, id: &str) -> Result<(), CoreError> {
    let mut ids = pending_index_ids(root)?;
    if !ids.iter().any(|pending| pending == id) {
        ids.push(id.into());
    }
    write_json(&root.join("games/index-pending.json"), &ids)?;
    Ok(())
}

fn persist_index(
    root: &Path,
    index: &ProjectIndex,
    committed_ids: &[String],
) -> Result<(), CoreError> {
    write_json(&root.join("games/index.json"), index)?;
    let mut pending = pending_index_ids(root)?;
    pending.retain(|id| !committed_ids.contains(id));
    if !pending.is_empty() {
        write_json(&root.join("games/index-pending.json"), &pending)?;
        return Ok(());
    }
    match fs::remove_file(root.join("games/index-pending.json")) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn remove_project_directory(directory: &Path) -> std::io::Result<()> {
    // Keep the metadata available for retry/recovery until the material deletion succeeds.
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        if entry.file_name() == "game.json" {
            continue;
        }
        if entry.file_type()?.is_dir() {
            fs::remove_dir_all(entry.path())?;
        } else {
            fs::remove_file(entry.path())?;
        }
    }
    fs::remove_file(directory.join("game.json"))?;
    fs::remove_dir(directory)
}

pub(crate) fn paths_overlap(left: &Path, right: &Path) -> bool {
    if cfg!(windows) {
        let normalize = |path: &Path| {
            path.to_string_lossy()
                .replace('/', "\\")
                .trim_end_matches('\\')
                .to_uppercase()
        };
        let left = normalize(left);
        let right = normalize(right);
        left == right
            || left.starts_with(&(right.clone() + "\\"))
            || right.starts_with(&(left + "\\"))
    } else {
        left.starts_with(right) || right.starts_with(left)
    }
}

fn validate_project(project: &GameProject, id: &str) -> Result<(), CoreError> {
    if project.schema_version != 2 || project.id != id || !project.game_directory.is_absolute() {
        return Err(CoreError::InvalidData(format!(
            "项目记录不完整或版本不支持：{id}"
        )));
    }
    Ok(())
}

pub(crate) fn validate_id(id: &str) -> Result<(), CoreError> {
    if id.len() != 24 || !id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(CoreError::InvalidData("无效的游戏项目 ID。".into()));
    }
    Ok(())
}

/// Existing Windows path-based identities use invariant simple uppercase before SHA-256.
pub fn project_id(directory: &Path) -> Result<String, CoreError> {
    let absolute = std::path::absolute(directory)?;
    let text = absolute
        .to_str()
        .ok_or_else(|| CoreError::InvalidData("游戏目录无法表示为 Unicode。".into()))?;
    let normalized: String = text
        .trim_end_matches(std::path::MAIN_SEPARATOR)
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
        .collect();
    Ok(hash_text(&normalized)[..24].to_owned())
}

fn same_path(left: &Path, right: &Path) -> bool {
    if cfg!(windows) {
        left.to_string_lossy()
            .eq_ignore_ascii_case(&right.to_string_lossy())
    } else {
        left == right
    }
}

/// Environment overrides remain compatible with the existing desktop application.
pub fn default_data_directory() -> PathBuf {
    if let Some(path) =
        std::env::var_os("GAMEL10N_DATA").or_else(|| std::env::var_os("AVG_TRANSLATOR_DATA"))
    {
        return path.into();
    }
    let local = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let current = local.join("GameL10n");
    let old = local.join("AvgTranslator");
    let base = if current.join("location.json").is_file()
        || current.join("games").is_dir()
        || !old.is_dir()
    {
        current
    } else {
        old
    };
    if let Ok(location) = read_json::<Value>(&base.join("location.json"))
        && let Some(path) = location.get("dataDirectory").and_then(Value::as_str)
    {
        return PathBuf::from(path);
    }
    base
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn index_commit_does_not_clear_another_projects_pending_deletion() {
        let root = tempfile::tempdir().unwrap();
        let game = root.path().join("game");
        fs::create_dir(&game).unwrap();
        let data = root.path().join("data");
        let store = ProjectStore::open(data.clone()).await.unwrap();
        let mut project = store.add(&game).await.unwrap();
        let deleting = "000000000000000000000001";
        mark_index_pending(&data, deleting).unwrap();
        project.name = "Updated".into();
        store.save(&project).await.unwrap();
        assert_eq!(pending_index_ids(&data).unwrap(), [deleting]);
        drop(store);
        let recovered = ProjectStore::open(data).await.unwrap();
        assert_eq!(recovered.list("").await[0].name, "Updated");
    }

    #[tokio::test]
    async fn failed_index_write_keeps_committed_metadata_and_recovers_only_pending_projects() {
        let temporary = tempfile::tempdir().unwrap();
        let data = temporary.path().join("data");
        let game = temporary.path().join("game");
        fs::create_dir(&game).unwrap();
        let store = ProjectStore::open(data.clone()).await.unwrap();
        let mut project = store.add(&game).await.unwrap();
        let index_path = data.join("games/index.json");
        let previous = fs::read(&index_path).unwrap();
        fs::remove_file(&index_path).unwrap();
        fs::create_dir(&index_path).unwrap();
        project.name = "Committed name".into();
        project.settings = json!({"translationProfileIds":["preserved"]});
        project
            .extra
            .insert("customData".into(), json!({"value":42}));

        assert!(store.save(&project).await.is_err());
        assert_eq!(store.list("").await[0].name, "Committed name");
        assert_eq!(
            store.get(&project.id).await.unwrap().settings,
            project.settings
        );
        assert!(data.join("games/index-pending.json").is_file());
        fs::remove_dir(&index_path).unwrap();
        fs::write(&index_path, previous).unwrap();
        let unrelated = data.join("games/000000000000000000000001");
        fs::create_dir(&unrelated).unwrap();
        fs::write(unrelated.join("game.json"), "unrelated unreadable metadata").unwrap();
        drop(store);

        let reopened = ProjectStore::open(data.clone()).await.unwrap();
        assert_eq!(reopened.list("").await[0].name, "Committed name");
        assert_eq!(
            reopened.get(&project.id).await.unwrap().extra["customData"],
            json!({"value":42})
        );
        reopened.ensure_writable().await.unwrap();
        let index: ProjectIndex = read_json(&index_path).unwrap();
        assert_eq!(index.projects[0].id, project.id);
        assert_eq!(index.projects[0].name, "Committed name");
        assert!(!data.join("games/index-pending.json").exists());
    }

    #[tokio::test]
    async fn a_committed_new_project_is_recovered_after_its_index_write_fails() {
        let temporary = tempfile::tempdir().unwrap();
        let data = temporary.path().join("data");
        let first_game = temporary.path().join("first");
        let second_game = temporary.path().join("second");
        fs::create_dir(&first_game).unwrap();
        fs::create_dir(&second_game).unwrap();
        let store = ProjectStore::open(data.clone()).await.unwrap();
        store.add(&first_game).await.unwrap();
        let index_path = data.join("games/index.json");
        let previous = fs::read(&index_path).unwrap();
        fs::remove_file(&index_path).unwrap();
        fs::create_dir(&index_path).unwrap();

        assert!(store.add(&second_game).await.is_err());
        let id = project_id(&second_game).unwrap();
        assert_eq!(store.get(&id).await.unwrap().game_directory, second_game);
        assert_eq!(store.list("").await.len(), 2);
        fs::remove_dir(&index_path).unwrap();
        fs::write(&index_path, previous).unwrap();
        drop(store);

        let reopened = ProjectStore::open(data).await.unwrap();
        assert_eq!(reopened.list("").await.len(), 2);
        assert_eq!(reopened.add(&second_game).await.unwrap().id, id);
    }

    #[tokio::test]
    async fn removed_projects_stay_removed_when_the_index_write_fails() {
        for delete_data in [false, true] {
            let temporary = tempfile::tempdir().unwrap();
            let data = temporary.path().join("data");
            let game = temporary.path().join("game");
            fs::create_dir(&game).unwrap();
            fs::write(game.join("original.txt"), "original").unwrap();
            let store = ProjectStore::open(data.clone()).await.unwrap();
            let project = store.add(&game).await.unwrap();
            let index_path = data.join("games/index.json");
            let previous = fs::read(&index_path).unwrap();
            fs::remove_file(&index_path).unwrap();
            fs::create_dir(&index_path).unwrap();

            assert!(store.remove(&project.id, delete_data).await.is_err());
            assert!(store.list("").await.is_empty());
            if delete_data {
                assert!(!store.directory(&project.id).unwrap().exists());
            } else {
                assert!(store.get(&project.id).await.unwrap().is_removed);
            }
            assert_eq!(
                fs::read_to_string(game.join("original.txt")).unwrap(),
                "original"
            );
            fs::remove_dir(&index_path).unwrap();
            fs::write(&index_path, previous).unwrap();
            drop(store);

            let reopened = ProjectStore::open(data.clone()).await.unwrap();
            assert!(reopened.list("").await.is_empty());
            reopened.ensure_writable().await.unwrap();
            assert!(!data.join("games/index-pending.json").exists());
        }
    }

    #[tokio::test]
    async fn pending_marker_failure_prevents_an_unrecoverable_metadata_commit() {
        let temporary = tempfile::tempdir().unwrap();
        let data = temporary.path().join("data");
        let game = temporary.path().join("game");
        fs::create_dir(&game).unwrap();
        let store = ProjectStore::open(data.clone()).await.unwrap();
        let mut project = store.add(&game).await.unwrap();
        let previous_name = project.name.clone();
        fs::create_dir(data.join("games/index-pending.json")).unwrap();
        project.name = "Must not commit".into();

        assert!(store.save(&project).await.is_err());
        assert_eq!(store.get(&project.id).await.unwrap().name, previous_name);
        assert_eq!(store.list("").await[0].name, previous_name);
    }

    #[tokio::test]
    async fn interrupted_first_project_creation_does_not_break_initial_index_recovery() {
        let temporary = tempfile::tempdir().unwrap();
        let id = "000000000000000000000001";
        fs::create_dir_all(temporary.path().join("games").join(id)).unwrap();
        write_json(&temporary.path().join("games/index-pending.json"), &[id]).unwrap();

        let store = ProjectStore::open(temporary.path().into()).await.unwrap();
        assert!(store.list("").await.is_empty());
        store.ensure_writable().await.unwrap();
        assert!(!temporary.path().join("games/index-pending.json").exists());
    }
}
