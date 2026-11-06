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

/// Full project metadata; unrecognized fields survive saves during migration.
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
                Ok((file, load_index(&root)?))
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
        let directory = std::path::absolute(directory)?;
        if !directory.is_dir() {
            return Err(CoreError::InvalidData("游戏目录不存在。".into()));
        }
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
        let summary = ProjectSummary::from(&project);
        let mut next = index.clone();
        next.projects.retain(|item| item.id != project.id);
        next.projects.push(summary);
        let index_path = self.root.join("games/index.json");
        let saved = project.clone();
        let committed_index = next.clone();
        tokio::task::spawn_blocking(move || -> Result<(), CoreError> {
            write_json(&game_path, &saved)?;
            write_json(&index_path, &committed_index)?;
            Ok(())
        })
        .await??;
        *index = next;
        Ok(project)
    }
}

fn load_index(root: &Path) -> Result<ProjectIndex, CoreError> {
    let path = root.join("games/index.json");
    if path.exists() {
        let index: ProjectIndex = read_json(&path)?;
        if index.schema_version != 1 {
            return Err(CoreError::InvalidData("不支持的项目列表版本。".into()));
        }
        for project in &index.projects {
            validate_id(&project.id)?;
        }
        return Ok(index);
    }
    let mut projects = Vec::new();
    let games = root.join("games");
    if games.is_dir() {
        for entry in fs::read_dir(games)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if validate_id(&name).is_err() || !entry.file_type()?.is_dir() {
                continue;
            }
            let project: GameProject = read_json(&entry.path().join("game.json"))?;
            validate_project(&project, &name)?;
            if !project.is_removed {
                projects.push(ProjectSummary::from(&project));
            }
        }
    }
    Ok(ProjectIndex {
        schema_version: 1,
        projects,
        extra: BTreeMap::new(),
    })
}

fn paths_overlap(left: &Path, right: &Path) -> bool {
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
