//! Batch planning, durable request receipts, cache reuse and refusal-only API fallback.

use crate::{
    TokenUsage, TranslationUnit,
    compatibility::legacy_json,
    edits::{EditChanges, EditStore},
    files::{hash_text, read_json, write_json},
    provider::{OpenAiProvider, ProviderError, TranslationResponse},
    storage::{
        MemoryEntry, StorageError, TranslationEdit, TranslationMemory, TranslationPolicy,
        memory_key, now, read_recoverable,
    },
    text,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque},
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    time::Duration,
};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct TranslationOptions {
    pub batch_size: usize,
    pub unit_ids: Option<Vec<String>>,
    pub candidate_only: bool,
    pub retranslation_id: Option<String>,
    pub retry_uncertain_requests: bool,
    /// Host requests a patch after successful translation; resource building remains in core.
    pub build_patch_after: bool,
}
impl Default for TranslationOptions {
    fn default() -> Self {
        Self {
            batch_size: 24,
            unit_ids: None,
            candidate_only: false,
            retranslation_id: None,
            retry_uncertain_requests: true,
            build_patch_after: false,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TranslationProgress {
    pub stage: String,
    pub completed: usize,
    pub total: usize,
    pub cache_hits: usize,
    pub failed: usize,
    pub usage: TokenUsage,
    pub message: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub updates: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub failure_updates: BTreeMap<String, Option<String>>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TranslationRunResult {
    pub translations: BTreeMap<String, String>,
    pub cache_hits: usize,
    pub usage: TokenUsage,
    pub total: usize,
    pub completed: usize,
    pub failed: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum ExecutionError {
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error(transparent)]
    File(#[from] crate::files::FileError),
    #[error("文件操作失败：{0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Invalid(String),
    #[error("任务已暂停，已完成译文已保存。再次开始可继续处理。")]
    Cancelled,
    #[error("上次请求结果未知，服务商可能已处理或计费。请确认是否重新发送；已保存译文不会重发。")]
    Uncertain,
    #[error("翻译 API 链均拒绝处理这批内容。已完成译文已保存。")]
    Refused,
    #[error("API 请求重试 5 次后仍失败。已完成译文已保存。最后一次失败：{0}")]
    Retries(String),
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordedIssue {
    pub code: String,
    pub message: String,
    pub unit_id: Option<String>,
    #[serde(default)]
    pub memory_key: Option<String>,
}
pub fn load_issues(directory: &Path) -> Vec<RecordedIssue> {
    read_json::<Vec<RecordedIssue>>(&directory.join("translation-errors.json")).unwrap_or_default()
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct PendingTranslation {
    id: String,
    memory_key: String,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct TranslationIntent {
    request_key: String,
    provider: String,
    sent_at: String,
    units: Vec<PendingTranslation>,
    #[serde(default)]
    candidate_only: bool,
    #[serde(default)]
    retranslation_id: Option<String>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct Receipt {
    request: TranslationIntent,
    #[serde(default)]
    response: Option<TranslationResponse>,
}
#[derive(Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct ExecutionJournal {
    #[serde(default)]
    pending: BTreeMap<String, Receipt>,
    #[serde(default)]
    refusals: BTreeMap<String, Value>,
    #[serde(default)]
    usage: TokenUsage,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    legacy_receipts: BTreeSet<String>,
}

fn add_usage(total: &mut TokenUsage, usage: &TokenUsage) {
    total.input_tokens += usage.input_tokens;
    total.output_tokens += usage.output_tokens;
    total.estimated |= usage.estimated;
    total.cached_tokens = match (total.cached_tokens, usage.cached_tokens) {
        (Some(a), Some(b)) => Some(a + b),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        _ => None,
    };
}
fn cancelled(cancellation: &CancellationToken) -> Result<(), ExecutionError> {
    if cancellation.is_cancelled() {
        Err(ExecutionError::Cancelled)
    } else {
        Ok(())
    }
}

pub struct TranslationEngine {
    directory: PathBuf,
    legacy_memory: Option<PathBuf>,
}
type ProgressChanges<'a> = (
    &'a [&'a TranslationUnit],
    &'a BTreeMap<String, String>,
    &'a [RecordedIssue],
);
struct CommitState {
    memory: TranslationMemory,
    completed: BTreeMap<String, String>,
    issues: Vec<RecordedIssue>,
}
struct CommitOptions {
    candidate_only: bool,
    retranslation_id: Option<String>,
}
impl TranslationEngine {
    pub fn new(directory: PathBuf, legacy_memory: Option<PathBuf>) -> Self {
        Self {
            directory,
            legacy_memory,
        }
    }
    pub async fn run<F>(
        &self,
        all_units: &[TranslationUnit],
        providers: &[OpenAiProvider],
        policy: &TranslationPolicy,
        options: &TranslationOptions,
        cancellation: &CancellationToken,
        mut report: F,
    ) -> Result<TranslationRunResult, ExecutionError>
    where
        F: FnMut(TranslationProgress),
    {
        if providers.is_empty()
            || !(1..=80).contains(&options.batch_size)
            || policy.target_language.trim().is_empty()
        {
            return Err(ExecutionError::Invalid(
                "请配置翻译 API 链、目标语言和每批文本数（1–80）。".into(),
            ));
        }
        if options.candidate_only && options.retranslation_id.is_some()
            || options.retranslation_id.as_deref().is_some_and(|id| {
                id.len() != 32 || !id.bytes().all(|byte| byte.is_ascii_hexdigit())
            })
            || options.retranslation_id.is_some() && options.unit_ids.is_none()
        {
            return Err(ExecutionError::Invalid(
                "重新翻译必须指定选区和有效任务标识。".into(),
            ));
        }
        let selected: Option<HashSet<&str>> = options
            .unit_ids
            .as_ref()
            .map(|ids| ids.iter().map(String::as_str).collect());
        let units: Vec<_> = all_units
            .iter()
            .filter(|unit| {
                selected
                    .as_ref()
                    .is_none_or(|ids| ids.contains(unit.id.as_str()))
            })
            .collect();
        if selected
            .as_ref()
            .is_some_and(|ids| ids.is_empty() || ids.len() != units.len())
        {
            return Err(ExecutionError::Invalid(
                "文本选区为空或已失效，请重新选择。".into(),
            ));
        }
        let continue_on_error = !options.candidate_only;
        let mut memory =
            TranslationMemory::load(self.directory.join("memory"), self.legacy_memory.as_deref())?;
        let journal_path = self.directory.join("translation-execution.json");
        let mut journal: ExecutionJournal = if journal_path.is_file() {
            read_json(&journal_path)?
        } else {
            ExecutionJournal::default()
        };
        let mut issues = load_issues(&self.directory);
        let mut completed = BTreeMap::new();
        let mut failed = BTreeSet::new();
        let mut cache_hits = 0;
        let mut usage = TokenUsage::default();
        let mut emit = |stage: &str,
                        message: String,
                        done: usize,
                        hits: usize,
                        failed: usize,
                        usage: &TokenUsage,
                        changes: Option<ProgressChanges<'_>>| {
            let updates = changes
                .map(|(units, completed, _)| {
                    units
                        .iter()
                        .filter_map(|unit| {
                            completed
                                .get(&unit.id)
                                .map(|text| (unit.id.clone(), text.clone()))
                        })
                        .collect()
                })
                .unwrap_or_default();
            let failure_updates = changes
                .map(|(units, completed, issues)| {
                    units
                        .iter()
                        .map(|unit| {
                            (
                                unit.id.clone(),
                                if completed.contains_key(&unit.id) {
                                    None
                                } else {
                                    issues
                                        .iter()
                                        .find(|issue| issue.unit_id.as_deref() == Some(&unit.id))
                                        .map(|issue| issue.message.clone())
                                },
                            )
                        })
                        .collect()
                })
                .unwrap_or_default();
            report(TranslationProgress {
                stage: stage.into(),
                completed: done,
                total: units.len(),
                cache_hits: hits,
                failed,
                usage: usage.clone(),
                message,
                updates,
                failure_updates,
            });
        };
        emit(
            "查询缓存",
            "正在加载历史译文。".into(),
            0,
            0,
            0,
            &usage,
            None,
        );
        self.import_legacy_receipts(&mut journal)?;
        // Recover every matching paid response before choosing a new batch size or scope.
        let mut recovered_receipts = Vec::new();
        for (receipt_key, receipt) in &journal.pending {
            if receipt.request.candidate_only != options.candidate_only
                || receipt.request.retranslation_id != options.retranslation_id
            {
                continue;
            }
            if let Some(response) = &receipt.response {
                let requested: HashSet<_> = receipt
                    .request
                    .units
                    .iter()
                    .map(|unit| unit.id.as_str())
                    .collect();
                let recovery_units: Vec<_> = all_units
                    .iter()
                    .filter(|unit| {
                        // Receipts store representatives; recover identical texts in the selection too.
                        !options.candidate_only && options.retranslation_id.is_none()
                            || requested.contains(unit.id.as_str())
                            || selected
                                .as_ref()
                                .is_none_or(|ids| ids.contains(unit.id.as_str()))
                    })
                    .collect();
                let committed = self
                    .commit_response(
                        &recovery_units,
                        policy,
                        options,
                        &receipt.request,
                        response,
                        CommitState {
                            memory,
                            completed: BTreeMap::new(),
                            issues,
                        },
                    )
                    .await?;
                memory = committed.memory;
                issues = committed.issues;
                completed.extend(committed.completed.into_iter().filter(|(id, _)| {
                    selected
                        .as_ref()
                        .is_none_or(|ids| ids.contains(id.as_str()))
                }));
                let compatible: HashSet<_> = recovery_units
                    .iter()
                    .map(|unit| memory_key(unit, policy))
                    .collect();
                if receipt
                    .request
                    .units
                    .iter()
                    .all(|unit| compatible.contains(&unit.memory_key))
                {
                    recovered_receipts.push(receipt_key.clone());
                }
            }
        }
        for key in recovered_receipts {
            journal.pending.remove(&key);
            if self
                .directory
                .join("responses")
                .join(format!("{key}.request.json"))
                .is_file()
            {
                journal.legacy_receipts.insert(key);
            }
        }
        write_json(&journal_path, &journal)?;
        let directory = self.directory.clone();
        let edit_store = tokio::task::spawn_blocking(move || EditStore::open(&directory))
            .await
            .map_err(|error| ExecutionError::Invalid(error.to_string()))??;
        edit_store.read(|edits| -> Result<(), ExecutionError> {
            for unit in &units {
                cancelled(cancellation)?;
                if options.candidate_only {
                    if let Some(candidate) = edits.candidate(unit, &policy.target_language) {
                        completed.insert(unit.id.clone(), candidate.text.clone());
                        cache_hits += 1;
                    }
                    continue;
                }
                let edit = edits.find(unit, &policy.target_language);
                if options.retranslation_id.is_some()
                    && edit.is_none_or(|edit| edit.retranslation_id != options.retranslation_id)
                {
                    continue;
                }
                let text = edit
                    .map(|edit| edit.text.as_str())
                    .or(memory.find(unit, policy)?);
                if let Some(text) = text
                    && (edit.is_some() || text::validate(unit, text).is_empty())
                {
                    completed.insert(unit.id.clone(), text.into());
                    cache_hits += 1;
                }
            }
            Ok(())
        })??;
        issues.retain(|issue| {
            issue
                .unit_id
                .as_ref()
                .is_none_or(|id| !completed.contains_key(id))
        });
        write_json(&self.directory.join("translation-errors.json"), &issues)?;
        let mut groups = Vec::<Vec<&TranslationUnit>>::new();
        let mut by_key = HashMap::<String, usize>::new();
        for unit in units
            .iter()
            .filter(|unit| !completed.contains_key(&unit.id))
        {
            let key = memory_key(unit, policy);
            let index = *by_key.entry(key).or_insert_with(|| {
                let index = groups.len();
                groups.push(Vec::new());
                index
            });
            groups[index].push(unit);
        }
        let uncertain: HashSet<_> = journal
            .pending
            .values()
            .filter(|receipt| {
                receipt.response.is_none()
                    && receipt.request.candidate_only == options.candidate_only
                    && receipt.request.retranslation_id == options.retranslation_id
            })
            .flat_map(|receipt| {
                receipt
                    .request
                    .units
                    .iter()
                    .map(|unit| unit.memory_key.clone())
            })
            .collect();
        if !options.retry_uncertain_requests {
            let mut blocked = Vec::new();
            groups.retain(|group| {
                if uncertain.contains(&memory_key(group[0], policy)) {
                    blocked.extend(group.iter().copied());
                    false
                } else {
                    true
                }
            });
            if !blocked.is_empty() {
                mark_failed(
                    &blocked,
                    policy,
                    "request-outcome-unknown",
                    "上次请求结果未知，未授权重新发送；请确认后重试。",
                    &mut issues,
                    &mut failed,
                );
                write_json(&self.directory.join("translation-errors.json"), &issues)?;
                if !continue_on_error {
                    return Err(ExecutionError::Uncertain);
                }
            }
        }
        emit(
            "查询缓存",
            format!("已加载 {cache_hits} 条历史译文。"),
            completed.len(),
            cache_hits,
            failed.len(),
            &usage,
            None,
        );
        let batch_size = providers[0].maximum_batch_size().min(options.batch_size);
        let mut batches = groups.chunks(batch_size);
        let mut fallback_batches = VecDeque::new();
        'batches: while let Some((start_provider, batch)) = fallback_batches
            .pop_front()
            .or_else(|| batches.next().map(|batch| (0, batch.to_vec())))
        {
            cancelled(cancellation)?;
            // Protect manual corrections saved during an ordinary background translation.
            let mut current = Vec::<Vec<&TranslationUnit>>::new();
            edit_store.read(|edits| {
                for group in &batch {
                    let mut remaining = Vec::new();
                    for unit in group {
                        if options.retranslation_id.is_none()
                            && !options.candidate_only
                            && let Some(edit) = edits.find(unit, &policy.target_language)
                        {
                            completed.insert(unit.id.clone(), edit.text.clone());
                            cache_hits += 1;
                            continue;
                        }
                        remaining.push(*unit);
                    }
                    if !remaining.is_empty() {
                        current.push(remaining);
                    }
                }
            })?;
            if current.is_empty() {
                continue;
            }
            let representatives: Vec<_> = current.iter().map(|group| group[0].clone()).collect();
            let feedback = representatives
                .iter()
                .map(|unit| {
                    (
                        unit.id.clone(),
                        issues
                            .iter()
                            .filter(|issue| {
                                issue.unit_id.as_deref() == Some(&unit.id)
                                    && issue
                                        .memory_key
                                        .as_ref()
                                        .is_none_or(|key| *key == memory_key(unit, policy))
                                    && !matches!(
                                        issue.code.as_str(),
                                        "provider-refusal"
                                            | "provider-request-failed"
                                            | "request-outcome-unknown"
                                    )
                            })
                            .map(|issue| issue.message.clone())
                            .collect::<Vec<_>>(),
                    )
                })
                .collect();
            let mut response = None;
            let mut batch_error = None;
            for (provider_index, provider) in providers.iter().enumerate().skip(start_provider) {
                cancelled(cancellation)?;
                if current.len() > provider.maximum_batch_size() {
                    // Continue the refused batch at this API; only it needs smaller requests.
                    // Commit each split response before moving on, using the usual receipt path.
                    for batch in current.chunks(provider.maximum_batch_size()).rev() {
                        fallback_batches.push_front((provider_index, batch.to_vec()));
                    }
                    continue 'batches;
                }
                let request_key =
                    request_key(provider.identity(), &representatives, policy, options);
                let legacy_refusal = self
                    .directory
                    .join("responses")
                    .join(format!("{request_key}.refusal.json"));
                if journal.refusals.contains_key(&request_key) || legacy_refusal.is_file() {
                    continue;
                }
                if let Some(saved) = journal
                    .pending
                    .get(&request_key)
                    .and_then(|receipt| receipt.response.clone())
                {
                    response = Some((saved, journal.pending[&request_key].request.clone()));
                    break;
                }
                let intent = TranslationIntent {
                    request_key: request_key.clone(),
                    provider: provider.identity().into(),
                    sent_at: now(),
                    units: representatives
                        .iter()
                        .map(|unit| PendingTranslation {
                            id: unit.id.clone(),
                            memory_key: memory_key(unit, policy),
                        })
                        .collect(),
                    candidate_only: options.candidate_only,
                    retranslation_id: options.retranslation_id.clone(),
                };
                journal.pending.insert(
                    request_key.clone(),
                    Receipt {
                        request: intent.clone(),
                        response: None,
                    },
                );
                write_json(&journal_path, &journal)?;
                emit(
                    "翻译",
                    format!(
                        "正在使用 {}（{}/{}）翻译 {} 条新文本。",
                        provider.display_name(),
                        provider_index + 1,
                        providers.len(),
                        representatives.len()
                    ),
                    completed.len(),
                    cache_hits,
                    failed.len(),
                    &usage,
                    None,
                );
                for attempt in 0..=5 {
                    match provider
                        .translate(&representatives, policy, &feedback, cancellation)
                        .await
                    {
                        Ok(value) => {
                            add_usage(&mut usage, &value.usage);
                            add_usage(&mut journal.usage, &value.usage);
                            // Persist the paid response before touching edits or memory shards.
                            journal.pending.insert(
                                request_key.clone(),
                                Receipt {
                                    request: intent.clone(),
                                    response: Some(value.clone()),
                                },
                            );
                            write_json(&journal_path, &journal)?;
                            emit(
                                "翻译",
                                format!(
                                    "{}（{}）已返回 {} 条译文，正在检查格式并保存。",
                                    provider.display_name(),
                                    provider.model(),
                                    value.translations.len()
                                ),
                                completed.len(),
                                cache_hits,
                                failed.len(),
                                &usage,
                                None,
                            );
                            response = Some((value, intent.clone()));
                            break;
                        }
                        Err(error) => {
                            if error.is_cancelled() {
                                return Err(ExecutionError::Cancelled);
                            }
                            add_usage(&mut usage, &error.usage);
                            add_usage(&mut journal.usage, &error.usage);
                            self.log_provider_error(&intent, attempt + 1, &error)?;
                            if error.is_refusal() {
                                journal.refusals.insert(request_key.clone(),json!({"request":intent,"recordedAt":now(),"diagnostic":error.diagnostic}));
                                journal.pending.remove(&request_key);
                                write_json(&journal_path, &journal)?;
                                emit(
                                    "切换翻译 API",
                                    format!(
                                        "第 {} 个翻译 API 拒绝处理当前内容，尝试链中的后续接口。",
                                        provider_index + 1
                                    ),
                                    completed.len(),
                                    cache_hits,
                                    failed.len(),
                                    &usage,
                                    None,
                                );
                                break;
                            }
                            if attempt == 5 {
                                if !error.outcome_unknown {
                                    journal.pending.remove(&request_key);
                                }
                                write_json(&journal_path, &journal)?;
                                batch_error =
                                    Some(ExecutionError::Retries(error.failure_summary()));
                                break;
                            }
                            let immediate = error.is_response_format_error();
                            let retry_timing = if immediate { "立即" } else { "10 秒后" };
                            emit(
                                if immediate {
                                    "重试翻译"
                                } else {
                                    "等待重试"
                                },
                                format!(
                                    "API 请求失败：{}。{retry_timing}进行第 {} / 5 次重试。",
                                    error.failure_summary(),
                                    attempt + 1
                                ),
                                completed.len(),
                                cache_hits,
                                failed.len(),
                                &usage,
                                None,
                            );
                            if !immediate {
                                tokio::select! {biased;_=cancellation.cancelled()=>return Err(ExecutionError::Cancelled),_=tokio::time::sleep(Duration::from_secs(10))=>{}}
                            }
                        }
                    }
                }
                if response.is_some() || batch_error.is_some() {
                    break;
                }
            }
            let Some((response, intent)) = response else {
                let error = batch_error.unwrap_or(ExecutionError::Refused);
                let reason = error.to_string();
                let code = if matches!(error, ExecutionError::Refused) {
                    "provider-refusal"
                } else {
                    "provider-request-failed"
                };
                let batch_units: Vec<_> = current.iter().flatten().copied().collect();
                mark_failed(
                    &batch_units,
                    policy,
                    code,
                    &reason,
                    &mut issues,
                    &mut failed,
                );
                write_json(&self.directory.join("translation-errors.json"), &issues)?;
                emit(
                    "翻译",
                    format!(
                        "本批 {} 条文本翻译失败，{}",
                        batch_units.len(),
                        if continue_on_error {
                            "继续后续文本。"
                        } else {
                            "已保存失败记录。"
                        }
                    ),
                    completed.len(),
                    cache_hits,
                    failed.len(),
                    &usage,
                    Some((&batch_units, &completed, &issues)),
                );
                if !continue_on_error {
                    return Err(error);
                }
                continue;
            };
            let batch_units: Vec<_> = current.iter().flatten().copied().collect();
            let committed = self
                .commit_response(
                    &batch_units,
                    policy,
                    options,
                    &intent,
                    &response,
                    CommitState {
                        memory,
                        completed,
                        issues,
                    },
                )
                .await?;
            memory = committed.memory;
            completed = committed.completed;
            issues = committed.issues;
            for group in &current {
                if completed.contains_key(&group[0].id) {
                    cache_hits += group.len() - 1;
                }
                for unit in group {
                    if !completed.contains_key(&unit.id) {
                        failed.insert(unit.id.clone());
                    } else {
                        failed.remove(&unit.id);
                    }
                }
            }
            journal.pending.remove(&intent.request_key);
            write_json(&journal_path, &journal)?;
            emit(
                "翻译",
                format!(
                    "已完成 {} / {} 条文本，失败 {} 条。",
                    completed.len(),
                    units.len(),
                    failed.len()
                ),
                completed.len(),
                cache_hits,
                failed.len(),
                &usage,
                Some((&batch_units, &completed, &issues)),
            );
            if !continue_on_error && batch_units.iter().any(|unit| failed.contains(&unit.id)) {
                return Err(ExecutionError::Invalid(
                    "部分译文未通过格式检查，有效译文已保存。继续时只重试失败文本。".into(),
                ));
            }
        }
        // Existing edits remain usable after a failed retranslation, but do not fulfil that request.
        if !options.candidate_only && options.retranslation_id.is_none() && !failed.is_empty() {
            let mut resolved = Vec::new();
            edit_store.read(|edits| {
                for unit in &units {
                    if failed.contains(&unit.id)
                        && let Some(edit) = edits.find(unit, &policy.target_language)
                        && text::validate(unit, &edit.text).is_empty()
                    {
                        completed.insert(unit.id.clone(), edit.text.clone());
                        failed.remove(&unit.id);
                        issues.retain(|issue| issue.unit_id.as_deref() != Some(&unit.id));
                        resolved.push(*unit);
                    }
                }
            })?;
            if !resolved.is_empty() {
                write_json(&self.directory.join("translation-errors.json"), &issues)?;
                emit(
                    "翻译",
                    format!("已保存 {} 条手工修正的译文。", resolved.len()),
                    completed.len(),
                    cache_hits,
                    failed.len(),
                    &usage,
                    Some((&resolved, &completed, &issues)),
                );
            }
        }
        tokio::task::spawn_blocking(move || edit_store.compact())
            .await
            .map_err(|error| ExecutionError::Invalid(error.to_string()))??;
        let result = TranslationRunResult {
            completed: completed.len(),
            total: units.len(),
            failed: failed.len(),
            translations: completed,
            cache_hits,
            usage,
        };
        emit(
            if result.failed > 0 {
                "翻译完成（有失败）"
            } else {
                "翻译完成"
            },
            format!(
                "本次翻译已保存：成功 {} 条，失败 {} 条。",
                result.completed, result.failed
            ),
            result.completed,
            result.cache_hits,
            result.failed,
            &result.usage,
            None,
        );
        Ok(result)
    }

    async fn commit_response(
        &self,
        units: &[&TranslationUnit],
        policy: &TranslationPolicy,
        options: &TranslationOptions,
        intent: &TranslationIntent,
        response: &TranslationResponse,
        mut state: CommitState,
    ) -> Result<CommitState, ExecutionError> {
        let engine = Self::new(self.directory.clone(), None);
        let units: Vec<_> = units.iter().map(|unit| (*unit).clone()).collect();
        let policy = policy.clone();
        let options = CommitOptions {
            candidate_only: options.candidate_only,
            retranslation_id: options.retranslation_id.clone(),
        };
        let intent = intent.clone();
        let response = response.clone();
        tokio::task::spawn_blocking(move || {
            let units: Vec<_> = units.iter().collect();
            engine.commit_response_locked(
                &units, &policy, &options, &intent, &response, &mut state,
            )?;
            Ok(state)
        })
        .await
        .map_err(|error| ExecutionError::Invalid(format!("保存翻译结果失败：{error}")))?
    }

    fn commit_response_locked(
        &self,
        units: &[&TranslationUnit],
        policy: &TranslationPolicy,
        options: &CommitOptions,
        intent: &TranslationIntent,
        response: &TranslationResponse,
        state: &mut CommitState,
    ) -> Result<(), ExecutionError> {
        let CommitState {
            memory,
            completed,
            issues,
        } = state;
        let translations: HashMap<_, _> = intent
            .units
            .iter()
            .filter_map(|unit| {
                response
                    .translations
                    .get(&unit.id)
                    .map(|text| (unit.memory_key.as_str(), text))
            })
            .collect();
        let mut entries = BTreeMap::new();
        let store = EditStore::open(&self.directory)?;
        store.update(|edits| -> Result<_, ExecutionError> {
            let mut changes = EditChanges::default();
            for unit in units {
                if options.retranslation_id.is_some()
                    && let Some(edit) = edits
                        .find(unit, &policy.target_language)
                        .filter(|edit| edit.retranslation_id == options.retranslation_id)
                {
                    completed.insert(unit.id.clone(), edit.text.clone());
                    continue;
                }
                let key = memory_key(unit, policy);
                let Some(text) = translations.get(key.as_str()) else {
                    continue;
                };
                let validation = text::validate(unit, text);
                issues.retain(|issue| issue.unit_id.as_deref() != Some(&unit.id));
                if !validation.is_empty() {
                    issues.extend(validation.into_iter().map(|issue| RecordedIssue {
                        code: issue.code,
                        message: issue.message,
                        unit_id: Some(unit.id.clone()),
                        memory_key: Some(key.clone()),
                    }));
                    continue;
                }
                if options.candidate_only {
                    let mut edit =
                        TranslationEdit::new(unit, &policy.target_language, (*text).clone(), false);
                    edit.is_generated = true;
                    changes.candidates.insert(unit.id.clone(), Some(edit));
                } else if options.retranslation_id.is_some() {
                    // Keep review state changed after an interrupted task committed this exact result.
                    if edits
                        .find(unit, &policy.target_language)
                        .is_none_or(|edit| {
                            edit.retranslation_id != options.retranslation_id || edit.text != **text
                        })
                    {
                        let mut edit = TranslationEdit::new(
                            unit,
                            &policy.target_language,
                            (*text).clone(),
                            false,
                        );
                        edit.retranslation_id = options.retranslation_id.clone();
                        edit.is_generated = true;
                        changes.entries.insert(unit.id.clone(), Some(edit));
                        changes.candidates.insert(unit.id.clone(), None);
                    }
                } else {
                    entries.insert(
                        key,
                        MemoryEntry {
                            key: None,
                            source_text: unit.source_text.clone(),
                            context: unit.context.clone(),
                            translation: (*text).clone(),
                            provider: intent.provider.clone(),
                            created_at: now(),
                        },
                    );
                }
                completed.insert(unit.id.clone(), (*text).clone());
            }
            Ok(((), changes))
        })?;
        memory.store_batch(entries.into_iter().collect())?;
        write_json(&self.directory.join("translation-errors.json"), issues)?;
        Ok(())
    }

    fn import_legacy_receipts(&self, journal: &mut ExecutionJournal) -> Result<(), ExecutionError> {
        let directory = self.directory.join("responses");
        if !directory.is_dir() {
            return Ok(());
        }
        for entry in fs::read_dir(directory)? {
            let path = entry?.path();
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            let Some(key) = name.strip_suffix(".request.json") else {
                continue;
            };
            if !crate::storage::valid_key(key)
                || journal.pending.contains_key(key)
                || journal.legacy_receipts.contains(key)
            {
                continue;
            }
            let intent: TranslationIntent = read_json(&path)?;
            if intent.request_key != key {
                return Err(ExecutionError::Invalid("旧版请求回执键不一致。".into()));
            }
            let response = read_recoverable(&path.with_file_name(format!("{key}.json")))?;
            if !path.with_file_name(format!("{key}.refusal.json")).is_file() {
                journal.pending.insert(
                    key.into(),
                    Receipt {
                        request: intent,
                        response,
                    },
                );
            }
        }
        Ok(())
    }

    fn log_provider_error(
        &self,
        intent: &TranslationIntent,
        attempt: usize,
        error: &ProviderError,
    ) -> Result<(), ExecutionError> {
        let directory = self.directory.join("logs");
        fs::create_dir_all(&directory)?;
        let mut output = OpenOptions::new()
            .create(true)
            .append(true)
            .open(directory.join("translation-provider-errors.jsonl"))?;
        let record = json!({"timestamp":now(),"requestKey":intent.request_key,"provider":intent.provider,"attempt":attempt,"message":error.message,"usage":error.usage,
            "outcomeUnknown":error.outcome_unknown,"statusCode":error.status_code,"diagnostic":error.diagnostic});
        serde_json::to_writer(&mut output, &record)
            .map_err(|error| ExecutionError::Invalid(error.to_string()))?;
        output.write_all(b"\n")?;
        output.flush()?;
        Ok(())
    }
}

fn mark_failed(
    units: &[&TranslationUnit],
    policy: &TranslationPolicy,
    code: &str,
    message: &str,
    issues: &mut Vec<RecordedIssue>,
    failed: &mut BTreeSet<String>,
) {
    let ids: HashSet<_> = units.iter().map(|unit| unit.id.as_str()).collect();
    issues.retain(|issue| issue.unit_id.as_deref().is_none_or(|id| !ids.contains(id)));
    for unit in units {
        failed.insert(unit.id.clone());
        issues.push(RecordedIssue {
            code: code.into(),
            message: message.into(),
            unit_id: Some(unit.id.clone()),
            memory_key: Some(memory_key(unit, policy)),
        });
    }
}

fn request_key(
    provider: &str,
    units: &[TranslationUnit],
    policy: &TranslationPolicy,
    options: &TranslationOptions,
) -> String {
    hash_text(
        &(legacy_json(
            &json!({"Provider":provider,"Units":units.iter().map(|unit|json!({"Id":unit.id,"Key":memory_key(unit,policy)})).collect::<Vec<_>>()}),
        ) + if options.candidate_only {
            "\ncandidate"
        } else {
            ""
        } + &options
            .retranslation_id
            .as_ref()
            .map(|id| format!("\nretranslate:{id}"))
            .unwrap_or_default()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{ApiProtocol, ProviderSettings, TranslationFormat};
    use crate::storage::TranslationEdits;
    fn unit(id: &str, source: &str) -> TranslationUnit {
        TranslationUnit {
            id: id.into(),
            resource_path: "story".into(),
            locator: id.into(),
            source_text: source.into(),
            context: "scene".into(),
            speaker: None,
            kind: "dialogue".into(),
            protected_tokens: None,
        }
    }
    fn provider() -> OpenAiProvider {
        OpenAiProvider::new(ProviderSettings {
            endpoint: "http://127.0.0.1:1/v1".into(),
            api_key: String::new(),
            model: "test".into(),
            protocol: ApiProtocol::ChatCompletions,
            translation_format: TranslationFormat::JsonBatch,
            analysis_reasoning_effort: None,
        })
        .unwrap()
    }
    fn mock_once(
        status: u16,
        body: Value,
    ) -> (
        String,
        std::sync::mpsc::Receiver<Value>,
        std::thread::JoinHandle<()>,
    ) {
        mock_sequence(vec![(status, body)])
    }
    fn mock_sequence(
        responses: Vec<(u16, Value)>,
    ) -> (
        String,
        std::sync::mpsc::Receiver<Value>,
        std::thread::JoinHandle<()>,
    ) {
        use std::io::Read;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (sender, receiver) = std::sync::mpsc::channel();
        let handle = std::thread::spawn(move || {
            for (status, body) in responses {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut request = Vec::new();
                let mut buffer = [0_u8; 4096];
                let mut expected = None;
                loop {
                    let size = stream.read(&mut buffer).unwrap();
                    assert!(size > 0);
                    request.extend_from_slice(&buffer[..size]);
                    if expected.is_none()
                        && let Some(start) =
                            request.windows(4).position(|bytes| bytes == b"\r\n\r\n")
                    {
                        let headers = String::from_utf8_lossy(&request[..start]);
                        let size = headers
                            .lines()
                            .find_map(|line| {
                                line.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|value| value.trim().parse::<usize>().unwrap())
                            })
                            .unwrap();
                        expected = Some((start + 4, size));
                    }
                    if let Some((start, size)) = expected
                        && request.len() >= start + size
                    {
                        sender
                            .send(serde_json::from_slice(&request[start..start + size]).unwrap())
                            .unwrap();
                        break;
                    }
                }
                let body = body.to_string();
                write!(stream,"HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nRetry-After: 0\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
            }
        });
        (format!("http://{address}/v1"), receiver, handle)
    }
    fn local_provider(endpoint: String) -> OpenAiProvider {
        local_provider_with_format(endpoint, TranslationFormat::JsonBatch)
    }
    fn local_provider_with_format(
        endpoint: String,
        translation_format: TranslationFormat,
    ) -> OpenAiProvider {
        OpenAiProvider::new(ProviderSettings {
            endpoint,
            api_key: String::new(),
            model: "test".into(),
            protocol: ApiProtocol::ChatCompletions,
            translation_format,
            analysis_reasoning_effort: Some("high".into()),
        })
        .unwrap()
    }

    #[tokio::test]
    async fn plain_text_fallback_does_not_limit_json_batch_size() {
        let reply = |text: Value| {
            (
                200,
                json!({"choices":[{"finish_reason":"stop","message":{"content":text.to_string()}}]}),
            )
        };
        let server = mock_sequence(vec![
            reply(json!({"0":"你好", "1":"再见"})),
            reply(json!({"0":"谢谢"})),
        ]);
        let providers = [
            local_provider(server.0),
            local_provider_with_format(
                "http://127.0.0.1:1/v1".into(),
                TranslationFormat::PlainText,
            ),
        ];
        let directory = tempfile::tempdir().unwrap();
        let units = [
            unit("hello", "Hello"),
            unit("bye", "Goodbye"),
            unit("thanks", "Thank you"),
            unit("duplicate", "Hello"),
        ];
        let cancellation = CancellationToken::new();
        let result = TranslationEngine::new(directory.path().into(), None)
            .run(
                &units,
                &providers,
                &TranslationPolicy::default(),
                &TranslationOptions {
                    batch_size: 2,
                    ..Default::default()
                },
                &cancellation,
                |update| {
                    if update.stage == "等待重试" {
                        cancellation.cancel();
                    }
                },
            )
            .await
            .unwrap();
        assert_eq!(
            (result.completed, result.failed, result.cache_hits),
            (4, 0, 1)
        );
        assert_eq!(result.translations["duplicate"], "你好");
        let requests: Vec<_> = server.1.try_iter().collect();
        let sizes: Vec<_> = requests
            .iter()
            .map(|request| {
                serde_json::from_str::<Value>(request["messages"][1]["content"].as_str().unwrap())
                    .unwrap()["units"]
                    .as_array()
                    .unwrap()
                    .len()
            })
            .collect();
        assert_eq!(sizes, [2, 1]);
        assert_eq!(providers[1].request_count(), 0);
        server.2.join().unwrap();
    }

    #[tokio::test]
    async fn refused_json_batch_splits_only_at_plain_text_fallback() {
        let first = mock_sequence(vec![
            (
                200,
                json!({"choices":[{"finish_reason":"stop","message":{"refusal":"Content is restricted"}}]}),
            ),
            (
                200,
                json!({"choices":[{"finish_reason":"stop","message":{"content":"{\"0\":\"谢谢\"}"}}]}),
            ),
        ]);
        let second = mock_sequence(vec![
            (
                200,
                json!({"choices":[{"finish_reason":"stop","message":{"content":"你好"}}]}),
            ),
            (
                200,
                json!({"choices":[{"finish_reason":"stop","message":{"refusal":"Content is restricted"}}]}),
            ),
        ]);
        let third = mock_once(
            200,
            json!({"choices":[{"finish_reason":"stop","message":{"content":"{\"0\":\"再见\"}"}}]}),
        );
        let providers = [
            local_provider(first.0),
            local_provider_with_format(second.0, TranslationFormat::PlainText),
            local_provider(third.0),
        ];
        let directory = tempfile::tempdir().unwrap();
        let units = [
            unit("hello", "Hello"),
            unit("bye", "Goodbye"),
            unit("thanks", "Thank you"),
            unit("duplicate", "Hello"),
        ];
        let cancellation = CancellationToken::new();
        let result = TranslationEngine::new(directory.path().into(), None)
            .run(
                &units,
                &providers,
                &TranslationPolicy::default(),
                &TranslationOptions {
                    batch_size: 2,
                    ..Default::default()
                },
                &cancellation,
                |update| {
                    if update.stage == "等待重试" {
                        cancellation.cancel();
                    }
                },
            )
            .await
            .unwrap();
        assert_eq!(
            (result.completed, result.failed, result.cache_hits),
            (4, 0, 1)
        );
        assert_eq!(result.translations["hello"], "你好");
        assert_eq!(result.translations["bye"], "再见");
        assert_eq!(result.translations["thanks"], "谢谢");
        assert_eq!(result.translations["duplicate"], "你好");
        let first_requests: Vec<_> = first.1.try_iter().collect();
        assert_eq!(first_requests.len(), 2);
        let first_batch: Value = serde_json::from_str(
            first_requests[0]["messages"][1]["content"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(first_batch["units"].as_array().unwrap().len(), 2);
        let plain_sources: Vec<_> = second
            .1
            .try_iter()
            .map(|request| {
                request["messages"][1]["content"]
                    .as_str()
                    .unwrap()
                    .to_owned()
            })
            .collect();
        assert_eq!(plain_sources, ["Hello", "Goodbye"]);
        assert_eq!(third.1.try_iter().count(), 1);
        first.2.join().unwrap();
        second.2.join().unwrap();
        third.2.join().unwrap();
    }

    #[tokio::test]
    async fn split_fallback_saves_completed_texts_before_cancellation() {
        let first = mock_once(
            200,
            json!({"choices":[{"finish_reason":"stop","message":{"refusal":"Content is restricted"}}]}),
        );
        let second = mock_once(
            200,
            json!({"choices":[{"finish_reason":"stop","message":{"content":"你好"}}]}),
        );
        let providers = [
            local_provider(first.0),
            local_provider_with_format(second.0, TranslationFormat::PlainText),
        ];
        let directory = tempfile::tempdir().unwrap();
        let engine = TranslationEngine::new(directory.path().into(), None);
        let units = [
            unit("hello", "Hello"),
            unit("bye", "Goodbye"),
            unit("duplicate", "Hello"),
        ];
        let cancellation = CancellationToken::new();
        let result = engine
            .run(
                &units,
                &providers,
                &TranslationPolicy::default(),
                &TranslationOptions::default(),
                &cancellation,
                |update| {
                    if update.completed == 2 || update.stage == "等待重试" {
                        cancellation.cancel();
                    }
                },
            )
            .await;
        assert!(matches!(result, Err(ExecutionError::Cancelled)));
        first.2.join().unwrap();
        second.2.join().unwrap();
        assert_eq!(providers[0].request_count(), 1);
        assert_eq!(providers[1].request_count(), 1);

        // A later run, including a changed API chain, sends only the uncommitted text.
        let retry = mock_once(
            200,
            json!({"choices":[{"finish_reason":"stop","message":{"content":"{\"0\":\"再见\"}"}}]}),
        );
        let result = engine
            .run(
                &units,
                &[local_provider(retry.0)],
                &TranslationPolicy::default(),
                &TranslationOptions::default(),
                &CancellationToken::new(),
                |_| {},
            )
            .await
            .unwrap();
        assert_eq!(
            (result.completed, result.failed, result.cache_hits),
            (3, 0, 2)
        );
        assert_eq!(result.translations["hello"], "你好");
        assert_eq!(result.translations["duplicate"], "你好");
        let request = retry.1.recv().unwrap();
        let data: Value =
            serde_json::from_str(request["messages"][1]["content"].as_str().unwrap()).unwrap();
        assert_eq!(data["units"].as_array().unwrap().len(), 1);
        assert_eq!(data["units"][0]["sourceText"], "Goodbye");
        retry.2.join().unwrap();
    }

    #[tokio::test]
    async fn explicit_api_refusal_uses_next_provider_and_accounts_cached_tokens() {
        let first = mock_once(
            200,
            json!({"id":"refused","choices":[{"finish_reason":"stop","message":{"refusal":"Content is restricted"}}],"usage":{"prompt_tokens":20,"completion_tokens":1,"prompt_tokens_details":{"cached_tokens":15}}}),
        );
        let second = mock_once(
            200,
            json!({"id":"translated","choices":[{"finish_reason":"stop","message":{"content":"{\"0\":\"你好\"}"}}],"usage":{"prompt_tokens":10,"completion_tokens":4,"prompt_tokens_details":{"cached_tokens":8}}}),
        );
        let directory = tempfile::tempdir().unwrap();
        let units = [unit("original-id", "hello")];
        let providers = [local_provider(first.0), local_provider(second.0)];
        let result = TranslationEngine::new(directory.path().into(), None)
            .run(
                &units,
                &providers,
                &TranslationPolicy::default(),
                &TranslationOptions::default(),
                &CancellationToken::new(),
                |_| {},
            )
            .await
            .unwrap();
        assert_eq!(result.translations["original-id"], "你好");
        assert_eq!(
            (
                result.usage.input_tokens,
                result.usage.output_tokens,
                result.usage.cached_tokens
            ),
            (30, 5, Some(23))
        );
        let request = first.1.recv().unwrap();
        assert!(request.get("reasoning_effort").is_none());
        let _ = second.1.recv().unwrap();
        first.2.join().unwrap();
        second.2.join().unwrap();
    }

    #[tokio::test]
    async fn full_and_selected_runs_keep_failed_texts_and_continue_later_batches() {
        let reply = |text: &str| json!({"choices":[{"finish_reason":"stop","message":{"content":json!({"0":text}).to_string()}}]});
        for mode in ["all", "selected", "retranslate", "trial"] {
            let directory = tempfile::tempdir().unwrap();
            let policy = TranslationPolicy::default();
            let mut units = vec![
                unit("refused", "Hello"),
                unit("invalid", "Hello [name]"),
                unit("success", "Goodbye"),
            ];
            let options = TranslationOptions {
                batch_size: 1,
                unit_ids: (mode != "all")
                    .then(|| units.iter().map(|unit| unit.id.clone()).collect()),
                retranslation_id: (mode == "retranslate").then(|| "e".repeat(32)),
                build_patch_after: mode == "trial",
                ..Default::default()
            };
            if options.retranslation_id.is_some() {
                let mut edits = TranslationEdits::default();
                for unit in &units {
                    edits.entries.insert(
                        unit.id.clone(),
                        TranslationEdit::new(
                            unit,
                            &policy.target_language,
                            unit.source_text.clone(),
                            true,
                        ),
                    );
                }
                edits.save(directory.path()).unwrap();
            }
            if options.unit_ids.is_some() {
                units.push(unit("outside-selection", "Unselected text"));
            }
            let server = mock_sequence(vec![
                (
                    200,
                    json!({"choices":[{"finish_reason":"stop","message":{"refusal":"Content is restricted"}}]}),
                ),
                (200, reply("你好")),
                (200, reply("再见")),
            ]);
            let engine = TranslationEngine::new(directory.path().into(), None);
            let mut progress = Vec::new();
            let result = engine
                .run(
                    &units,
                    &[local_provider(server.0)],
                    &policy,
                    &options,
                    &CancellationToken::new(),
                    |update| progress.push(update),
                )
                .await
                .unwrap();
            assert_eq!(
                (result.total, result.completed, result.failed),
                (3, 1, 2),
                "{mode}"
            );
            assert_eq!(
                result.translations,
                BTreeMap::from([("success".into(), "再见".into())]),
                "{mode}"
            );
            let failures = load_issues(directory.path());
            assert_eq!(
                failures
                    .iter()
                    .filter_map(|issue| issue.unit_id.as_deref())
                    .collect::<BTreeSet<_>>(),
                BTreeSet::from(["refused", "invalid"]),
                "{mode}"
            );
            assert!(
                failures
                    .iter()
                    .all(|issue| !issue.message.contains("任务已暂停")),
                "{mode}"
            );
            for id in ["refused", "invalid"] {
                assert!(
                    progress
                        .iter()
                        .any(|update| update.failure_updates.get(id).is_some_and(Option::is_some)),
                    "{mode}: {id}"
                );
            }
            assert_eq!(server.1.try_iter().count(), 3, "{mode}");
            server.2.join().unwrap();
            if options.retranslation_id.is_some() {
                let edits = TranslationEdits::load(directory.path()).unwrap();
                for unit in &units[..2] {
                    let saved = edits.find(unit, &policy.target_language).unwrap();
                    assert_eq!(saved.text, unit.source_text);
                    assert!(saved.reviewed);
                }
            }

            // A later retry sends only the failed texts; committed successes remain reusable.
            let retry = mock_sequence(vec![(200, reply("你好")), (200, reply("你好{v0_0}"))]);
            let result = engine
                .run(
                    &units,
                    &[local_provider(retry.0)],
                    &policy,
                    &options,
                    &CancellationToken::new(),
                    |_| {},
                )
                .await
                .unwrap();
            assert_eq!(
                (
                    result.total,
                    result.completed,
                    result.failed,
                    result.cache_hits
                ),
                (3, 3, 0, 1),
                "{mode}"
            );
            assert!(load_issues(directory.path()).is_empty(), "{mode}");
            assert!(!result.translations.contains_key("outside-selection"));
            assert_eq!(retry.1.try_iter().count(), 2, "{mode}");
            retry.2.join().unwrap();
        }
    }

    #[tokio::test]
    async fn response_format_errors_retry_immediately_on_the_same_provider() {
        let reply = |content: &str| json!({"choices":[{"finish_reason":"stop","message":{"content":content}}]});
        let server = mock_sequence(vec![
            (200, json!({"choices":[]})),
            (200, reply("invalid JSON")),
            (200, reply("{}")),
            (200, reply("{\"unknown\":\"你好\"}")),
            (200, reply("{\"0\":42}")),
            (200, reply("{\"0\":\"你好\"}")),
        ]);
        let directory = tempfile::tempdir().unwrap();
        let engine = TranslationEngine::new(directory.path().into(), None);
        let providers = [local_provider(server.0), provider()];
        let mut retries = Vec::new();
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            engine.run(
                &[unit("1", "hello")],
                &providers,
                &TranslationPolicy::default(),
                &TranslationOptions::default(),
                &CancellationToken::new(),
                |progress| {
                    if matches!(progress.stage.as_str(), "重试翻译" | "等待重试") {
                        retries.push(progress);
                    }
                },
            ),
        )
        .await
        .expect("format errors must retry without the 10-second delay")
        .unwrap();
        assert_eq!(result.translations["1"], "你好");
        assert_eq!(retries.len(), 5);
        for (index, retry) in retries.iter().enumerate() {
            assert_eq!(retry.stage, "重试翻译");
            assert!(
                retry
                    .message
                    .contains(&format!("立即进行第 {} / 5 次重试", index + 1))
            );
        }
        assert_eq!(providers[0].request_count(), 6);
        assert_eq!(providers[1].request_count(), 0);
        assert_eq!(server.1.try_iter().count(), 6);
        server.2.join().unwrap();
    }

    #[tokio::test]
    async fn operational_failure_cancels_retry_delay_without_falling_back() {
        let server = mock_once(503, json!({"error":"unavailable"}));
        let directory = tempfile::tempdir().unwrap();
        let cancellation = CancellationToken::new();
        let cancel = cancellation.clone();
        let result = TranslationEngine::new(directory.path().into(), None)
            .run(
                &[unit("1", "hello")],
                &[local_provider(server.0), provider()],
                &TranslationPolicy::default(),
                &TranslationOptions::default(),
                &cancellation,
                move |progress| {
                    if progress.stage == "等待重试" {
                        assert!(progress.message.contains("10 秒后"));
                        cancel.cancel()
                    }
                },
            )
            .await;
        assert!(matches!(result, Err(ExecutionError::Cancelled)));
        let _ = server.1.recv().unwrap();
        server.2.join().unwrap();
        let journal: ExecutionJournal =
            read_json(&directory.path().join("translation-execution.json")).unwrap();
        assert_eq!(journal.pending.len(), 1);
        assert!(journal.refusals.is_empty());
    }
    #[tokio::test]
    async fn plain_text_mode_sends_only_source_and_keeps_refusal_like_dialogue() {
        let server = mock_once(
            200,
            json!({"choices":[{"finish_reason":"stop","message":{"content":"抱歉，我无法帮助你。"}}],"usage":{"prompt_tokens":5,"completion_tokens":5}}),
        );
        let provider = OpenAiProvider::new(ProviderSettings {
            endpoint: server.0,
            api_key: String::new(),
            model: "local-translator".into(),
            protocol: ApiProtocol::ChatCompletions,
            translation_format: TranslationFormat::PlainText,
            analysis_reasoning_effort: Some("high".into()),
        })
        .unwrap();
        let directory = tempfile::tempdir().unwrap();
        let units = [unit("dialogue-id", "I cannot help you.")];
        let result = TranslationEngine::new(directory.path().into(), None)
            .run(
                &units,
                &[provider],
                &TranslationPolicy::default(),
                &TranslationOptions::default(),
                &CancellationToken::new(),
                |_| {},
            )
            .await
            .unwrap();
        assert_eq!(result.translations["dialogue-id"], "抱歉，我无法帮助你。");
        let request = server.1.recv().unwrap();
        assert_eq!(request["messages"][1]["content"], "I cannot help you.");
        assert_eq!(request["max_tokens"], 4096);
        assert!(request.get("reasoning_effort").is_none());
        server.2.join().unwrap();
    }
    #[tokio::test]
    async fn responses_mode_restores_markers_and_reads_native_usage_fields() {
        let server = mock_once(
            200,
            json!({"id":"response-id","status":"completed","output":[{"type":"message","role":"assistant","status":"completed","content":[{"type":"output_text","text":"{\"0\":\"你好{v0_0}\"}"}]}],"usage":{"input_tokens":7,"output_tokens":2,"input_tokens_details":{"cached_tokens":5}}}),
        );
        let provider = OpenAiProvider::new(ProviderSettings {
            endpoint: server.0,
            api_key: String::new(),
            model: "responses-model".into(),
            protocol: ApiProtocol::Responses,
            translation_format: TranslationFormat::JsonBatch,
            analysis_reasoning_effort: None,
        })
        .unwrap();
        let directory = tempfile::tempdir().unwrap();
        let result = TranslationEngine::new(directory.path().into(), None)
            .run(
                &[unit("original-id", "Hello [name]")],
                &[provider],
                &TranslationPolicy::default(),
                &TranslationOptions::default(),
                &CancellationToken::new(),
                |_| {},
            )
            .await
            .unwrap();
        assert_eq!(result.translations["original-id"], "你好[name]");
        assert_eq!(
            (
                result.usage.input_tokens,
                result.usage.output_tokens,
                result.usage.cached_tokens
            ),
            (7, 2, Some(5))
        );
        let request = server.1.recv().unwrap();
        assert_eq!(request["store"], false);
        assert_eq!(request["max_output_tokens"], 4096);
        assert!(request.get("messages").is_none());
        server.2.join().unwrap();
    }
    #[tokio::test]
    async fn tool_requests_retry_rate_limits_three_times_and_stop() {
        let server = mock_sequence(vec![(429, json!({})), (429, json!({})), (429, json!({}))]);
        let provider = local_provider(server.0);
        let error=provider.complete(&[json!({"role":"user","content":"inspect"})],Some(&[json!({"name":"read","description":"read","parameters":{"type":"object","properties":{}}})]),32,&CancellationToken::new()).await.unwrap_err();
        assert_eq!(error.status_code, Some(429));
        for _ in 0..3 {
            let request = server.1.recv().unwrap();
            assert_eq!(request["tools"][0]["function"]["strict"], false);
            assert_eq!(request["reasoning_effort"], "high");
        }
        server.2.join().unwrap();
    }
    #[tokio::test]
    async fn stateless_responses_tools_keep_encrypted_reasoning_for_resume() {
        let server = mock_once(
            200,
            json!({"status":"completed","output":[{"type":"reasoning","id":"r","encrypted_content":"opaque"},{"type":"function_call","call_id":"call-a","name":"read","arguments":"{}"}],"usage":{"input_tokens":3,"output_tokens":2}}),
        );
        let provider = OpenAiProvider::new(ProviderSettings {
            endpoint: server.0,
            api_key: String::new(),
            model: "responses-model".into(),
            protocol: ApiProtocol::Responses,
            translation_format: TranslationFormat::JsonBatch,
            analysis_reasoning_effort: Some("high".into()),
        })
        .unwrap();
        let completion=provider.complete(&[json!({"role":"user","content":"inspect"})],Some(&[json!({"type":"function","function":{"name":"read","description":"read","parameters":{"type":"object","properties":{}}}})]),32,&CancellationToken::new()).await.unwrap();
        assert_eq!(completion.tool_calls[0].id, "call-a");
        assert_eq!(completion.output_items[0]["encrypted_content"], "opaque");
        let request = server.1.recv().unwrap();
        assert_eq!(request["include"][0], "reasoning.encrypted_content");
        assert_eq!(request["tools"][0]["strict"], false);
        assert_eq!(request["store"], false);
        server.2.join().unwrap();
    }
    #[tokio::test]
    async fn recovers_paid_legacy_response_before_replanning_batches() {
        let directory = tempfile::tempdir().unwrap();
        let units = vec![unit("old-id", "hello"), unit("duplicate-id", "hello")];
        let policy = TranslationPolicy::default();
        let provider = provider();
        let intent = TranslationIntent {
            request_key: "a".repeat(64),
            provider: provider.identity().into(),
            sent_at: now(),
            units: vec![PendingTranslation {
                id: "old-id".into(),
                memory_key: memory_key(&units[0], &policy),
            }],
            candidate_only: false,
            retranslation_id: None,
        };
        let response = TranslationResponse {
            translations: BTreeMap::from([("old-id".into(), "你好".into())]),
            usage: TokenUsage {
                input_tokens: 12,
                output_tokens: 3,
                estimated: false,
                cached_tokens: Some(8),
            },
            provider_request_id: Some("paid-request".into()),
        };
        write_json(
            &directory
                .path()
                .join("responses")
                .join(format!("{}.request.json", intent.request_key)),
            &intent,
        )
        .unwrap();
        write_json(
            &directory
                .path()
                .join("responses")
                .join(format!("{}.json", intent.request_key)),
            &response,
        )
        .unwrap();
        let result = TranslationEngine::new(directory.path().into(), None)
            .run(
                &units,
                &[provider],
                &policy,
                &TranslationOptions {
                    batch_size: 1,
                    ..Default::default()
                },
                &CancellationToken::new(),
                |_| {},
            )
            .await
            .unwrap();
        assert_eq!(
            (
                result.completed,
                result.cache_hits,
                result.usage.input_tokens
            ),
            (2, 2, 0)
        );
        assert_eq!(result.translations["duplicate-id"], "你好");
    }
    #[tokio::test]
    async fn uncertain_request_is_not_resent_when_batch_size_changes() {
        let directory = tempfile::tempdir().unwrap();
        let units = vec![unit("1", "hello"), unit("2", "bye")];
        let policy = TranslationPolicy::default();
        let provider = provider();
        let intent = TranslationIntent {
            request_key: "b".repeat(64),
            provider: provider.identity().into(),
            sent_at: now(),
            units: units
                .iter()
                .map(|unit| PendingTranslation {
                    id: unit.id.clone(),
                    memory_key: memory_key(unit, &policy),
                })
                .collect(),
            candidate_only: false,
            retranslation_id: None,
        };
        let journal = ExecutionJournal {
            pending: BTreeMap::from([(
                intent.request_key.clone(),
                Receipt {
                    request: intent,
                    response: None,
                },
            )]),
            ..Default::default()
        };
        write_json(
            &directory.path().join("translation-execution.json"),
            &journal,
        )
        .unwrap();
        let result = TranslationEngine::new(directory.path().into(), None)
            .run(
                &units,
                &[provider],
                &policy,
                &TranslationOptions {
                    batch_size: 1,
                    retry_uncertain_requests: false,
                    ..Default::default()
                },
                &CancellationToken::new(),
                |_| {},
            )
            .await
            .unwrap();
        assert_eq!((result.completed, result.failed), (0, 2));
    }
    #[tokio::test]
    async fn paid_retranslation_recovers_duplicate_selected_ids_without_resending() {
        let directory = tempfile::tempdir().unwrap();
        let units = [unit("1", "hello"), unit("2", "hello"), unit("3", "hello")];
        let policy = TranslationPolicy::default();
        let options = TranslationOptions {
            unit_ids: Some(vec!["1".into(), "2".into()]),
            retranslation_id: Some("a".repeat(32)),
            ..Default::default()
        };
        let intent = TranslationIntent {
            request_key: "b".repeat(64),
            provider: "test".into(),
            sent_at: now(),
            units: vec![PendingTranslation {
                id: "1".into(),
                memory_key: memory_key(&units[0], &policy),
            }],
            candidate_only: false,
            retranslation_id: options.retranslation_id.clone(),
        };
        let journal = ExecutionJournal {
            pending: BTreeMap::from([(
                intent.request_key.clone(),
                Receipt {
                    request: intent,
                    response: Some(TranslationResponse {
                        translations: BTreeMap::from([("1".into(), "你好".into())]),
                        usage: TokenUsage::default(),
                        provider_request_id: Some("paid-request".into()),
                    }),
                },
            )]),
            ..Default::default()
        };
        write_json(
            &directory.path().join("translation-execution.json"),
            &journal,
        )
        .unwrap();
        let cancellation = CancellationToken::new();
        let stop_new_request = cancellation.clone();
        let result = TranslationEngine::new(directory.path().into(), None)
            .run(
                &units,
                &[provider()],
                &policy,
                &options,
                &cancellation,
                move |progress| {
                    if progress.stage == "翻译" {
                        stop_new_request.cancel();
                    }
                },
            )
            .await
            .expect("the saved paid result must complete the selection without another API call");
        assert_eq!((result.completed, result.cache_hits), (2, 2));
        let edits = TranslationEdits::load(directory.path()).unwrap();
        assert_eq!(
            edits.find(&units[1], &policy.target_language).unwrap().text,
            "你好"
        );
        assert!(edits.find(&units[2], &policy.target_language).is_none());
    }
    #[tokio::test]
    async fn retranslation_updates_selected_ids_without_changing_shared_draft() {
        let directory = tempfile::tempdir().unwrap();
        let units = [unit("1", "hello"), unit("2", "hello")];
        let policy = TranslationPolicy::default();
        let mut memory = TranslationMemory::load(directory.path().join("memory"), None).unwrap();
        let key = memory_key(&units[0], &policy);
        memory
            .store_batch(vec![(
                key.clone(),
                MemoryEntry {
                    key: None,
                    source_text: "hello".into(),
                    context: "scene".into(),
                    translation: "旧译文".into(),
                    provider: "old".into(),
                    created_at: now(),
                },
            )])
            .unwrap();
        let options = TranslationOptions {
            unit_ids: Some(vec!["1".into()]),
            retranslation_id: Some("c".repeat(32)),
            ..Default::default()
        };
        let intent = TranslationIntent {
            request_key: "d".repeat(64),
            provider: "new".into(),
            sent_at: now(),
            units: vec![PendingTranslation {
                id: "1".into(),
                memory_key: key,
            }],
            candidate_only: false,
            retranslation_id: options.retranslation_id.clone(),
        };
        let response = TranslationResponse {
            translations: BTreeMap::from([("1".into(), "新译文".into())]),
            usage: TokenUsage::default(),
            provider_request_id: None,
        };
        let committed = TranslationEngine::new(directory.path().into(), None)
            .commit_response(
                &[&units[0]],
                &policy,
                &options,
                &intent,
                &response,
                CommitState {
                    memory,
                    completed: BTreeMap::new(),
                    issues: Vec::new(),
                },
            )
            .await
            .unwrap();
        let edits = TranslationEdits::load(directory.path()).unwrap();
        assert_eq!(
            edits.find(&units[0], &policy.target_language).unwrap().text,
            "新译文"
        );
        assert!(edits.find(&units[1], &policy.target_language).is_none());
        assert_eq!(
            committed.memory.find(&units[1], &policy).unwrap(),
            Some("旧译文")
        );
    }
}
