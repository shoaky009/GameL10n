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

use crate::{Application, CoreError};

pub const SOFTWARE_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const BUILD_GIT_HASH: &str = env!("GAMEL10N_BUILD_GIT_HASH");

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConsumptionRecord {
    pub id: String,
    pub project_id: String,
    pub project_name: String,
    pub software_version: String,
    pub build_git_hash: String,
    pub model: Option<String>,
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
            self.build_git_hash.chars().take(8).collect::<String>()
        )
    }

    pub fn model_label(&self) -> &str {
        self.model.as_deref().unwrap_or("未调用模型")
    }
}

pub fn task_label(task: &str) -> &str {
    match task {
        "recognize" => "识别与适配",
        "adapt" => "智能适配",
        "repair" => "修复适配",
        "expand" => "扩展覆盖",
        "review" => "经验总结",
        _ => task,
    }
}

pub fn mode_label(mode: &str) -> &str {
    match mode {
        "new" => "新运行",
        "continued" => "续接",
        "reused" => "复用结果",
        _ => mode,
    }
}

pub fn status_label(status: &str) -> &str {
    match status {
        "running" => "运行中",
        "completed" => "完成",
        "failed" => "失败",
        "cancelled" => "取消",
        "paused" => "等待继续",
        "interrupted" => "中断",
        "unsupported" => "不支持",
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

#[derive(Default)]
pub(crate) struct MetricsState {
    active: HashMap<String, ActiveRun>,
}

#[derive(Default)]
pub struct ConsumptionHistory {
    pub records: Vec<ConsumptionRecord>,
    pub unreadable_lines: usize,
}

fn append(root: &Path, record: &ConsumptionRecord) -> Result<(), CoreError> {
    let directory = root.join("metrics");
    fs::create_dir_all(&directory)?;
    let path = directory.join(format!("runs-{}.jsonl", record.started_at.format("%Y%m")));
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

fn read_history(root: &Path) -> Result<ConsumptionHistory, CoreError> {
    let directory = root.join("metrics");
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
        if !name.starts_with("runs-") || !name.ends_with(".jsonl") || !entry.file_type()?.is_file()
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
        if !matches!(task, "recognize" | "adapt" | "review") {
            return;
        }
        let Ok(project) = self.store.get(id).await else {
            return;
        };
        let now = Utc::now();
        let record = ConsumptionRecord {
            id: hash_text(&format!("{id}:{}:{}", now.to_rfc3339(), std::process::id()))[..32]
                .into(),
            project_id: id.into(),
            project_name: project.name,
            software_version: SOFTWARE_VERSION.into(),
            build_git_hash: BUILD_GIT_HASH.into(),
            model: None,
            task: task.into(),
            mode: "new".into(),
            status: "running".into(),
            started_at: now,
            ended_at: None,
            elapsed_ms: 0,
            api: RequestMetrics::default(),
        };
        self.metrics.lock().await.active.insert(
            id.into(),
            ActiveRun {
                record,
                started: Instant::now(),
                last_saved: Instant::now(),
                provider: None,
            },
        );
    }

    pub(crate) async fn metrics_provider(&self, id: &str, provider: &OpenAiProvider) {
        if let Some(run) = self.metrics.lock().await.active.get_mut(id) {
            run.record.model = Some(provider.model().into());
            run.provider = Some(provider.clone());
        }
    }

    pub(crate) async fn metrics_details(
        &self,
        id: &str,
        task: Option<&str>,
        mode: Option<&str>,
        status: Option<&str>,
    ) {
        if let Some(run) = self.metrics.lock().await.active.get_mut(id) {
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

    pub(crate) async fn checkpoint_metrics(&self, id: &str) {
        let mut metrics = self.metrics.lock().await;
        let Some(run) = metrics.active.get_mut(id) else {
            return;
        };
        if run.last_saved.elapsed().as_secs() < 60 {
            return;
        }
        run.last_saved = Instant::now();
        let record = run.snapshot();
        let root = self.data_directory().to_owned();
        if let Err(error) = tokio::task::spawn_blocking(move || append(&root, &record))
            .await
            .map_err(CoreError::from)
            .and_then(|result| result)
        {
            self.report_task_warning(id, &format!("消耗指标暂未保存：{error}"));
        }
    }

    pub(crate) async fn finish_metrics(&self, id: &str, succeeded: Option<bool>) {
        let mut metrics = self.metrics.lock().await;
        let Some(run) = metrics.active.remove(id) else {
            return;
        };
        let mut record = run.snapshot();
        record.ended_at = Some(Utc::now());
        record.status = match succeeded {
            None => "cancelled",
            Some(false) => "failed",
            Some(true) if record.status == "running" => "completed",
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

    pub async fn consumption_history(&self) -> Result<ConsumptionHistory, CoreError> {
        let root = self.data_directory().to_owned();
        let mut history = tokio::task::spawn_blocking(move || read_history(&root)).await??;
        let metrics = self.metrics.lock().await;
        let active: BTreeMap<_, _> = metrics
            .active
            .values()
            .map(|run| (run.record.id.as_str(), run))
            .collect();
        history
            .records
            .retain(|record| !active.contains_key(record.id.as_str()));
        for record in &mut history.records {
            if record.status == "running" {
                record.status = "interrupted".into();
            }
        }
        history
            .records
            .extend(active.values().map(|run| run.snapshot()));
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
}

impl ConsumptionFilter {
    pub fn matches(&self, record: &ConsumptionRecord) -> bool {
        let date = record.started_at.with_timezone(&Local).date_naive();
        self.from.is_none_or(|from| date >= from)
            && self.through.is_none_or(|through| date <= through)
            && (self.version.is_empty() || self.version == record.version_key())
            && (self.model.is_empty() || self.model == record.model_label())
            && (self.project.is_empty() || self.project == record.project_id)
            && (self.task.is_empty() || self.task == record.task)
            && (self.mode.is_empty() || self.mode == record.mode)
            && (self.status.is_empty() || self.status == record.status)
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
            ConsumptionDimension::Model => {
                (record.model_label().into(), record.model_label().into())
            }
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
            model: "model-b".into(),
            project: "game-a".into(),
            task: "repair".into(),
            mode: "continued".into(),
            status: "completed".into(),
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
        app.begin(&project.id, "adapt", "Preparing adapter")
            .await
            .unwrap();
        let live = app.consumption_history().await.unwrap();
        assert_eq!(live.records[0].status, "running");
        app.metrics
            .lock()
            .await
            .active
            .get_mut(&project.id)
            .unwrap()
            .last_saved = Instant::now() - std::time::Duration::from_secs(61);
        app.checkpoint_metrics(&project.id).await;
        app.finish(&project.id, "adapt", Ok("Adapter completed".into()))
            .await
            .unwrap();
        let token = app
            .begin(&project.id, "review", "Preparing review")
            .await
            .unwrap();
        token.cancel();
        app.finish(&project.id, "review", Err("Cancelled".into()))
            .await
            .unwrap();
        let mut interrupted = record("interrupted", 100, Some(80));
        interrupted.status = "running".into();
        append(app.data_directory(), &interrupted).unwrap();
        let history = app.consumption_history().await.unwrap();
        assert_eq!(history.records.len(), 3);
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
        drop(app);
        let reopened = Application::open(directory, &tools).await.unwrap();
        assert_eq!(
            reopened.consumption_history().await.unwrap().records.len(),
            3
        );
    }
}
