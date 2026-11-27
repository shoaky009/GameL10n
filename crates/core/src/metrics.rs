//! Compact execution counters, independent of diagnostic logs and adaptation history.
use std::{
    collections::{BTreeMap, HashMap},
    fs::{self, OpenOptions},
    io::{BufRead, BufReader, Write},
    path::Path,
    time::Instant,
};

use chrono::{DateTime, Local, NaiveDate, Utc};
use gamel10n_translation::{
    files::hash_text,
    provider::{OpenAiProvider, RequestMetrics},
};
use serde::{Deserialize, Serialize};

use crate::{Application, CoreError, task_op};

pub const SOFTWARE_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const BUILD_GIT_HASH: &str = env!("GAMEL10N_BUILD_GIT_HASH");

pub(crate) const MODE_NEW: &str = "new";
pub(crate) const MODE_CONTINUED: &str = "continued";
pub(crate) const MODE_REUSED: &str = "reused";

pub const STATUS_RUNNING: &str = "running";
pub const STATUS_COMPLETED: &str = "completed";
pub const STATUS_FAILED: &str = "failed";
pub const STATUS_CANCELLED: &str = "cancelled";
pub const STATUS_PAUSED: &str = "paused";
pub const STATUS_INTERRUPTED: &str = "interrupted";
pub const STATUS_UNSUPPORTED: &str = "unsupported";

const METRICS_DIR: &str = "metrics";
const RUNS_FILE_PREFIX: &str = "runs-";
const RUNS_FILE_SUFFIX: &str = ".jsonl";
const CHECKPOINT_INTERVAL_SECS: u64 = 60;
const RECORD_ID_HEX_LEN: usize = 32;
const SHORT_GIT_HASH_LEN: usize = 8;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConsumptionRecord {
    pub id: String,
    pub project_id: String,
    pub project_name: String,
    pub software_version: String,
    pub build_git_hash: String,
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    pub task: String,
    pub mode: String,
    pub status: String,
    pub started_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    pub elapsed_ms: u64,
    pub api: RequestMetrics,
}

impl ConsumptionRecord {
    pub fn version_key(&self) -> String {
        format!("{} · {}", self.software_version, self.build_git_hash)
    }

    pub fn version_label(&self) -> String {
        format!(
            "{} · {}",
            self.software_version,
            self.build_git_hash
                .chars()
                .take(SHORT_GIT_HASH_LEN)
                .collect::<String>()
        )
    }

    pub fn model_key(&self) -> String {
        serde_json::json!([self.profile_name, self.model, self.reasoning_effort]).to_string()
    }

    pub fn model_label(&self) -> String {
        let Some(model) = &self.model else {
            return "未调用模型".into();
        };
        let reasoning =
            self.reasoning_effort
                .as_deref()
                .unwrap_or(if self.profile_name.is_some() {
                    "默认"
                } else {
                    "未记录强度"
                });
        format!(
            "{} / {model} / {reasoning}",
            self.profile_name.as_deref().unwrap_or("未记录配置")
        )
    }
}

pub fn task_label(task: &str) -> &str {
    match task {
        task_op::TRANSLATE => "文本翻译",
        task_op::PREVIEW_TRANSLATE => "试译",
        task_op::RETRANSLATE => "重新翻译",
        task_op::CANDIDATE => "候选译文",
        task_op::TERMINOLOGY => "术语提取",
        task_op::RECOGNIZE => "常规识别",
        task_op::ADAPT => "智能适配",
        task_op::REPAIR => "修复适配",
        task_op::EXPAND => "扩展覆盖",
        task_op::REVIEW => "经验总结",
        _ => task,
    }
}

pub fn mode_label(mode: &str) -> &str {
    match mode {
        MODE_NEW => "新运行",
        MODE_CONTINUED => "续接",
        MODE_REUSED => "复用结果",
        _ => mode,
    }
}

pub fn status_label(status: &str) -> &str {
    match status {
        STATUS_RUNNING => "运行中",
        STATUS_COMPLETED => "完成",
        STATUS_FAILED => "失败",
        STATUS_CANCELLED => "取消",
        STATUS_PAUSED => "等待继续",
        STATUS_INTERRUPTED => "中断",
        STATUS_UNSUPPORTED => "不支持",
        _ => status,
    }
}

struct ActiveRun {
    record: ConsumptionRecord,
    started: Instant,
    last_saved: Instant,
    provider: Option<OpenAiProvider>,
}

impl ActiveRun {
    fn snapshot(&self) -> ConsumptionRecord {
        let mut record = self.record.clone();
        record.elapsed_ms = self.started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
        record.api = self
            .provider
            .as_ref()
            .map(OpenAiProvider::request_metrics)
            .unwrap_or_default();
        record
    }
}

struct ActiveTranslationRun {
    base_id: String,
    project_id: String,
    project_name: String,
    task: String,
    mode: String,
    started_at: DateTime<Utc>,
    started: Instant,
    last_saved: Instant,
    providers: Vec<OpenAiProvider>,
}

fn has_recorded_api_usage(api: &RequestMetrics) -> bool {
    api.requests > 0 || api.input_tokens > 0 || api.output_tokens > 0
}

fn allocate_elapsed(total_ms: u64, weights: &[u64]) -> Vec<u64> {
    if weights.is_empty() {
        return Vec::new();
    }
    if weights.len() == 1 {
        return vec![total_ms];
    }
    let total_weight: u128 = weights.iter().map(|&w| u128::from(w)).sum();
    let mut shares: Vec<u64> = if total_weight == 0 {
        let count = weights.len() as u64;
        vec![total_ms / count; weights.len()]
    } else {
        weights
            .iter()
            .map(|&w| (u128::from(total_ms) * u128::from(w) / total_weight) as u64)
            .collect()
    };
    let allocated: u64 = shares.iter().sum();
    if let Some(first) = shares.first_mut() {
        *first = first.saturating_add(total_ms.saturating_sub(allocated));
    }
    shares
}

impl ActiveTranslationRun {
    fn snapshots(&self, ended_at: Option<DateTime<Utc>>, status: &str) -> Vec<ConsumptionRecord> {
        let total_elapsed_ms = self.started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
        let active: Vec<(usize, &OpenAiProvider, RequestMetrics)> = self
            .providers
            .iter()
            .enumerate()
            .filter_map(|(index, provider)| {
                let api = provider.request_metrics();
                has_recorded_api_usage(&api).then_some((index, provider, api))
            })
            .collect();
        if active.is_empty() {
            return Vec::new();
        }
        let weights: Vec<u64> = active.iter().map(|(_, _, api)| api.api_ms).collect();
        let elapsed_shares = allocate_elapsed(total_elapsed_ms, &weights);
        active
            .into_iter()
            .zip(elapsed_shares)
            .map(|((index, provider, api), elapsed_ms)| ConsumptionRecord {
                id: if index == 0 {
                    self.base_id.clone()
                } else {
                    format!("{}-{index}", self.base_id)
                },
                project_id: self.project_id.clone(),
                project_name: self.project_name.clone(),
                software_version: SOFTWARE_VERSION.into(),
                build_git_hash: BUILD_GIT_HASH.into(),
                model: Some(provider.model().into()),
                profile_name: Some(provider.display_name().into()),
                reasoning_effort: None,
                task: self.task.clone(),
                mode: self.mode.clone(),
                status: status.into(),
                started_at: self.started_at,
                ended_at,
                elapsed_ms,
                api,
            })
            .collect()
    }
}

#[derive(Default)]
pub(crate) struct MetricsState {
    active: HashMap<String, ActiveRun>,
    reviews: HashMap<String, ActiveRun>,
    translations: HashMap<String, ActiveTranslationRun>,
}

impl MetricsState {
    fn runs(&mut self, operation: &str) -> &mut HashMap<String, ActiveRun> {
        if operation == task_op::REVIEW {
            &mut self.reviews
        } else {
            &mut self.active
        }
    }
}

#[derive(Default)]
pub struct ConsumptionHistory {
    pub records: Vec<ConsumptionRecord>,
    pub unreadable_lines: usize,
}

fn append(root: &Path, record: &ConsumptionRecord) -> Result<(), CoreError> {
    let directory = root.join(METRICS_DIR);
    fs::create_dir_all(&directory)?;
    let path = directory.join(format!(
        "{RUNS_FILE_PREFIX}{}{RUNS_FILE_SUFFIX}",
        record.started_at.format("%Y%m")
    ));
    let mut bytes = vec![b'\n'];
    serde_json::to_writer(&mut bytes, record)
        .map_err(|error| CoreError::InvalidData(error.to_string()))?;
    bytes.push(b'\n');
    // A separator keeps a truncated previous line from consuming the next record.
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?
        .write_all(&bytes)?;
    Ok(())
}

fn append_all(root: &Path, records: &[ConsumptionRecord]) -> Result<(), CoreError> {
    for record in records {
        append(root, record)?;
    }
    Ok(())
}

fn new_record_id(project_id: &str, task: &str, started_at: DateTime<Utc>) -> String {
    hash_text(&format!(
        "{project_id}:{task}:{}:{}",
        started_at.to_rfc3339(),
        std::process::id()
    ))[..RECORD_ID_HEX_LEN]
        .into()
}

fn read_history(root: &Path) -> Result<ConsumptionHistory, CoreError> {
    let directory = root.join(METRICS_DIR);
    if !directory.is_dir() {
        return Ok(ConsumptionHistory::default());
    }
    let mut files = fs::read_dir(directory)?.collect::<Result<Vec<_>, _>>()?;
    files.sort_by_key(|entry| entry.file_name());
    let mut records = BTreeMap::new();
    let mut unreadable_lines = 0;
    for entry in files {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with(RUNS_FILE_PREFIX)
            || !name.ends_with(RUNS_FILE_SUFFIX)
            || !entry.file_type()?.is_file()
        {
            continue;
        }
        for line in BufReader::new(fs::File::open(entry.path())?).lines() {
            let line = match line {
                Ok(line) => line,
                Err(error) if error.kind() == std::io::ErrorKind::InvalidData => {
                    unreadable_lines += 1;
                    continue;
                }
                Err(error) => return Err(error.into()),
            };
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<ConsumptionRecord>(&line) {
                Ok(record) => {
                    records.insert(record.id.clone(), record);
                }
                Err(_) => unreadable_lines += 1,
            }
        }
    }
    Ok(ConsumptionHistory {
        records: records.into_values().collect(),
        unreadable_lines,
    })
}

impl Application {
    pub(crate) async fn start_metrics(&self, id: &str, task: &str) {
        if !matches!(task, task_op::RECOGNIZE | task_op::ADAPT | task_op::REVIEW) {
            return;
        }
        let Ok(project) = self.store.get(id).await else {
            return;
        };
        let now = Utc::now();
        let record = ConsumptionRecord {
            id: new_record_id(id, task, now),
            project_id: id.into(),
            project_name: project.name,
            software_version: SOFTWARE_VERSION.into(),
            build_git_hash: BUILD_GIT_HASH.into(),
            model: None,
            profile_name: None,
            reasoning_effort: None,
            task: task.into(),
            mode: MODE_NEW.into(),
            status: STATUS_RUNNING.into(),
            started_at: now,
            ended_at: None,
            elapsed_ms: 0,
            api: RequestMetrics::default(),
        };
        self.metrics.lock().await.runs(task).insert(
            id.into(),
            ActiveRun {
                record,
                started: Instant::now(),
                last_saved: Instant::now(),
                provider: None,
            },
        );
    }

    pub(crate) async fn metrics_provider(
        &self,
        id: &str,
        operation: &str,
        provider: &OpenAiProvider,
    ) {
        if let Some(run) = self.metrics.lock().await.runs(operation).get_mut(id) {
            run.record.model = Some(provider.model().into());
            run.record.profile_name = Some(provider.display_name().into());
            run.record.reasoning_effort = provider.analysis_reasoning_effort().map(str::to_owned);
            run.provider = Some(provider.clone());
        }
    }

    pub(crate) async fn metrics_details(
        &self,
        id: &str,
        operation: &str,
        task: Option<&str>,
        mode: Option<&str>,
        status: Option<&str>,
    ) {
        if let Some(run) = self.metrics.lock().await.runs(operation).get_mut(id) {
            if let Some(task) = task {
                run.record.task = task.into();
            }
            if let Some(mode) = mode {
                run.record.mode = mode.into();
            }
            if let Some(status) = status {
                run.record.status = status.into();
            }
        }
    }

    pub(crate) async fn checkpoint_metrics(&self, id: &str, operation: &str) {
        let record = {
            let mut metrics = self.metrics.lock().await;
            let Some(run) = metrics.runs(operation).get_mut(id) else {
                return;
            };
            // Save enough progress for crash recovery without writing on every task update.
            if run.last_saved.elapsed().as_secs() < CHECKPOINT_INTERVAL_SECS {
                return;
            }
            run.last_saved = Instant::now();
            run.snapshot()
        };
        let root = self.data_directory().to_owned();
        if let Err(error) = tokio::task::spawn_blocking(move || append(&root, &record))
            .await
            .map_err(CoreError::from)
            .and_then(|result| result)
        {
            self.report_task_warning(id, &format!("消耗指标暂未保存：{error}"));
        }
    }

    pub(crate) async fn finish_metrics(&self, id: &str, operation: &str, succeeded: Option<bool>) {
        let run = self.metrics.lock().await.runs(operation).remove(id);
        let Some(run) = run else {
            return;
        };
        let mut record = run.snapshot();
        record.ended_at = Some(Utc::now());
        record.status = match succeeded {
            None => STATUS_CANCELLED,
            Some(false) => STATUS_FAILED,
            Some(true) if record.status == STATUS_RUNNING => STATUS_COMPLETED,
            Some(true) => &record.status,
        }
        .into();
        let root = self.data_directory().to_owned();
        if let Err(error) = tokio::task::spawn_blocking(move || append(&root, &record))
            .await
            .map_err(CoreError::from)
            .and_then(|result| result)
        {
            self.report_task_warning(id, &format!("消耗指标保存失败：{error}"));
        }
    }

    pub(crate) async fn start_translation_metrics(
        &self,
        id: &str,
        task: &str,
        mode: &str,
        providers: &[OpenAiProvider],
    ) {
        if !matches!(
            task,
            task_op::TRANSLATE
                | task_op::PREVIEW_TRANSLATE
                | task_op::RETRANSLATE
                | task_op::CANDIDATE
        ) {
            return;
        }
        let Ok(project) = self.store.get(id).await else {
            return;
        };
        let now = Utc::now();
        let run = ActiveTranslationRun {
            base_id: new_record_id(id, task, now),
            project_id: id.into(),
            project_name: project.name,
            task: task.into(),
            mode: mode.into(),
            started_at: now,
            started: Instant::now(),
            last_saved: Instant::now(),
            providers: providers.to_vec(),
        };
        self.metrics
            .lock()
            .await
            .translations
            .insert(id.into(), run);
    }

    pub(crate) async fn checkpoint_translation_metrics(&self, id: &str) {
        let records = {
            let mut metrics = self.metrics.lock().await;
            let Some(run) = metrics.translations.get_mut(id) else {
                return;
            };
            if run.last_saved.elapsed().as_secs() < CHECKPOINT_INTERVAL_SECS {
                return;
            }
            let snapshots = run.snapshots(None, STATUS_RUNNING);
            if snapshots.is_empty() {
                return;
            }
            run.last_saved = Instant::now();
            snapshots
        };
        let root = self.data_directory().to_owned();
        if let Err(error) = tokio::task::spawn_blocking(move || append_all(&root, &records))
            .await
            .map_err(CoreError::from)
            .and_then(|result| result)
        {
            self.report_task_warning(id, &format!("消耗指标暂未保存：{error}"));
        }
    }

    pub(crate) async fn finish_translation_metrics(&self, id: &str, succeeded: Option<bool>) {
        let run = self.metrics.lock().await.translations.remove(id);
        let Some(run) = run else {
            return;
        };
        let status = match succeeded {
            None => STATUS_CANCELLED,
            Some(false) => STATUS_FAILED,
            Some(true) => STATUS_COMPLETED,
        };
        let records = run.snapshots(Some(Utc::now()), status);
        if records.is_empty() {
            return;
        }
        let root = self.data_directory().to_owned();
        if let Err(error) = tokio::task::spawn_blocking(move || append_all(&root, &records))
            .await
            .map_err(CoreError::from)
            .and_then(|result| result)
        {
            self.report_task_warning(id, &format!("消耗指标保存失败：{error}"));
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn record_single_provider_metrics(
        &self,
        id: &str,
        task: &str,
        provider: &OpenAiProvider,
        include_reasoning: bool,
        started_at: DateTime<Utc>,
        elapsed: std::time::Duration,
        status: &str,
    ) {
        let api = provider.request_metrics();
        if !has_recorded_api_usage(&api) {
            return;
        }
        let Ok(project) = self.store.get(id).await else {
            return;
        };
        let record = ConsumptionRecord {
            id: new_record_id(id, task, started_at),
            project_id: id.into(),
            project_name: project.name,
            software_version: SOFTWARE_VERSION.into(),
            build_git_hash: BUILD_GIT_HASH.into(),
            model: Some(provider.model().into()),
            profile_name: Some(provider.display_name().into()),
            reasoning_effort: include_reasoning
                .then(|| provider.analysis_reasoning_effort().map(str::to_owned))
                .flatten(),
            task: task.into(),
            mode: MODE_NEW.into(),
            status: status.into(),
            started_at,
            ended_at: Some(Utc::now()),
            elapsed_ms: elapsed.as_millis().min(u128::from(u64::MAX)) as u64,
            api,
        };
        let root = self.data_directory().to_owned();
        if let Err(error) = tokio::task::spawn_blocking(move || append(&root, &record))
            .await
            .map_err(CoreError::from)
            .and_then(|result| result)
        {
            self.report_task_warning(id, &format!("消耗指标保存失败：{error}"));
        }
    }

    pub async fn consumption_history(&self) -> Result<ConsumptionHistory, CoreError> {
        let root = self.data_directory().to_owned();
        let mut history = tokio::task::spawn_blocking(move || read_history(&root)).await??;
        let active_records: Vec<ConsumptionRecord> = {
            let metrics = self.metrics.lock().await;
            metrics
                .active
                .values()
                .chain(metrics.reviews.values())
                .map(ActiveRun::snapshot)
                .chain(
                    metrics
                        .translations
                        .values()
                        .flat_map(|run| run.snapshots(None, STATUS_RUNNING)),
                )
                .collect()
        };
        let active_ids: std::collections::HashSet<&str> = active_records
            .iter()
            .map(|record| record.id.as_str())
            .collect();
        history
            .records
            .retain(|record| !active_ids.contains(record.id.as_str()));
        for record in &mut history.records {
            if record.status == STATUS_RUNNING {
                record.status = STATUS_INTERRUPTED.into();
            }
        }
        history.records.extend(active_records);
        history.records.sort_by(|a, b| {
            b.started_at
                .cmp(&a.started_at)
                .then_with(|| a.id.cmp(&b.id))
        });
        Ok(history)
    }
}

#[derive(Clone, Debug, Default)]
pub struct ConsumptionFilter {
    pub from: Option<NaiveDate>,
    pub through: Option<NaiveDate>,
    pub version: String,
    pub model: String,
    pub project: String,
    pub task: String,
    pub mode: String,
    pub status: String,
    pub only_with_api: bool,
}

impl ConsumptionFilter {
    pub fn matches(&self, record: &ConsumptionRecord) -> bool {
        let date = record.started_at.with_timezone(&Local).date_naive();
        let task_matches = match self.task.as_str() {
            "" => true,
            "translation" => matches!(
                record.task.as_str(),
                task_op::TRANSLATE
                    | task_op::PREVIEW_TRANSLATE
                    | task_op::RETRANSLATE
                    | task_op::CANDIDATE
            ),
            "adaptation" => matches!(
                record.task.as_str(),
                task_op::ADAPT | task_op::REPAIR | task_op::EXPAND | task_op::RECOGNIZE
            ),
            exact => exact == record.task,
        };
        let api_matches = !self.only_with_api
            || has_recorded_api_usage(&record.api)
            || record.status == STATUS_RUNNING;
        self.from.is_none_or(|from| date >= from)
            && self.through.is_none_or(|through| date <= through)
            && (self.version.is_empty() || self.version == record.version_key())
            && (self.model.is_empty() || self.model == record.model_key())
            && (self.project.is_empty() || self.project == record.project_id)
            && task_matches
            && (self.mode.is_empty() || self.mode == record.mode)
            && (self.status.is_empty() || self.status == record.status)
            && api_matches
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub enum ConsumptionDimension {
    #[default]
    Date,
    Version,
    Model,
    Task,
    Project,
    Mode,
    Status,
}

#[derive(Clone, Debug, Default)]
pub struct ConsumptionTotals {
    pub runs: usize,
    pub requests: u64,
    pub failed_requests: u64,
    pub usage_requests: u64,
    pub cache_requests: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_tokens: u64,
    pub cache_input_tokens: u64,
    pub elapsed_ms: u64,
    pub api_ms: u64,
    pub waiting_ms: u64,
}

impl ConsumptionTotals {
    fn add(&mut self, record: &ConsumptionRecord) {
        self.runs += 1;
        self.requests += record.api.requests;
        self.failed_requests += record.api.failed_requests;
        self.usage_requests += record.api.usage_requests;
        self.cache_requests += record.api.cache_requests;
        self.input_tokens += record.api.input_tokens;
        self.output_tokens += record.api.output_tokens;
        self.cached_tokens += record.api.cached_tokens;
        self.cache_input_tokens += record.api.cache_input_tokens;
        self.elapsed_ms += record.elapsed_ms;
        self.api_ms += record.api.api_ms;
        self.waiting_ms += record.api.waiting_ms;
    }

    pub fn cache_hit_rate(&self) -> Option<f64> {
        (self.cache_input_tokens > 0)
            .then(|| self.cached_tokens as f64 / self.cache_input_tokens as f64)
    }

    pub fn uncached_tokens(&self) -> u64 {
        self.cache_input_tokens.saturating_sub(self.cached_tokens)
    }

    pub fn total_tokens(&self) -> u64 {
        self.input_tokens.saturating_add(self.output_tokens)
    }

    pub fn non_cached_input_tokens(&self) -> u64 {
        self.input_tokens.saturating_sub(self.cached_tokens)
    }
}

pub struct ConsumptionGroup {
    pub key: String,
    pub label: String,
    pub totals: ConsumptionTotals,
}

pub fn consumption_groups(
    records: &[ConsumptionRecord],
    filter: &ConsumptionFilter,
    dimension: ConsumptionDimension,
) -> (ConsumptionTotals, Vec<ConsumptionGroup>) {
    let mut total = ConsumptionTotals::default();
    let mut groups: BTreeMap<String, ConsumptionGroup> = BTreeMap::new();
    for record in records.iter().filter(|record| filter.matches(record)) {
        total.add(record);
        let (key, label) = match dimension {
            ConsumptionDimension::Date => {
                let date = record
                    .started_at
                    .with_timezone(&Local)
                    .format("%Y-%m-%d")
                    .to_string();
                (date.clone(), date)
            }
            ConsumptionDimension::Version => (record.version_key(), record.version_label()),
            ConsumptionDimension::Model => (record.model_key(), record.model_label()),
            ConsumptionDimension::Task => (record.task.clone(), task_label(&record.task).into()),
            ConsumptionDimension::Project => {
                (record.project_id.clone(), record.project_name.clone())
            }
            ConsumptionDimension::Mode => (record.mode.clone(), mode_label(&record.mode).into()),
            ConsumptionDimension::Status => {
                (record.status.clone(), status_label(&record.status).into())
            }
        };
        groups
            .entry(key.clone())
            .or_insert_with(|| ConsumptionGroup {
                key,
                label,
                totals: ConsumptionTotals::default(),
            })
            .totals
            .add(record);
    }
    (total, groups.into_values().collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ToolPaths;
    use chrono::TimeZone;

    fn record(id: &str, input: u64, cached: Option<u64>) -> ConsumptionRecord {
        ConsumptionRecord {
            id: id.into(),
            project_id: "game-a".into(),
            project_name: "Game A".into(),
            software_version: "0.1.0".into(),
            build_git_hash: "aaaaaaaa11111111".into(),
            model: Some("model-a".into()),
            profile_name: Some("Profile A".into()),
            reasoning_effort: Some("high".into()),
            task: "adapt".into(),
            mode: "new".into(),
            status: "completed".into(),
            started_at: Local
                .with_ymd_and_hms(2026, 11, 12, 12, 0, 0)
                .single()
                .unwrap()
                .with_timezone(&Utc),
            ended_at: None,
            elapsed_ms: 1000,
            api: RequestMetrics {
                requests: 1,
                usage_requests: 1,
                cache_requests: u64::from(cached.is_some()),
                input_tokens: input,
                output_tokens: 20,
                cached_tokens: cached.unwrap_or(0),
                cache_input_tokens: if cached.is_some() { input } else { 0 },
                ..Default::default()
            },
        }
    }

    #[test]
    fn cache_rates_are_weighted_and_missing_counts_do_not_become_cache_misses() {
        let records = vec![
            record("a", 100, Some(80)),
            record("b", 900, Some(90)),
            record("c", 500, None),
        ];
        let (totals, groups) = consumption_groups(
            &records,
            &ConsumptionFilter::default(),
            ConsumptionDimension::Model,
        );
        assert_eq!(groups.len(), 1);
        assert_eq!(
            (
                totals.input_tokens,
                totals.cached_tokens,
                totals.uncached_tokens()
            ),
            (1500, 170, 830)
        );
        assert_eq!((totals.usage_requests, totals.cache_requests), (3, 2));
        assert!((totals.cache_hit_rate().unwrap() - 0.17).abs() < 1e-12);
    }

    #[test]
    fn filters_combine_local_dates_full_build_identity_model_and_execution_kind() {
        let first = record("a", 100, Some(80));
        let mut second = first.clone();
        second.id = "b".into();
        second.build_git_hash = "aaaaaaaa22222222".into();
        second.model = Some("model-b".into());
        second.mode = "continued".into();
        second.task = "repair".into();
        let date = NaiveDate::from_ymd_opt(2026, 11, 12).unwrap();
        let filter = ConsumptionFilter {
            from: Some(date),
            through: Some(date),
            version: second.version_key(),
            model: second.model_key(),
            project: "game-a".into(),
            task: "repair".into(),
            mode: "continued".into(),
            status: "completed".into(),
            ..Default::default()
        };
        assert!(!filter.matches(&first));
        assert!(filter.matches(&second));
        let records = [first, second];
        let (totals, groups) = consumption_groups(&records, &filter, ConsumptionDimension::Version);
        assert_eq!((totals.runs, groups.len()), (1, 1));
        let (_, groups) = consumption_groups(
            &records,
            &ConsumptionFilter::default(),
            ConsumptionDimension::Version,
        );
        assert_eq!(groups.len(), 2);
        let tomorrow = ConsumptionFilter {
            from: date.succ_opt(),
            ..Default::default()
        };
        assert!(!tomorrow.matches(&records[0]));
    }

    #[test]
    fn model_configuration_groups_keep_profile_and_reasoning_snapshots_and_legacy_unknowns() {
        let first = record("a", 100, Some(80));
        let mut second = first.clone();
        second.id = "b".into();
        second.profile_name = Some("Profile B".into());
        let mut third = first.clone();
        third.id = "c".into();
        third.reasoning_effort = Some("low".into());
        let mut default_reasoning = first.clone();
        default_reasoning.id = "default".into();
        default_reasoning.reasoning_effort = None;
        assert_eq!(
            default_reasoning.model_label(),
            "Profile A / model-a / 默认"
        );
        let mut legacy_value = serde_json::to_value(&first).unwrap();
        legacy_value.as_object_mut().unwrap().remove("profileName");
        legacy_value
            .as_object_mut()
            .unwrap()
            .remove("reasoningEffort");
        let mut legacy: ConsumptionRecord = serde_json::from_value(legacy_value).unwrap();
        legacy.id = "legacy".into();
        assert_eq!(legacy.model_label(), "未记录配置 / model-a / 未记录强度");
        let filter = ConsumptionFilter {
            model: first.model_key(),
            ..Default::default()
        };
        assert!(filter.matches(&first));
        assert!(
            !filter.matches(&second)
                && !filter.matches(&third)
                && !filter.matches(&default_reasoning)
                && !filter.matches(&legacy)
        );
        let records = [first, second, third, default_reasoning, legacy];
        let (_, groups) = consumption_groups(
            &records,
            &ConsumptionFilter::default(),
            ConsumptionDimension::Model,
        );
        assert_eq!(groups.len(), 5);
        assert!(groups.iter().all(|group| group.totals.runs == 1));
    }

    #[test]
    fn append_snapshots_merge_by_run_and_recover_after_truncated_or_invalid_lines() {
        let root = tempfile::tempdir().unwrap();
        let first = record("a", 100, Some(80));
        append(root.path(), &first).unwrap();
        let path = root.path().join("metrics/runs-202611.jsonl");
        OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"{\"id\":")
            .unwrap();
        let second = record("a", 200, Some(180));
        append(root.path(), &second).unwrap();
        OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"\xff\n")
            .unwrap();
        append(root.path(), &record("b", 300, None)).unwrap();
        let history = read_history(root.path()).unwrap();
        assert_eq!((history.records.len(), history.unreadable_lines), (2, 2));
        let (totals, _) = consumption_groups(
            &history.records,
            &ConsumptionFilter::default(),
            ConsumptionDimension::Date,
        );
        assert_eq!((totals.runs, totals.input_tokens), (2, 500));
    }

    #[tokio::test]
    async fn task_history_keeps_completed_and_cancelled_runs_and_latest_interrupted_snapshot() {
        let root = tempfile::tempdir().unwrap();
        let game = root.path().join("game");
        fs::create_dir(&game).unwrap();
        let tools = ToolPaths::new(root.path().join("tools"));
        let app = Application::open(root.path().join("data"), &tools)
            .await
            .unwrap();
        let project = app.add_project(&game).await.unwrap();
        let _task = app
            .begin(&project.id, "adapt", "Preparing adapter")
            .await
            .unwrap();
        let provider = OpenAiProvider::new(gamel10n_translation::provider::ProviderSettings {
            endpoint: "http://127.0.0.1:1/v1".into(),
            api_key: String::new(),
            model: "model-a".into(),
            protocol: Default::default(),
            translation_format: Default::default(),
            analysis_reasoning_effort: Some("high".into()),
            max_concurrency: None,
        })
        .unwrap()
        .with_display_name("Profile A".into());
        app.metrics_provider(&project.id, "adapt", &provider).await;
        drop(provider);
        let live = app.consumption_history().await.unwrap();
        assert_eq!(live.records[0].status, "running");
        assert_eq!(live.records[0].model_label(), "Profile A / model-a / high");
        app.metrics
            .lock()
            .await
            .active
            .get_mut(&project.id)
            .unwrap()
            .last_saved = Instant::now() - std::time::Duration::from_secs(61);
        app.checkpoint_metrics(&project.id, "adapt").await;
        app.finish(&project.id, "adapt", Ok("Adapter completed".into()))
            .await
            .unwrap();
        let token = app.begin_review(&project.id).await.unwrap();
        token.cancel();
        app.finish_review(&project.id, Err("Cancelled".into()))
            .await
            .unwrap();
        let mut interrupted = record("interrupted", 100, Some(80));
        interrupted.status = "running".into();
        append(app.data_directory(), &interrupted).unwrap();
        let history = app.consumption_history().await.unwrap();
        assert_eq!(history.records.len(), 3);
        assert_eq!(
            history
                .records
                .iter()
                .find(|record| record.task == "adapt" && record.status == "completed")
                .unwrap()
                .model_label(),
            "Profile A / model-a / high"
        );
        assert!(
            history
                .records
                .iter()
                .any(|record| record.status == "completed")
        );
        assert!(
            history
                .records
                .iter()
                .any(|record| record.status == "cancelled")
        );
        assert!(
            history
                .records
                .iter()
                .any(|record| record.status == "interrupted" && record.ended_at.is_none())
        );
        assert!(
            history
                .records
                .iter()
                .filter(|record| record.id != "interrupted")
                .all(|record| record.software_version == SOFTWARE_VERSION
                    && record.build_git_hash == BUILD_GIT_HASH
                    && record.api.requests == 0)
        );
        let directory = app.data_directory().to_owned();
        drop(token);
        drop(_task);
        drop(app);
        let reopened = Application::open(directory, &tools).await.unwrap();
        assert_eq!(
            reopened.consumption_history().await.unwrap().records.len(),
            3
        );
    }

    #[test]
    fn allocate_elapsed_preserves_total_across_single_proportional_and_zero_weights() {
        assert_eq!(allocate_elapsed(500, &[]), Vec::<u64>::new());
        assert_eq!(allocate_elapsed(500, &[120]), vec![500]);
        assert_eq!(allocate_elapsed(100, &[1, 3]), vec![25, 75]);
        assert_eq!(allocate_elapsed(10, &[1, 1, 1]), vec![4, 3, 3]);
        assert_eq!(allocate_elapsed(10, &[0, 0, 0]), vec![4, 3, 3]);
    }

    #[test]
    fn task_category_and_api_only_filters_group_tasks_and_skip_zero_api_completed_runs() {
        let mut translate = record("t1", 400, Some(200));
        translate.task = "translate".into();
        let mut retranslate = record("t2", 100, None);
        retranslate.task = "retranslate".into();
        let mut adapt = record("a1", 800, Some(600));
        adapt.task = "adapt".into();
        let mut local_recognize = record("r0", 0, None);
        local_recognize.task = "recognize".into();
        local_recognize.model = None;
        local_recognize.api = RequestMetrics::default();
        let mut running_empty = record("r1", 0, None);
        running_empty.task = "adapt".into();
        running_empty.status = "running".into();
        running_empty.api = RequestMetrics::default();
        let mut terminology = record("term", 150, None);
        terminology.task = "terminology".into();

        let all = [
            translate,
            retranslate,
            adapt,
            local_recognize,
            running_empty,
            terminology,
        ];

        let translation_only = ConsumptionFilter {
            task: "translation".into(),
            only_with_api: true,
            ..Default::default()
        };
        let (totals, _) = consumption_groups(&all, &translation_only, ConsumptionDimension::Task);
        assert_eq!((totals.runs, totals.input_tokens), (2, 500));

        let adaptation_only = ConsumptionFilter {
            task: "adaptation".into(),
            only_with_api: true,
            ..Default::default()
        };
        let (totals, _) = consumption_groups(&all, &adaptation_only, ConsumptionDimension::Task);
        assert_eq!((totals.runs, totals.input_tokens), (2, 800));

        let terminology_only = ConsumptionFilter {
            task: "terminology".into(),
            only_with_api: true,
            ..Default::default()
        };
        let (totals, _) = consumption_groups(&all, &terminology_only, ConsumptionDimension::Task);
        assert_eq!((totals.runs, totals.input_tokens), (1, 150));
    }

    #[tokio::test]
    async fn translation_and_terminology_metrics_record_usage_and_skip_cache_only_runs() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        fn spawn_mock_openai(bodies: Vec<&'static str>) -> (String, std::thread::JoinHandle<()>) {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let handle = std::thread::spawn(move || {
                for body in bodies {
                    let (mut stream, _) = listener.accept().unwrap();
                    let mut buf = [0_u8; 8192];
                    let _ = stream.read(&mut buf);
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    stream.write_all(response.as_bytes()).unwrap();
                }
            });
            (format!("http://{address}/v1"), handle)
        }

        let root = tempfile::tempdir().unwrap();
        let game = root.path().join("game");
        fs::create_dir(&game).unwrap();
        fs::write(game.join("story.txt"), "Hello\nGoodbye").unwrap();
        let app = Application::open(
            root.path().join("data"),
            &ToolPaths::new(root.path().join("tools")),
        )
        .await
        .unwrap();
        let project = app.add_project(&game).await.unwrap();

        let (endpoint, server) = spawn_mock_openai(vec![
            r#"{"choices":[{"finish_reason":"stop","message":{"content":"{\"0\":\"你好勇者\",\"1\":\"再见\"}"}}],"usage":{"prompt_tokens":320,"completion_tokens":45,"prompt_tokens_details":{"cached_tokens":120}}}"#,
            r#"{"choices":[{"finish_reason":"stop","message":{"content":"{\"terms\":[{\"source\":\"Hero\",\"target\":\"勇者\",\"category\":\"角色\"}]}"}}],"usage":{"prompt_tokens":180,"completion_tokens":30}}"#,
        ]);
        app.save_api_profile(
            crate::settings::ApiProfile {
                id: "primary".into(),
                name: "Primary Trans".into(),
                endpoint,
                model: "trans-model".into(),
                protocol: "ChatCompletions".into(),
                analysis_reasoning_effort: Some("high".into()),
                ..Default::default()
            },
            Some("secret".into()),
            false,
        )
        .await
        .unwrap();
        app.save_project_settings(
            &project.id,
            serde_json::json!({"translationProfileIds": ["primary"]}),
        )
        .await
        .unwrap();

        let units = vec![
            crate::TranslationUnit {
                id: "line-1".into(),
                resource_path: "story.txt".into(),
                locator: "1".into(),
                source_text: "Hello Hero".into(),
                context: "opening".into(),
                speaker: Some("Hero".into()),
                kind: "dialogue".into(),
                protected_tokens: None,
            },
            crate::TranslationUnit {
                id: "line-2".into(),
                resource_path: "story.txt".into(),
                locator: "2".into(),
                source_text: "Goodbye".into(),
                context: "ending".into(),
                speaker: None,
                kind: "dialogue".into(),
                protected_tokens: None,
            },
        ];
        gamel10n_translation::files::write_json(
            &app.project_directory(&project.id)
                .unwrap()
                .join("extraction/native.json"),
            &crate::StoredAnalysis {
                schema_version: 2,
                coverage_pending: false,
                format: "fixture".into(),
                source_directory: game.clone(),
                resource_files: vec!["story.txt".into()],
                source_hashes: BTreeMap::from([("story.txt".into(), hash_text("Hello\nGoodbye"))]),
                adapter: None,
                probe: None,
                extraction: gamel10n_engines::protocol::ExtractionResult {
                    units,
                    warnings: vec![],
                },
            },
        )
        .unwrap();

        let run = app
            .translate_project(&project.id, Default::default())
            .await
            .unwrap();
        assert_eq!((run.completed, run.failed), (2, 0));

        // Second translation is 100% cache hit and must not append a 0-request record.
        let cached_run = app
            .translate_project(&project.id, Default::default())
            .await
            .unwrap();
        assert_eq!(cached_run.cache_hits, 2);

        let terms = app
            .extract_project_terminology(&project.id, &crate::CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(terms.candidates.len(), 1);
        server.join().unwrap();

        let history = app.consumption_history().await.unwrap();
        assert_eq!(history.records.len(), 2);
        let trans = history
            .records
            .iter()
            .find(|r| r.task == "translate")
            .unwrap();
        assert_eq!(trans.status, "completed");
        assert_eq!(trans.profile_name.as_deref(), Some("Primary Trans"));
        assert_eq!(trans.model.as_deref(), Some("trans-model"));
        // Translation does not send reasoning effort, so it must stay None.
        assert_eq!(trans.reasoning_effort, None);
        assert_eq!(
            (
                trans.api.requests,
                trans.api.input_tokens,
                trans.api.output_tokens,
                trans.api.cached_tokens
            ),
            (1, 320, 45, 120)
        );
        let term = history
            .records
            .iter()
            .find(|r| r.task == "terminology")
            .unwrap();
        assert_eq!(term.status, "completed");
        assert_eq!(term.reasoning_effort, None);
        assert_eq!(
            (
                term.api.requests,
                term.api.input_tokens,
                term.api.output_tokens
            ),
            (1, 180, 30)
        );
    }
}
