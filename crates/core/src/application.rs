use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    fs,
    path::{Component, Path, PathBuf},
    sync::{
        Arc, Weak,
        atomic::{AtomicU64, Ordering},
    },
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
    CancellationToken, CoreError, GameProject, ProjectSummary, ToolPaths, logs::FileProgress,
    projects::ProjectStore,
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

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct StoredTaskState {
    #[serde(flatten)]
    state: TaskState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    task_summary: Option<serde_json::Value>,
}

#[derive(Clone, Debug)]
pub enum AppEvent {
    ProjectsChanged,
    EngineLibraryChanged,
    TaskChanged(TaskState),
    TranslationChanged {
        project_id: String,
        progress: gamel10n_translation::execution::TranslationProgress,
    },
    Log {
        project_id: String,
        line: String,
    },
    EngineProgress {
        project_id: String,
        progress: gamel10n_engines::EngineProgress,
    },
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
    #[serde(default)]
    pub adapter: Option<gamel10n_engines::AdapterHandle>,
    #[serde(default)]
    pub probe: Option<gamel10n_engines::protocol::ProbeResult>,
}

#[derive(Debug)]
pub struct TextPage {
    pub total: usize,
    pub offset: usize,
    pub units: Vec<TranslationUnit>,
}

pub(crate) enum CachedAnalysis {
    Native(Box<StoredAnalysis>),
    Legacy(ExtractionResult),
}

impl CachedAnalysis {
    pub(crate) fn extraction(&self) -> &ExtractionResult {
        match self {
            Self::Native(native) => &native.extraction,
            Self::Legacy(extraction) => extraction,
        }
    }

    pub(crate) fn native(&self) -> Result<&StoredAnalysis, CoreError> {
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

struct TaskEntry {
    state: TaskState,
    terminal_persisted: bool,
    summary: Option<serde_json::Value>,
    cancellation: CancellationToken,
    order: u64,
}

#[derive(Clone)]
pub struct Application {
    pub(crate) store: Arc<ProjectStore>,
    parser: VnTextPatch,
    analysis: Arc<Mutex<AnalysisCache>>,
    tasks: Arc<Mutex<HashMap<String, TaskEntry>>>,
    next_task_order: Arc<AtomicU64>,
    pub(crate) events: broadcast::Sender<AppEvent>,
    pub(crate) settings_gate: Arc<Mutex<()>>,
    pub(crate) project_lifecycle: Arc<Mutex<()>>,
    project_access: Arc<Mutex<HashMap<String, Weak<Mutex<()>>>>>,
    pub(crate) log_gate: Arc<Mutex<()>>,
    pub(crate) metrics: Arc<Mutex<crate::metrics::MetricsState>>,
    pub(crate) session_keys: Arc<Mutex<HashMap<String, String>>>,
    pub(crate) tools: ToolPaths,
    pub(crate) workbenches: Arc<Mutex<HashMap<String, Arc<Mutex<crate::workbench::Workbench>>>>>,
    pub(crate) translation_scheduler: Arc<crate::translation_tasks::TranslationScheduler>,
}

impl Application {
    pub async fn open(data_directory: PathBuf, tools: &ToolPaths) -> Result<Self, CoreError> {
        let (events, _) = broadcast::channel(256);
        Ok(Self {
            store: Arc::new(ProjectStore::open(data_directory).await?),
            parser: VnTextPatch::new(tools),
            analysis: Arc::new(Mutex::new(AnalysisCache::default())),
            tasks: Arc::new(Mutex::new(HashMap::new())),
            next_task_order: Arc::new(AtomicU64::new(0)),
            events,
            settings_gate: Arc::new(Mutex::new(())),
            project_lifecycle: Arc::new(Mutex::new(())),
            project_access: Arc::new(Mutex::new(HashMap::new())),
            log_gate: Arc::new(Mutex::new(())),
            metrics: Arc::new(Mutex::new(crate::metrics::MetricsState::default())),
            session_keys: Arc::new(Mutex::new(HashMap::new())),
            tools: tools.clone(),
            workbenches: Arc::new(Mutex::new(HashMap::new())),
            translation_scheduler: Arc::new(
                crate::translation_tasks::TranslationScheduler::default(),
            ),
        })
    }

    pub fn data_directory(&self) -> &Path {
        self.store.root()
    }

    pub async fn ensure_writable(&self) -> Result<(), CoreError> {
        self.store.ensure_writable().await
    }

    pub fn project_directory(&self, id: &str) -> Result<PathBuf, CoreError> {
        self.store.directory(id)
    }

    pub fn subscribe(&self) -> broadcast::Receiver<AppEvent> {
        self.events.subscribe()
    }

    pub async fn list_projects(&self, query: &str) -> Vec<ProjectSummary> {
        let mut projects = self.store.list(query.trim()).await;
        let tasks = self.tasks.lock().await;
        projects.sort_by_cached_key(|project| {
            let task_order = tasks
                .get(&project.id)
                .filter(|entry| entry.state.running)
                .map_or(u64::MAX, |entry| entry.order);
            (
                task_order,
                std::cmp::Reverse(project.updated_at()),
                project.name.to_lowercase(),
                project.id.clone(),
            )
        });
        projects
    }

    pub async fn add_project(&self, game_directory: &Path) -> Result<GameProject, CoreError> {
        let id = crate::projects::project_id(game_directory)?;
        {
            let _project = self.lock_project(&id).await?;
            let _lifecycle = self.project_lifecycle.lock().await;
            self.store.add(game_directory).await?;
            let _ = self.events.send(AppEvent::ProjectsChanged);
        }
        self.restore_known_adapter(&id).await?;
        self.store.get(&id).await
    }

    /// Reads saved metadata only; never starts recognition or resource hashing.
    pub async fn open_project(&self, id: &str) -> Result<GameProject, CoreError> {
        let _project = self.lock_project(id).await?;
        let _lifecycle = self.project_lifecycle.lock().await;
        let mut project = self.store.get(id).await?;
        let cached = self.tasks.lock().await.get(id).and_then(|entry| {
            (entry.state.running || entry.terminal_persisted).then(|| StoredTaskState {
                state: entry.state.clone(),
                task_summary: entry.summary.clone(),
            })
        });
        if cached.as_ref().is_some_and(|saved| saved.state.running) {
            return Ok(project);
        }
        let saved = if let Some(saved) = cached {
            saved
        } else {
            let path = self.project_directory(id)?.join("rust-task.json");
            if !path.is_file() {
                return Ok(project);
            }
            let saved_path = path.clone();
            let saved =
                tokio::task::spawn_blocking(move || read_json::<StoredTaskState>(&saved_path))
                    .await?;
            let Ok(mut saved) = saved else {
                return Ok(project);
            };
            if saved.state.project_id != id {
                return Ok(project);
            }
            if saved.state.running {
                // A second read-only host must not replace the current writer's task record.
                if let Err(error) = self.store.ensure_writable().await {
                    self.report_task_warning(id, &format!("任务恢复状态暂未保存：{error}"));
                    return Ok(project);
                }
                // The previous writer may have finished between the first read and lease acquisition.
                project = self.store.get(id).await?;
                let saved_path = path.clone();
                saved =
                    tokio::task::spawn_blocking(move || read_json::<StoredTaskState>(&saved_path))
                        .await??;
                if saved.state.project_id != id {
                    return Ok(project);
                }
                if saved.state.running {
                    saved.state.running = false;
                    saved.state.succeeded = None;
                    saved.state.message = "上次任务已中断，已保存结果可继续使用。".into();
                    saved.task_summary = Some(task_summary(&saved.state));
                    let recovered = saved.clone();
                    tokio::task::spawn_blocking(move || write_json(&path, &recovered)).await??;
                }
            }
            self.tasks.lock().await.insert(
                id.into(),
                TaskEntry {
                    state: saved.state.clone(),
                    terminal_persisted: true,
                    summary: saved.task_summary.clone(),
                    cancellation: CancellationToken::new(),
                    order: 0,
                },
            );
            saved
        };
        let matches = if let Some(summary) = &saved.task_summary {
            project.extra.get("taskSummary") == Some(summary)
        } else {
            let summary = task_summary(&saved.state);
            project.extra.get("taskSummary").is_some_and(|saved| {
                summary.as_object().is_some_and(|expected| {
                    expected
                        .iter()
                        .all(|(key, value)| saved.get(key) == Some(value))
                })
            })
        };
        let recovery = if matches {
            Ok(())
        } else {
            async {
                let summary = match saved.task_summary {
                    Some(summary) => summary,
                    None => self.completed_task_summary(&saved.state)?,
                };
                self.save_task_summary(&mut project, summary).await
            }
            .await
        };
        if let Err(error) = recovery {
            self.report_task_warning(
                id,
                &format!("任务结果已保存，项目列表摘要暂未更新：{error}"),
            );
        }
        Ok(project)
    }

    pub async fn task_states(&self) -> HashMap<String, TaskState> {
        self.tasks
            .lock()
            .await
            .iter()
            .map(|(id, entry)| (id.clone(), entry.state.clone()))
            .collect()
    }

    pub async fn cancel(&self, id: &str) {
        if let Some(entry) = self.tasks.lock().await.get(id) {
            entry.cancellation.cancel();
        }
    }

    pub async fn task_state(&self, id: &str) -> Option<TaskState> {
        self.tasks
            .lock()
            .await
            .get(id)
            .map(|entry| entry.state.clone())
    }

    pub(crate) async fn cached_analysis(
        &self,
        id: &str,
    ) -> Result<Option<Arc<CachedAnalysis>>, CoreError> {
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
                    return Ok(Some(CachedAnalysis::Native(Box::new(native))));
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
        let files = probe.required_files.clone();
        if files.is_empty() {
            return Err(CoreError::InvalidData("解析器未声明必需资源。".into()));
        }
        let source = project.game_directory.clone();
        let copied_files = files.clone();
        let snapshot_root = project_directory.join("sources");
        let token = cancellation.clone();
        let (snapshot, hashes) = self
            .run_file_task(&project.id, "extract", move |progress| {
                prepare_snapshot(&source, &snapshot_root, &copied_files, &token, &progress)
            })
            .await?;
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
            adapter: None,
            probe: Some(probe),
        };
        self.commit_analysis(&project.id, stored).await?;
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
        crate::cleanup::require_source_materials(&analysis.source_directory)?;
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

    pub(crate) async fn commit_analysis(
        &self,
        id: &str,
        stored: StoredAnalysis,
    ) -> Result<(), CoreError> {
        let count = stored.extraction.units.len();
        let mut project = self.store.get(id).await?;
        let probe = stored.probe.as_ref();
        project.extra.remove("materialsCleanup");
        project.extra.insert("analysis".into(),serde_json::json!({"adapterId":probe.map(|p|p.adapter_id.as_str()).unwrap_or(&stored.format),"adapterVersion":probe.map(|p|p.adapter_version.as_str()).unwrap_or("native-v1"),"sourceFingerprint":hash_text(&serde_json::to_string(&stored.source_hashes).map_err(|e|CoreError::InvalidData(e.to_string()))?),"unitCount":count,"resourceCount":stored.resource_files.len(),"engineId":probe.map(|p|p.engine_id.as_str()).unwrap_or(&stored.format),"completedAt":chrono::Utc::now().to_rfc3339(),"warnings":stored.extraction.warnings}));
        if let Some(probe) = probe {
            project
                .extra
                .insert("adapterId".into(), serde_json::json!(probe.adapter_id));
            project.extra.insert(
                "adapterVersion".into(),
                serde_json::json!(probe.adapter_version),
            );
        }
        let path = self.project_directory(id)?.join("extraction/native.json");
        let stored = tokio::task::spawn_blocking(move || -> Result<StoredAnalysis, CoreError> {
            write_json(&path, &stored)?;
            Ok(stored)
        })
        .await??;
        // The extraction file is the commit point. Even if the lightweight summary
        // cannot be saved, subsequent operations must use these committed texts.
        self.analysis
            .lock()
            .await
            .insert(id, Arc::new(CachedAnalysis::Native(Box::new(stored))));
        self.workbenches.lock().await.remove(id);
        self.store.save(&project).await?;
        let _ = self.events.send(AppEvent::ProjectsChanged);
        Ok(())
    }

    pub(crate) async fn invalidate_project_cache(&self, id: &str) {
        let mut cache = self.analysis.lock().await;
        if let Some(position) = cache.entries.iter().position(|(key, _, _)| key == id)
            && let Some((_, _, bytes)) = cache.entries.remove(position)
        {
            cache.retained_bytes -= bytes;
        }
        drop(cache);
        self.workbenches.lock().await.remove(id);
        self.tasks.lock().await.remove(id);
    }

    /// Excludes deletion and short writes for one project without blocking other projects.
    pub(crate) async fn lock_project(
        &self,
        id: &str,
    ) -> Result<tokio::sync::OwnedMutexGuard<()>, CoreError> {
        crate::projects::validate_id(id)?;
        let gate = {
            let mut gates = self.project_access.lock().await;
            gates.retain(|_, gate| gate.strong_count() > 0);
            match gates.get(id).and_then(Weak::upgrade) {
                Some(gate) => gate,
                None => {
                    let gate = Arc::new(Mutex::new(()));
                    gates.insert(id.into(), Arc::downgrade(&gate));
                    gate
                }
            }
        };
        Ok(gate.lock_owned().await)
    }

    pub(crate) async fn project_access_active(&self) -> bool {
        self.project_access
            .lock()
            .await
            .values()
            .any(|gate| gate.strong_count() > 0)
    }

    pub(crate) async fn begin(
        &self,
        id: &str,
        operation: &str,
        message: &str,
    ) -> Result<CancellationToken, CoreError> {
        let _project = self.lock_project(id).await?;
        let _lifecycle = self.project_lifecycle.lock().await;
        if self.store.get(id).await?.is_removed {
            return Err(CoreError::ProjectMissing(id.into()));
        }
        self.store.ensure_writable().await?;
        if self
            .tasks
            .lock()
            .await
            .get(id)
            .is_some_and(|entry| entry.state.running)
        {
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
        let path = self.project_directory(id)?.join("rust-task.json");
        let saved = state.clone();
        tokio::task::spawn_blocking(move || write_json(&path, &saved)).await??;
        let order = self.next_task_order.fetch_add(1, Ordering::Relaxed);
        self.tasks.lock().await.insert(
            id.into(),
            TaskEntry {
                state: state.clone(),
                terminal_persisted: false,
                summary: None,
                cancellation: cancellation.clone(),
                order,
            },
        );
        self.start_metrics(id, operation).await;
        let _ = self.events.send(AppEvent::TaskChanged(state));
        drop(_lifecycle);
        self.log_task_message(id, message).await;
        Ok(cancellation)
    }

    pub(crate) async fn progress(&self, id: &str, operation: &str, message: &str) {
        if !self.update_task_progress(id, operation, message).await {
            return;
        }
        self.log_task_message(id, message).await;
    }

    pub(crate) async fn update_task_progress(
        &self,
        id: &str,
        operation: &str,
        message: &str,
    ) -> bool {
        let mut tasks = self.tasks.lock().await;
        let Some(entry) = tasks.get_mut(id) else {
            return false;
        };
        if !entry.state.running
            || (entry.state.operation == operation && entry.state.message == message)
        {
            return false;
        }
        let state = TaskState {
            project_id: id.into(),
            operation: operation.into(),
            running: true,
            message: message.into(),
            succeeded: None,
        };
        entry.state = state.clone();
        let _ = self.events.send(AppEvent::TaskChanged(state));
        true
    }

    pub(crate) async fn finish(
        &self,
        id: &str,
        operation: &str,
        result: Result<String, String>,
    ) -> Result<(), CoreError> {
        let project_guard = self.lock_project(id).await?;
        let _lifecycle = self.project_lifecycle.lock().await;
        let cancelled = self
            .tasks
            .lock()
            .await
            .get(id)
            .is_some_and(|entry| entry.cancellation.is_cancelled());
        let succeeded = result.is_ok();
        let mut state = TaskState {
            project_id: id.into(),
            operation: operation.into(),
            running: false,
            message: if cancelled {
                "任务已暂停，已保存的结果可继续使用。".into()
            } else {
                result.unwrap_or_else(|error| error)
            },
            succeeded: if cancelled { None } else { Some(succeeded) },
        };
        let (summary, detail_error) = match self.completed_task_summary(&state) {
            Ok(summary) => (summary, None),
            Err(error) => (task_summary(&state), Some(error)),
        };
        self.finish_metrics(id, state.succeeded).await;
        let path = self.store.directory(id)?.join("rust-task.json");
        let saved = StoredTaskState {
            state: state.clone(),
            task_summary: Some(summary.clone()),
        };
        let persisted = tokio::task::spawn_blocking(move || write_json(&path, &saved))
            .await
            .map_err(CoreError::from)
            .and_then(|result| result.map_err(CoreError::from));
        if let Err(error) = &persisted {
            state.message = format!("任务终态保存失败：{error}；任务结果：{}", state.message);
            state.succeeded = Some(false);
        }
        if let Some(entry) = self.tasks.lock().await.get_mut(id) {
            entry.state = state.clone();
            entry.terminal_persisted = persisted.is_ok();
            entry.summary = Some(summary.clone());
        }
        let _ = self.events.send(AppEvent::TaskChanged(state.clone()));
        persisted?;
        let summary_result = async {
            let mut project = self.store.get(id).await?;
            self.save_task_summary(&mut project, summary).await
        }
        .await;
        drop(_lifecycle);
        self.log_task_message(id, &state.message).await;
        drop(project_guard);
        if let Some(error) = detail_error {
            self.report_task_warning(
                id,
                &format!("任务结果已保存，适配详情暂未加入摘要：{error}"),
            );
        }
        if let Err(error) = summary_result {
            self.report_task_warning(
                id,
                &format!("任务结果已保存，项目列表摘要暂未更新：{error}"),
            );
        }
        if operation != "cleanup" {
            let _lifecycle = self.project_lifecycle.lock().await;
            if let Err(error) = self.clean_replaced_adapters().await {
                self.report_task_warning(
                    id,
                    &format!("任务结果已保存，旧适配器版本暂未清理：{error}"),
                );
            }
        }
        Ok(())
    }

    async fn save_task_summary(
        &self,
        project: &mut GameProject,
        summary: serde_json::Value,
    ) -> Result<(), CoreError> {
        if project.extra.get("taskSummary") == Some(&summary) {
            return Ok(());
        }
        project.extra.insert("taskSummary".into(), summary);
        self.store.save(project).await?;
        let _ = self.events.send(AppEvent::ProjectsChanged);
        Ok(())
    }

    fn completed_task_summary(&self, state: &TaskState) -> Result<serde_json::Value, CoreError> {
        let mut summary = task_summary(state);
        if state.operation == "adapt" && state.succeeded == Some(true) {
            let path = self
                .store
                .directory(&state.project_id)?
                .join("adaptation-result.json");
            if path.is_file() {
                let result: serde_json::Value = read_json(&path)?;
                summary["adaptationStatus"] = result["status"].clone();
                summary["pendingCoverageCount"] =
                    serde_json::json!(result["pendingCoverage"].as_array().map_or(0, Vec::len));
            }
        }
        Ok(summary)
    }

    pub(crate) async fn log_task_message(&self, id: &str, message: &str) {
        if let Err(error) = self.log(id, message).await {
            self.report_task_warning(id, &format!("日志写入失败：{error}"));
        }
    }

    pub(crate) fn report_task_warning(&self, id: &str, message: &str) {
        let _ = self.events.send(AppEvent::Log {
            project_id: id.into(),
            line: message.into(),
        });
    }
}

fn task_summary(state: &TaskState) -> serde_json::Value {
    serde_json::json!({
        "stage": match state.succeeded {
            Some(true) => "任务已完成",
            Some(false) => "任务失败",
            None => "任务已暂停",
        },
        "operation": state.operation,
        "succeeded": state.succeeded,
        "running": false,
    })
}

fn prepare_snapshot(
    source: &Path,
    root: &Path,
    files: &[String],
    cancellation: &CancellationToken,
    progress: &FileProgress,
) -> Result<(PathBuf, BTreeMap<String, String>), CoreError> {
    let mut hashes = BTreeMap::new();
    for file in files {
        if cancellation.is_cancelled() {
            return Err(gamel10n_engines::EngineError::Cancelled.into());
        }
        let path = resolve_relative(source, file)?;
        progress.file("正在校验原始脚本", &path, file);
        hashes.insert(file.clone(), hash_file(&path)?);
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
            let path = resolve_relative(&snapshot, relative)?;
            progress.file("正在校验已有脚本快照", &path, relative);
            if hash_file(&path)? != *expected {
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
        let path = resolve_relative(source, relative)?;
        progress.file("正在复制脚本快照", &path, relative);
        fs::copy(path, &destination)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn repeated_successful_adaptation_recovers_its_new_summary_without_rereading_details() {
        let temporary = tempfile::tempdir().unwrap();
        let data = temporary.path().join("data");
        let game = temporary.path().join("game");
        fs::create_dir(&game).unwrap();
        let app = Application::open(data.clone(), &ToolPaths::new("missing"))
            .await
            .unwrap();
        let project = app.add_project(&game).await.unwrap();
        let directory = app.project_directory(&project.id).unwrap();
        let details = directory.join("adaptation-result.json");
        write_json(
            &details,
            &serde_json::json!({"status":"Activated", "pendingCoverage":[]}),
        )
        .unwrap();
        app.begin(&project.id, "adapt", "first adaptation")
            .await
            .unwrap();
        app.finish(&project.id, "adapt", Ok("first result".into()))
            .await
            .unwrap();
        let metadata = directory.join("game.json");
        let previous = fs::read(&metadata).unwrap();

        write_json(
            &details,
            &serde_json::json!({"status":"Activated", "pendingCoverage":["menu", "settings"]}),
        )
        .unwrap();
        app.begin(&project.id, "adapt", "second adaptation")
            .await
            .unwrap();
        fs::remove_file(&metadata).unwrap();
        fs::create_dir(&metadata).unwrap();
        app.finish(&project.id, "adapt", Ok("second result".into()))
            .await
            .unwrap();
        fs::remove_dir(&metadata).unwrap();
        fs::write(&metadata, previous).unwrap();
        fs::write(&details, "must recover from the committed task summary").unwrap();
        drop(app);

        let reopened = Application::open(data, &ToolPaths::new("missing"))
            .await
            .unwrap();
        let recovered = reopened.open_project(&project.id).await.unwrap();
        assert_eq!(recovered.extra["taskSummary"]["pendingCoverageCount"], 2);
        let saved: GameProject = read_json(&metadata).unwrap();
        assert_eq!(saved.extra["taskSummary"], recovered.extra["taskSummary"]);
        assert_eq!(
            reopened.task_state(&project.id).await.unwrap().message,
            "second result"
        );
    }

    #[tokio::test]
    async fn unavailable_adaptation_details_do_not_prevent_persisting_a_terminal_result() {
        let temporary = tempfile::tempdir().unwrap();
        let game = temporary.path().join("game");
        fs::create_dir(&game).unwrap();
        let app = Application::open(temporary.path().join("data"), &ToolPaths::new("missing"))
            .await
            .unwrap();
        let project = app.add_project(&game).await.unwrap();
        let directory = app.project_directory(&project.id).unwrap();
        fs::write(
            directory.join("adaptation-result.json"),
            "invalid detail payload",
        )
        .unwrap();
        app.begin(&project.id, "adapt", "starting").await.unwrap();
        let mut events = app.subscribe();

        app.finish(&project.id, "adapt", Ok("finished".into()))
            .await
            .unwrap();
        let saved: StoredTaskState = read_json(&directory.join("rust-task.json")).unwrap();
        assert_eq!(saved.state.succeeded, Some(true));
        assert_eq!(saved.task_summary.unwrap()["succeeded"], true);
        assert!(std::iter::from_fn(|| events.try_recv().ok()).any(|event| {
            matches!(event, AppEvent::Log { line, .. } if line.contains("适配详情暂未加入摘要"))
        }));
    }

    #[tokio::test]
    async fn cached_terminal_tasks_do_not_reread_task_or_adaptation_payloads() {
        let temporary = tempfile::tempdir().unwrap();
        let game = temporary.path().join("game");
        fs::create_dir(&game).unwrap();
        let app = Application::open(temporary.path().join("data"), &ToolPaths::new("missing"))
            .await
            .unwrap();
        let project = app.add_project(&game).await.unwrap();
        let directory = app.project_directory(&project.id).unwrap();
        write_json(
            &directory.join("adaptation-result.json"),
            &serde_json::json!({
                "status":"Activated", "pendingCoverage":[],
            }),
        )
        .unwrap();
        app.begin(&project.id, "adapt", "starting").await.unwrap();
        app.finish(&project.id, "adapt", Ok("finished".into()))
            .await
            .unwrap();
        fs::write(directory.join("rust-task.json"), "must not be read again").unwrap();
        fs::write(
            directory.join("adaptation-result.json"),
            "must not be read again",
        )
        .unwrap();
        let mut events = app.subscribe();

        app.open_project(&project.id).await.unwrap();
        assert_eq!(
            app.task_state(&project.id).await.unwrap().message,
            "finished"
        );
        assert!(events.try_recv().is_err());
    }

    #[tokio::test]
    async fn unavailable_logs_do_not_prevent_starting_or_finishing_tasks() {
        let temporary = tempfile::tempdir().unwrap();
        let game = temporary.path().join("game");
        fs::create_dir(&game).unwrap();
        let app = Application::open(temporary.path().join("data"), &ToolPaths::new("missing"))
            .await
            .unwrap();
        let project = app.add_project(&game).await.unwrap();
        let directory = app.project_directory(&project.id).unwrap();
        fs::write(directory.join("logs"), "unavailable").unwrap();
        let mut events = app.subscribe();

        app.begin(&project.id, "translate", "starting")
            .await
            .unwrap();
        assert!(matches!(
            app.begin(&project.id, "build", "overlapping").await,
            Err(CoreError::ProjectBusy)
        ));
        app.finish(&project.id, "translate", Ok("finished".into()))
            .await
            .unwrap();

        let saved: TaskState = read_json(&directory.join("rust-task.json")).unwrap();
        assert!(!saved.running && saved.succeeded == Some(true));
        assert_eq!(
            app.task_state(&project.id).await.unwrap().message,
            "finished"
        );
        assert!(std::iter::from_fn(|| events.try_recv().ok()).any(|event| {
            matches!(event, AppEvent::Log { line, .. } if line.contains("日志写入失败"))
        }));
    }

    #[tokio::test]
    async fn failed_start_state_write_preserves_the_previous_terminal_state() {
        let temporary = tempfile::tempdir().unwrap();
        let game = temporary.path().join("game");
        fs::create_dir(&game).unwrap();
        let app = Application::open(temporary.path().join("data"), &ToolPaths::new("missing"))
            .await
            .unwrap();
        let project = app.add_project(&game).await.unwrap();
        app.begin(&project.id, "translate", "starting")
            .await
            .unwrap();
        app.finish(&project.id, "translate", Ok("finished".into()))
            .await
            .unwrap();
        let path = app
            .project_directory(&project.id)
            .unwrap()
            .join("rust-task.json");
        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();

        assert!(app.begin(&project.id, "build", "starting").await.is_err());
        let state = app.task_state(&project.id).await.unwrap();
        assert!(!state.running && state.succeeded == Some(true) && state.message == "finished");
    }

    #[tokio::test]
    async fn terminal_state_write_failure_is_reported_without_leaving_a_busy_task() {
        let temporary = tempfile::tempdir().unwrap();
        let game = temporary.path().join("game");
        fs::create_dir(&game).unwrap();
        let app = Application::open(temporary.path().join("data"), &ToolPaths::new("missing"))
            .await
            .unwrap();
        let project = app.add_project(&game).await.unwrap();
        app.begin(&project.id, "translate", "starting")
            .await
            .unwrap();
        let path = app
            .project_directory(&project.id)
            .unwrap()
            .join("rust-task.json");
        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();
        let mut events = app.subscribe();

        assert!(
            app.finish(&project.id, "translate", Ok("finished".into()))
                .await
                .is_err()
        );
        let state = app.task_state(&project.id).await.unwrap();
        assert!(!state.running && state.succeeded == Some(false));
        assert!(state.message.contains("任务终态保存失败"));
        assert!(
            matches!(events.try_recv().unwrap(), AppEvent::TaskChanged(state) if !state.running && state.succeeded == Some(false))
        );
        assert!(
            !app.update_task_progress(&project.id, "translate", "late progress")
                .await
        );
        fs::remove_dir(&path).unwrap();
        app.begin(&project.id, "build", "next task").await.unwrap();
    }

    #[tokio::test]
    async fn interrupted_tasks_are_durably_recovered_with_their_summary() {
        let temporary = tempfile::tempdir().unwrap();
        let data = temporary.path().join("data");
        let game = temporary.path().join("game");
        fs::create_dir(&game).unwrap();
        let app = Application::open(data.clone(), &ToolPaths::new("missing"))
            .await
            .unwrap();
        let project = app.add_project(&game).await.unwrap();
        app.begin(&project.id, "translate", "starting")
            .await
            .unwrap();
        drop(app);

        let reopened = Application::open(data, &ToolPaths::new("missing"))
            .await
            .unwrap();
        let recovered = reopened.open_project(&project.id).await.unwrap();
        let state: TaskState = read_json(
            &reopened
                .project_directory(&project.id)
                .unwrap()
                .join("rust-task.json"),
        )
        .unwrap();
        assert!(!state.running && state.succeeded.is_none() && state.message.contains("中断"));
        assert_eq!(
            recovered.extra["taskSummary"],
            serde_json::json!({
                "stage":"任务已暂停", "operation":"translate", "succeeded":null, "running":false,
            })
        );
        assert_eq!(
            reopened.list_projects("").await[0].extra["taskSummary"],
            recovered.extra["taskSummary"]
        );
    }

    #[tokio::test]
    async fn opening_a_terminal_task_recovers_a_failed_project_summary_write() {
        let temporary = tempfile::tempdir().unwrap();
        let data = temporary.path().join("data");
        let game = temporary.path().join("game");
        fs::create_dir(&game).unwrap();
        let app = Application::open(data.clone(), &ToolPaths::new("missing"))
            .await
            .unwrap();
        let project = app.add_project(&game).await.unwrap();
        app.begin(&project.id, "translate", "starting")
            .await
            .unwrap();
        let path = app
            .project_directory(&project.id)
            .unwrap()
            .join("game.json");
        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();

        app.finish(&project.id, "translate", Ok("finished".into()))
            .await
            .unwrap();
        assert_eq!(
            app.task_state(&project.id).await.unwrap().succeeded,
            Some(true)
        );
        fs::remove_dir(&path).unwrap();
        write_json(&path, &project).unwrap();
        drop(app);

        let reopened = Application::open(data, &ToolPaths::new("missing"))
            .await
            .unwrap();
        let recovered = reopened.open_project(&project.id).await.unwrap();
        let saved: GameProject = read_json(&path).unwrap();
        assert_eq!(saved.extra["taskSummary"]["succeeded"], true);
        assert_eq!(saved.extra["taskSummary"], recovered.extra["taskSummary"]);
    }

    #[tokio::test]
    async fn completed_tasks_keep_full_details_outside_the_project_index() {
        let temporary = tempfile::tempdir().unwrap();
        let data = temporary.path().join("data");
        let game = temporary.path().join("game");
        fs::create_dir(&game).unwrap();
        let app = Application::open(data.clone(), &ToolPaths::new("missing"))
            .await
            .unwrap();
        let project = app.add_project(&game).await.unwrap();
        let message = "完整适配结果。\n支持对白，但设置文字未覆盖。\n先试译并进游戏检查字体。";
        write_json(
            &app.project_directory(&project.id)
                .unwrap()
                .join("adaptation-result.json"),
            &serde_json::json!({"status":"Activated", "pendingCoverage":["设置文字"]}),
        )
        .unwrap();
        app.begin(&project.id, "adapt", "starting").await.unwrap();
        app.finish(&project.id, "adapt", Ok(message.into()))
            .await
            .unwrap();
        let index = app.list_projects("").await.remove(0);
        let reopened = Application::open(data, &ToolPaths::new("missing"))
            .await
            .unwrap();
        reopened.open_project(&project.id).await.unwrap();
        assert_eq!(
            (
                &index.extra["taskSummary"],
                reopened
                    .task_state(&project.id)
                    .await
                    .unwrap()
                    .message
                    .as_str()
            ),
            (
                &serde_json::json!({"stage":"任务已完成", "operation":"adapt", "succeeded":true, "running":false, "adaptationStatus":"Activated", "pendingCoverageCount":1}),
                message
            ),
        );
    }

    #[tokio::test]
    async fn running_projects_keep_start_order_until_the_task_finishes() {
        let temporary = tempfile::tempdir().unwrap();
        let app = Application::open(temporary.path().join("data"), &ToolPaths::new("missing"))
            .await
            .unwrap();
        let mut ids = Vec::new();
        for (index, name) in ["Z first task", "A second task", "Newest idle"]
            .iter()
            .enumerate()
        {
            let directory = temporary.path().join(name);
            fs::create_dir(&directory).unwrap();
            let mut project = app.add_project(&directory).await.unwrap();
            project.extra.insert(
                "updatedAt".into(),
                serde_json::json!(format!("2026-11-0{}T00:00:00Z", index + 1)),
            );
            app.store.save(&project).await.unwrap();
            ids.push(project.id);
        }
        app.begin(&ids[0], "translate", "queued").await.unwrap();
        app.begin(&ids[1], "translate", "queued").await.unwrap();
        let initial: Vec<_> = app
            .list_projects("")
            .await
            .into_iter()
            .map(|p| p.id)
            .collect();
        app.update_task_progress(&ids[1], "translate", "progress")
            .await;
        let progress: Vec<_> = app
            .list_projects("")
            .await
            .into_iter()
            .map(|p| p.id)
            .collect();
        app.finish(&ids[0], "translate", Ok("finished".into()))
            .await
            .unwrap();
        let finished: Vec<_> = app
            .list_projects("")
            .await
            .into_iter()
            .map(|p| p.id)
            .collect();
        assert_eq!(
            [initial, progress, finished],
            [
                vec![ids[0].clone(), ids[1].clone(), ids[2].clone()],
                vec![ids[0].clone(), ids[1].clone(), ids[2].clone()],
                vec![ids[1].clone(), ids[2].clone(), ids[0].clone()],
            ]
        );
    }
}
