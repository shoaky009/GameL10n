//! UI-independent text browsing, saved edits, review state and candidates.

mod editing;
pub(crate) mod page_file;
pub(crate) mod paged;
mod query;

use crate::{Application, CoreError, application::CachedAnalysis};
use gamel10n_translation::{
    TranslationUnit, ValidationIssue,
    edits::EditChanges,
    execution::load_issues,
    storage::{
        TranslationEdit, TranslationEdits, TranslationMemory, TranslationOrigin, TranslationPolicy,
        memory_key,
    },
    text,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::PathBuf,
    sync::{Arc, LazyLock, Weak},
};
use tokio::sync::Mutex;

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
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
    pub origin: TranslationOrigin,
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

#[derive(Clone, Default)]
struct RowState {
    model: Option<String>,
    edit: Option<RowEdit>,
    candidate: Option<String>,
    issues: Vec<ValidationIssue>,
    failure: Option<String>,
    previous_edit: Option<String>,
}

#[derive(Clone)]
struct RowEdit {
    text: String,
    reviewed: bool,
    generated: bool,
    origin: TranslationOrigin,
}

impl RowState {
    fn is_empty(&self) -> bool {
        self.text().is_none()
            && self.candidate.is_none()
            && self.failure.is_none()
            && self.previous_edit.is_none()
    }

    fn read_edits(&mut self, edits: &TranslationEdits, unit: &TranslationUnit, language: &str) {
        self.edit = edits.find(unit, language).map(|edit| RowEdit {
            text: edit.text.clone(),
            reviewed: edit.reviewed,
            generated: edit.is_generated,
            origin: edit.origin(),
        });
        self.previous_edit = if self.edit.is_none() {
            edits.entries.get(&unit.id).map(|edit| edit.text.clone())
        } else {
            None
        };
        self.candidate = edits
            .candidate(unit, language)
            .map(|edit| edit.text.clone());
        self.validate(unit);
    }

    fn validate(&mut self, unit: &TranslationUnit) {
        self.issues = self
            .text()
            .map(|text| text::validate(unit, text))
            .unwrap_or_default();
    }
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
        self.edit.as_ref().is_some_and(|edit| !edit.generated)
    }
    fn origin(&self) -> TranslationOrigin {
        self.edit
            .as_ref()
            .map(|edit| edit.origin)
            .unwrap_or(if self.model.is_some() {
                TranslationOrigin::Automatic
            } else {
                TranslationOrigin::Unknown
            })
    }
    fn row(&self, unit: &TranslationUnit) -> WorkbenchRow {
        WorkbenchRow {
            unit: unit.clone(),
            text: self.text().map(str::to_owned),
            model_text: self.model.clone(),
            candidate: self.candidate.clone(),
            reviewed: self.reviewed(),
            edited: self.edited(),
            origin: self.origin(),
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
    analysis: Weak<CachedAnalysis>,
    policy: TranslationPolicy,
    // Ordinals belong to this extraction only. Rows with no translation or review state share an empty view.
    by_id: Vec<usize>,
    rows: HashMap<usize, RowState>,
    edits: Arc<gamel10n_translation::edits::EditStore>,
    resources: Vec<String>,
    kinds: Vec<String>,
    last_access: std::time::Instant,
}
pub(crate) struct TranslationSource<'a> {
    analysis: &'a Arc<CachedAnalysis>,
    policy: &'a TranslationPolicy,
    by_id: std::sync::OnceLock<HashMap<&'a str, &'a TranslationUnit>>,
}

impl<'a> TranslationSource<'a> {
    pub(crate) fn new(analysis: &'a Arc<CachedAnalysis>, policy: &'a TranslationPolicy) -> Self {
        Self {
            analysis,
            policy,
            by_id: Default::default(),
        }
    }

    pub(crate) fn apply(
        &self,
        workbench: &mut Workbench,
        progress: &mut gamel10n_translation::execution::TranslationProgress,
        options: &gamel10n_translation::execution::TranslationOptions,
    ) {
        if !workbench.matches_input(self.analysis, self.policy) {
            let Some(current) = workbench.analysis.upgrade() else {
                return;
            };
            let units = &current.extraction().units;
            // A later extraction may reuse an ID for a different translation input.
            let by_id = self.by_id.get_or_init(|| {
                self.analysis
                    .extraction()
                    .units
                    .iter()
                    .map(|unit| (unit.id.as_str(), unit))
                    .collect()
            });
            let compatible = |id: &str| {
                let Some(old) = by_id.get(id) else {
                    return false;
                };
                workbench.index(units, id).is_some_and(|index| {
                    memory_key(old, self.policy) == memory_key(&units[index], &workbench.policy)
                })
            };
            progress.updates.retain(|id, _| compatible(id));
            progress.failure_updates.retain(|id, _| compatible(id));
        }
        workbench.apply_progress(progress, options);
    }
}

impl Workbench {
    pub(crate) fn load(
        directory: PathBuf,
        legacy_memory: PathBuf,
        analysis: &Arc<CachedAnalysis>,
        policy: TranslationPolicy,
    ) -> Result<Self, CoreError> {
        let units = &analysis.extraction().units;
        let mut memory = TranslationMemory::load(directory.join("memory"), Some(&legacy_memory))
            .map_err(storage_error)?;
        let edits =
            gamel10n_translation::edits::EditStore::open(&directory).map_err(storage_error)?;
        let failures: HashMap<_, _> = load_issues(&directory)
            .into_iter()
            .filter_map(|issue| issue.unit_id.clone().map(|id| (id, issue)))
            .collect();
        let mut rows = HashMap::new();
        let mut resources = std::collections::BTreeSet::new();
        let mut kinds = std::collections::BTreeSet::new();
        let mut ordered: Vec<_> = units
            .iter()
            .enumerate()
            .map(|(index, unit)| (memory_key(unit, &policy), index))
            .collect();
        ordered.sort_unstable();
        edits
            .read(|edits| -> Result<(), CoreError> {
                for (key, index) in ordered {
                    let unit = &units[index];
                    let failure = failures
                        .get(&unit.id)
                        .filter(|issue| {
                            issue
                                .memory_key
                                .as_ref()
                                .is_none_or(|failed_key| *failed_key == key)
                        })
                        .map(|issue| issue.message.clone());
                    let model = memory
                        .find_with_key(unit, &policy, key)
                        .map_err(storage_error)?
                        .map(str::to_owned);
                    let mut row = RowState {
                        model,
                        failure,
                        ..Default::default()
                    };
                    row.read_edits(edits, unit, &policy.target_language);
                    if !row.is_empty() {
                        rows.insert(index, row);
                    }
                    resources.insert(unit.resource_path.as_str());
                    kinds.insert(unit.kind.as_str());
                }
                Ok(())
            })
            .map_err(storage_error)??;
        let mut by_id: Vec<_> = (0..units.len()).collect();
        by_id.sort_unstable_by(|&a, &b| units[a].id.cmp(&units[b].id));
        Ok(Self {
            policy,
            analysis: Arc::downgrade(analysis),
            by_id,
            rows,
            edits,
            resources: resources.into_iter().map(str::to_owned).collect(),
            kinds: kinds.into_iter().map(str::to_owned).collect(),
            last_access: std::time::Instant::now(),
        })
    }

    fn matches_input(&self, analysis: &Arc<CachedAnalysis>, policy: &TranslationPolicy) -> bool {
        self.policy == *policy && self.analysis.ptr_eq(&Arc::downgrade(analysis))
    }

    fn index(&self, units: &[TranslationUnit], id: &str) -> Option<usize> {
        self.by_id
            .binary_search_by(|&index| units[index].id.as_str().cmp(id))
            .ok()
            .map(|pos| self.by_id[pos])
    }

    fn selected_index(&self, units: &[TranslationUnit], id: &str) -> Result<usize, CoreError> {
        self.index(units, id)
            .ok_or_else(|| CoreError::InvalidData("找不到所选文本。".into()))
    }

    fn state(&self, index: usize) -> &RowState {
        static EMPTY: LazyLock<RowState> = LazyLock::new(RowState::default);
        self.rows.get(&index).unwrap_or_else(|| &EMPTY)
    }
    fn retained_bytes(&self) -> usize {
        let edit_bytes = |edit: &TranslationEdit| {
            edit.text.len()
                + edit.source_signature.len()
                + edit.language.len()
                + edit.updated_at.len()
                + edit.retranslation_id.as_ref().map_or(0, String::len)
        };
        self.by_id.capacity() * std::mem::size_of::<usize>()
            + self.rows.capacity() * std::mem::size_of::<(usize, RowState)>()
            + self
                .rows
                .values()
                .map(|row| {
                    row.model.as_ref().map_or(0, String::len)
                        + row.previous_edit.as_ref().map_or(0, String::len)
                        + row.failure.as_ref().map_or(0, String::len)
                        + row.edit.as_ref().map_or(0, |edit| edit.text.len())
                        + row.candidate.as_ref().map_or(0, String::len)
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
        current: &dyn Fn() -> bool,
    ) -> Result<WorkbenchPage, CoreError> {
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
        for (index, unit) in units.iter().enumerate() {
            if index.is_multiple_of(page_file::SIZE) && !current() {
                return Err(gamel10n_engines::EngineError::Cancelled.into());
            }
            let row = self.state(index);
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
        Ok(WorkbenchPage {
            total,
            offset: offset.min(total),
            rows,
            summary,
            resources: self.resources.clone(),
            kinds: self.kinds.clone(),
        })
    }
    fn refresh_row(&mut self, index: usize, unit: &TranslationUnit) -> Result<(), CoreError> {
        self.edits
            .read(|edits| {
                let row = self.rows.entry(index).or_default();
                row.read_edits(edits, unit, &self.policy.target_language);
                if row.is_empty() {
                    self.rows.remove(&index);
                }
            })
            .map_err(storage_error)?;
        Ok(())
    }
    pub(crate) fn apply_progress(
        &mut self,
        progress: &gamel10n_translation::execution::TranslationProgress,
        options: &gamel10n_translation::execution::TranslationOptions,
    ) {
        let Some(analysis) = self.analysis.upgrade() else {
            return;
        };
        let units = &analysis.extraction().units;
        for (id, text) in &progress.updates {
            let Some(index) = self.index(units, id) else {
                continue;
            };
            let row = self.rows.entry(index).or_default();
            if options.candidate_only {
                row.candidate = Some(text.clone());
            } else if options.retranslation_id.is_some() {
                row.edit = Some(RowEdit {
                    text: text.clone(),
                    reviewed: false,
                    generated: true,
                    origin: TranslationOrigin::Automatic,
                });
                row.candidate = None;
            } else {
                row.model = Some(text.clone());
            }
            row.validate(&units[index]);
        }
        for (id, failure) in &progress.failure_updates {
            let Some(index) = self.index(units, id) else {
                continue;
            };
            if let Some(failure) = failure {
                self.rows.entry(index).or_default().failure = Some(failure.clone());
            } else if let Some(row) = self.rows.get_mut(&index) {
                row.failure = None;
                if row.is_empty() {
                    self.rows.remove(&index);
                }
            }
        }
    }
}

fn context_text(
    unit: &TranslationUnit,
    before: Option<&TranslationUnit>,
    after: Option<&TranslationUnit>,
) -> String {
    let before = before
        .filter(|v| v.resource_path == unit.resource_path)
        .map_or("（本资源首条文本）", |u| u.source_text.as_str());
    let after = after
        .filter(|v| v.resource_path == unit.resource_path)
        .map_or("（本资源末条文本）", |u| u.source_text.as_str());
    format!(
        "上一条：{before}\n\n下一条：{after}{}",
        if unit.context.is_empty() {
            String::new()
        } else {
            format!("\n\n提取上下文：{}", unit.context)
        }
    )
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
    if !query::Status::effective(row).matches(&filter.status)
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
    ) -> Result<(Arc<CachedAnalysis>, Arc<Mutex<Workbench>>), CoreError> {
        let analysis = self.workbench_analysis(id).await?;
        self.workbench_for(id, analysis).await
    }

    async fn workbench_for(
        &self,
        id: &str,
        analysis: Option<Arc<CachedAnalysis>>,
    ) -> Result<(Arc<CachedAnalysis>, Arc<Mutex<Workbench>>), CoreError> {
        let analysis =
            analysis.ok_or_else(|| CoreError::InvalidData("请先识别并提取游戏文本。".into()))?;
        let project = self.store.get(id).await?;
        let policy = validated_policy(&project.settings)?;
        let analysis_key = Arc::downgrade(&analysis);
        if let Some(workbench) = self.workbenches.lock().await.get(id).cloned() {
            let mut current = workbench.lock().await;
            if current.matches_input(&analysis, &policy) {
                current.last_access = std::time::Instant::now();
                drop(current);
                return Ok((analysis, workbench));
            }
        }
        let directory = self.store.directory(id)?;
        let legacy = self.data_directory().join("memory").join(id);
        let revision = self
            .workbench_revision
            .load(std::sync::atomic::Ordering::Relaxed);
        let cached = Arc::clone(&analysis);
        let workbench = tokio::task::spawn_blocking(move || {
            Workbench::load(directory, legacy, &cached, policy)
        })
        .await??;
        let mut cache = self.workbenches.lock().await;
        if let Some(current) = cache.get(id).cloned() {
            let mut current_data = current.lock().await;
            if current_data.policy == workbench.policy
                && current_data.analysis.ptr_eq(&analysis_key)
            {
                current_data.last_access = std::time::Instant::now();
                drop(current_data);
                return Ok((analysis, current));
            }
        }
        // A load that overlapped the last batch can still contain drafts from before the commit.
        if revision
            != self
                .workbench_revision
                .load(std::sync::atomic::Ordering::Relaxed)
        {
            return Ok((analysis, Arc::new(Mutex::new(workbench))));
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
        self.workbench_page_while(id, filter, offset, limit, || true)
            .await
    }

    /// Stops scanning a superseded view between pages. A cancelled scan never replaces cached matches.
    pub async fn workbench_page_while(
        &self,
        id: &str,
        filter: &WorkbenchFilter,
        offset: usize,
        limit: usize,
        current: impl Fn() -> bool + Clone + Send + 'static,
    ) -> Result<WorkbenchPage, CoreError> {
        if let Some(value) = self
            .disk_page(id, filter, offset, limit, current.clone())
            .await?
        {
            return Ok(value);
        }
        let analysis = self.workbench_analysis(id).await?;
        if analysis.is_none() {
            return Ok(WorkbenchPage::default());
        }
        let (analysis, workbench) = self.workbench_for(id, analysis).await?;
        let filter = filter.clone();
        tokio::task::spawn_blocking(move || {
            workbench.blocking_lock().page(
                &analysis.extraction().units,
                &filter,
                offset,
                limit,
                &current,
            )
        })
        .await?
    }
    pub async fn workbench_row(&self, id: &str, unit_id: &str) -> Result<WorkbenchRow, CoreError> {
        if let Some(value) = self.disk_row(id, unit_id).await? {
            return Ok(value);
        }

        let (analysis, workbench) = self.workbench(id).await?;
        let workbench = workbench.lock().await;
        let units = &analysis.extraction().units;
        let index = workbench.selected_index(units, unit_id)?;
        Ok(workbench.state(index).row(&units[index]))
    }
    pub async fn matching_text_ids(
        &self,
        id: &str,
        filter: &WorkbenchFilter,
    ) -> Result<Vec<String>, CoreError> {
        if let Some(value) = self.disk_ids(id, filter).await? {
            return Ok(value);
        }

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
                .enumerate()
                .filter(|(index, unit)| {
                    matches_filter(
                        unit,
                        workbench.state(*index),
                        &filter,
                        &query,
                        &source,
                        &translation,
                        selected.as_ref(),
                    )
                })
                .map(|(_, unit)| unit.id.clone())
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
        if let Some(value) = self.disk_adjacent(id, filter, current, direction).await? {
            return Ok(value);
        }

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
        let current_index = workbench.index(units, current);
        let previous = direction < 0;
        let matches = units.iter().enumerate().filter(|(index, unit)| {
            matches_filter(
                unit,
                workbench.state(*index),
                filter,
                &query,
                &source,
                &translation,
                selected.as_ref(),
            )
        });
        let mut adjacent = None;
        for (rank, (index, unit)) in matches.enumerate() {
            if previous && current_index.is_none_or(|current| index >= current) {
                adjacent.get_or_insert((unit, rank));
                break;
            }
            adjacent = Some((unit, rank));
            if !previous && current_index.is_some_and(|current| index > current) {
                break;
            }
        }
        Ok(adjacent.map(|(unit, rank)| (unit.id.clone(), rank)))
    }
    pub async fn workbench_context(&self, id: &str, unit_id: &str) -> Result<String, CoreError> {
        if let Some(value) = self.disk_context(id, unit_id).await? {
            return Ok(value);
        }

        let analysis = self
            .workbench_analysis(id)
            .await?
            .ok_or_else(|| CoreError::InvalidData("请先提取游戏文本。".into()))?;
        let units = &analysis.extraction().units;
        let index = units
            .iter()
            .position(|unit| unit.id == unit_id)
            .ok_or_else(|| CoreError::InvalidData("找不到所选文本。".into()))?;
        let unit = &units[index];
        Ok(context_text(
            unit,
            index.checked_sub(1).and_then(|i| units.get(i)),
            units.get(index + 1),
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
        if let Some(valid) = self.disk_selection(id, selection.clone(), false).await? {
            return Ok(valid);
        }
        let Some(analysis) = self.workbench_analysis(id).await? else {
            return Ok(Vec::new());
        };
        let mut remaining: HashSet<_> = analysis
            .extraction()
            .units
            .iter()
            .map(|unit| unit.id.as_str())
            .collect();
        Ok(selection
            .into_iter()
            .filter(|id| remaining.remove(id.as_str()))
            .collect())
    }
    pub async fn save_text_selection(
        &self,
        id: &str,
        selection: &[String],
    ) -> Result<(), CoreError> {
        let _project = self.lock_project(id).await?;
        self.store.ensure_writable().await?;
        if self
            .disk_selection(id, selection.to_vec(), true)
            .await?
            .is_none()
        {
            let analysis = self
                .workbench_analysis(id)
                .await?
                .ok_or_else(|| CoreError::InvalidData("请先提取游戏文本。".into()))?;
            let mut remaining: HashSet<_> = analysis
                .extraction()
                .units
                .iter()
                .map(|unit| unit.id.as_str())
                .collect();
            if selection.iter().any(|id| !remaining.remove(id.as_str())) {
                return Err(CoreError::InvalidData(
                    "文本选区已失效，请重新选择。".into(),
                ));
            }
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
                    "retranslate" | "extract" | "recognize" | "adapt" | "workflow"
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
        self.edit_rows(id, &[unit_id.into()], editing::Edit::Save(text, reviewed))
            .await
            .map(|_| ())
    }
    pub async fn mark_reviewed(
        &self,
        id: &str,
        unit_ids: &[String],
        reviewed: bool,
    ) -> Result<usize, CoreError> {
        self.edit_rows(id, unit_ids, editing::Edit::Review(reviewed))
            .await
    }
    pub async fn undo_translation_edit(&self, id: &str, unit_id: &str) -> Result<(), CoreError> {
        self.edit_rows(id, &[unit_id.into()], editing::Edit::Undo)
            .await
            .map(|_| ())
    }
    pub async fn adopt_candidate(&self, id: &str, unit_id: &str) -> Result<(), CoreError> {
        self.edit_rows(id, &[unit_id.into()], editing::Edit::Adopt)
            .await
            .map(|_| ())
    }
    pub async fn discard_candidate(&self, id: &str, unit_id: &str) -> Result<(), CoreError> {
        self.edit_rows(id, &[unit_id.into()], editing::Edit::Discard)
            .await
            .map(|_| ())
    }
    pub(crate) async fn check_patch_copies(
        &self,
        id: &str,
        copies: &BTreeMap<String, String>,
    ) -> Result<(), CoreError> {
        let (analysis, workbench) = self
            .workbench_for(id, self.cached_analysis(id).await?)
            .await?;
        let workbench = workbench.lock().await;
        let units = &analysis.extraction().units;
        for (id, text) in copies {
            let index = workbench
                .index(units, id)
                .ok_or_else(|| CoreError::InvalidData("脚本副本与提取记录不一致。".into()))?;
            if workbench
                .state(index)
                .text()
                .is_some_and(|saved| saved != text)
            {
                let unit = &units[index];
                return Err(CoreError::InvalidData(format!(
                    "{} · {} 的脚本副本已有不同译文，请明确选择各副本的译文。",
                    unit.resource_path, unit.locator
                )));
            }
        }
        Ok(())
    }

    pub async fn saved_translations(
        &self,
        id: &str,
        unit_ids: Option<&[String]>,
        reviewed_only: bool,
    ) -> Result<BTreeMap<String, String>, CoreError> {
        let (analysis, workbench) = self
            .workbench_for(id, self.cached_analysis(id).await?)
            .await?;
        let workbench = workbench.lock().await;
        let selected: Option<HashSet<_>> =
            unit_ids.map(|ids| ids.iter().map(String::as_str).collect());
        let mut result = BTreeMap::new();
        let mut selected_count = 0;
        for (index, unit) in analysis.extraction().units.iter().enumerate() {
            if selected
                .as_ref()
                .is_some_and(|ids| !ids.contains(unit.id.as_str()))
            {
                continue;
            }
            selected_count += 1;
            let Some(row) = workbench.rows.get(&index) else {
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
                coverage_pending: false,
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
    async fn candidate_only_row_can_be_found_and_adopted_from_an_old_snapshot() {
        let (_root, app, id, units) = fixture().await;
        let unit = &units[2];
        let mut edits = TranslationEdits::default();
        edits.candidates.insert(
            unit.id.clone(),
            TranslationEdit::new(unit, "简体中文", "选项".into(), false),
        );
        edits.save(&app.project_directory(&id).unwrap()).unwrap();
        let filter = WorkbenchFilter {
            status: "candidate".into(),
            ..Default::default()
        };
        assert_eq!(
            app.matching_text_ids(&id, &filter).await.unwrap(),
            [unit.id.clone()]
        );
        app.adopt_candidate(&id, &unit.id).await.unwrap();
        let row = app.workbench_row(&id, &unit.id).await.unwrap();
        assert_eq!(row.text.as_deref(), Some("选项"));
        assert!(row.candidate.is_none());
    }

    #[tokio::test]
    async fn untranslated_row_retains_old_edits_and_candidates_until_discarded() {
        let (_root, app, id, units) = fixture().await;
        let unit = &units[2];
        let language = &policy(&Value::Null).target_language;
        let mut edits = TranslationEdits::default();
        let mut old_unit = unit.clone();
        old_unit.source_text = "Previous source".into();
        edits.entries.insert(
            unit.id.clone(),
            TranslationEdit::new(&old_unit, language, "历史译文".into(), true),
        );
        edits.candidates.insert(
            unit.id.clone(),
            TranslationEdit::new(unit, language, "候选译文".into(), false),
        );
        edits.save(&app.project_directory(&id).unwrap()).unwrap();

        let row = app.workbench_row(&id, &unit.id).await.unwrap();
        assert!(row.text.is_none() && !row.reviewed);
        assert_eq!(row.previous_edit.as_deref(), Some("历史译文"));
        assert_eq!(row.candidate.as_deref(), Some("候选译文"));
        app.discard_candidate(&id, &unit.id).await.unwrap();
        assert_eq!(
            app.workbench_row(&id, &unit.id)
                .await
                .unwrap()
                .previous_edit
                .as_deref(),
            Some("历史译文")
        );
        app.undo_translation_edit(&id, &unit.id).await.unwrap();

        app.begin(&id, "translate", "翻译中").await.unwrap();
        let (_, workbench) = app.workbench(&id).await.unwrap();
        assert_eq!(workbench.lock().await.rows.len(), 2);
        let missing = WorkbenchFilter {
            status: "missing".into(),
            ..Default::default()
        };
        assert_eq!(
            app.matching_text_ids(&id, &missing).await.unwrap(),
            [unit.id.clone()]
        );
        assert!(app.workbench_row(&id, "absent").await.is_err());
        app.check_patch_copies(&id, &BTreeMap::from([(unit.id.clone(), "试译".into())]))
            .await
            .unwrap();
        app.save_translation_edit(&id, &unit.id, "新译文".into(), true)
            .await
            .unwrap();
        assert_eq!(
            app.saved_translations(&id, Some(std::slice::from_ref(&unit.id)), true)
                .await
                .unwrap()
                .get(&unit.id)
                .map(String::as_str),
            Some("新译文")
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

        app.begin(&id, "translate", "翻译中").await.unwrap();
        let (_, workbench) = app.workbench(&id).await.unwrap();
        workbench.lock().await.apply_progress(
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
        assert!(
            app.save_text_selection(&id, &["2".into(), "2".into()])
                .await
                .is_err()
        );
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }
    #[tokio::test]
    async fn translation_origin_is_independent_of_review_and_overwrite_protection() {
        let (root, app, id, _) = fixture().await;
        let automatic = app.workbench_row(&id, "1").await.unwrap();
        assert_eq!(automatic.origin, TranslationOrigin::Automatic);
        app.save_translation_edit(&id, "1", automatic.text.unwrap(), true)
            .await
            .unwrap();
        let reviewed = app.workbench_row(&id, "1").await.unwrap();
        assert_eq!(reviewed.origin, TranslationOrigin::Automatic);
        assert!(reviewed.reviewed && reviewed.edited);
        app.mark_reviewed(&id, &["1".into(), "2".into()], true)
            .await
            .unwrap();
        assert_eq!(
            app.workbench_row(&id, "2").await.unwrap().origin,
            TranslationOrigin::Automatic
        );
        app.mark_reviewed(&id, &["1".into()], false).await.unwrap();
        assert_eq!(
            app.workbench_row(&id, "1").await.unwrap().origin,
            TranslationOrigin::Automatic
        );
        app.save_translation_edit(&id, "1", "人工修订[name]".into(), false)
            .await
            .unwrap();
        let manual = app.workbench_row(&id, "1").await.unwrap();
        assert_eq!(manual.origin, TranslationOrigin::Manual);
        assert!(manual.edited && !manual.reviewed);
        let filter = WorkbenchFilter {
            status: "manual".into(),
            ..Default::default()
        };
        assert_eq!(
            app.workbench_page(&id, &filter, 0, 100)
                .await
                .unwrap()
                .total,
            1
        );
        drop(app);
        let app = Application::open(root.path().join("data"), &crate::ToolPaths::new("missing"))
            .await
            .unwrap();
        assert_eq!(
            app.workbench_row(&id, "1").await.unwrap().origin,
            TranslationOrigin::Manual
        );
        assert_eq!(
            app.workbench_row(&id, "2").await.unwrap().origin,
            TranslationOrigin::Automatic
        );
    }

    #[tokio::test]
    async fn legacy_edits_keep_unknown_origin_while_generated_candidates_remain_automatic() {
        let (root, app, id, units) = fixture().await;
        let directory = app.project_directory(&id).unwrap();
        let language = policy(&Value::Null).target_language;
        let mut edits = TranslationEdits::default();
        let mut legacy = serde_json::to_value(TranslationEdit::new(
            &units[0],
            &language,
            "历史译文[name]".into(),
            true,
        ))
        .unwrap();
        legacy.as_object_mut().unwrap().remove("origin");
        edits
            .entries
            .insert("1".into(), serde_json::from_value(legacy).unwrap());
        let mut candidate =
            TranslationEdit::new(&units[1], &language, "新的自动候选".into(), false);
        candidate.origin = TranslationOrigin::Unknown;
        candidate.is_generated = true;
        edits.candidates.insert("2".into(), candidate);
        edits.save(&directory).unwrap();
        let legacy = app.workbench_row(&id, "1").await.unwrap();
        assert_eq!(legacy.origin, TranslationOrigin::Unknown);
        app.save_translation_edit(&id, "1", legacy.text.unwrap(), true)
            .await
            .unwrap();
        assert_eq!(
            app.workbench_row(&id, "1").await.unwrap().origin,
            TranslationOrigin::Unknown
        );
        app.adopt_candidate(&id, "2").await.unwrap();
        let adopted = app.workbench_row(&id, "2").await.unwrap();
        assert_eq!(adopted.origin, TranslationOrigin::Automatic);
        assert!(adopted.edited && !adopted.reviewed);
        drop(app);
        let app = Application::open(root.path().join("data"), &crate::ToolPaths::new("missing"))
            .await
            .unwrap();
        assert_eq!(
            app.workbench_row(&id, "1").await.unwrap().origin,
            TranslationOrigin::Unknown
        );
        assert_eq!(
            app.workbench_row(&id, "2").await.unwrap().origin,
            TranslationOrigin::Automatic
        );
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
    mod pagination;
}
