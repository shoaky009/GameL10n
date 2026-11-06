//! UI-independent text browsing, saved edits, review state and candidates.

use crate::{Application, CoreError};
use gamel10n_translation::{
    TranslationUnit, ValidationIssue,
    edits::EditChanges,
    execution::load_issues,
    storage::{TranslationEdit, TranslationMemory, TranslationPolicy, memory_key},
    text,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::PathBuf,
    sync::Arc,
};
use tokio::sync::Mutex;

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct WorkbenchFilter {
    pub query: String,
    pub source_query: String,
    pub translation_query: String,
    pub status: String,
    pub kind: String,
    pub resource_path: Option<String>,
    pub selected_ids: Option<Vec<String>>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkbenchRow {
    pub unit: TranslationUnit,
    pub text: Option<String>,
    pub model_text: Option<String>,
    pub candidate: Option<String>,
    pub reviewed: bool,
    pub edited: bool,
    pub issues: Vec<ValidationIssue>,
    pub failure: Option<String>,
    pub previous_edit: Option<String>,
}
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkbenchSummary {
    pub total: usize,
    pub translated: usize,
    pub reviewed: usize,
    pub invalid: usize,
    pub failed: usize,
    pub edited: usize,
}
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkbenchPage {
    pub total: usize,
    pub offset: usize,
    pub rows: Vec<WorkbenchRow>,
    pub summary: WorkbenchSummary,
    pub resources: Vec<String>,
    pub kinds: Vec<String>,
}

struct RowState {
    index: usize,
    model: Option<String>,
    edit: Option<TranslationEdit>,
    candidate: Option<TranslationEdit>,
    issues: Vec<ValidationIssue>,
    failure: Option<String>,
    previous_edit: Option<String>,
}
impl RowState {
    fn text(&self) -> Option<&str> {
        self.edit
            .as_ref()
            .map(|edit| edit.text.as_str())
            .or(self.model.as_deref())
    }
    fn reviewed(&self) -> bool {
        self.edit.as_ref().is_some_and(|edit| edit.reviewed) && self.issues.is_empty()
    }
    fn failure(&self) -> Option<&str> {
        self.failure.as_deref()
    }
    fn edited(&self) -> bool {
        self.edit.as_ref().is_some_and(|edit| !edit.is_generated)
    }
    fn row(&self, unit: &TranslationUnit) -> WorkbenchRow {
        WorkbenchRow {
            unit: unit.clone(),
            text: self.text().map(str::to_owned),
            model_text: self.model.clone(),
            candidate: self.candidate.as_ref().map(|edit| edit.text.clone()),
            reviewed: self.reviewed(),
            edited: self.edited(),
            issues: self.issues.clone(),
            failure: self.failure().map(|failure| {
                if self.text().is_some() && self.issues.is_empty() {
                    format!("上次请求失败（当前已保存译文仍可使用）：{failure}")
                } else {
                    format!("上次请求失败：{failure}")
                }
            }),
            previous_edit: self.previous_edit.clone(),
        }
    }
}

/// Holds drafts and edit metadata only; source units stay in the application's extraction cache.
pub struct Workbench {
    pub(crate) analysis_key: usize,
    pub(crate) policy: TranslationPolicy,
    rows: HashMap<String, RowState>,
    edits: Arc<gamel10n_translation::edits::EditStore>,
    resources: Vec<String>,
    kinds: Vec<String>,
    last_access: std::time::Instant,
}
impl Workbench {
    fn load(
        directory: PathBuf,
        legacy_memory: PathBuf,
        units: &[TranslationUnit],
        policy: TranslationPolicy,
        analysis_key: usize,
    ) -> Result<Self, CoreError> {
        let memory = TranslationMemory::load(directory.join("memory"), Some(&legacy_memory))
            .map_err(storage_error)?;
        let edits =
            gamel10n_translation::edits::EditStore::open(&directory).map_err(storage_error)?;
        let failures: HashMap<_, _> = load_issues(&directory)
            .into_iter()
            .filter_map(|issue| issue.unit_id.clone().map(|id| (id, issue)))
            .collect();
        let mut rows = HashMap::with_capacity(units.len());
        let mut resources = std::collections::BTreeSet::new();
        let mut kinds = std::collections::BTreeSet::new();
        edits
            .read(|edits| -> Result<(), CoreError> {
                for (index, unit) in units.iter().enumerate() {
                    let model = memory
                        .find(unit, &policy)
                        .map_err(storage_error)?
                        .map(str::to_owned);
                    let edit = edits.find(unit, &policy.target_language).cloned();
                    let candidate = edits.candidate(unit, &policy.target_language).cloned();
                    let previous_edit = if edit.is_none() {
                        edits.entries.get(&unit.id).map(|edit| edit.text.clone())
                    } else {
                        None
                    };
                    let text = edit
                        .as_ref()
                        .map(|edit| edit.text.as_str())
                        .or(model.as_deref());
                    let issues = text
                        .map(|text| text::validate(unit, text))
                        .unwrap_or_default();
                    let failure = failures
                        .get(&unit.id)
                        .filter(|issue| {
                            issue
                                .memory_key
                                .as_ref()
                                .is_none_or(|key| *key == memory_key(unit, &policy))
                        })
                        .map(|issue| issue.message.clone());
                    rows.insert(
                        unit.id.clone(),
                        RowState {
                            index,
                            model,
                            edit,
                            candidate,
                            issues,
                            failure,
                            previous_edit,
                        },
                    );
                    resources.insert(unit.resource_path.clone());
                    kinds.insert(unit.kind.clone());
                }
                Ok(())
            })
            .map_err(storage_error)??;
        Ok(Self {
            policy,
            analysis_key,
            rows,
            edits,
            resources: resources.into_iter().collect(),
            kinds: kinds.into_iter().collect(),
            last_access: std::time::Instant::now(),
        })
    }
    fn retained_bytes(&self) -> usize {
        let edit_bytes = |edit: &TranslationEdit| {
            edit.text.len()
                + edit.source_signature.len()
                + edit.language.len()
                + edit.updated_at.len()
                + edit.retranslation_id.as_ref().map_or(0, String::len)
        };
        self.rows.capacity() * std::mem::size_of::<(String, RowState)>()
            + self
                .rows
                .iter()
                .map(|(id, row)| {
                    id.len()
                        + row.model.as_ref().map_or(0, String::len)
                        + row.previous_edit.as_ref().map_or(0, String::len)
                        + row.failure.as_ref().map_or(0, String::len)
                        + row.edit.as_ref().map_or(0, &edit_bytes)
                        + row.candidate.as_ref().map_or(0, &edit_bytes)
                        + row
                            .issues
                            .iter()
                            .map(|issue| {
                                std::mem::size_of::<ValidationIssue>()
                                    + issue.code.len()
                                    + issue.message.len()
                                    + issue.unit_id.as_ref().map_or(0, String::len)
                            })
                            .sum::<usize>()
                })
                .sum::<usize>()
            + self
                .edits
                .read(|edits| {
                    edits
                        .entries
                        .iter()
                        .chain(&edits.candidates)
                        .map(|(id, edit)| id.len() + edit_bytes(edit) + 64)
                        .sum::<usize>()
                })
                .unwrap_or_default()
            + self
                .resources
                .iter()
                .chain(&self.kinds)
                .map(String::len)
                .sum::<usize>()
    }
    fn page(
        &self,
        units: &[TranslationUnit],
        filter: &WorkbenchFilter,
        offset: usize,
        limit: usize,
    ) -> WorkbenchPage {
        let query = filter.query.to_lowercase();
        let source = filter.source_query.to_lowercase();
        let translation = filter.translation_query.to_lowercase();
        let selected: Option<HashSet<_>> = filter
            .selected_ids
            .as_ref()
            .map(|ids| ids.iter().map(String::as_str).collect());
        let mut total = 0;
        let mut rows = Vec::new();
        let mut summary = WorkbenchSummary::default();
        let limit = limit.clamp(1, 500);
        for unit in units {
            let Some(row) = self.rows.get(&unit.id) else {
                continue;
            };
            summary.total += 1;
            summary.translated += usize::from(row.text().is_some());
            summary.reviewed += usize::from(row.reviewed());
            summary.invalid += usize::from(!row.issues.is_empty());
            summary.failed += usize::from(row.failure().is_some());
            summary.edited += usize::from(row.edited());
            if !matches_filter(
                unit,
                row,
                filter,
                &query,
                &source,
                &translation,
                selected.as_ref(),
            ) {
                continue;
            }
            if total >= offset && rows.len() < limit {
                rows.push(row.row(unit));
            }
            total += 1;
        }
        WorkbenchPage {
            total,
            offset: offset.min(total),
            rows,
            summary,
            resources: self.resources.clone(),
            kinds: self.kinds.clone(),
        }
    }
    fn refresh_row(&mut self, unit: &TranslationUnit) -> Result<(), CoreError> {
        self.edits
            .read(|edits| {
                if let Some(row) = self.rows.get_mut(&unit.id) {
                    row.edit = edits.find(unit, &self.policy.target_language).cloned();
                    row.previous_edit = if row.edit.is_none() {
                        edits.entries.get(&unit.id).map(|edit| edit.text.clone())
                    } else {
                        None
                    };
                    row.candidate = edits.candidate(unit, &self.policy.target_language).cloned();
                    row.issues = row
                        .text()
                        .map(|text| text::validate(unit, text))
                        .unwrap_or_default();
                }
            })
            .map_err(storage_error)?;
        Ok(())
    }
    pub(crate) fn apply_progress(
        &mut self,
        units: &[TranslationUnit],
        progress: &gamel10n_translation::execution::TranslationProgress,
        options: &gamel10n_translation::execution::TranslationOptions,
    ) {
        for (id, text) in &progress.updates {
            let Some(row) = self.rows.get_mut(id) else {
                continue;
            };
            let Some(unit) = units.get(row.index) else {
                continue;
            };
            if options.candidate_only {
                let mut edit =
                    TranslationEdit::new(unit, &self.policy.target_language, text.clone(), false);
                edit.is_generated = true;
                row.candidate = Some(edit);
            } else if options.retranslation_id.is_some() {
                let mut edit =
                    TranslationEdit::new(unit, &self.policy.target_language, text.clone(), false);
                edit.is_generated = true;
                edit.retranslation_id = options.retranslation_id.clone();
                row.edit = Some(edit);
                row.candidate = None;
            } else {
                row.model = Some(text.clone());
            }
            row.issues = row
                .text()
                .map(|text| text::validate(unit, text))
                .unwrap_or_default();
        }
        for (id, failure) in &progress.failure_updates {
            if let Some(row) = self.rows.get_mut(id) {
                row.failure = failure.clone();
            }
        }
    }
}

fn matches_filter(
    unit: &TranslationUnit,
    row: &RowState,
    filter: &WorkbenchFilter,
    query: &str,
    source: &str,
    translation: &str,
    selected: Option<&HashSet<&str>>,
) -> bool {
    if filter
        .resource_path
        .as_ref()
        .is_some_and(|path| *path != unit.resource_path)
        || !matches!(filter.kind.as_str(), "" | "all") && filter.kind != unit.kind
        || selected.is_some_and(|ids| !ids.contains(unit.id.as_str()))
    {
        return false;
    }
    let status = match filter.status.as_str() {
        "missing" => row.text().is_none(),
        "failed" => row.failure().is_some(),
        "pending" => row.text().is_some() && !row.reviewed(),
        "edited" => row.edited(),
        "reviewed" => row.reviewed(),
        "invalid" => !row.issues.is_empty(),
        "candidate" => row.candidate.is_some(),
        _ => true,
    };
    if !status
        || !source.is_empty() && !unit.source_text.to_lowercase().contains(source)
        || !translation.is_empty()
            && row
                .text()
                .is_none_or(|text| !text.to_lowercase().contains(translation))
    {
        return false;
    }
    query.is_empty()
        || unit.source_text.to_lowercase().contains(query)
        || row
            .text()
            .is_some_and(|text| text.to_lowercase().contains(query))
        || unit.resource_path.to_lowercase().contains(query)
        || unit.locator.to_lowercase().contains(query)
        || unit
            .speaker
            .as_ref()
            .is_some_and(|speaker| speaker.to_lowercase().contains(query))
}

pub(crate) fn policy(settings: &Value) -> TranslationPolicy {
    let terminology = settings
        .get("terminology")
        .map(parse_terminology)
        .unwrap_or_default();
    TranslationPolicy {
        target_language: settings
            .get("targetLanguage")
            .and_then(Value::as_str)
            .filter(|text| !text.trim().is_empty())
            .unwrap_or("简体中文")
            .into(),
        style: settings
            .get("style")
            .and_then(Value::as_str)
            .unwrap_or("自然流畅，保留人物语气、情绪和原文含义。")
            .into(),
        terminology,
        version: "1".into(),
    }
}
pub(crate) fn validated_policy(settings: &Value) -> Result<TranslationPolicy, CoreError> {
    if let Some(terminology) = settings.get("terminology").and_then(Value::as_str) {
        let mut seen = HashSet::new();
        for line in terminology
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
        {
            let (source, target) = line
                .split_once('=')
                .filter(|(source, target)| !source.trim().is_empty() && !target.trim().is_empty())
                .ok_or_else(|| CoreError::InvalidData("术语表每行必须为：原文=译文。".into()))?;
            if !seen.insert(source.trim()) {
                return Err(CoreError::InvalidData("术语表有重复的原文。".into()));
            }
            let _ = target;
        }
    }
    Ok(policy(settings))
}
pub fn parse_terminology(value: &Value) -> BTreeMap<String, String> {
    if let Some(map) = value.as_object() {
        return map
            .iter()
            .filter_map(|(key, value)| value.as_str().map(|value| (key.clone(), value.into())))
            .collect();
    }
    let mut result = BTreeMap::new();
    for line in value.as_str().unwrap_or_default().lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some((source, target)) = line.split_once('=')
            && !source.trim().is_empty()
        {
            result.insert(source.trim().into(), target.trim().into());
        }
    }
    result
}
fn storage_error(error: gamel10n_translation::storage::StorageError) -> CoreError {
    CoreError::InvalidData(error.to_string())
}

impl Application {
    async fn workbench(
        &self,
        id: &str,
    ) -> Result<
        (
            Arc<crate::application::CachedAnalysis>,
            Arc<Mutex<Workbench>>,
        ),
        CoreError,
    > {
        let analysis = self
            .cached_analysis(id)
            .await?
            .ok_or_else(|| CoreError::InvalidData("请先识别并提取游戏文本。".into()))?;
        let project = self.store.get(id).await?;
        let policy = validated_policy(&project.settings)?;
        let analysis_key = Arc::as_ptr(&analysis) as usize;
        if let Some(workbench) = self.workbenches.lock().await.get(id).cloned() {
            let mut current = workbench.lock().await;
            if current.policy == policy && current.analysis_key == analysis_key {
                current.last_access = std::time::Instant::now();
                drop(current);
                return Ok((analysis, workbench));
            }
        }
        let directory = self.store.directory(id)?;
        let legacy = self.data_directory().join("memory").join(id);
        let cached = Arc::clone(&analysis);
        let workbench = tokio::task::spawn_blocking(move || {
            Workbench::load(
                directory,
                legacy,
                &cached.extraction().units,
                policy,
                analysis_key,
            )
        })
        .await??;
        let mut cache = self.workbenches.lock().await;
        if let Some(current) = cache.get(id).cloned() {
            let mut current_data = current.lock().await;
            if current_data.policy == workbench.policy && current_data.analysis_key == analysis_key
            {
                current_data.last_access = std::time::Instant::now();
                drop(current_data);
                return Ok((analysis, current));
            }
        }
        cache.remove(id);
        let incoming = workbench.retained_bytes();
        let mut cached = Vec::with_capacity(cache.len());
        for (key, value) in cache.iter() {
            let value = value.lock().await;
            cached.push((key.clone(), value.last_access, value.retained_bytes()));
        }
        cached.sort_by_key(|(_, access, _)| *access);
        let mut retained = incoming + cached.iter().map(|(_, _, bytes)| *bytes).sum::<usize>();
        for (key, _, bytes) in cached {
            if cache.len() < 4 && retained <= 128 * 1024 * 1024 {
                break;
            }
            cache.remove(&key);
            retained = retained.saturating_sub(bytes);
        }
        let workbench = Arc::new(Mutex::new(workbench));
        cache.insert(id.into(), Arc::clone(&workbench));
        Ok((analysis, workbench))
    }
    pub async fn workbench_page(
        &self,
        id: &str,
        filter: &WorkbenchFilter,
        offset: usize,
        limit: usize,
    ) -> Result<WorkbenchPage, CoreError> {
        if self.cached_analysis(id).await?.is_none() {
            return Ok(WorkbenchPage::default());
        }
        let (analysis, workbench) = self.workbench(id).await?;
        let filter = filter.clone();
        tokio::task::spawn_blocking(move || {
            workbench
                .blocking_lock()
                .page(&analysis.extraction().units, &filter, offset, limit)
        })
        .await
        .map_err(CoreError::from)
    }
    pub async fn workbench_row(&self, id: &str, unit_id: &str) -> Result<WorkbenchRow, CoreError> {
        let (analysis, workbench) = self.workbench(id).await?;
        let workbench = workbench.lock().await;
        let unit = analysis
            .extraction()
            .units
            .iter()
            .find(|unit| unit.id == unit_id)
            .ok_or_else(|| CoreError::InvalidData("找不到所选文本。".into()))?;
        workbench
            .rows
            .get(unit_id)
            .map(|row| row.row(unit))
            .ok_or_else(|| CoreError::InvalidData("找不到所选文本。".into()))
    }
    pub async fn matching_text_ids(
        &self,
        id: &str,
        filter: &WorkbenchFilter,
    ) -> Result<Vec<String>, CoreError> {
        let (analysis, workbench) = self.workbench(id).await?;
        let filter = filter.clone();
        tokio::task::spawn_blocking(move || {
            let workbench = workbench.blocking_lock();
            let selected = filter
                .selected_ids
                .as_ref()
                .map(|ids| ids.iter().map(String::as_str).collect());
            let query = filter.query.to_lowercase();
            let source = filter.source_query.to_lowercase();
            let translation = filter.translation_query.to_lowercase();
            analysis
                .extraction()
                .units
                .iter()
                .filter(|unit| {
                    workbench.rows.get(&unit.id).is_some_and(|row| {
                        matches_filter(
                            unit,
                            row,
                            &filter,
                            &query,
                            &source,
                            &translation,
                            selected.as_ref(),
                        )
                    })
                })
                .map(|unit| unit.id.clone())
                .collect()
        })
        .await
        .map_err(CoreError::from)
    }
    pub async fn adjacent_text(
        &self,
        id: &str,
        filter: &WorkbenchFilter,
        current: &str,
        direction: i32,
    ) -> Result<Option<(String, usize)>, CoreError> {
        let (analysis, workbench) = self.workbench(id).await?;
        let workbench = workbench.lock().await;
        let units = &analysis.extraction().units;
        let selected = filter
            .selected_ids
            .as_ref()
            .map(|ids| ids.iter().map(String::as_str).collect());
        let query = filter.query.to_lowercase();
        let source = filter.source_query.to_lowercase();
        let translation = filter.translation_query.to_lowercase();
        let matches: Vec<_> = units
            .iter()
            .enumerate()
            .filter(|(_, unit)| {
                workbench.rows.get(&unit.id).is_some_and(|row| {
                    matches_filter(
                        unit,
                        row,
                        filter,
                        &query,
                        &source,
                        &translation,
                        selected.as_ref(),
                    )
                })
            })
            .collect();
        if matches.is_empty() {
            return Ok(None);
        }
        let current_position = units.iter().position(|unit| unit.id == current);
        let next = if let Some(position) = matches.iter().position(|(_, unit)| unit.id == current) {
            if direction < 0 {
                position.saturating_sub(1)
            } else {
                (position + 1).min(matches.len() - 1)
            }
        } else if direction < 0 {
            current_position
                .and_then(|current| matches.iter().rposition(|(index, _)| *index < current))
                .unwrap_or(0)
        } else {
            current_position
                .and_then(|current| matches.iter().position(|(index, _)| *index > current))
                .unwrap_or(matches.len() - 1)
        };
        Ok(Some((matches[next].1.id.clone(), next)))
    }
    pub async fn workbench_context(&self, id: &str, unit_id: &str) -> Result<String, CoreError> {
        let analysis = self
            .cached_analysis(id)
            .await?
            .ok_or_else(|| CoreError::InvalidData("请先提取游戏文本。".into()))?;
        let units = &analysis.extraction().units;
        let index = units
            .iter()
            .position(|unit| unit.id == unit_id)
            .ok_or_else(|| CoreError::InvalidData("找不到所选文本。".into()))?;
        let unit = &units[index];
        let before = index
            .checked_sub(1)
            .and_then(|index| units.get(index))
            .filter(|neighbor| neighbor.resource_path == unit.resource_path)
            .map_or("（本资源首条文本）", |unit| {
                unit.source_text.as_str()
            });
        let after = units
            .get(index + 1)
            .filter(|neighbor| neighbor.resource_path == unit.resource_path)
            .map_or("（本资源末条文本）", |unit| {
                unit.source_text.as_str()
            });
        Ok(format!(
            "上一条：{before}\n\n下一条：{after}{}",
            if unit.context.is_empty() {
                String::new()
            } else {
                format!("\n\n提取上下文：{}", unit.context)
            }
        ))
    }
    pub async fn load_text_selection(&self, id: &str) -> Result<Vec<String>, CoreError> {
        let directory = self.store.directory(id)?;
        let selection: Vec<String> = gamel10n_translation::storage::read_recoverable(
            &directory.join("trial-selection.json"),
        )
        .map_err(storage_error)?
        .unwrap_or_default();
        if selection.is_empty() {
            return Ok(selection);
        }
        let Some(analysis) = self.cached_analysis(id).await? else {
            return Ok(Vec::new());
        };
        let valid: HashSet<_> = analysis
            .extraction()
            .units
            .iter()
            .map(|unit| unit.id.as_str())
            .collect();
        let mut seen = HashSet::new();
        Ok(selection
            .into_iter()
            .filter(|id| valid.contains(id.as_str()) && seen.insert(id.clone()))
            .collect())
    }
    pub async fn save_text_selection(
        &self,
        id: &str,
        selection: &[String],
    ) -> Result<(), CoreError> {
        let _project = self.lock_project(id).await?;
        self.store.ensure_writable().await?;
        let analysis = self
            .cached_analysis(id)
            .await?
            .ok_or_else(|| CoreError::InvalidData("请先提取游戏文本。".into()))?;
        let valid: HashSet<_> = analysis
            .extraction()
            .units
            .iter()
            .map(|unit| unit.id.as_str())
            .collect();
        let mut seen = HashSet::new();
        if selection
            .iter()
            .any(|id| !valid.contains(id.as_str()) || !seen.insert(id))
        {
            return Err(CoreError::InvalidData(
                "文本选区已失效，请重新选择。".into(),
            ));
        }
        Ok(gamel10n_translation::files::write_json(
            &self.store.directory(id)?.join("trial-selection.json"),
            &selection,
        )?)
    }
    async fn ensure_editable(&self, id: &str) -> Result<(), CoreError> {
        if self.task_state(id).await.is_some_and(|task| {
            task.running
                && matches!(
                    task.operation.as_str(),
                    "retranslate" | "extract" | "recognize"
                )
        }) {
            return Err(CoreError::ProjectBusy);
        }
        self.store.ensure_writable().await
    }
    pub async fn save_translation_edit(
        &self,
        id: &str,
        unit_id: &str,
        text: String,
        reviewed: bool,
    ) -> Result<(), CoreError> {
        let _project = self.lock_project(id).await?;
        self.ensure_editable(id).await?;
        let (analysis, workbench) = self.workbench(id).await?;
        let unit_id = unit_id.to_owned();
        tokio::task::spawn_blocking(move || {
            let mut workbench = workbench.blocking_lock();
            let unit = analysis
                .extraction()
                .units
                .iter()
                .find(|unit| unit.id == unit_id)
                .ok_or_else(|| CoreError::InvalidData("找不到所选文本。".into()))?;
            if reviewed && !text::validate(unit, &text).is_empty() {
                return Err(CoreError::InvalidData(
                    "译文存在格式问题，修正后才能标记为已校对。".into(),
                ));
            }
            workbench.edits.update(|edits| -> Result<_, CoreError> {
                let mut changes = EditChanges::default();
                let mut edit =
                    TranslationEdit::new(unit, &workbench.policy.target_language, text, reviewed);
                edit.retranslation_id = edits
                    .find(unit, &workbench.policy.target_language)
                    .and_then(|old| old.retranslation_id.clone());
                changes.entries.insert(unit_id, Some(edit));
                Ok(((), changes))
            })?;
            workbench.refresh_row(unit)?;
            Ok(())
        })
        .await?
    }
    pub async fn mark_reviewed(
        &self,
        id: &str,
        unit_ids: &[String],
        reviewed: bool,
    ) -> Result<usize, CoreError> {
        let _project = self.lock_project(id).await?;
        self.ensure_editable(id).await?;
        let (analysis, workbench) = self.workbench(id).await?;
        let unit_ids = unit_ids.to_vec();
        tokio::task::spawn_blocking(move || {
            let mut workbench = workbench.blocking_lock();
            let ids: HashSet<_> = unit_ids.iter().map(String::as_str).collect();
            let selected: Vec<_> = analysis
                .extraction()
                .units
                .iter()
                .filter(|unit| ids.contains(unit.id.as_str()))
                .collect();
            if ids.is_empty() || selected.len() != ids.len() {
                return Err(CoreError::InvalidData("文本选区为空或已失效。".into()));
            }
            let count = workbench.edits.update(|edits| -> Result<_, CoreError> {
                let mut changes = EditChanges::default();
                let mut count = 0;
                for unit in &selected {
                    let Some(row) = workbench.rows.get(&unit.id) else {
                        continue;
                    };
                    let Some(text) = row.text() else {
                        continue;
                    };
                    if reviewed && !row.issues.is_empty() {
                        continue;
                    }
                    if row.reviewed() == reviewed && row.edit.is_some() {
                        continue;
                    }
                    let mut edit = edits
                        .find(unit, &workbench.policy.target_language)
                        .cloned()
                        .unwrap_or_else(|| {
                            TranslationEdit::new(
                                unit,
                                &workbench.policy.target_language,
                                text.into(),
                                reviewed,
                            )
                        });
                    edit.reviewed = reviewed;
                    edit.is_generated = false;
                    edit.updated_at = gamel10n_translation::storage::now();
                    changes.entries.insert(unit.id.clone(), Some(edit));
                    count += 1;
                }
                Ok((count, changes))
            })?;
            for unit in selected {
                workbench.refresh_row(unit)?;
            }
            Ok(count)
        })
        .await?
    }
    pub async fn undo_translation_edit(&self, id: &str, unit_id: &str) -> Result<(), CoreError> {
        let _project = self.lock_project(id).await?;
        self.ensure_editable(id).await?;
        let (analysis, workbench) = self.workbench(id).await?;
        let unit_id = unit_id.to_owned();
        tokio::task::spawn_blocking(move || {
            let mut workbench = workbench.blocking_lock();
            let unit = analysis
                .extraction()
                .units
                .iter()
                .find(|unit| unit.id == unit_id)
                .ok_or_else(|| CoreError::InvalidData("找不到所选文本。".into()))?;
            workbench.edits.update(|_| -> Result<_, CoreError> {
                let mut changes = EditChanges::default();
                changes.entries.insert(unit_id.clone(), None);
                changes.candidates.insert(unit_id, None);
                Ok(((), changes))
            })?;
            workbench.refresh_row(unit)?;
            Ok(())
        })
        .await?
    }
    pub async fn adopt_candidate(&self, id: &str, unit_id: &str) -> Result<(), CoreError> {
        let _project = self.lock_project(id).await?;
        self.ensure_editable(id).await?;
        let (analysis, workbench) = self.workbench(id).await?;
        let unit_id = unit_id.to_owned();
        tokio::task::spawn_blocking(move || {
            let mut workbench = workbench.blocking_lock();
            let unit = analysis
                .extraction()
                .units
                .iter()
                .find(|unit| unit.id == unit_id)
                .ok_or_else(|| CoreError::InvalidData("找不到所选文本。".into()))?;
            workbench.edits.update(|edits| -> Result<_, CoreError> {
                let mut changes = EditChanges::default();
                let mut candidate = edits
                    .candidate(unit, &workbench.policy.target_language)
                    .cloned()
                    .ok_or_else(|| {
                        CoreError::InvalidData("当前文本没有已保存的候选译文。".into())
                    })?;
                candidate.is_generated = false;
                candidate.retranslation_id = edits
                    .find(unit, &workbench.policy.target_language)
                    .and_then(|old| old.retranslation_id.clone());
                changes.entries.insert(unit_id.clone(), Some(candidate));
                changes.candidates.insert(unit_id, None);
                Ok(((), changes))
            })?;
            workbench.refresh_row(unit)?;
            Ok(())
        })
        .await?
    }
    pub async fn discard_candidate(&self, id: &str, unit_id: &str) -> Result<(), CoreError> {
        let _project = self.lock_project(id).await?;
        self.ensure_editable(id).await?;
        let (analysis, workbench) = self.workbench(id).await?;
        let unit_id = unit_id.to_owned();
        tokio::task::spawn_blocking(move || {
            let mut workbench = workbench.blocking_lock();
            let unit = analysis
                .extraction()
                .units
                .iter()
                .find(|unit| unit.id == unit_id)
                .ok_or_else(|| CoreError::InvalidData("找不到所选文本。".into()))?;
            workbench.edits.update(|_| -> Result<_, CoreError> {
                let mut changes = EditChanges::default();
                changes.candidates.insert(unit_id, None);
                Ok(((), changes))
            })?;
            workbench.refresh_row(unit)?;
            Ok(())
        })
        .await?
    }
    pub async fn saved_translations(
        &self,
        id: &str,
        unit_ids: Option<&[String]>,
        reviewed_only: bool,
    ) -> Result<BTreeMap<String, String>, CoreError> {
        let (analysis, workbench) = self.workbench(id).await?;
        let workbench = workbench.lock().await;
        let selected: Option<HashSet<_>> =
            unit_ids.map(|ids| ids.iter().map(String::as_str).collect());
        let mut result = BTreeMap::new();
        let mut selected_count = 0;
        for unit in &analysis.extraction().units {
            if selected
                .as_ref()
                .is_some_and(|ids| !ids.contains(unit.id.as_str()))
            {
                continue;
            }
            selected_count += 1;
            let Some(row) = workbench.rows.get(&unit.id) else {
                continue;
            };
            if reviewed_only && !row.reviewed() {
                continue;
            }
            if let Some(text) = row.text() {
                if !row.issues.is_empty() {
                    return Err(CoreError::InvalidData(format!(
                        "{} · {}：{}",
                        unit.resource_path, unit.locator, row.issues[0].message
                    )));
                }
                result.insert(unit.id.clone(), text.into());
            }
        }
        if selected
            .as_ref()
            .is_some_and(|ids| ids.is_empty() || ids.len() != selected_count)
        {
            return Err(CoreError::InvalidData("文本选区为空或已失效。".into()));
        }
        if result.is_empty() {
            return Err(CoreError::InvalidData(
                "所选范围还没有可用于补丁的译文。".into(),
            ));
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gamel10n_translation::storage::TranslationEdits;
    async fn fixture() -> (tempfile::TempDir, Application, String, Vec<TranslationUnit>) {
        let root = tempfile::tempdir().unwrap();
        let game = root.path().join("game");
        std::fs::create_dir(&game).unwrap();
        let app = Application::open(
            root.path().join("data"),
            &crate::ToolPaths::new(root.path().join("tools")),
        )
        .await
        .unwrap();
        let project = app.add_project(&game).await.unwrap();
        let units = vec![
            TranslationUnit {
                id: "1".into(),
                resource_path: "story".into(),
                locator: "1".into(),
                source_text: "Hello [name]".into(),
                context: "scene".into(),
                speaker: None,
                kind: "dialogue".into(),
                protected_tokens: None,
            },
            TranslationUnit {
                id: "2".into(),
                resource_path: "story".into(),
                locator: "2".into(),
                source_text: "Goodbye".into(),
                context: "scene".into(),
                speaker: None,
                kind: "dialogue".into(),
                protected_tokens: None,
            },
            TranslationUnit {
                id: "3".into(),
                resource_path: "menu".into(),
                locator: "3".into(),
                source_text: "Option".into(),
                context: "scene".into(),
                speaker: None,
                kind: "choice".into(),
                protected_tokens: None,
            },
        ];
        let directory = app.project_directory(&project.id).unwrap();
        gamel10n_translation::files::write_json(
            &directory.join("extraction/native.json"),
            &crate::StoredAnalysis {
                schema_version: 1,
                format: "test".into(),
                source_directory: game,
                resource_files: Vec::new(),
                source_hashes: BTreeMap::new(),
                adapter: None,
                probe: None,
                extraction: gamel10n_engines::protocol::ExtractionResult {
                    units: units.clone(),
                    warnings: Vec::new(),
                },
            },
        )
        .unwrap();
        let policy = policy(&project.settings);
        let mut memory = TranslationMemory::load(directory.join("memory"), None).unwrap();
        memory
            .store_batch(
                units[..2]
                    .iter()
                    .zip(["你好[name]", "再见"])
                    .map(|(unit, text)| {
                        (
                            memory_key(unit, &policy),
                            gamel10n_translation::storage::MemoryEntry {
                                key: None,
                                source_text: unit.source_text.clone(),
                                context: unit.context.clone(),
                                translation: text.into(),
                                provider: "test".into(),
                                created_at: gamel10n_translation::storage::now(),
                            },
                        )
                    })
                    .collect(),
            )
            .unwrap();
        (root, app, project.id, units)
    }
    #[test]
    fn glossary_uses_existing_equal_sign_and_preserves_values() {
        assert_eq!(
            parse_terminology(&Value::String("Alice=艾丽丝\n key = a=b\n".into())),
            BTreeMap::from([
                ("Alice".into(), "艾丽丝".into()),
                ("key".into(), "a=b".into())
            ])
        );
    }
    #[tokio::test]
    async fn proofreading_rejects_invalid_review_and_preserves_model_draft_on_undo() {
        let (_root, app, id, _) = fixture().await;
        app.save_translation_edit(&id, "1", "你好".into(), false)
            .await
            .unwrap();
        assert!(
            app.save_translation_edit(&id, "1", "你好".into(), true)
                .await
                .is_err()
        );
        assert_eq!(
            app.mark_reviewed(&id, &["1".into(), "2".into(), "3".into()], true)
                .await
                .unwrap(),
            1
        );
        let reviewed = app
            .matching_text_ids(
                &id,
                &WorkbenchFilter {
                    status: "reviewed".into(),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(reviewed, vec!["2"]);
        assert!(
            app.saved_translations(&id, Some(&["1".into()]), false)
                .await
                .is_err()
        );
        app.undo_translation_edit(&id, "1").await.unwrap();
        let row = app.workbench_row(&id, "1").await.unwrap();
        assert_eq!(row.text.as_deref(), Some("你好[name]"));
        assert!(!row.edited);
        assert!(row.issues.is_empty());
    }
    #[tokio::test]
    async fn failed_requests_keep_valid_translations_available_until_a_successful_request() {
        let (_root, app, id, units) = fixture().await;
        let failures: Vec<_> = units
            .iter()
            .map(|unit| gamel10n_translation::execution::RecordedIssue {
                code: "provider-error".into(),
                message: "请求超时".into(),
                unit_id: Some(unit.id.clone()),
                memory_key: Some(memory_key(unit, &policy(&Value::Null))),
            })
            .collect();
        gamel10n_translation::files::write_json(
            &app.project_directory(&id)
                .unwrap()
                .join("translation-errors.json"),
            &failures,
        )
        .unwrap();
        let filter = WorkbenchFilter {
            status: "failed".into(),
            ..Default::default()
        };
        let page = app.workbench_page(&id, &filter, 0, 100).await.unwrap();
        assert_eq!(page.total, 3);
        assert_eq!(page.summary.failed, 3);
        assert_eq!(page.summary.translated, 2);
        assert_eq!(page.summary.invalid, 0);
        assert_eq!(page.rows[0].text.as_deref(), Some("你好[name]"));
        assert_eq!(
            page.rows[0].failure.as_deref(),
            Some("上次请求失败（当前已保存译文仍可使用）：请求超时")
        );
        assert_eq!(
            page.rows[2].failure.as_deref(),
            Some("上次请求失败：请求超时")
        );
        assert_eq!(
            app.matching_text_ids(&id, &filter).await.unwrap(),
            vec!["1", "2", "3"]
        );

        app.save_translation_edit(&id, "1", "改译[name]".into(), false)
            .await
            .unwrap();
        assert_eq!(
            app.mark_reviewed(&id, &["2".into()], true).await.unwrap(),
            1
        );
        assert_eq!(
            app.matching_text_ids(&id, &filter).await.unwrap(),
            vec!["1", "2", "3"]
        );
        assert_eq!(
            app.saved_translations(&id, None, false).await.unwrap(),
            BTreeMap::from([
                ("1".into(), "改译[name]".into()),
                ("2".into(), "再见".into())
            ])
        );
        assert_eq!(
            app.saved_translations(&id, None, true).await.unwrap(),
            BTreeMap::from([("2".into(), "再见".into())])
        );

        let (_, workbench) = app.workbench(&id).await.unwrap();
        workbench.lock().await.apply_progress(
            &units,
            &gamel10n_translation::execution::TranslationProgress {
                stage: "翻译".into(),
                completed: 1,
                total: 1,
                cache_hits: 0,
                failed: 0,
                usage: Default::default(),
                message: "候选译文已保存".into(),
                updates: BTreeMap::from([("1".into(), "新候选[name]".into())]),
                failure_updates: BTreeMap::from([("1".into(), None)]),
            },
            &gamel10n_translation::execution::TranslationOptions {
                candidate_only: true,
                ..Default::default()
            },
        );
        let row = app.workbench_row(&id, "1").await.unwrap();
        assert!(row.failure.is_none());
        assert_eq!(row.text.as_deref(), Some("改译[name]"));
        assert_eq!(row.candidate.as_deref(), Some("新候选[name]"));
        assert_eq!(
            app.matching_text_ids(&id, &filter).await.unwrap(),
            vec!["2", "3"]
        );
    }
    #[tokio::test]
    async fn empty_selection_does_not_open_extraction() {
        let (_root, app, id, _) = fixture().await;
        let directory = app.project_directory(&id).unwrap();
        std::fs::write(directory.join("extraction/native.json"), b"not JSON").unwrap();
        assert!(app.load_text_selection(&id).await.unwrap().is_empty());
        std::fs::write(directory.join("trial-selection.json"), b"[]").unwrap();
        assert!(app.load_text_selection(&id).await.unwrap().is_empty());

        // A nonempty saved selection must still be validated against the extraction.
        std::fs::write(directory.join("trial-selection.json"), b"[\"2\"]").unwrap();
        assert!(app.load_text_selection(&id).await.is_err());
    }
    #[tokio::test]
    async fn legacy_selection_is_read_without_rewriting_and_invalid_ids_are_not_saved() {
        let (_root, app, id, _) = fixture().await;
        let path = app
            .project_directory(&id)
            .unwrap()
            .join("trial-selection.json");
        let bytes = b"[\"2\",\"missing\",\"2\"]";
        std::fs::write(&path, bytes).unwrap();
        assert_eq!(app.load_text_selection(&id).await.unwrap(), vec!["2"]);
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert!(
            app.save_text_selection(&id, &["missing".into()])
                .await
                .is_err()
        );
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }
    #[tokio::test]
    async fn manual_correction_and_review_keep_interrupted_retranslation_identity() {
        let (_root, app, id, units) = fixture().await;
        let directory = app.project_directory(&id).unwrap();
        let language = policy(&Value::Null).target_language;
        let mut edits = TranslationEdits::default();
        let mut generated =
            TranslationEdit::new(&units[0], &language, "新译文[name]".into(), false);
        generated.is_generated = true;
        generated.retranslation_id = Some("a".repeat(32));
        edits.entries.insert("1".into(), generated);
        edits.save(&directory).unwrap();
        app.save_translation_edit(&id, "1", "校对译文[name]".into(), false)
            .await
            .unwrap();
        app.mark_reviewed(&id, &["1".into()], true).await.unwrap();
        let saved = TranslationEdits::load(&directory).unwrap();
        let edit = saved.find(&units[0], &language).unwrap();
        assert_eq!(
            edit.retranslation_id.as_deref(),
            Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
        );
        assert_eq!(edit.text, "校对译文[name]");
        assert!(edit.reviewed);
        assert!(!edit.is_generated);
    }
    #[tokio::test]
    async fn visiting_projects_evicts_old_drafts_and_keeps_recent_workbench() {
        let (root, app, id, units) = fixture().await;
        let mut first_new = None;
        app.workbench_page(&id, &WorkbenchFilter::default(), 0, 1)
            .await
            .unwrap();
        for index in 0..4 {
            let game = root.path().join(format!("game-{index}"));
            std::fs::create_dir(&game).unwrap();
            let project = app.add_project(&game).await.unwrap();
            if first_new.is_none() {
                first_new = Some(project.id.clone());
            }
            gamel10n_translation::files::write_json(
                &app.project_directory(&project.id)
                    .unwrap()
                    .join("extraction/native.json"),
                &crate::StoredAnalysis {
                    schema_version: 1,
                    format: "test".into(),
                    source_directory: game,
                    resource_files: Vec::new(),
                    source_hashes: BTreeMap::new(),
                    extraction: gamel10n_engines::protocol::ExtractionResult {
                        units: units.clone(),
                        warnings: Vec::new(),
                    },
                    adapter: None,
                    probe: None,
                },
            )
            .unwrap();
            app.workbench_page(&project.id, &WorkbenchFilter::default(), 0, 1)
                .await
                .unwrap();
            app.workbench_page(&id, &WorkbenchFilter::default(), 0, 1)
                .await
                .unwrap();
        }
        let cache = app.workbenches.lock().await;
        assert_eq!(cache.len(), 4);
        assert!(cache.contains_key(&id));
        assert!(!cache.contains_key(first_new.as_deref().unwrap()));
    }
}
