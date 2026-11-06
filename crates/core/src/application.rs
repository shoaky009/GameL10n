use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    fs,
    path::{Component, Path, PathBuf},
    sync::Arc,
};

use gamel10n_engines::{
    protocol::{AdapterRequest, ExtractionResult},
    vntextpatch::VnTextPatch,
};
use gamel10n_translation::{
    TranslationUnit,
    files::{hash_file, hash_text, read_json, write_json},
    validate_units,
};
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, broadcast};

use crate::{
    CancellationToken, CoreError, GameProject, ProjectSummary, ToolPaths, projects::ProjectStore,
};

/// UI-neutral task state. Status records belong to the project, not to a window.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskState {
    pub project_id: String,
    pub operation: String,
    pub running: bool,
    pub message: String,
    pub succeeded: Option<bool>,
}

#[derive(Clone, Debug)]
pub enum AppEvent {
    ProjectsChanged,
    TaskChanged(TaskState),
}

/// One committed extraction file, cached in memory after its first load.
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredAnalysis {
    pub schema_version: u32,
    pub format: String,
    pub source_directory: PathBuf,
    pub resource_files: Vec<String>,
    pub source_hashes: BTreeMap<String, String>,
    pub extraction: ExtractionResult,
}

#[derive(Debug)]
pub struct TextPage {
    pub total: usize,
    pub offset: usize,
    pub units: Vec<TranslationUnit>,
}

enum CachedAnalysis {
    Native(StoredAnalysis),
    Legacy(ExtractionResult),
}

impl CachedAnalysis {
    fn extraction(&self) -> &ExtractionResult {
        match self {
            Self::Native(native) => &native.extraction,
            Self::Legacy(extraction) => extraction,
        }
    }

    fn native(&self) -> Result<&StoredAnalysis, CoreError> {
        match self {
            Self::Native(native) => Ok(native),
            Self::Legacy(_) => Err(CoreError::InvalidData(
                "已有旧版提取结果可浏览；对应引擎的补丁构建尚未迁入，不能改用通用脚本格式回填。"
                    .into(),
            )),
        }
    }

    fn retained_bytes(&self) -> usize {
        self.extraction()
            .units
            .iter()
            .map(|unit| {
                std::mem::size_of::<TranslationUnit>()
                    + unit.id.len()
                    + unit.resource_path.len()
                    + unit.locator.len()
                    + unit.source_text.len()
                    + unit.context.len()
                    + unit.kind.len()
                    + unit.speaker.as_ref().map_or(0, String::len)
                    + unit
                        .protected_tokens
                        .as_ref()
                        .map_or(0, |tokens| tokens.iter().map(String::len).sum())
            })
            .sum()
    }
}

#[derive(Default)]
struct AnalysisCache {
    entries: VecDeque<(String, Arc<CachedAnalysis>, usize)>,
    retained_bytes: usize,
}

impl AnalysisCache {
    fn get(&mut self, id: &str) -> Option<Arc<CachedAnalysis>> {
        let position = self.entries.iter().position(|(key, _, _)| key == id)?;
        let entry = self.entries.remove(position)?;
        let result = Arc::clone(&entry.1);
        self.entries.push_back(entry);
        Some(result)
    }

    fn insert(&mut self, id: &str, analysis: Arc<CachedAnalysis>) {
        if let Some(position) = self.entries.iter().position(|(key, _, _)| key == id)
            && let Some((_, _, bytes)) = self.entries.remove(position)
        {
            self.retained_bytes -= bytes;
        }
        let bytes = analysis.retained_bytes();
        self.retained_bytes += bytes;
        self.entries.push_back((id.into(), analysis, bytes));
        // Keep the current large project, but do not retain every game ever opened.
        while self.entries.len() > 1
            && (self.entries.len() > 4 || self.retained_bytes > 128 * 1024 * 1024)
        {
            if let Some((_, _, bytes)) = self.entries.pop_front() {
                self.retained_bytes -= bytes;
            }
        }
    }
}

#[derive(Clone)]
pub struct Application {
    store: Arc<ProjectStore>,
    parser: VnTextPatch,
    analysis: Arc<Mutex<AnalysisCache>>,
    tasks: Arc<Mutex<HashMap<String, (TaskState, CancellationToken)>>>,
    events: broadcast::Sender<AppEvent>,
}

impl Application {
    pub async fn open(data_directory: PathBuf, tools: &ToolPaths) -> Result<Self, CoreError> {
        let (events, _) = broadcast::channel(256);
        Ok(Self {
            store: Arc::new(ProjectStore::open(data_directory).await?),
            parser: VnTextPatch::new(tools),
            analysis: Arc::new(Mutex::new(AnalysisCache::default())),
            tasks: Arc::new(Mutex::new(HashMap::new())),
            events,
        })
    }

    pub fn data_directory(&self) -> &Path {
        self.store.root()
    }

    pub fn project_directory(&self, id: &str) -> Result<PathBuf, CoreError> {
        self.store.directory(id)
    }

    pub fn subscribe(&self) -> broadcast::Receiver<AppEvent> {
        self.events.subscribe()
    }

    pub async fn list_projects(&self, query: &str) -> Vec<ProjectSummary> {
        let mut projects = self.store.list(query).await;
        let tasks = self.tasks.lock().await;
        projects.sort_by(|left, right| {
            let running = |id: &str| tasks.get(id).is_some_and(|(state, _)| state.running);
            running(&right.id)
                .cmp(&running(&left.id))
                .then_with(|| left.name.cmp(&right.name))
                .then_with(|| left.id.cmp(&right.id))
        });
        projects
    }

    pub async fn add_project(&self, game_directory: &Path) -> Result<GameProject, CoreError> {
        let project = self.store.add(game_directory).await?;
        let _ = self.events.send(AppEvent::ProjectsChanged);
        Ok(project)
    }

    /// Reads saved metadata only; never starts recognition or resource hashing.
    pub async fn open_project(&self, id: &str) -> Result<GameProject, CoreError> {
        self.store.get(id).await
    }

    pub async fn cancel(&self, id: &str) {
        if let Some((_, cancellation)) = self.tasks.lock().await.get(id) {
            cancellation.cancel();
        }
    }

    pub async fn task_state(&self, id: &str) -> Option<TaskState> {
        self.tasks
            .lock()
            .await
            .get(id)
            .map(|(state, _)| state.clone())
    }

    async fn cached_analysis(&self, id: &str) -> Result<Option<Arc<CachedAnalysis>>, CoreError> {
        if let Some(cached) = self.analysis.lock().await.get(id) {
            return Ok(Some(cached));
        }
        let directory = self.store.directory(id)?;
        let project = self.store.get(id).await?;
        let stored =
            tokio::task::spawn_blocking(move || -> Result<Option<CachedAnalysis>, CoreError> {
                let path = directory.join("extraction/native.json");
                if path.is_file() {
                    let native: StoredAnalysis = read_json(&path)?;
                    if native.schema_version != 1 {
                        return Err(CoreError::InvalidData(
                            "不支持的提取缓存版本，请重新提取。".into(),
                        ));
                    }
                    validate_units(&native.extraction.units)?;
                    return Ok(Some(CachedAnalysis::Native(native)));
                }
                Ok(crate::legacy::load(&directory, &project)?.map(CachedAnalysis::Legacy))
            })
            .await??;
        let Some(stored) = stored else {
            return Ok(None);
        };
        let stored = Arc::new(stored);
        let mut cache = self.analysis.lock().await;
        if let Some(current) = cache.get(id) {
            return Ok(Some(current));
        }
        cache.insert(id, Arc::clone(&stored));
        Ok(Some(stored))
    }

    pub async fn text_page(
        &self,
        id: &str,
        query: &str,
        offset: usize,
        limit: usize,
    ) -> Result<TextPage, CoreError> {
        let Some(analysis) = self.cached_analysis(id).await? else {
            return Ok(TextPage {
                total: 0,
                offset: 0,
                units: Vec::new(),
            });
        };
        if query.is_empty() {
            let all = &analysis.extraction().units;
            let offset = offset.min(all.len());
            let end = offset.saturating_add(limit.clamp(1, 500)).min(all.len());
            return Ok(TextPage {
                total: all.len(),
                offset,
                units: all[offset..end].to_vec(),
            });
        }
        let query = query.to_lowercase();
        let mut total = 0;
        let mut units = Vec::new();
        for unit in &analysis.extraction().units {
            if !query.is_empty()
                && !unit.source_text.to_lowercase().contains(&query)
                && !unit.resource_path.to_lowercase().contains(&query)
            {
                continue;
            }
            if total >= offset && units.len() < limit.clamp(1, 500) {
                units.push(unit.clone());
            }
            total += 1;
        }
        Ok(TextPage {
            total,
            offset,
            units,
        })
    }

    pub async fn extract_native(&self, id: &str, format: &str) -> Result<usize, CoreError> {
        let project = self.store.get(id).await?;
        let cancellation = self.begin(id, "extract", "正在检查脚本格式。").await?;
        let result = self.extract_inner(&project, format, &cancellation).await;
        self.finish(
            id,
            "extract",
            result
                .as_ref()
                .map(|count| format!("提取完成，共 {count} 条文本。"))
                .map_err(ToString::to_string),
        )
        .await?;
        result
    }

    async fn extract_inner(
        &self,
        project: &GameProject,
        format: &str,
        cancellation: &CancellationToken,
    ) -> Result<usize, CoreError> {
        let response = self
            .parser
            .probe(&project.game_directory, format, cancellation)
            .await?;
        let probe = response.probe.ok_or_else(|| {
            CoreError::InvalidData(
                "该目录没有匹配的已解包脚本；封包或其他格式需要对应的引擎适配。".into(),
            )
        })?;
        self.progress(&project.id, "extract", "正在保存原始脚本快照。")
            .await;
        let project_directory = self.store.directory(&project.id)?;
        let files = probe.required_files;
        if files.is_empty() {
            return Err(CoreError::InvalidData("解析器未声明必需资源。".into()));
        }
        let source = project.game_directory.clone();
        let copied_files = files.clone();
        let snapshot_root = project_directory.join("sources");
        let token = cancellation.clone();
        let (snapshot, hashes) = tokio::task::spawn_blocking(move || {
            prepare_snapshot(&source, &snapshot_root, &copied_files, &token)
        })
        .await??;
        self.progress(&project.id, "extract", "正在提取脚本文本。")
            .await;
        let mut request = AdapterRequest::new("extract", &snapshot);
        request.format = Some(format.into());
        request.resource_files = Some(files.clone());
        let response = self.parser.execute(&request, cancellation).await?;
        let extraction = response
            .extraction
            .ok_or_else(|| CoreError::InvalidData("解析器未返回提取结果。".into()))?;
        validate_units(&extraction.units)?;
        self.progress(&project.id, "extract", "正在验证原文字节回填。")
            .await;
        let roundtrip = tempfile::tempdir()?;
        request.operation = "build".into();
        request.output_directory = Some(roundtrip.path().to_path_buf());
        self.parser.execute(&request, cancellation).await?;
        for (relative, expected) in &hashes {
            if hash_file(&resolve_relative(roundtrip.path(), relative)?)? != *expected {
                return Err(CoreError::InvalidData(format!(
                    "原文回填改变了文件：{relative}"
                )));
            }
        }
        let count = extraction.units.len();
        let stored = StoredAnalysis {
            schema_version: 1,
            format: format.into(),
            source_directory: snapshot,
            resource_files: files,
            source_hashes: hashes,
            extraction,
        };
        let path = project_directory.join("extraction/native.json");
        let stored = tokio::task::spawn_blocking(move || -> Result<StoredAnalysis, CoreError> {
            write_json(&path, &stored)?;
            Ok(stored)
        })
        .await??;
        self.analysis
            .lock()
            .await
            .insert(&project.id, Arc::new(CachedAnalysis::Native(stored)));
        Ok(count)
    }

    /// Builds only inside the project domain. No files are installed in the game directory.
    pub async fn build_native(
        &self,
        id: &str,
        translations: BTreeMap<String, String>,
    ) -> Result<PathBuf, CoreError> {
        let cancellation = self.begin(id, "build", "正在检查原始脚本和译文。").await?;
        let result = self.build_inner(id, translations, &cancellation).await;
        self.finish(
            id,
            "build",
            result
                .as_ref()
                .map(|path| format!("补丁已生成：{}", path.display()))
                .map_err(ToString::to_string),
        )
        .await?;
        result
    }

    async fn build_inner(
        &self,
        id: &str,
        translations: BTreeMap<String, String>,
        cancellation: &CancellationToken,
    ) -> Result<PathBuf, CoreError> {
        let cached = self
            .cached_analysis(id)
            .await?
            .ok_or_else(|| CoreError::InvalidData("请先提取脚本文本。".into()))?;
        let analysis = cached.native()?;
        let snapshot = Arc::clone(&cached);
        tokio::task::spawn_blocking(move || -> Result<(), CoreError> {
            let snapshot = snapshot.native()?;
            for (relative, expected) in &snapshot.source_hashes {
                if hash_file(&resolve_relative(&snapshot.source_directory, relative)?)? != *expected
                {
                    return Err(CoreError::InvalidData(format!(
                        "原始脚本快照已改变：{relative}"
                    )));
                }
            }
            Ok(())
        })
        .await??;
        let patch_root = self.store.directory(id)?.join("patches");
        fs::create_dir_all(&patch_root)?;
        let patch = tempfile::Builder::new()
            .prefix("native-")
            .tempdir_in(&patch_root)?;
        let mut request = AdapterRequest::new("build", &analysis.source_directory);
        request.format = Some(analysis.format.clone());
        request.resource_files = Some(analysis.resource_files.clone());
        request.output_directory = Some(patch.path().to_path_buf());
        request.translations = Some(translations);
        self.progress(id, "build", "正在回填译文并检查脚本结构。")
            .await;
        self.parser.execute(&request, cancellation).await?;
        request.operation = "validate".into();
        let response = self.parser.execute(&request, cancellation).await?;
        let validation = response
            .validation
            .ok_or_else(|| CoreError::InvalidData("解析器未返回验证结果。".into()))?;
        if !validation.passed || !validation.issues.is_empty() {
            return Err(CoreError::InvalidData("补丁未通过脚本结构验证。".into()));
        }
        Ok(patch.keep())
    }

    async fn begin(
        &self,
        id: &str,
        operation: &str,
        message: &str,
    ) -> Result<CancellationToken, CoreError> {
        self.store.get(id).await?;
        self.store.ensure_writable().await?;
        let mut tasks = self.tasks.lock().await;
        if tasks.get(id).is_some_and(|(state, _)| state.running) {
            return Err(CoreError::ProjectBusy);
        }
        let cancellation = CancellationToken::new();
        let state = TaskState {
            project_id: id.into(),
            operation: operation.into(),
            running: true,
            message: message.into(),
            succeeded: None,
        };
        tasks.insert(id.into(), (state.clone(), cancellation.clone()));
        let _ = self.events.send(AppEvent::TaskChanged(state));
        Ok(cancellation)
    }

    async fn progress(&self, id: &str, operation: &str, message: &str) {
        let state = TaskState {
            project_id: id.into(),
            operation: operation.into(),
            running: true,
            message: message.into(),
            succeeded: None,
        };
        if let Some((current, _)) = self.tasks.lock().await.get_mut(id) {
            *current = state.clone();
        }
        let _ = self.events.send(AppEvent::TaskChanged(state));
    }

    async fn finish(
        &self,
        id: &str,
        operation: &str,
        result: Result<String, String>,
    ) -> Result<(), CoreError> {
        let succeeded = result.is_ok();
        let state = TaskState {
            project_id: id.into(),
            operation: operation.into(),
            running: false,
            message: result.unwrap_or_else(|error| error),
            succeeded: Some(succeeded),
        };
        if let Some((current, _)) = self.tasks.lock().await.get_mut(id) {
            *current = state.clone();
        }
        let _ = self.events.send(AppEvent::TaskChanged(state.clone()));
        let path = self.store.directory(id)?.join("rust-task.json");
        tokio::task::spawn_blocking(move || write_json(&path, &state)).await??;
        Ok(())
    }
}

fn prepare_snapshot(
    source: &Path,
    root: &Path,
    files: &[String],
    cancellation: &CancellationToken,
) -> Result<(PathBuf, BTreeMap<String, String>), CoreError> {
    let mut hashes = BTreeMap::new();
    for file in files {
        if cancellation.is_cancelled() {
            return Err(gamel10n_engines::EngineError::Cancelled.into());
        }
        hashes.insert(file.clone(), hash_file(&resolve_relative(source, file)?)?);
    }
    let fingerprint = hash_text(
        &hashes
            .iter()
            .map(|(path, hash)| format!("{path}\n{hash}\n"))
            .collect::<String>(),
    );
    let snapshot = root.join(&fingerprint[..24]);
    if snapshot.is_dir() {
        for (relative, expected) in &hashes {
            if hash_file(&resolve_relative(&snapshot, relative)?)? != *expected {
                return Err(CoreError::InvalidData(
                    "原文快照不完整或已经修改，请保留诊断并重新提取。".into(),
                ));
            }
        }
        return Ok((snapshot, hashes));
    }
    fs::create_dir_all(root)?;
    let pending = tempfile::Builder::new()
        .prefix(".snapshot-")
        .tempdir_in(root)?;
    for (relative, expected) in &hashes {
        if cancellation.is_cancelled() {
            return Err(gamel10n_engines::EngineError::Cancelled.into());
        }
        let destination = resolve_relative(pending.path(), relative)?;
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(resolve_relative(source, relative)?, &destination)?;
        if hash_file(&destination)? != *expected {
            return Err(CoreError::InvalidData(format!(
                "读取期间原文件发生变化：{relative}"
            )));
        }
    }
    fs::rename(pending.path(), &snapshot)?;
    Ok((snapshot, hashes))
}

pub(crate) fn resolve_relative(root: &Path, relative: &str) -> Result<PathBuf, CoreError> {
    let path = Path::new(relative);
    if relative.is_empty()
        || path
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err(CoreError::InvalidData(format!(
            "无效的资源相对路径：{relative}"
        )));
    }
    let mut resolved = root.to_path_buf();
    for part in path.components() {
        resolved.push(part);
        if let Ok(metadata) = fs::symlink_metadata(&resolved) {
            #[cfg(windows)]
            {
                use std::os::windows::fs::MetadataExt;
                if metadata.file_attributes() & 0x400 != 0 {
                    return Err(CoreError::InvalidData("资源路径包含重解析点。".into()));
                }
            }
            if metadata.file_type().is_symlink() {
                return Err(CoreError::InvalidData("资源路径包含符号链接。".into()));
            }
        }
    }
    Ok(resolved)
}
