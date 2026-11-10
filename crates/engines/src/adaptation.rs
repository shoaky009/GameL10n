//! Persisted, budgeted tool conversations. Published adapters run without a model.

use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet},
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::Arc,
    time::Instant,
};

use gamel10n_translation::{
    TokenUsage, TranslationUnit,
    files::{hash_file, hash_text, read_json, write_json},
    provider::{OpenAiProvider, ToolCall},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[path = "adaptation_automatic.rs"]
mod automatic;
#[path = "adaptation_components.rs"]
mod components;
#[path = "adaptation_decision.rs"]
mod decision;
#[path = "adaptation_failure.rs"]
mod failure;
#[path = "adaptation_flow.rs"]
mod flow;
#[path = "adaptation_history.rs"]
mod history;
#[path = "adaptation_reuse.rs"]
mod published_reuse;
#[path = "adaptation_resources.rs"]
mod resources;
#[path = "adaptation_session.rs"]
mod session_store;
#[path = "adaptation_verification.rs"]
mod verification;

pub use failure::{AdaptationFailure, AdaptationFailureCategory};
pub use resources::AdaptationInputs;

use crate::{
    AdapterHandle, CancellationToken, EngineError, EngineProgress, EngineService, PackageManifest,
    ProgressCallback, VerificationResult, VerifiedExtraction,
    discovery::{Inventory, resolve_relative, validate_pattern},
    protocol::AdapterRequest,
    service::hashes,
};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdaptationLimits {
    pub maximum_turns: u32,
    pub maximum_tokens: u64,
    pub maximum_output_tokens: u32,
}
impl Default for AdaptationLimits {
    fn default() -> Self {
        Self {
            maximum_turns: 60,
            maximum_tokens: 1_000_000,
            maximum_output_tokens: 10_000,
        }
    }
}
impl AdaptationLimits {
    pub(crate) fn validate(&self) -> Result<(), EngineError> {
        if !(1..=400).contains(&self.maximum_turns)
            || !(1000..=20_000_000).contains(&self.maximum_tokens)
            || !(128..=32_000).contains(&self.maximum_output_tokens)
        {
            return Err(EngineError::Protocol("智能适配预算无效。".into()));
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct AdaptationRequest<'a> {
    pub game_id: String,
    pub game_directory: PathBuf,
    pub inputs: AdaptationInputs,
    pub project_directory: PathBuf,
    /// Shared extension root; packages are written into its packages/ directory.
    pub package_directory: PathBuf,
    pub package_directories: Vec<PathBuf>,
    pub user_instruction: String,
    pub objective: String,
    pub improve_existing: bool,
    /// Requires the model to assess the game; local parser success cannot finish the task early.
    pub require_model_analysis: bool,
    pub reuse_failures: Vec<crate::GeneratedAdapterReuseFailure>,
    pub builtin_probes: Vec<crate::BuiltinAdapterProbeObservation>,
    /// Inventory from a completed regular discovery pass in this task. Avoids probing twice.
    pub discovery_inventory: Option<Inventory>,
    pub limits: AdaptationLimits,
    pub baseline_units: &'a [TranslationUnit],
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub enum AdaptationStatus {
    Activated,
    Reused,
    #[default]
    Failed,
    Unavailable,
    BudgetExhausted,
    Cancelled,
    Incomplete,
    WaitingForUser,
    Unsupported,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdaptationResult {
    pub status: AdaptationStatus,
    pub adapter: Option<AdapterHandle>,
    pub usage: TokenUsage,
    #[serde(default)]
    pub api_requests: Option<u64>,
    #[serde(default)]
    pub api_requests_estimated: bool,
    pub summary: String,
    pub next_action: String,
    pub diagnostics: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<AdaptationFailure>,
    pub pending_coverage: Vec<String>,
    pub pending_runtime_checks: Vec<String>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub progress_note: Value,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub last_breakthrough: Value,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub blocker: Value,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub verification: Value,
    #[serde(skip)]
    pub verified_extraction: Option<Arc<VerifiedExtraction>>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ReleaseFile {
    pub path: String,
    pub role: String,
    pub purpose: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct Session {
    pub game_id: String,
    pub game_directory: PathBuf,
    pub objective: String,
    pub user_instruction: String,
    #[serde(default, skip_serializing)]
    pub messages: Vec<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history_storage: Option<session_store::HistoryStorage>,
    #[serde(skip)]
    history_offsets: Vec<u64>,
    #[serde(skip)]
    history_file: PathBuf,
    pub usage: TokenUsage,
    pub total_turns: u32,
    pub summary: String,
    pub next_action: String,
    pub last_status: AdaptationStatus,
    pub notes: String,
    pub checkpoint: String,
    pub history_start: usize,
    pub pending_calls: Vec<ToolCall>,
    pub running_tool_id: Option<String>,
    pub release_files: Vec<ReleaseFile>,
    pub resource_patterns: Vec<String>,
    pub recognition: Option<crate::discovery::Recognition>,
    pub family: Value,
    pub text_coverage: Value,
    pub progress_note: Value,
    pub decision: decision::DecisionState,
    pub verified_package: Option<Value>,
    pub verified_fingerprint: Option<String>,
    pub verification: Option<VerificationResult>,
    #[serde(skip)]
    reported_knowledge: BTreeSet<(String, String)>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl Session {
    fn api_requests(&self) -> u64 {
        self.extra
            .get("apiRequests")
            .and_then(Value::as_u64)
            .unwrap_or(u64::from(self.total_turns))
    }

    fn api_requests_estimated(&self) -> bool {
        self.extra
            .get("apiRequestsEstimated")
            .and_then(Value::as_bool)
            .unwrap_or(!self.extra.contains_key("apiRequests") && self.total_turns > 0)
    }

    fn record_api_requests(&mut self, count: u64) {
        let total = self.api_requests().saturating_add(count);
        let estimated = self.api_requests_estimated();
        self.extra.insert("apiRequests".into(), json!(total));
        self.extra
            .insert("apiRequestsEstimated".into(), json!(estimated));
    }

    fn progress(&self, mut event: EngineProgress) -> EngineProgress {
        event.usage = self.usage.clone();
        event.turn = self.total_turns;
        event.api_requests = Some(self.api_requests());
        event.api_requests_estimated = self.api_requests_estimated();
        event.progress_note = self.progress_note.clone();
        event.last_breakthrough = self
            .extra
            .get("lastBreakthrough")
            .cloned()
            .unwrap_or_default();
        event
    }

    fn update_progress(
        &mut self,
        kind: &str,
        message: &str,
        tool: Option<&str>,
        callback: ProgressCallback<'_>,
    ) -> bool {
        if self.progress_note["kind"] == kind && self.progress_note["message"] == message {
            return false;
        }
        self.progress_note =
            json!({"kind":kind,"message":message,"updatedAt":chrono::Utc::now().to_rfc3339()});
        if matches!(kind, "breakthrough" | "verified") {
            self.extra
                .insert("lastBreakthrough".into(), self.progress_note.clone());
        } else if kind == "correction" {
            self.extra.remove("lastBreakthrough");
        }
        if let Some(callback) = callback {
            callback(
                self.progress(EngineProgress {
                    stage: match kind {
                        "exploring" => "仍在探索",
                        "breakthrough" => "关键突破",
                        "implementing" => "方案已确定",
                        "correction" => "进展更正",
                        "verified" => "资源验证进展",
                        _ => "模型说明",
                    }
                    .into(),
                    message: message.into(),
                    tool_name: tool.map(str::to_owned),
                    ..EngineProgress::default()
                }),
            );
        }
        true
    }
}

/// Reads numeric C# enums, nullable optional fields and complete provider output without rewriting history.
pub(crate) fn load_session(path: &Path) -> Result<Session, EngineError> {
    let mut value: Value = serde_json::from_reader(std::io::BufReader::new(fs::File::open(path)?))?;
    let object = value
        .as_object_mut()
        .ok_or_else(|| EngineError::Protocol("适配会话必须是 JSON 对象。".into()))?;
    if let Some(index) = object.get("objective").and_then(Value::as_u64) {
        let name = match index {
            0 => "Develop",
            1 => "Repair",
            2 => "ExpandCoverage",
            _ => return Err(EngineError::Protocol("适配会话目标无效。".into())),
        };
        object.insert("objective".into(), json!(name));
    }
    if let Some(index) = object.get("lastStatus").and_then(Value::as_u64) {
        let name = match index {
            0 => "Activated",
            1 => "Reused",
            2 => "Failed",
            3 => "Unavailable",
            4 => "BudgetExhausted",
            5 => "Cancelled",
            6 => "Incomplete",
            7 => "WaitingForUser",
            8 => "Unsupported",
            _ => return Err(EngineError::Protocol("适配会话状态无效。".into())),
        };
        object.insert("lastStatus".into(), json!(name));
    }
    for field in [
        "objective",
        "lastStatus",
        "checkpoint",
        "releaseFiles",
        "resourcePatterns",
        "notes",
        "userInstruction",
        "summary",
        "nextAction",
        "pendingCalls",
        "messages",
        "historyStart",
        "totalTurns",
    ] {
        if object.get(field).is_some_and(Value::is_null) {
            object.remove(field);
        }
    }
    let mut session: Session = serde_json::from_value(value)?;
    session_store::restore(path, &mut session)?;
    session.history_start = session.history_start.min(session.messages.len());
    if !session.extra.contains_key("protocolRead") {
        let calls: BTreeSet<_> = session
            .messages
            .iter()
            .flat_map(history::calls)
            .filter(|(_, name)| name == "adapter_protocol")
            .map(|(id, _)| id)
            .collect();
        let read = session.messages.iter().any(|message| {
            message
                .get("tool_call_id")
                .or_else(|| message.get("toolCallId"))
                .or_else(|| message.get("call_id"))
                .and_then(Value::as_str)
                .is_some_and(|id| calls.contains(id))
                && message
                    .get("content")
                    .or_else(|| message.get("output"))
                    .and_then(Value::as_str)
                    .and_then(|content| serde_json::from_str::<Value>(content).ok())
                    .is_some_and(|result| {
                        result["success"] != false && result["protocol"].as_str().is_some()
                    })
        });
        if read {
            session.extra.insert("protocolRead".into(), json!(true));
        }
    }
    Ok(session)
}

pub(crate) fn visible_json(value: &Value) -> Value {
    match value {
        Value::Object(object) => Value::Object(
            object
                .iter()
                .filter(|(key, _)| key.as_str() != "encrypted_content")
                .map(|(key, value)| (key.clone(), visible_json(value)))
                .collect(),
        ),
        Value::Array(values) => Value::Array(values.iter().map(visible_json).collect()),
        _ => value.clone(),
    }
}

fn visible_json_bytes(value: &Value) -> usize {
    match value {
        Value::Object(object) => object
            .iter()
            .filter(|(key, _)| key.as_str() != "encrypted_content")
            .map(|(key, value)| key.len() + visible_json_bytes(value) + 4)
            .sum(),
        Value::Array(values) => values.iter().map(visible_json_bytes).sum(),
        Value::String(value) => value.len() + 2,
        _ => 16,
    }
}

pub(crate) fn add_usage(total: &mut TokenUsage, usage: &TokenUsage) {
    total.input_tokens = total.input_tokens.saturating_add(usage.input_tokens);
    total.output_tokens = total.output_tokens.saturating_add(usage.output_tokens);
    total.estimated |= usage.estimated;
    if usage.cached_tokens.is_some() {
        total.cached_tokens = Some(
            total
                .cached_tokens
                .unwrap_or(0)
                .saturating_add(usage.cached_tokens.unwrap_or(0)),
        );
    }
}
pub(crate) fn tokens(usage: &TokenUsage) -> u64 {
    usage.input_tokens.saturating_add(usage.output_tokens)
}
impl EngineService {
    pub async fn adapt(
        &self,
        request: &AdaptationRequest<'_>,
        provider: &OpenAiProvider,
        cancellation: &CancellationToken,
        progress: ProgressCallback<'_>,
    ) -> Result<AdaptationResult, EngineError> {
        request.limits.validate()?;
        if request.user_instruction.chars().count() > 6000
            || request.game_id.is_empty()
            || !request.game_directory.is_dir()
        {
            return Err(EngineError::Protocol(
                "游戏目录、项目身份或补充要求无效。".into(),
            ));
        }
        let workspace = request.project_directory.join("adaptation/current");
        fs::create_dir_all(&workspace)?;
        for name in ["resources", "work"] {
            fs::create_dir_all(workspace.join(name))?;
        }
        let path = workspace.join("session.json");
        let continuing = path.is_file();
        let mut diagnostics = Vec::new();
        let mut state: Session = if continuing {
            // Read errors must not be mistaken for corrupt JSON and overwrite a valid session.
            match load_session(&path) {
                Ok(state) => state,
                Err(error @ EngineError::Io(_)) => return Err(error),
                Err(EngineError::Json(error)) if error.is_io() => {
                    return Err(EngineError::Json(error));
                }
                Err(error) => {
                    let external_history =
                        path.with_file_name("session-history.jsonl").try_exists()?
                            || read_json::<Value>(&path).ok().is_some_and(|value| {
                                value
                                    .get("historyStorage")
                                    .is_some_and(|storage| !storage.is_null())
                            });
                    if external_history {
                        return Err(EngineError::Protocol(format!(
                            "适配会话无法安全恢复，已有源码与历史记录已保留：{error}"
                        )));
                    }
                    diagnostics.push(format!(
                        "之前的会话状态无法读取，继续使用现有源码和工作文件：{error}"
                    ));
                    Session {
                        game_id: request.game_id.clone(),
                        game_directory: request.game_directory.clone(),
                        objective: request.objective.clone(),
                        summary:
                            "之前的会话状态无法读取，现有源码和工作文件仍可使用，请先检查这些文件。"
                                .into(),
                        ..Session::default()
                    }
                }
            }
        } else {
            Session {
                game_id: request.game_id.clone(),
                game_directory: request.game_directory.clone(),
                objective: request.objective.clone(),
                ..Session::default()
            }
        };
        if state.game_id != request.game_id
            || !same_directory(&state.game_directory, &request.game_directory)
        {
            return Err(EngineError::Rejected(
                "适配记录属于另一个游戏，已有记录已保留。".into(),
            ));
        }
        normalize_messages(&mut state.messages);
        flow::recover(&mut state, request, provider.identity());
        state.extra.remove("resourceChanges");
        if !request.reuse_failures.is_empty() {
            state.extra.insert(
                "reuseFailures".into(),
                serde_json::to_value(&request.reuse_failures)?,
            );
        }
        if !request.builtin_probes.is_empty() {
            state.extra.insert(
                "builtinProbes".into(),
                serde_json::to_value(
                    request
                        .builtin_probes
                        .iter()
                        .rev()
                        .take(6)
                        .collect::<Vec<_>>(),
                )?,
            );
        }
        let mut inventory = match &request.discovery_inventory {
            Some(inventory) => Cow::Borrowed(inventory),
            None => Cow::Owned(Inventory::scan(&request.game_directory)?),
        };
        if !request.inputs.files.is_empty() || !request.inputs.excluded.is_empty() {
            inventory = Cow::Owned(request.inputs.inventory(inventory.into_owned())?);
        }
        let run_id = hash_text(&format!(
            "{}:{}:{}",
            request.game_id,
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default(),
            std::process::id()
        ))[..32]
            .to_string();
        let trace_error = std::sync::Mutex::new(None);
        let trace_run = |mut entry: Value| {
            entry["runId"] = json!(run_id);
            if let Err(error) = trace(request, entry)
                && let Ok(mut saved) = trace_error.lock()
                && saved.is_none()
            {
                *saved = Some(format!("适配耗时日志未能写入：{error}"));
            }
            Ok(())
        };
        trace_run(
            json!({"operation":"run-start","continuing":continuing,"model":provider.model(),"modelIdentity":provider.identity(),"maximumTurns":request.limits.maximum_turns,"maximumTokens":request.limits.maximum_tokens}),
        )?;
        if !continuing
            && !request.require_model_analysis
            && request.discovery_inventory.is_none()
            && state.objective.eq_ignore_ascii_case("develop")
            && !request.improve_existing
            && request.user_instruction.trim().is_empty()
            && request.inputs.files.is_empty()
            && request.inputs.excluded.is_empty()
        {
            let discovery = self
                .discover_with_failures(
                    &request.game_directory,
                    &request.package_directories,
                    &request.reuse_failures,
                    cancellation,
                    progress,
                )
                .await;
            match discovery {
                Ok(discovered) => {
                    state.extra.insert(
                        "reuseFailures".into(),
                        serde_json::to_value(&discovered.reuse_failures)?,
                    );
                    state.extra.insert(
                        "builtinProbes".into(),
                        serde_json::to_value(&discovered.builtin_probes)?,
                    );
                    diagnostics.extend(discovered.diagnostics);
                    if let Some(adapter) = discovered.adapter {
                        let mut completed = result(
                            &mut state,
                            AdaptationStatus::Reused,
                            "已找到可用适配器，可以选择文本试译。",
                            diagnostics,
                        );
                        completed.adapter = Some(adapter);
                        save_session(&path, &mut state).await?;
                        trace_run(
                            json!({"operation":"run-end","status":completed.status,"usage":state.usage,"totalTurns":state.total_turns}),
                        )?;
                        return Ok(completed);
                    }
                }
                Err(EngineError::Cancelled) => {
                    let completed = result(
                        &mut state,
                        AdaptationStatus::Cancelled,
                        "任务已取消，现有进度已保留。",
                        diagnostics,
                    );
                    save_session(&path, &mut state).await?;
                    trace_run(
                        json!({"operation":"run-end","status":completed.status,"usage":state.usage,"totalTurns":state.total_turns}),
                    )?;
                    return Ok(completed);
                }
                Err(error) => diagnostics.push(error.to_string()),
            }
        }
        let preparation_started = Instant::now();
        let prepared = async {
            let changes = resources::reconcile(request, &workspace, &mut state, cancellation, progress)?;
            if !changes.is_empty() {
                Self::report(progress, "更新验证资源", format!("已同步制作输入，更新 {} 个资源副本，移除 {} 个过期记录；已有适配源码保留。", changes.rebound.len(), changes.removed.len()));
                trace_run(json!({"operation":"resources-refreshed","changes":changes}))?;
                state.extra.insert("resourceChanges".into(), json!(changes));
            }
            state.extra.insert("inputBinding".into(), json!({"boundFileCount":request.inputs.files.len(),"boundFiles":request.inputs.files.keys().take(40).collect::<Vec<_>>(),"excludedFileCount":request.inputs.excluded.len(),"excludedFiles":request.inputs.excluded.iter().take(20).collect::<Vec<_>>(),"baselineUnits":request.baseline_units.len()}));
            crate::adaptation_context::ensure_builtin_discovery(
                &request.game_directory,
                &mut state,
                cancellation,
            )?;
            if let Some(progress) = progress {
                progress(state.progress(EngineProgress {
                    stage: "检查游戏启动环境".into(),
                    message: "正在读取转区配置、启动脚本和已有错误日志。".into(),
                    ..EngineProgress::default()
                }));
            }
            let launch = self
                .helper(
                    &json!({"operation":"inspect_game_launch","root":request.game_directory}),
                    cancellation,
                )
                .await?;
            if state.extra.get("launchInspection") != Some(&launch) {
                state.verified_package = None;
                state.verified_fingerprint = None;
            }
            state.extra.insert("launchInspection".into(), launch);
            if continuing {
                automatic::run_trials(
                    self,
                    request,
                    &workspace,
                    &mut state,
                    &path,
                    cancellation,
                    progress,
                )
                .await?;
                Ok::<_, EngineError>(None)
            } else {
                automatic::run(
                    self,
                    request,
                    &workspace,
                    &mut state,
                    &path,
                    cancellation,
                    progress,
                )
                .await
            }
        }
        .await;
        trace_run(
            json!({"operation":"local-preparation","elapsedMs":preparation_started.elapsed().as_millis(),"automaticTrials":state.extra.get("automaticTrials"),"automaticTemplate":state.extra.get("automaticTemplate")}),
        )?;
        match prepared {
            Ok(Some(adapter)) => {
                let summary = state.summary.clone();
                let mut completed = result(
                    &mut state,
                    AdaptationStatus::Activated,
                    &summary,
                    diagnostics,
                );
                completed.adapter = Some(adapter);
                save_session(&path, &mut state).await?;
                trace_run(
                    json!({"operation":"run-end","status":completed.status,"usage":state.usage,"totalTurns":state.total_turns}),
                )?;
                return Ok(completed);
            }
            Ok(None) => {}
            Err(error) => {
                let completed = failed_result(
                    &mut state,
                    &error,
                    "智能适配准备未完成，现有源码和进度已保留。",
                    diagnostics,
                );
                save_session(&path, &mut state).await?;
                trace_run(
                    json!({"operation":"run-end","status":completed.status,"usage":state.usage,"totalTurns":state.total_turns}),
                )?;
                return Ok(completed);
            }
        };
        let continuation = flow::continuation(
            &state,
            &workspace,
            &inventory,
            request.baseline_units.len(),
            &self.disabled_families,
        )?;
        save_session(&path, &mut state).await?;
        let run = flow::run(
            self,
            request,
            provider,
            &workspace,
            &path,
            &mut state,
            continuation,
            cancellation,
            progress,
            &trace_run,
        )
        .await;
        let mut completed = match run {
            Ok((status, summary, adapter)) => {
                let mut completed = result(&mut state, status, &summary, diagnostics);
                completed.adapter = adapter;
                completed
            }
            Err(error) => failed_result(
                &mut state,
                &error,
                "智能适配尚未完成，当前源码、调查记录和最近失败已保存。",
                diagnostics,
            ),
        };
        if let Ok(mut error) = trace_error.lock()
            && let Some(error) = error.take()
        {
            completed.diagnostics.push(error);
        }
        save_session(&path, &mut state).await?;
        trace_run(
            json!({"operation":"run-end","status":completed.status,"usage":state.usage,"totalTurns":state.total_turns,"failure":completed.failure}),
        )?;
        Ok(completed)
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "Tool execution keeps request, persisted state and execution controls explicit at the host boundary."
    )]
    async fn adaptation_tool(
        &self,
        request: &AdaptationRequest<'_>,
        workspace: &Path,
        state: &mut Session,
        name: &str,
        args: &Value,
        cancellation: &CancellationToken,
        progress: ProgressCallback<'_>,
    ) -> Result<Value, EngineError> {
        if !args.is_object() || serde_json::to_vec(args)?.len() > 512 * 1024 {
            return Err(EngineError::Protocol(
                "工具参数必须为不超过 512 KiB 的对象。".into(),
            ));
        }
        if name != "report_blocker" {
            state.extra.remove("unresolvedStopReports");
        }
        match name {
            "load_tool_groups" => {
                if args
                    .get("mode")
                    .and_then(Value::as_str)
                    .is_some_and(|mode| !matches!(mode, "add" | "replace"))
                {
                    return Err(EngineError::Protocol("mode 必须为 add 或 replace。".into()));
                }
                let groups = strings(args, "groups")?;
                let mut selected = if args.get("mode").and_then(Value::as_str) == Some("replace") {
                    Vec::new()
                } else {
                    state
                        .extra
                        .get("enabledToolGroups")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default()
                };
                for group in groups {
                    if ![
                        "reuse",
                        "development",
                        "research",
                        "native",
                        "unity",
                        "siglus",
                        "all",
                    ]
                    .contains(&group.as_str())
                    {
                        return Err(EngineError::Protocol(format!("未知工具组：{group}")));
                    }
                    let value = json!(group);
                    if !selected.contains(&value) {
                        selected.push(value);
                    }
                }
                state
                    .extra
                    .insert("enabledToolGroups".into(), json!(selected));
                Ok(json!({"groups":selected,"success":true}))
            }
            "report_progress" => {
                let kind = text(args, "kind")?;
                if !matches!(
                    kind,
                    "exploring" | "breakthrough" | "implementing" | "correction"
                ) {
                    return Err(EngineError::Protocol(
                        "进展类型必须是 exploring、breakthrough、implementing 或 correction。"
                            .into(),
                    ));
                }
                let message = text(args, "message")?.trim();
                if message.is_empty() || message.chars().count() > 400 {
                    return Err(EngineError::Protocol(
                        "进度说明必须为 1 到 400 字符。".into(),
                    ));
                }
                decision::update(state, args)?;
                let changed = state.update_progress(kind, message, Some(name), progress);
                Ok(
                    json!({"changed":changed,"note":state.progress_note,"problem":state.decision.current_problem,"validation":false}),
                )
            }
            "list_files" | "read_file" | "search_text" => {
                let mut params = args.clone();
                params["operation"] = json!(name);
                params["root"] = json!(tool_root(request, workspace, args)?);
                params["workspaceRoot"] = json!(args["root"] == "workspace");
                let inputs = args["root"] == "game";
                params["inputFiles"] = if inputs && name == "read_file" {
                    let path = text(args, "path")?;
                    json!({path:request.inputs.file(&request.game_directory, path)?})
                } else if inputs {
                    json!(request.inputs.files)
                } else {
                    json!({})
                };
                params["excludedFiles"] = if inputs {
                    json!(request.inputs.excluded)
                } else {
                    json!([])
                };
                self.helper(&params, cancellation).await
            }
            "find_game_file" => {
                let requested = text(args, "path")?.replace('\\', "/");
                let exact = resolve_relative(&request.game_directory, &requested)?;
                let query = requested.rsplit('/').next().unwrap_or("").to_lowercase();
                let inventory = Inventory::scan(&request.game_directory)?;
                let matches: Vec<_> = inventory
                    .files
                    .iter()
                    .filter(|file| {
                        file.path.rsplit('/').next().unwrap_or("").to_lowercase() == query
                    })
                    .take(21)
                    .map(|file| file.path.clone())
                    .collect();
                Ok(
                    json!({"requested":requested,"exactPathExists":exact.is_file(),"matches":matches.iter().take(20).collect::<Vec<_>>(),"partial":!inventory.complete||matches.len()>20,"note":"文件存在不代表游戏进程能够按相同路径访问；还需核对工作目录、转区环境与脚本引用的原始字节。"}),
                )
            }
            "inspect_existing_text" => {
                let offset = number(args, "offset", 0, 0, 1_000_000)? as usize;
                let limit = number(args, "limit", 10, 1, 20)? as usize;
                let resource = args
                    .get("resourcePath")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .replace('\\', "/");
                let units: Vec<_> = request
                    .baseline_units
                    .iter()
                    .filter(|unit| {
                        resource.is_empty()
                            || unit
                                .resource_path
                                .replace('\\', "/")
                                .eq_ignore_ascii_case(&resource)
                    })
                    .collect();
                Ok(
                    json!({"available":true,"units":units.iter().skip(offset).take(limit).collect::<Vec<_>>(),"total":units.len(),"offset":offset,"nextOffset":(offset+limit).min(units.len()),"more":offset+limit<units.len(),"resources":request.baseline_units.iter().map(|unit|&unit.resource_path).collect::<BTreeSet<_>>().into_iter().take(40).collect::<Vec<_>>()}),
                )
            }
            "inspect_game_launch" => {
                self.helper(
                    &json!({"operation":"inspect_game_launch","root":request.game_directory}),
                    cancellation,
                )
                .await
            }
            "inspect_pe_imports" | "inspect_pe_code" | "inspect_unity_schema" => {
                let mut payload = args.clone();
                payload["path"] = json!(tool_file(request, workspace, args, text(args, "path")?)?);
                payload["query"] = json!(optional_text(args, "query", "")?);
                if name == "inspect_pe_imports" {
                    payload["offset"] = json!(number(args, "offset", 0, 0, 8192)?);
                    payload["limit"] = json!(number(args, "limit", 20, 1, u64::MAX)?.min(40));
                } else if name == "inspect_pe_code" {
                    let ranges = args
                        .get("ranges")
                        .and_then(Value::as_array)
                        .filter(|ranges| !ranges.is_empty() && ranges.len() <= 8)
                        .ok_or_else(|| {
                            EngineError::Protocol("ranges 必须包含 1–8 个代码范围。".into())
                        })?;
                    let mut total = 0;
                    let mut normalized = Vec::new();
                    for range in ranges {
                        let address = text(range, "address")?.trim();
                        let parsed = if address.to_ascii_lowercase().starts_with("0x") {
                            u64::from_str_radix(&address[2..], 16)
                        } else {
                            address.parse::<u64>()
                        };
                        if parsed.is_err() {
                            return Err(EngineError::Protocol(
                                "address 必须是 0x 开头的十六进制或十进制地址字符串。".into(),
                            ));
                        }
                        let length = number(range, "length", 256, 1, 4096)?;
                        total += length;
                        normalized.push(json!({"address":address,"length":length}));
                    }
                    if total > 16384 {
                        return Err(EngineError::Protocol(
                            "代码范围合计不得超过 16384 字节。".into(),
                        ));
                    }
                    payload["ranges"] = json!(normalized);
                    payload["addressKind"] = json!(optional_text(args, "addressKind", "va")?);
                    payload["instructionLimit"] =
                        json!(number(args, "instructionLimit", 80, 1, 200)?);
                } else {
                    payload["offset"] = json!(number(args, "offset", 0, 0, 1_000_000)?);
                    payload["limit"] = json!(number(args, "limit", 4, 1, 8)?);
                    payload["mode"] = json!(optional_text(args, "mode", "types")?);
                    payload["queryKind"] = json!(optional_text(args, "queryKind", "literal")?);
                }
                payload["operation"] = json!(match name {
                    "inspect_pe_imports" => "pe_imports",
                    "inspect_pe_code" => "pe_code",
                    _ =>
                        if args.get("mode").and_then(Value::as_str) == Some("references") {
                            "unity_references"
                        } else {
                            "unity_schema"
                        },
                });
                if let Some(assembly) = args.get("assemblyPath").and_then(Value::as_str) {
                    payload["assemblyPath"] = json!(tool_file(request, workspace, args, assembly)?);
                }
                let mut result = self.inspect_native(&payload, cancellation).await?;
                if name == "inspect_pe_imports" {
                    crate::adaptation_context::annotate_font_imports(&mut result);
                }
                Ok(result)
            }
            "adapter_protocol" => {
                state.extra.insert("protocolRead".into(), json!(true));
                Ok(json!({"protocol":crate::protocol::ADAPTER_PROTOCOL}))
            }
            "list_adapters" => self.adapter_list(request, state, args, cancellation),
            "inspect_builtin_adapter" => {
                let format = text(args, "format")?;
                if format.eq_ignore_ascii_case("Siglus") || format == "engine.siglus" {
                    if let Some(path) = args.get("path").and_then(Value::as_str) {
                        let prepared = self.siglus(cancellation).await?;
                        return self
                            .read_siglus_source(&prepared, path, args, cancellation)
                            .await;
                    }
                    return Ok(crate::adaptation_assets::siglus_description());
                }
                crate::adaptation_assets::inspect_builtin(self, request, state, args, cancellation)
                    .await
            }
            "try_builtin_adapter" => {
                self.try_builtin_sample(workspace, state, args, cancellation)
                    .await
            }
            "inspect_siglus_game" => {
                number(args, "offset", 0, 0, i32::MAX as u64)?;
                number(args, "length", 6000, 1, 12000)?;
                for key in ["source", "query"] {
                    if optional_text(args, key, "")?.encode_utf16().count() > 256 {
                        return Err(EngineError::Protocol("源码名称或查询词过长。".into()));
                    }
                }
                let mut probe = AdapterRequest::new("inspect", &request.game_directory);
                for key in ["source", "query", "offset", "length"] {
                    if let Some(value) = args.get(key) {
                        probe.extra.insert(key.into(), value.clone());
                    }
                }
                if !request.inputs.files.is_empty() || !request.inputs.excluded.is_empty() {
                    let inventory = request
                        .inputs
                        .inventory(Inventory::scan(&request.game_directory)?)?;
                    probe.resource_files =
                        Some(inventory.files.into_iter().map(|file| file.path).collect());
                    probe
                        .extra
                        .insert("inspectionInputs".into(), json!(request.inputs.files));
                }
                let response = self
                    .execute(&AdapterHandle::BuiltinSiglus, &probe, cancellation)
                    .await?;
                response
                    .inspection
                    .ok_or_else(|| EngineError::Protocol("Siglus 未返回检查结果。".into()))
            }
            "stage_resources" => {
                let files = strings(args, "files")?;
                resources::stage(
                    request,
                    workspace,
                    state,
                    &files,
                    boolean(args, "replace", false)?,
                    cancellation,
                    progress,
                )
            }
            "write_file" => {
                let normalized = text(args, "path")?.replace('\\', "/");
                let relative = normalized.as_str();
                let path = write_path(workspace, relative)?;
                let content = text(args, "content")?;
                if content.len() > 512 * 1024 {
                    return Err(EngineError::Rejected(
                        "单次源码写入过大，请拆分实现。".into(),
                    ));
                }
                let release_files = written_selection(state, relative, args)?;
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent)?;
                }
                let changed = fs::read(&path).ok().as_deref() != Some(content.as_bytes());
                if changed {
                    write_source_file(&path, content.as_bytes())?;
                }
                state.release_files = release_files;
                if changed {
                    state.verified_fingerprint = None;
                    if relative.starts_with("src/") {
                        state
                            .extra
                            .insert("lastImplementationTurn".into(), json!(state.total_turns));
                    }
                }
                Ok(json!({"path":args["path"],"bytes":content.len(),"changed":changed}))
            }
            "replace_text" => {
                let relative = text(args, "path")?.replace('\\', "/");
                let path = write_path(workspace, &relative)?;
                if path.metadata()?.len() > 512 * 1024 {
                    return Err(EngineError::Protocol(
                        "局部编辑文件不得超过 512 KiB。".into(),
                    ));
                }
                let original = fs::read_to_string(&path)?;
                let old = text(args, "oldText")?;
                if old.is_empty() || original.matches(old).count() != 1 {
                    return Err(EngineError::Rejected("oldText 必须精确且唯一匹配。".into()));
                }
                let updated = original.replacen(old, text(args, "newText")?, 1);
                if updated.len() > 512 * 1024 {
                    return Err(EngineError::Protocol(
                        "局部编辑结果不得超过 512 KiB。".into(),
                    ));
                }
                let changed = updated != original;
                if changed {
                    write_source_file(&path, updated.as_bytes())?;
                    state.verified_fingerprint = None;
                    if relative.starts_with("src/") {
                        state
                            .extra
                            .insert("lastImplementationTurn".into(), json!(state.total_turns));
                    }
                }
                Ok(json!({"path":args["path"],"bytes":updated.len(),"changed":changed}))
            }
            "remove_file" => {
                let normalized = text(args, "path")?.replace('\\', "/");
                let relative = normalized.as_str();
                let path = write_path(workspace, relative)?;
                if path.exists() {
                    fs::remove_file(path)?;
                }
                state
                    .release_files
                    .retain(|file| !workspace_file(&file.path).eq_ignore_ascii_case(relative));
                state.verified_fingerprint = None;
                Ok(json!({"removed":args["path"]}))
            }
            "update_release_files" => {
                let mut next = state.release_files.clone();
                if let Some(files) = args.get("files") {
                    let files: Vec<ReleaseFile> = serde_json::from_value(files.clone())?;
                    for file in files {
                        if !resolve_relative(workspace, &workspace_file(&file.path))?.is_file() {
                            return Err(EngineError::Rejected(format!(
                                "发布文件不存在：{}",
                                file.path
                            )));
                        }
                        select_release(&mut next, file)?;
                    }
                }
                if let Some(remove) = args.get("remove") {
                    let remove: Vec<String> = serde_json::from_value(remove.clone())?;
                    for path in &remove {
                        resolve_relative(workspace, path)?;
                    }
                    next.retain(|file| {
                        !remove
                            .iter()
                            .any(|path| path.eq_ignore_ascii_case(&file.path))
                    });
                }
                let inventory = Inventory::scan(&workspace.join("src"))?;
                if next.iter().any(|file| file.path.starts_with("tools/"))
                    && workspace.join("src/tools/sources.json").is_file()
                {
                    select_release(
                        &mut next,
                        ReleaseFile {
                            path: "tools/sources.json".into(),
                            role: "license".into(),
                            purpose: "下载依赖的来源与校验记录".into(),
                        },
                    )?;
                }
                let unregistered: Vec<_> = inventory
                    .files
                    .iter()
                    .map(|file| {
                        (
                            if file.path.starts_with("tools/") {
                                file.path.clone()
                            } else {
                                format!("src/{}", file.path)
                            },
                            file.length,
                        )
                    })
                    .filter(|(path, _)| {
                        !next.iter().any(|file| file.path.eq_ignore_ascii_case(path))
                    })
                    .collect();
                state.release_files = next;
                Ok(
                    json!({"releaseFiles":state.release_files,"unregisteredCount":unregistered.len(),"unregisteredBytes":unregistered.iter().map(|(_,bytes)|bytes).sum::<u64>(),"unregistered":unregistered.iter().take(30).map(|(path,bytes)|json!({"path":path,"bytes":bytes})).collect::<Vec<_>>(),"nextAction":"未登记文件不会发布；只补充确实需要的依赖。分析脚本与样本保留在工作区即可，不必登记或删除。"}),
                )
            }
            "inspect_adapter_source" => published_reuse::inspect(request, args),
            "copy_adapter" => {
                self.copy_adapter(request, workspace, state, args, cancellation)
                    .await
            }
            "inspect_adapter_component" => self.inspect_component(args, cancellation).await,
            "copy_adapter_components" => self.copy_components(workspace, state, args),
            "search_engine_references" | "inspect_engine_reference" => {
                let mut params = args.clone();
                params["operation"] = json!(name);
                let bundled_siglus = name == "search_engine_references"
                    && text(args, "query").is_ok_and(|query| {
                        matches!(
                            query
                                .trim()
                                .chars()
                                .filter(|character| character.is_alphanumeric())
                                .flat_map(char::to_lowercase)
                                .collect::<String>()
                                .as_str(),
                            "siglus" | "siglusengine" | "scenepck"
                        )
                    });
                if bundled_siglus {
                    return self.helper(&params, cancellation).await;
                }
                let reference = self.adaptation_reference(workspace, state);
                let library = reference
                    .as_ref()
                    .map_err(|error| EngineError::Rejected(error.to_string()))
                    .and_then(|reference| self.reference_library(reference));
                match library {
                    Ok(library) => {
                        params["library"] = json!(library);
                        params["revision"] = reference?["revision"].clone();
                    }
                    Err(error) if name == "inspect_engine_reference" => return Err(error),
                    Err(_) => {
                        let url = "https://raw.githubusercontent.com/GalTransl/GalTransl-GalGameReverse-Skill/main/catalog/engines.json";
                        match self.fetch_reference(url, false, cancellation).await {
                            Ok(document) => {
                                params["referenceIndexContent"] =
                                    json!(String::from_utf8_lossy(&document.content));
                                params["referenceIndexMetadata"] = json!({"url":url,"contentSha256":document.sha256,"retrievedAt":document.retrieved_at,"reused":document.reused,"stale":document.stale});
                            }
                            Err(EngineError::Cancelled) => return Err(EngineError::Cancelled),
                            Err(error) => {
                                params["referenceIndexError"] = json!(error.to_string());
                            }
                        }
                    }
                }
                self.helper(&params, cancellation).await
            }
            "use_engine_reference" => {
                self.bind_reference(workspace, state, None, cancellation)
                    .await
            }
            "run_engine_python" => {
                if let Some(id) = args.get("questionId") {
                    decision::validate_question_id(id.as_str().ok_or_else(|| {
                        EngineError::Protocol("questionId 必须是字符串。".into())
                    })?)?;
                }
                let mut params = args.clone();
                params["operation"] = json!("run");
                params["workspace"] = json!(workspace);
                if params.get("questionId").is_none()
                    && let Some(problem) = &state.decision.current_problem
                {
                    params["questionId"] = json!(problem.id);
                }
                params["toolPaths"] = self
                    .tool_paths(Some(&workspace.join("src")), cancellation)
                    .await?;
                let reference = if let Ok(pin) =
                    read_json::<Value>(&workspace.join("src/tools/engine-reference.json"))
                {
                    pin["reference"].clone()
                } else {
                    self.adaptation_reference(workspace, state)?
                };
                params["library"] = if reference["kind"].as_str() == Some("siglus") {
                    self.siglus(cancellation).await?["library"].clone()
                } else {
                    json!(self.reference_library(&reference)?)
                };
                state
                    .extra
                    .insert("engineReference".into(), reference.clone());
                let mut result = self.helper(&params, cancellation).await?;
                result["revision"] = reference["revision"].clone();
                Ok(result)
            }
            "run_tool" => {
                let mut params = args.clone();
                params["operation"] = json!("run_tool");
                params["workspace"] = json!(workspace);
                self.helper(&params, cancellation).await
            }
            "inspect_engine_component" => describe_engine_components(args),
            "run_engine_component" => {
                self.run_component(workspace, state, args, cancellation)
                    .await
            }
            "compose_adapter_template" => {
                self.compose_template(workspace, state, args, cancellation)
                    .await
            }
            "inspect_engine_knowledge" => {
                if args.get("sourceOffset").is_some()
                    && args["id"].as_str().is_none_or(str::is_empty)
                {
                    return Err(EngineError::Protocol(
                        "读取来源证据需要指定经验 id。".into(),
                    ));
                }
                let candidates = if ["id", "query"].iter().any(|key| {
                    args[key]
                        .as_str()
                        .is_some_and(|value| !value.trim().is_empty())
                }) {
                    None
                } else {
                    Some(knowledge_candidates(request, state)?)
                };
                let value = crate::review::read_knowledge(
                    &request.package_directory.join("engine-knowledge"),
                    args,
                    candidates.as_deref(),
                )?;
                if let Some(id) = args
                    .get("id")
                    .and_then(Value::as_str)
                    .filter(|id| !id.is_empty())
                {
                    report_knowledge(state, &value, id, None, progress);
                    let sources = value["origins"]
                        .as_array()
                        .map(Vec::as_slice)
                        .unwrap_or(&[]);
                    if args.get("sourceOffset").is_some() {
                        let offset = number(args, "sourceOffset", 0, 0, 100000)? as usize;
                        let page = crate::review::knowledge_page(sources.iter().skip(offset).map(
                            |origin| json!({"title":origin["title"],"evidence":origin["evidence"]}),
                        ));
                        let next_offset = offset + page.len();
                        return Ok(
                            json!({"id":id,"revision":value["revision"],"sourceGames":page,"sourceGameCount":sources.len(),"nextSourceOffset":next_offset,"more":next_offset<sources.len()}),
                        );
                    }
                    let mut family = value["family"].clone();
                    if family["adapterId"].is_null() {
                        family["adapterId"] = json!(format!(
                            "{}.{}",
                            text(&family, "kind")?,
                            text(&family, "key")?
                        ));
                    }
                    return Ok(
                        json!({"id":id,"revision":value["revision"],"knowledgeName":family["name"],"family":family,"conditions":value["conditions"],"summary":value["summary"],"guidance":value["guidance"],"reuseOptions":self.knowledge_reuse_options(request,&value)?,"engineCandidateMatches":crate::adaptation_context::knowledge_match_score(&value,&knowledge_candidates(request,state)?)>0,"tools":value["tools"].as_array().into_iter().flatten().map(|tool|json!({"name":tool["name"],"description":tool["description"],"usage":tool["usage"],"files":tool["files"].as_object().map(|files|files.keys().collect::<Vec<_>>())})).collect::<Vec<_>>(),"sourceGameCount":sources.len(),"limitation":"先根据当前资源核对适用条件。来源游戏的结论不能代替本游戏的提取、回填、字形或运行验证；需要来源证据时，使用相同 id 和 sourceOffset=0 分页读取。"}),
                    );
                }
                Ok(value)
            }
            "copy_engine_knowledge_tools" => {
                let entry = crate::review::read_knowledge(
                    &request.package_directory.join("engine-knowledge"),
                    &json!({"id":text(args,"id")?}),
                    None,
                )?;
                let name = text(args, "tool")?;
                let tool = entry["tools"]
                    .as_array()
                    .and_then(|tools| {
                        tools
                            .iter()
                            .find(|tool| tool["name"].as_str() == Some(name))
                    })
                    .ok_or_else(|| EngineError::Rejected("所选引擎经验没有该工具。".into()))?;
                let files = tool["files"]
                    .as_object()
                    .ok_or_else(|| EngineError::Protocol("经验工具源码无效。".into()))?;
                let id = text(args, "id")?;
                let revision = text(&entry, "revision")?;
                let copied = copy_knowledge_sources(workspace, id, revision, files)?;
                report_knowledge(state, &entry, text(args, "id")?, Some(name), progress);
                Ok(
                    json!({"id":id,"revision":revision,"knowledgeName":entry["family"]["name"],"knowledgeTool":name,"files":copied,"usage":tool["usage"],"next":format!("{}工具源码不代表适配器已验收；回填仍须 test_adapter。",if files.keys().any(|path|path.ends_with(".cs")){"历史 C# 辅助源码仅供参考，请将需要的分析逻辑改为 work/*.py 后用 run_engine_python 运行。"}else{"核对资源条件和输入路径后，用 run_engine_python 在当前样本上运行。"})}),
                )
            }
            "search_repositories" => {
                let query = text(args, "query")?;
                if query.len() > 512 {
                    return Err(EngineError::Protocol("检索条件过长。".into()));
                }
                let mut url = reqwest::Url::parse("https://api.github.com/search/repositories")
                    .map_err(file_error)?;
                url.query_pairs_mut()
                    .append_pair("per_page", "8")
                    .append_pair("q", query);
                let document = self
                    .fetch_reference(
                        url.as_str(),
                        args.get("refresh")
                            .and_then(Value::as_bool)
                            .unwrap_or(false),
                        cancellation,
                    )
                    .await?;
                let data: Value = serde_json::from_slice(&document.content)?;
                let items = data["items"].as_array().cloned().unwrap_or_default();
                Ok(
                    json!({"retrievedAt":document.retrieved_at,"reused":document.reused,"stale":document.stale,"repositories":items.iter().map(|item|json!({"name":item["full_name"],"url":item["html_url"],"description":item["description"],"license":item["license"]["spdx_id"]})).collect::<Vec<_>>()}),
                )
            }
            "fetch_url" => self.fetch_url(args, cancellation).await,
            "download_tool" => {
                if cancellation.is_cancelled() {
                    return Err(EngineError::Cancelled);
                }
                let normalized = text(args, "path")?.replace('\\', "/");
                let relative = normalized.as_str();
                if !relative.starts_with("src/tools/") && !relative.starts_with("work/") {
                    return Err(EngineError::Protocol(
                        "下载只能写入 src/tools/ 或 work/。".into(),
                    ));
                }
                let path = write_path(workspace, relative)?;
                let mut release_files = written_selection(state, relative, args)?;
                let url = text(args, "url")?;
                crate::research::validate_url(&reqwest::Url::parse(url).map_err(file_error)?)?;
                let refresh = boolean(args, "refresh", false)?;
                if relative.starts_with("src/tools/")
                    && release_files
                        .iter()
                        .any(|file| file.path.starts_with("tools/"))
                {
                    select_release(
                        &mut release_files,
                        ReleaseFile {
                            path: "tools/sources.json".into(),
                            role: "license".into(),
                            purpose: "下载依赖的来源与校验记录".into(),
                        },
                    )?;
                }
                let manifest = workspace.join("src/tools/sources.json");
                let mut provenance: BTreeMap<String, Value> =
                    if relative.starts_with("src/tools/") && manifest.is_file() {
                        read_json(&manifest).map_err(file_error)?
                    } else {
                        BTreeMap::new()
                    };
                let key = relative.trim_start_matches("src/tools/");
                let pinned = provenance
                    .get(key)
                    .or_else(|| {
                        state
                            .extra
                            .get("toolSources")
                            .and_then(|sources| sources.get(relative))
                    })
                    .filter(|source| source["url"] == url)
                    .and_then(|source| source["sha256"].as_str());
                let (sha256, bytes, reused, changed, source) = if !refresh
                    && path.is_file()
                    && pinned.is_some()
                {
                    let digest = hash_file(&path).map_err(file_error)?;
                    if Some(digest.as_str()) != pinned {
                        return Err(EngineError::Rejected(format!(
                            "目标工具已有本地修改，未覆盖：{relative}。请选择新路径，或用 refresh=true 明确重新下载。"
                        )));
                    }
                    (digest, path.metadata()?.len(), true, false, "project")
                } else {
                    let download = self
                        .fetch_public_download(
                            url,
                            refresh,
                            if refresh { None } else { pinned },
                            cancellation,
                        )
                        .await?;
                    let changed = !path.is_file()
                        || hash_file(&path).map_err(file_error)? != download.source.sha256;
                    let parent = path
                        .parent()
                        .ok_or_else(|| EngineError::Protocol("下载路径缺少目录。".into()))?;
                    fs::create_dir_all(parent)?;
                    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
                    let mut input = fs::File::open(&download.path)?;
                    std::io::copy(&mut input, &mut temporary)?;
                    if hash_file(temporary.path()).map_err(file_error)? != download.source.sha256 {
                        return Err(EngineError::Rejected("下载副本与共享缓存不一致。".into()));
                    }
                    if cancellation.is_cancelled() {
                        return Err(EngineError::Cancelled);
                    }
                    temporary
                        .persist(&path)
                        .map_err(|error| EngineError::Io(error.error))?;
                    (
                        download.source.sha256,
                        download.source.bytes,
                        download.reused,
                        changed,
                        if download.reused {
                            "shared_cache"
                        } else {
                            "network"
                        },
                    )
                };
                if cancellation.is_cancelled() {
                    return Err(EngineError::Cancelled);
                }
                if relative.starts_with("src/tools/") {
                    provenance.insert(key.into(), json!({"url":url,"sha256":sha256}));
                    write_json(&manifest, &provenance).map_err(file_error)?;
                }
                state.release_files = release_files;
                let sources = state
                    .extra
                    .entry("toolSources".into())
                    .or_insert_with(|| json!({}));
                if !sources.is_object() {
                    *sources = json!({});
                }
                sources[relative] = json!({"url":url,"sha256":sha256});
                if changed {
                    state.verified_fingerprint = None;
                }
                if let Some(progress) = progress {
                    let (stage, action) = match source {
                        "project" => ("复用本地工具", "已复用经校验的项目文件"),
                        "shared_cache" => ("命中下载缓存", "已从共享下载缓存恢复"),
                        _ => ("联网下载完成", "已联网下载"),
                    };
                    progress(state.progress(EngineProgress {
                        stage: stage.into(),
                        message: format!("{action}：{relative}（{bytes} 字节）；来源：{url}"),
                        tool_name: Some(name.into()),
                        ..EngineProgress::default()
                    }));
                }
                Ok(
                    json!({"path":relative,"sha256":sha256,"reused":reused,"source":source,"bytes":bytes,"releaseRegistered":state.release_files.iter().any(|file|workspace_file(&file.path)==relative)}),
                )
            }
            "read_tool_result" => read_tool_result(workspace, state, args),
            "read_session_history" => read_session_history(state, args),
            "test_adapter" => {
                self.test_adapter(request, workspace, state, args, cancellation, progress)
                    .await
            }
            "finish" => {
                self.finish_adapter(request, workspace, state, args, cancellation)
                    .await
            }
            "checkpoint" => {
                let summary = text(args, "summary")?;
                if !(1..=8000).contains(&summary.encode_utf16().count()) {
                    return Err(EngineError::Protocol(
                        "进度摘要必须在 1 到 8000 字符之间。".into(),
                    ));
                }
                apply_coverage(state, args)?;
                state.checkpoint = summary.into();
                state.notes = summary.into();
                Ok(
                    json!({"saved":true,"summary":state.checkpoint,"nextAction":"继续处理下一项具体实现或验收；完整验收通过后直接 finish。"}),
                )
            }
            "report_blocker" => {
                let kind = text(args, "kind")?;
                if !matches!(kind, "needs_input" | "unsupported") {
                    return Err(EngineError::Protocol("卡点类型无效。".into()));
                }
                for field in ["summary", "reason", "nextAction"] {
                    let value = text(args, field)?.trim();
                    if value.is_empty() || value.encode_utf16().count() > 2000 {
                        return Err(EngineError::Protocol(format!(
                            "{field} 必须说明具体原因或操作，且不能过长。"
                        )));
                    }
                }
                let can_continue = args
                    .get("canContinueLocally")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                if kind == "unsupported"
                    && (args
                        .get("evidence")
                        .and_then(Value::as_str)
                        .is_none_or(|value| value.trim().is_empty())
                        || text(args, "evidence")
                            .is_ok_and(|value| value.encode_utf16().count() > 2000)
                        || args
                            .get("canContinueLocally")
                            .and_then(Value::as_bool)
                            .is_none()
                        || can_continue)
                {
                    let count = state
                        .extra
                        .get("unresolvedStopReports")
                        .and_then(Value::as_u64)
                        .unwrap_or(0)
                        + 1;
                    state
                        .extra
                        .insert("unresolvedStopReports".into(), json!(count));
                    if count >= 2 {
                        state.summary = text(args, "summary")?.trim().into();
                        state.next_action = text(args, "nextAction")?.trim().into();
                        state.extra.insert("blocker".into(),json!({"needsUserInput":false,"reason":args["reason"],"nextAction":args["nextAction"],"workRemaining":true}));
                        return Ok(
                            json!({"stopped":true,"status":"incomplete","workRemaining":true,"reason":"模型连续请求停止，但未提供充分的停止说明。已保存未完成步骤，可继续适配。"}),
                        );
                    }
                    return Ok(
                        json!({"stopped":false,"nextAction":"尚有本地工作或缺少不可支持的证据，请继续实现与验收。"}),
                    );
                }
                state.summary = text(args, "summary")?.into();
                state.next_action = text(args, "nextAction")?.into();
                state.extra.insert("blocker".into(), args.clone());
                Ok(json!({"stopped":true,"kind":kind,"success":true}))
            }
            _ => Err(EngineError::Rejected(format!(
                "此宿主不提供工具 {name}；请使用当前工具清单和 Python 入口。"
            ))),
        }
    }

    async fn try_builtin_sample(
        &self,
        workspace: &Path,
        state: &mut Session,
        args: &Value,
        cancellation: &CancellationToken,
    ) -> Result<Value, EngineError> {
        let requested = text(args, "format")?;
        let format = crate::vntextpatch::FORMATS
            .iter()
            .copied()
            .find(|format| format.eq_ignore_ascii_case(requested))
            .ok_or_else(|| EngineError::Protocol("未找到该格式的内置解析器。".into()))?;
        let directory = text(args, "directory")?.replace('\\', "/");
        if !matches!(directory.split('/').next(), Some("resources" | "work")) {
            return Err(EngineError::Protocol(
                "试提取目录只能是 resources、work 或它们的子目录。".into(),
            ));
        }
        let source = resolve_relative(workspace, &directory)?;
        let files: Vec<_> = strings(args, "files")?
            .into_iter()
            .map(|file| file.replace('\\', "/"))
            .collect();
        let check_roundtrip = boolean(args, "checkRoundtrip", true)?;
        let mut result = json!({"format":format,"status":"rejected","roundtripStatus":if check_roundtrip{"not-run"}else{"skipped"},"unitCount":0,"samples":[],"warnings":[],"sourceHashes":{},"error":null,"diagnostic":null,"runtimeStatus":"not-tested"});
        let mut source_hashes = BTreeMap::new();
        let outcome=async {
            if files.is_empty() { return Err(EngineError::Rejected("请明确选择要试提取的样本文件。".into())); }
            if files.len()>12 { result["status"]=json!("limit-exceeded"); return Err(EngineError::Rejected("单次试提取最多 12 个完整资源；这不是引擎不受支持的结论。".into())); }
            let selected:BTreeSet<_>=files.iter().map(|file|file.to_lowercase()).collect();
            if selected.len()!=files.len() { return Err(EngineError::Rejected("样本文件列表包含重复路径。".into())); }
            require_builtin_companions(format,&source,&files)?;
            let mut bytes=0_u64;
            for file in &files {
                if cancellation.is_cancelled(){return Err(EngineError::Cancelled);}
                let path=resolve_relative(&source,file)?;
                bytes=bytes.saturating_add(path.metadata()?.len());
                if bytes>64*1024*1024 { result["status"]=json!("limit-exceeded"); return Err(EngineError::Rejected("所选样本合计超过单次 64 MiB；这不是引擎不受支持的结论。".into())); }
                source_hashes.insert(file.clone(),hash_file(&path).map_err(file_error)?);
            }
            result["sourceHashes"]=json!(source_hashes);
            result["status"]=json!("failed");
            let mut extract=AdapterRequest::new("extract",&source);
            extract.resource_files=Some(files.clone());
            let extraction=automatic::trial_response(self,&extract,format,&mut result,cancellation).await?.extraction
                .ok_or_else(||EngineError::Protocol("内置解析器未返回提取结果。".into()))?;
            let mut ids=BTreeSet::new();
            for unit in &extraction.units {
                if unit.id.trim().is_empty() || !ids.insert(&unit.id) || !selected.contains(&unit.resource_path.replace('\\',"/").to_lowercase()) {
                    return Err(EngineError::Protocol("内置解析器返回重复文本 ID 或样本范围之外的文本。".into()));
                }
            }
            result["unitCount"]=json!(extraction.units.len());
            result["samples"]=json!(extraction.units.iter().take(5).map(|unit|json!({"resourcePath":unit.resource_path,"locator":unit.locator,"text":unit.source_text.chars().take(300).collect::<String>(),"kind":unit.kind})).collect::<Vec<_>>());
            let mut warnings=extraction.warnings;
            warnings.push("本次仅检查所选样本；原文回填通过不代表中文写入或游戏运行已验证。".into());
            result["warnings"]=json!(warnings.into_iter().take(12).map(|warning|warning.chars().take(500).collect::<String>()).collect::<Vec<_>>());
            if check_roundtrip {
                result["roundtripStatus"]=json!("failed");
                let temporary=tempfile::tempdir()?;
                let mut build=AdapterRequest::new("build",&source);
                build.resource_files=Some(files.clone());
                build.output_directory=Some(temporary.path().into());
                build.translations=Some(BTreeMap::new());
                let build=automatic::trial_response(self,&build,format,&mut result,cancellation).await?.build.ok_or_else(||EngineError::Protocol("内置解析器未返回原文回填结果。".into()))?;
                let inventory=Inventory::scan(temporary.path())?;
                let actual:BTreeSet<_>=inventory.files.iter().map(|file|file.path.to_lowercase()).collect();
                let reported:BTreeSet<_>=build.files.iter().map(|file|file.replace('\\',"/").to_lowercase()).collect();
                if !inventory.complete || actual!=selected || reported!=selected || build.files.len()!=selected.len() || build.launch_executable.is_some() {
                    return Err(EngineError::Rejected("原文回填产生的完整文件集合与所选样本不一致。".into()));
                }
                if hashes(temporary.path(),&files)?!=source_hashes { return Err(EngineError::Rejected("原文回填改变了样本字节。".into())); }
                result["roundtripStatus"]=json!("passed");
            }
            result["status"]=json!(if extraction.units.is_empty(){"no-text"}else{"passed"});
            Ok::<_,EngineError>(())
        }.await;
        if matches!(outcome, Err(EngineError::Cancelled)) {
            return Err(EngineError::Cancelled);
        }
        if let Err(error) = outcome {
            result["error"] = json!(error.to_string());
        }
        if !source_hashes.is_empty()
            && hashes(&source, &source_hashes.keys().cloned().collect::<Vec<_>>())
                .ok()
                .as_ref()
                != Some(&source_hashes)
        {
            result["status"] = json!("failed");
            result["error"] = json!("试提取期间源样本发生变化，结果不能作为验证依据。");
        }
        let mut trials = state
            .extra
            .get("builtinTrials")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        trials.retain(|trial| trial["format"].as_str() != Some(format));
        let mut observation = result.clone();
        observation["directory"] = json!(directory);
        observation["files"] = json!(files);
        observation["observedAt"] = json!(chrono::Utc::now().to_rfc3339());
        trials.push(observation);
        if trials.len() > 6 {
            trials.remove(0);
        }
        state.extra.insert("builtinTrials".into(), json!(trials));
        Ok(result)
    }

    fn knowledge_reuse_options(
        &self,
        request: &AdaptationRequest<'_>,
        entry: &Value,
    ) -> Result<Value, EngineError> {
        let family = &entry["family"];
        let family_id = format!("{}.{}", text(family, "kind")?, text(family, "key")?);
        let root = crate::adaptation_context::engine_root(text(family, "key")?);
        let mut published = Vec::new();
        let mut seen = BTreeSet::new();
        for directory in &request.package_directories {
            let Ok(manifest) = PackageManifest::load(directory) else {
                continue;
            };
            let related = root.is_some()
                && root
                    == crate::adaptation_context::engine_root(
                        manifest.family["key"].as_str().unwrap_or(&manifest.id),
                    );
            if (manifest.id == family_id || related)
                && seen.insert((manifest.id.clone(), manifest.version.clone()))
            {
                published.push((
                    manifest.id == family_id,
                    manifest
                        .metadata
                        .get("publishedAt")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned(),
                    manifest.id,
                    manifest.version,
                ));
            }
        }
        published.sort_by(|a, b| {
            b.0.cmp(&a.0)
                .then_with(|| b.1.cmp(&a.1))
                .then_with(|| a.3.cmp(&b.3))
        });
        let components = components::related(root.as_deref())?;
        let builtins:Vec<_>=crate::adaptation_context::builtin_catalog().into_iter().filter(|format|root.is_some()&&root==crate::adaptation_context::engine_root(format["format"].as_str().unwrap_or(""))).take(4).map(|format|json!({"format":format["format"],"inspectCall":{"tool":"inspect_builtin_adapter","arguments":{"format":format["format"]}}})).collect();
        Ok(
            json!({"components":components,"publishedAdapters":published.into_iter().take(4).map(|(_,_,id,version)|json!({"sameFamilyOnly":id!=family_id,"id":id,"version":version,"metadataOnly":true,"inspectCall":{"tool":"inspect_adapter_source","arguments":{"id":id,"version":version}},"copyCall":{"tool":"copy_adapter","arguments":{"id":id,"version":version}}})).collect::<Vec<_>>(),"builtInParsers":builtins,"usage":"先查看相关组件的检测入口或已发布实现的文件索引，按层核对当前样本。inspect_adapter_source 可按 path/query 读取片段；copy_adapter(files=[发布路径]) 只复制选定参考到 work/，已有 src 也可使用；不指定 files 的整包复用才要求空 src。仅所选文件读取或复制时核验，不自动加载完整源码。某层不匹配不排除其他模块；接入时核对导入、固定依赖及许可，仍须当前游戏验收。新实现使用 Python。"}),
        )
    }

    fn adaptation_reference(
        &self,
        workspace: &Path,
        state: &Session,
    ) -> Result<Value, EngineError> {
        if let Some(reference) = state
            .extra
            .get("engineReference")
            .filter(|reference| reference["kind"].as_str() == Some("galtransl"))
        {
            return Ok(reference.clone());
        }
        let path = workspace.join("src/tools/engine-reference.json");
        if path.is_file() {
            let dependency: Value = read_json(&path).map_err(file_error)?;
            if dependency["reference"]["kind"].as_str() != Some("siglus") {
                return Ok(dependency["reference"].clone());
            }
        }
        self.current_reference()
    }

    async fn read_siglus_source(
        &self,
        prepared: &Value,
        path: &str,
        args: &Value,
        cancellation: &CancellationToken,
    ) -> Result<Value, EngineError> {
        let library = Path::new(text(prepared, "library")?);
        let mut params = args.clone();
        params["path"] = json!(path);
        if library.is_file() {
            params["operation"] = json!("library");
            params["library"] = json!(library);
        } else {
            params["operation"] = json!("read_file");
            params["root"] = json!(library);
            params["encoding"] = json!("utf8");
        }
        self.helper(&params, cancellation).await
    }

    fn adapter_list(
        &self,
        request: &AdaptationRequest<'_>,
        state: &mut Session,
        args: &Value,
        cancellation: &CancellationToken,
    ) -> Result<Value, EngineError> {
        let query = args
            .get("query")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_lowercase();
        if boolean(args, "refreshCandidates", false)? {
            state.extra.insert(
                "builtinDiscovery".into(),
                crate::adaptation_context::inspect_builtin_discovery(
                    &request.game_directory,
                    cancellation,
                )?,
            );
            state.extra.insert(
                "builtinDiscoveryObservedAt".into(),
                json!(chrono::Utc::now().to_rfc3339()),
            );
        } else {
            crate::adaptation_context::ensure_builtin_discovery(
                &request.game_directory,
                state,
                cancellation,
            )?;
        }
        let pinned = read_json::<Value>(
            &request
                .package_directory
                .join("games")
                .join(hash_text(&request.game_id))
                .join("active.json"),
        )
        .ok();
        let mut seen = BTreeSet::new();
        let mut packages = Vec::new();
        for directory in &request.package_directories {
            if let Ok(manifest) = PackageManifest::load(directory)
                && (query.is_empty()
                    || format!("{} {}", manifest.id, manifest.family)
                        .to_lowercase()
                        .contains(&query))
                && seen.insert((manifest.id.clone(), manifest.version.clone()))
            {
                packages.push(json!({"id":manifest.id,"version":manifest.version,"family":manifest.family,"pinned":pinned.as_ref().is_some_and(|pin|pin["id"]==manifest.id&&pin["version"]==manifest.version),"enabledForNewGames":!self.disabled_families.contains(&manifest.id),"manifestVersion":manifest.manifest_version,"resourcePatterns":manifest.resource_patterns,"runnable":manifest.manifest_version==4,"inspectCall":{"tool":"inspect_adapter_source","arguments":{"id":manifest.id,"version":manifest.version}}}));
            }
        }
        let builtins: Vec<_> = crate::adaptation_context::builtin_catalog()
            .into_iter()
            .filter(|item| {
                !self
                    .disabled_families
                    .contains(item["id"].as_str().unwrap_or(""))
                    && (query.is_empty()
                        || format!("{} {} {}", item["id"], item["format"], item["name"])
                            .to_lowercase()
                            .contains(&query))
            })
            .collect();
        let components = components::list(&query)?;
        let templates: Vec<_> = crate::adaptation_assets::template_catalog()
            .into_iter()
            .filter(|item| {
                query.is_empty()
                    || format!(
                        "{} {} {}",
                        item["id"], item["format"], item["family"]["name"]
                    )
                    .to_lowercase()
                    .contains(&query)
            })
            .collect();
        let engine_components: Vec<_> = crate::adaptation_assets::engine_components()
            .into_iter()
            .filter(|item| {
                query.is_empty()
                    || format!("{} {}", item["id"], item["format"])
                        .to_lowercase()
                        .contains(&query)
            })
            .collect();
        let mut candidates = state
            .extra
            .get("builtinDiscovery")
            .cloned()
            .unwrap_or(Value::Null);
        if let Some(items) = candidates
            .get_mut("candidates")
            .and_then(Value::as_array_mut)
        {
            items.retain(|item| {
                !self
                    .disabled_families
                    .contains(item["adapterId"].as_str().unwrap_or(""))
            });
        }
        Ok(
            json!({"candidates":candidates,"observedAt":state.extra.get("builtinDiscoveryObservedAt"),"regularProbes":state.extra.get("builtinProbes").cloned().unwrap_or(json!([])),"published":packages,"builtIns":builtins,"siglus":if !self.disabled_families.contains("engine.siglus")&&(query.is_empty()||"siglus engine.siglus scene.pck fkds".contains(&query)){crate::adaptation_assets::siglus_description()}else{Value::Null},"components":components,"engineComponents":engine_components,"templates":templates,"usage":"候选是本地线索，不代表兼容验证。已发布实现可用 inspect_adapter_source 定向读源码，copy_adapter(files=[发布路径]) 将选定模块参考复制到 work/，不覆盖已有 src。按容器、过滤、脚本、编码、字体和加载分别复用，仅调查不兼容部分。内置解析器用 inspect_builtin_adapter、try_builtin_adapter 小样验证后 copy_adapter。封包用 inspect_engine_component / run_engine_component；文本及运行组件用 inspect_adapter_component、copy_adapter_components。复用仍须当前游戏验收。"}),
        )
    }

    async fn fetch_url(
        &self,
        args: &Value,
        cancellation: &CancellationToken,
    ) -> Result<Value, EngineError> {
        let url = text(args, "url")?;
        let parsed = reqwest::Url::parse(url).map_err(file_error)?;
        let mode = optional_text(args, "mode", "text")?;
        if !matches!(mode, "text" | "github_tree") {
            return Err(EngineError::Protocol(
                "mode 必须为 text 或 github_tree。".into(),
            ));
        }
        let repository = crate::research::github_git_repository(&parsed, "trees");
        if mode == "github_tree" && repository.is_none() {
            return Err(EngineError::Protocol(
                "github_tree 仅支持 https://api.github.com/repos/{owner}/{repo}/git/trees/{ref}。"
                    .into(),
            ));
        }
        let query = optional_text(args, "query", "")?;
        let query_length = query.encode_utf16().count();
        if query_length > 256 {
            return Err(EngineError::Protocol("检索条件不能超过 256 字符。".into()));
        }
        let document = self
            .fetch_reference(url, boolean(args, "refresh", false)?, cancellation)
            .await?;
        if mode == "github_tree" {
            return github_tree_page(&document, &parsed, args, query);
        }
        let content = if document.media_type.as_deref() == Some("text/html") {
            let directory = tempfile::tempdir()?;
            let source = directory.path().join("source.html");
            let output = directory.path().join("decoded.txt");
            fs::write(&source, &document.content)?;
            self.helper(&json!({"operation":"decode_reference_html","source":source,"output":output,"timeoutSeconds":1}), cancellation).await?;
            fs::read_to_string(output)?
        } else {
            String::from_utf8_lossy(&document.content).into_owned()
        };
        let upstream_truncated = if repository.is_some() {
            serde_json::from_str::<Value>(&content)?
                .get("truncated")
                .is_some_and(|flag| flag == true)
        } else {
            false
        };
        let units: Vec<_> = content.encode_utf16().collect();
        let offset = number(args, "offset", 0, 0, units.len() as u64)? as usize;
        let length = number(args, "length", 12000, 1, 24000)? as usize;
        if length < query_length {
            return Err(EngineError::Protocol("片段长度不能小于检索词长度。".into()));
        }
        let mut result = json!({"url":document.url,"contentSha256":hash_text(&content),"retrievedAt":document.retrieved_at,"upstreamTruncated":upstream_truncated,"reused":document.reused,"stale":document.stale,"totalCharacters":units.len()});
        if query.is_empty() {
            let next = units.len().min(offset + length);
            result["offset"] = json!(offset);
            result["nextOffset"] = json!(next);
            result["more"] = json!(next < units.len());
            result["content"] = json!(String::from_utf16_lossy(&units[offset..next]));
        } else {
            let mut matches = Vec::new();
            let mut position = offset;
            let mut remaining = length;
            while matches.len() < 8 && remaining >= query_length {
                let Some(found) = find_ordinal_ignore_case(&content, query, position) else {
                    position = units.len();
                    break;
                };
                let start = found.saturating_sub(240.min((remaining - query_length) / 4));
                let count = (units.len() - start).min(query_length + 720).min(remaining);
                matches.push(json!({"matchOffset":found,"offset":start,"content":String::from_utf16_lossy(&units[start..start+count])}));
                remaining -= count;
                position = found + query_length;
            }
            result["query"] = json!(query);
            result["matches"] = json!(matches);
            result["nextOffset"] = json!(position);
            result["more"] = json!(
                position < units.len()
                    && find_ordinal_ignore_case(&content, query, position).is_some()
            );
        }
        Ok(result)
    }

    async fn bind_reference(
        &self,
        workspace: &Path,
        state: &mut Session,
        reference: Option<Value>,
        cancellation: &CancellationToken,
    ) -> Result<Value, EngineError> {
        let reference = match reference {
            Some(value) => value,
            None => self.current_reference()?,
        };
        if reference.get("kind").and_then(Value::as_str) != Some("siglus") {
            self.reference_library(&reference)?;
        }
        let runtime: Value =
            read_json(&self.tools.root.join("python/runtime.json")).map_err(file_error)?;
        let pin = json!({"reference":reference,"pythonVersion":"3.14.10","pythonSha256":runtime["archiveSha256"]});
        let native_pin_path = workspace.join("src/tools/vntextpatch-reference.json");
        let native_pin = if native_pin_path.is_file() {
            let pin: Value = read_json(&native_pin_path).map_err(file_error)?;
            self.resolve_native_tools(&pin)?;
            pin
        } else {
            self.create_native_pin()?
        };
        let helper_source = self.asset_root.join("adapters/components/gamel10n_host.py");
        let helper_content = fs::read(&helper_source)?;
        let helper_path = workspace.join("src/tools/gamel10n_host.py");
        let pins = [
            (
                workspace.join("src/tools/engine-reference.json"),
                &pin,
                "当前源码固定到另一个引擎依赖版本，请先核实原有依赖。",
            ),
            (
                workspace.join("src/tools/vntextpatch-reference.json"),
                &native_pin,
                "现有源码固定到其他原生工具；确认接口和行为兼容后再更换依赖。",
            ),
        ];
        for (path, expected, message) in &pins {
            if path.is_file() {
                let current: Value = read_json(path).map_err(file_error)?;
                if current != **expected {
                    return Err(EngineError::Rejected((*message).into()));
                }
            } else {
                preflight_unmodified(path, &[])?;
            }
        }
        preflight_unmodified(&helper_path, &helper_content)?;
        let mut selection = state.release_files.clone();
        for (path, purpose) in [
            (
                "tools/engine-reference.json",
                "固定共享 Python 与引擎工具版本。",
            ),
            (
                "tools/gamel10n_host.py",
                "调用固定原生解析器的 Python 协议桥。",
            ),
            (
                "tools/vntextpatch-reference.json",
                "固定原生脚本、编码与字体组件宿主。",
            ),
        ] {
            select_release(
                &mut selection,
                ReleaseFile {
                    path: path.into(),
                    role: "runtime".into(),
                    purpose: purpose.into(),
                },
            )?;
        }
        if cancellation.is_cancelled() {
            return Err(EngineError::Cancelled);
        }
        for (path, value, _) in pins {
            if !path.exists() {
                write_json(&path, value).map_err(file_error)?;
            }
        }
        copy_unmodified(&helper_source, &helper_path)?;
        state.release_files = selection;
        Ok(
            json!({"success":true,"dependency":pin,"helper":"tools/gamel10n_host.py","usage":"from tools.gamel10n_host import parser, write_response; parser(request, format) calls the bundled NativeAOT parser. Shared Python modules import python.engines.*; inspect signatures first."}),
        )
    }

    async fn copy_adapter(
        &self,
        request: &AdaptationRequest<'_>,
        workspace: &Path,
        state: &mut Session,
        args: &Value,
        cancellation: &CancellationToken,
    ) -> Result<Value, EngineError> {
        let id = text(args, "id")?;
        let version = text(args, "version")?;
        if args.get("files").is_some() {
            return published_reuse::copy_selected(request, workspace, args, cancellation);
        }
        if id == "engine.siglus" && version == "1.0.0" {
            let files = [
                (
                    "src/adapter.py",
                    "import runpy\nrunpy.run_module('tools.adapter', run_name='__main__', alter_sys=True)\n",
                ),
                (
                    "src/tools/adapter.py",
                    "import sys\nfrom gamel10n_siglus import main\nsys.exit(main())\n",
                ),
                ("src/tools/siglus-rules.json", "{\"commandRules\":[]}\n"),
            ];
            for (relative, content) in &files {
                preflight_unmodified(&workspace.join(relative), content.as_bytes())?;
            }
            let prepared = self.siglus(cancellation).await?;
            self.bind_reference(workspace,state,Some(json!({"revision":prepared["revision"],"sourceSha256":prepared["sourceSha256"],"librarySha256":prepared["librarySha256"],"kind":"siglus"})),cancellation).await?;
            for (relative, content) in files {
                write_unmodified(&workspace.join(relative), content)?;
            }
            register_python(state, "src/adapter.py")?;
            for path in ["tools/adapter.py", "tools/siglus-rules.json"] {
                register_release(
                    state,
                    ReleaseFile {
                        path: path.into(),
                        role: "runtime".into(),
                        purpose: "共享 Siglus 协议桥与显示调用规则。".into(),
                    },
                )?;
            }
            return Ok(
                json!({"success":true,"id":id,"nextAction":"可在 src/tools/siglus-rules.json 增补已证实显示调用；完成后 test_adapter。"}),
            );
        }
        let format = id
            .strip_prefix("engine.vntextpatch-")
            .and_then(|key| {
                crate::vntextpatch::FORMATS
                    .iter()
                    .copied()
                    .find(|format| format.to_lowercase() == key)
            })
            .unwrap_or("");
        if crate::vntextpatch::FORMATS.contains(&format) && version == "1.0.1" {
            let entry = format!(
                "import json\nimport sys\nfrom tools.gamel10n_host import parser, write_response\nrequest=json.load(sys.stdin)\nwrite_response(request,parser(request,{format:?}))\n"
            );
            preflight_unmodified(&workspace.join("src/adapter.py"), entry.as_bytes())?;
            self.bind_reference(workspace, state, None, cancellation)
                .await?;
            write_unmodified(&workspace.join("src/adapter.py"), &entry)?;
            register_python(state, "src/adapter.py")?;
            return Ok(
                json!({"success":true,"format":format,"nextAction":"补齐资源发现或容器胶水后执行 test_adapter。"}),
            );
        }
        for directory in &request.package_directories {
            let Ok(manifest) = PackageManifest::load(directory) else {
                continue;
            };
            if manifest.id == id && manifest.version == version {
                let source = workspace.join("src");
                if source.exists() && (!source.is_dir() || fs::read_dir(&source)?.next().is_some())
                {
                    return Err(EngineError::Rejected(
                        "src 已包含代码。请先检查现有文件，避免覆盖已有开发成果。".into(),
                    ));
                }
                if manifest.manifest_version == 4 {
                    manifest.handle(directory)?;
                }
                let mut files = Vec::new();
                for relative in manifest.package_files.keys() {
                    if relative.starts_with("src/") || relative.starts_with("tools/") {
                        let target = if manifest.manifest_version == 4 {
                            resolve_relative(workspace, &workspace_file(relative))?
                        } else {
                            resolve_relative(&workspace.join("work/legacy"), relative)?
                        };
                        let content = fs::read(resolve_relative(directory, relative)?)?;
                        preflight_unmodified(&target, &content)?;
                        files.push((target, content));
                    }
                }
                for (target, content) in files {
                    if !target.exists() {
                        if let Some(parent) = target.parent() {
                            fs::create_dir_all(parent)?;
                        }
                        OpenOptions::new()
                            .write(true)
                            .create_new(true)
                            .open(target)?
                            .write_all(&content)?;
                    }
                }
                if manifest.manifest_version == 4 {
                    if let Some(selection) = manifest.metadata.get("releaseFiles") {
                        state.release_files = serde_json::from_value(selection.clone())?;
                    } else {
                        for relative in manifest.package_files.keys() {
                            if relative.ends_with(".py") && relative.starts_with("src/") {
                                register_python(state, relative)?;
                            }
                        }
                    }
                    state.resource_patterns = manifest.resource_patterns;
                    state.recognition = manifest.recognition;
                    state.family = manifest.family;
                }
                return Ok(
                    json!({"success":true,"legacyReferenceOnly":manifest.manifest_version!=4,"nextAction":if manifest.manifest_version==4{"保留既有文本身份与回填方案，修改后重新验收。"}else{"旧 C# 源码保存在 work/legacy，仅供移植参考；实现 src/adapter.py 后完整验收。"}}),
                );
            }
        }
        Err(EngineError::Rejected("没有找到所选适配器版本。".into()))
    }

    async fn inspect_component(
        &self,
        args: &Value,
        cancellation: &CancellationToken,
    ) -> Result<Value, EngineError> {
        components::inspect(self, args, cancellation)
    }

    fn copy_components(
        &self,
        workspace: &Path,
        state: &mut Session,
        args: &Value,
    ) -> Result<Value, EngineError> {
        components::copy(self, workspace, state, args)
    }

    async fn run_component(
        &self,
        workspace: &Path,
        state: &Session,
        args: &Value,
        cancellation: &CancellationToken,
    ) -> Result<Value, EngineError> {
        let component = text(args, "id")?;
        if !matches!(component, "archive.rpa3" | "archive.xp3") {
            return Err(EngineError::Protocol("未知封包组件。".into()));
        }
        let operation = text(args, "operation")?;
        if !matches!(operation, "inspect" | "extract" | "rebuild") {
            return Err(EngineError::Protocol("不支持的封包组件操作。".into()));
        }
        if component == "archive.xp3" && args["filter"].as_str() != Some("none") {
            return Err(EngineError::Rejected(
                "请先确认 XP3 未使用过滤器，再明确指定 filter=none。".into(),
            ));
        }
        let archive = text(args, "archive")?.replace('\\', "/");
        if !archive.starts_with("resources/") && !archive.starts_with("work/") {
            return Err(EngineError::Protocol("组件输入必须是已暂存资源。".into()));
        }
        let archive_path = resolve_relative(workspace, &archive)?;
        let maximum = if component == "archive.rpa3" {
            2_u64 * 1024 * 1024 * 1024
        } else {
            512 * 1024 * 1024
        };
        if archive_path.metadata()?.len() > maximum {
            return Err(EngineError::Rejected("封包超过组件输入大小限制。".into()));
        }
        let output_relative = text(args, "output")?.replace('\\', "/");
        if !output_relative.starts_with("work/") {
            return Err(EngineError::Protocol(
                "组件输出必须是 work/ 下的新目录。".into(),
            ));
        }
        let output = resolve_relative(workspace, &output_relative)?;
        if output.exists() {
            return Err(EngineError::Rejected(
                "组件输出目录已存在，请读取已有 manifest.json 或选择新的目录。".into(),
            ));
        }
        if archive_path.starts_with(&output) {
            return Err(EngineError::Protocol("组件输入和输出目录不能重叠。".into()));
        }
        let mut payload = args.clone();
        payload["protocolVersion"] = json!(1);
        payload["component"] = json!(component);
        payload["archive"] = json!(archive_path);
        payload["responseFile"] = json!(output.join("manifest.json"));
        payload["outputDirectory"] = json!(output.join("files"));
        payload["outputFile"] = json!(
            output.join("rebuilt").join(
                archive_path
                    .file_name()
                    .ok_or_else(|| EngineError::Protocol("封包文件名无效。".into()))?
            )
        );
        payload["verifyContents"] = json!(boolean(args, "verifyContents", false)?);
        if let Some(replacements) = payload
            .get_mut("replacements")
            .and_then(Value::as_object_mut)
        {
            if operation != "rebuild" || replacements.len() > 128 {
                return Err(EngineError::Protocol(
                    "replacements 仅用于 rebuild，最多 128 项。".into(),
                ));
            }
            for (_, value) in replacements.iter_mut() {
                let relative = value
                    .as_str()
                    .ok_or_else(|| EngineError::Protocol("替换文件路径无效。".into()))?;
                if !relative.starts_with("work/") {
                    return Err(EngineError::Protocol("替换文件必须位于 work/。".into()));
                }
                let path = resolve_relative(workspace, relative)?;
                if !path.is_file() || path.starts_with(&output) {
                    return Err(EngineError::Rejected(
                        "替换文件不存在或与输出目录重叠。".into(),
                    ));
                }
                *value = json!(path);
            }
        }
        let reference = self.adaptation_reference(workspace, state)?;
        let library = self.reference_library(&reference)?;
        let mut response=self.helper(&json!({"operation":"archive_component","request":payload,"components":self.asset_root.join("adapters/components"),"library":library}),cancellation).await?;
        response["status"] = json!(if response["success"] == true {
            "passed"
        } else {
            "failed"
        });
        response["version"] = json!(if component == "archive.rpa3" { 2 } else { 1 });
        response["revision"] = reference["revision"].clone();
        response["manifest"] = json!(format!("{output_relative}/manifest.json"));
        response["runtimeStatus"] = json!("not-tested");
        if response["success"] == true && operation == "extract" {
            response["extractedDirectory"] = json!(format!("{output_relative}/files"));
        }
        if response["success"] == true && operation == "rebuild" {
            response["rebuiltArchive"] = json!(format!(
                "{output_relative}/rebuilt/{}",
                archive_path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
            ));
        }
        Ok(response)
    }

    async fn compose_template(
        &self,
        workspace: &Path,
        state: &mut Session,
        args: &Value,
        cancellation: &CancellationToken,
    ) -> Result<Value, EngineError> {
        let id = text(args, "id")?;
        let (format, archive, extensions, patterns) = match id {
            "loose-renpy" => ("Renpy", None, vec![".rpy"], vec!["**/*.rpy"]),
            "loose-kirikiri-ks" => ("KirikiriKs", None, vec![".ks"], vec!["**/*.ks"]),
            "rpa3-renpy" => (
                "Renpy",
                Some("archive.rpa3"),
                vec![".rpy"],
                vec!["**/*.rpa"],
            ),
            "loose-renpy-rpyc" => ("Renpy", None, vec![".rpyc"], vec!["**/*.rpyc"]),
            "rpa3-renpy-rpyc" => (
                "Renpy",
                Some("archive.rpa3"),
                vec![".rpyc"],
                vec!["**/*.rpa"],
            ),
            "renpy-mixed" => (
                "Renpy",
                Some("archive.rpa3"),
                vec![".rpy", ".rpyc"],
                vec!["**/*.rpy", "**/*.rpyc", "**/*.rpa"],
            ),
            "xp3-kirikiri-ks" => (
                "KirikiriKs",
                Some("archive.xp3"),
                vec![".ks"],
                vec!["**/*.xp3"],
            ),
            _ => return Err(EngineError::Protocol("未知组合模板。".into())),
        };
        if archive == Some("archive.xp3")
            && args.get("filter").and_then(Value::as_str) != Some("none")
        {
            return Err(EngineError::Rejected(
                "XP3 模板要求已独立确认 filter=none。".into(),
            ));
        }
        self.bind_reference(workspace, state, None, cancellation)
            .await?;
        let patterns = args
            .get("resourcePatterns")
            .cloned()
            .unwrap_or(json!(patterns));
        let config = json!({"schemaVersion":1,"templateId":id,"templateVersion":1,"format":format,"archiveComponent":archive,"filter":args.get("filter"),"scriptExtensions":extensions,"resourcePatterns":patterns});
        write_json(&workspace.join("src/tools/adapter-template.json"), &config)
            .map_err(file_error)?;
        register_release(
            state,
            ReleaseFile {
                path: "tools/adapter-template.json".into(),
                role: "runtime".into(),
                purpose: "固定资源、容器与脚本组合模板。".into(),
            },
        )?;
        let module = if id == "renpy-mixed" {
            "renpy_mixed_adapter"
        } else if id.contains("rpyc") {
            "renpy_rpyc_adapter"
        } else {
            "script_template"
        };
        for file in [
            "engine_archive.py",
            "renpy_rpyc_adapter.py",
            "renpy_mixed_adapter.py",
        ] {
            if module == "script_template" && file != "engine_archive.py" {
                continue;
            }
            copy_unmodified(
                &self.asset_root.join("adapters/components").join(file),
                &workspace.join("src/tools").join(file),
            )?;
            register_release(
                state,
                ReleaseFile {
                    path: format!("tools/{file}"),
                    role: "runtime".into(),
                    purpose: "固定组合模板实现与封包处理。".into(),
                },
            )?;
        }
        if module == "script_template" {
            write_unmodified(
                &workspace.join("src/tools/script_template.py"),
                include_str!("../python/script_template.py"),
            )?;
            register_release(
                state,
                ReleaseFile {
                    path: "tools/script_template.py".into(),
                    role: "runtime".into(),
                    purpose: "复用原生解析器的完整资源组合模板。".into(),
                },
            )?;
        }
        let entry = format!("import sys\nfrom tools.{module} import main\nsys.exit(main())\n");
        write_unmodified(&workspace.join("src/adapter.py"), &entry)?;
        register_python(state, "src/adapter.py")?;
        state.resource_patterns = serde_json::from_value(patterns)?;
        let template = crate::adaptation_assets::template_catalog()
            .into_iter()
            .find(|template| template["id"] == id)
            .ok_or_else(|| EngineError::Protocol("未知组合模板。".into()))?;
        Ok(
            json!({"success":true,"template":template,"files":state.release_files.iter().map(|file|format!("src/{}",file.path)).collect::<Vec<_>>(),"releaseFiles":state.release_files,"testArguments":{"kind":"template","key":id,"name":template["family"]["name"],"resourcePatterns":state.resource_patterns},"limitations":"只连接已声明的脚本与封包组件。RPYC2 保留非文本对象及源码摘要；其他脚本编码沿用解析器。不生成未经当前游戏验证的字体、转区或运行时补丁。","nextAction":"暂存与规则匹配的真实资源，用返回的 testArguments 调用完整 test_adapter。模板生成不代表已验证，也不会发布。"}),
        )
    }

    async fn test_adapter(
        &self,
        request: &AdaptationRequest<'_>,
        workspace: &Path,
        state: &mut Session,
        args: &Value,
        cancellation: &CancellationToken,
        progress: ProgressCallback<'_>,
    ) -> Result<Value, EngineError> {
        verification::test_adapter(
            self,
            request,
            workspace,
            state,
            args,
            cancellation,
            progress,
        )
        .await
    }

    async fn finish_adapter(
        &self,
        request: &AdaptationRequest<'_>,
        workspace: &Path,
        state: &mut Session,
        args: &Value,
        cancellation: &CancellationToken,
    ) -> Result<Value, EngineError> {
        let summary = text(args, "summary")?;
        if summary.trim().is_empty() {
            return Err(EngineError::Rejected("发布说明不能为空。".into()));
        }
        if request.improve_existing
            && !(request.objective.eq_ignore_ascii_case("repair")
                && !request.baseline_units.is_empty())
            && state.text_coverage["samples"]
                .as_array()
                .is_none_or(|samples| samples.is_empty())
        {
            return Err(EngineError::Rejected("继续完善已有适配器时，请在 test_adapter 提供独立定位的 coverageSamples，确认反馈涉及的原文和字段已正确提取。".into()));
        }
        let package = state
            .verified_package
            .as_ref()
            .ok_or_else(|| EngineError::Rejected("没有通过完整验收的适配器，不能发布。".into()))?;
        if state
            .extra
            .get("rustVerifiedContextFingerprint")
            .and_then(Value::as_str)
            != Some(verification_context(request).as_str())
        {
            return Err(EngineError::Rejected(
                "适配目标或已有文本基线发生变化，请重新完整验收。".into(),
            ));
        }
        if state.verified_fingerprint.as_deref() != Some(&workspace_fingerprint(workspace, state)?)
        {
            return Err(EngineError::Rejected(
                "适配器、依赖或已知覆盖发生变化，请重新完整验收。".into(),
            ));
        }
        if state
            .extra
            .get("rustVerifiedCoverageFingerprint")
            .and_then(Value::as_str)
            != Some(&hash_text(&state.text_coverage["samples"].to_string()))
        {
            return Err(EngineError::Rejected(
                "覆盖锚点或遗漏记录发生变化，请重新验收。".into(),
            ));
        }
        let resources: Vec<String> = serde_json::from_value(package["resourceFiles"].clone())?;
        let expected: BTreeMap<String, String> =
            serde_json::from_value(package["resourceHashes"].clone())?;
        if verification::input_hashes(
            &request.game_directory,
            &resources.iter().cloned().collect(),
            &request.inputs,
            &mut verification::FingerprintStats::default(),
        )? != expected
        {
            return Err(EngineError::Rejected(
                "制作输入发生变化，请重新验收。".into(),
            ));
        }
        let pending = coverage_list(state, "pending");
        if !pending.is_empty()
            && !args
                .get("allowPartial")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        {
            return Err(EngineError::Rejected(
                "仍有已知覆盖遗漏；完成当前目标或明确 allowPartial 并在 summary 披露。".into(),
            ));
        }
        let source = PathBuf::from(
            package["directoryPath"]
                .as_str()
                .ok_or_else(|| EngineError::Protocol("验收包目录无效。".into()))?,
        );
        let manifest = PackageManifest::load(&source)?;
        let handle = manifest.handle(&source)?;
        let game = request
            .inputs
            .inventory(Inventory::scan(&request.game_directory)?)?;
        if manifest.recognition.is_some()
            && game
                .recognize_with(manifest.recognition.as_ref(), &|relative| {
                    request.inputs.file(&request.game_directory, relative).ok()
                })
                .0
                != crate::discovery::RecognitionStatus::Match
        {
            return Err(EngineError::Rejected(
                "声明的必要识别条件不再符合实际游戏，请重新验收。".into(),
            ));
        }
        let probe = self
            .probe_bounded_with_inputs(
                &handle,
                &request.game_directory,
                &manifest.resources(&game),
                &request.inputs,
                cancellation,
            )
            .await?
            .ok_or_else(|| {
                EngineError::Rejected("适配器不再识别制作输入，请重新完整验收。".into())
            })?;
        let paths = |files: &[String]| {
            files
                .iter()
                .map(|path| path.replace('\\', "/").to_lowercase())
                .collect::<BTreeSet<_>>()
        };
        if paths(&probe.required_files) != paths(&resources) {
            return Err(EngineError::Rejected(
                "游戏必需资源集合发生变化，请重新暂存并完整验收。".into(),
            ));
        }
        for executable in resources
            .iter()
            .filter(|path| path.to_ascii_lowercase().ends_with(".exe"))
        {
            let profile = format!("{executable}.le.config");
            if !request.inputs.excludes(&profile)
                && request
                    .inputs
                    .file(&request.game_directory, &profile)?
                    .is_file()
                && !resources
                    .iter()
                    .any(|path| path.eq_ignore_ascii_case(&profile))
            {
                return Err(EngineError::Rejected(format!(
                    "所选程序有转区配置尚未纳入验证，请 stage_resources 并声明资源发现规则：{profile}"
                )));
            }
        }
        let destination = resolve_relative(
            &request.package_directory.join("packages"),
            &format!("{}/{}", manifest.id, manifest.version),
        )?;
        fs::create_dir_all(&destination)?;
        for relative in manifest.package_files.keys() {
            copy_unmodified(
                &resolve_relative(&source, relative)?,
                &resolve_relative(&destination, relative)?,
            )?;
        }
        copy_unmodified(
            &source.join("package.json"),
            &destination.join("package.json"),
        )?;
        if let Some(verification) = &state.verification {
            write_json(&destination.join("verification.json"), verification).map_err(file_error)?;
        }
        PackageManifest::load(&destination)?.handle(&destination)?;
        let runtime_checks = coverage_list(state, "runtimeChecks");
        state.summary = format!(
            "{}{}{}{}",
            if pending.is_empty() {
                ""
            } else {
                "已发布部分文字支持；仍有未覆盖内容。\n"
            },
            summary.trim().chars().take(2000).collect::<String>(),
            if pending.is_empty() {
                String::new()
            } else {
                format!("\n已知未覆盖内容：{}", pending.join("；"))
            },
            if runtime_checks.is_empty() {
                String::new()
            } else {
                format!("\n运行效果待验证：{}", runtime_checks.join("；"))
            }
        );
        state.next_action = "可以提取文本、选择试译并检查游戏显示；静态验收未运行游戏。".into();
        Ok(
            json!({"success":true,"packageDirectory":destination,"id":manifest.id,"version":manifest.version,"summary":state.summary,"runtimeTested":false,"coverageStatus":if !pending.is_empty(){"partial"}else if state.text_coverage["samples"].as_array().is_some_and(|samples|!samples.is_empty()){"samples-verified"}else{"not-assessed"},"pendingCoverage":pending,"runtimeChecks":runtime_checks}),
        )
    }
}

fn same_directory(a: &Path, b: &Path) -> bool {
    a.canonicalize()
        .unwrap_or_else(|_| a.into())
        .to_string_lossy()
        .eq_ignore_ascii_case(
            &b.canonicalize()
                .unwrap_or_else(|_| b.into())
                .to_string_lossy(),
        )
}
fn copy_knowledge_sources(
    workspace: &Path,
    id: &str,
    revision: &str,
    files: &serde_json::Map<String, Value>,
) -> Result<Vec<String>, EngineError> {
    let prefix = format!(
        "work/engine-knowledge/{id}/{}/",
        revision
            .get(..12)
            .ok_or_else(|| EngineError::Protocol("经验版本无效。".into()))?
    );
    let mut copied = Vec::new();
    for (relative, content) in files {
        let relative = format!("{prefix}{relative}");
        let path = resolve_relative(workspace, &relative)?;
        let content = content
            .as_str()
            .ok_or_else(|| EngineError::Protocol("经验源码必须是文本。".into()))?;
        if path.is_file() && fs::read(&path)? != content.as_bytes() {
            return Err(EngineError::Rejected(format!(
                "目标辅助源码已修改，未覆盖：{relative}"
            )));
        }
        copied.push(relative);
    }
    for (relative, content) in files {
        write_unmodified(
            &resolve_relative(workspace, &format!("{prefix}{relative}"))?,
            content
                .as_str()
                .ok_or_else(|| EngineError::Protocol("经验源码必须是文本。".into()))?,
        )?;
    }
    Ok(copied)
}
fn knowledge_candidates(
    _request: &AdaptationRequest<'_>,
    state: &Session,
) -> Result<Vec<String>, EngineError> {
    Ok(crate::adaptation_context::knowledge_candidates(state))
}

fn report_knowledge(
    state: &mut Session,
    entry: &Value,
    id: &str,
    tool: Option<&str>,
    progress: ProgressCallback<'_>,
) {
    if !state
        .reported_knowledge
        .insert((id.into(), tool.unwrap_or("").into()))
    {
        return;
    }
    let name = [
        entry["title"].as_str(),
        entry["name"].as_str(),
        entry["family"]["name"].as_str(),
    ]
    .into_iter()
    .flatten()
    .map(str::trim)
    .find(|name| !name.is_empty())
    .unwrap_or(id);
    let name: String = name
        .chars()
        .take(160)
        .collect::<String>()
        .replace(['\r', '\n'], " ");
    let label = if name == id {
        name
    } else {
        format!("{name}（{id}）")
    };
    if let Some(progress) = progress {
        progress(EngineProgress {
            stage: if tool.is_some() {
                "复用经验工具"
            } else {
                "参考引擎经验"
            }
            .into(),
            message: if let Some(tool) = tool {
                format!("已复制经验辅助工具：{label} / {tool}。")
            } else {
                format!("模型已读取引擎经验：{label}。")
            },
            usage: state.usage.clone(),
            status_only: false,
            tool_name: Some(
                if tool.is_some() {
                    "copy_engine_knowledge_tools"
                } else {
                    "inspect_engine_knowledge"
                }
                .into(),
            ),
            turn: state.total_turns,
            api_requests: Some(state.api_requests()),
            api_requests_estimated: state.api_requests_estimated(),
            progress_note: state.progress_note.clone(),
            last_breakthrough: state
                .extra
                .get("lastBreakthrough")
                .cloned()
                .unwrap_or_default(),
        });
    }
}

fn failed_result(
    state: &mut Session,
    error: &EngineError,
    summary: &str,
    diagnostics: Vec<String>,
) -> AdaptationResult {
    if matches!(error, EngineError::Cancelled) {
        return result(
            state,
            AdaptationStatus::Cancelled,
            "任务已取消，现有进度已保留。",
            diagnostics,
        );
    }
    let mut completed = result(state, AdaptationStatus::Failed, summary, diagnostics);
    let failure = AdaptationFailure::from_error(error);
    state.next_action.clone_from(&failure.next_action);
    completed.next_action.clone_from(&failure.next_action);
    completed.failure = Some(failure);
    completed
}

fn result(
    state: &mut Session,
    status: AdaptationStatus,
    summary: &str,
    diagnostics: Vec<String>,
) -> AdaptationResult {
    state.last_status = status;
    state.summary = summary.into();
    state.next_action = match status {
        AdaptationStatus::Activated | AdaptationStatus::Reused =>
            "选择少量文本试译，生成并安装补丁后进游戏确认显示效果。".into(),
        AdaptationStatus::WaitingForUser | AdaptationStatus::Unsupported if !state.next_action.is_empty() => state.next_action.clone(),
        AdaptationStatus::Incomplete if state.extra.get("blocker").is_some_and(|blocker|blocker["workRemaining"]==true) => state.next_action.clone(),
        AdaptationStatus::Unavailable => "使用带工具链的完整发行包，再继续适配。".into(),
        AdaptationStatus::Incomplete => "点击“继续适配”处理剩余问题，可补充具体要求；若模型反复只给总结，可在项目设置中更换分析 API 后继续。".into(),
        AdaptationStatus::Failed => "根据错误信息处理问题，再点击“继续适配”；已有代码和已提交步骤会继续使用。".into(),
        _ => "点击“继续适配”，沿用当前源码和进度，并设置本次分析预算。".into(),
    };
    AdaptationResult {
        status,
        adapter: None,
        usage: state.usage.clone(),
        api_requests: Some(state.api_requests()),
        api_requests_estimated: state.api_requests_estimated(),
        summary: state.summary.clone(),
        next_action: state.next_action.clone(),
        diagnostics,
        failure: None,
        pending_coverage: coverage_list(state, "pending"),
        pending_runtime_checks: coverage_list(state, "runtimeChecks"),
        progress_note: state.progress_note.clone(),
        last_breakthrough: state
            .extra
            .get("lastBreakthrough")
            .cloned()
            .unwrap_or_default(),
        blocker: state.extra.get("blocker").cloned().unwrap_or_default(),
        verification: state
            .verification
            .as_ref()
            .map(|verification| {
                json!({
                    "passed": verification.passed,
                    "originalRoundtripPassed": verification.original_roundtrip_passed,
                    "runtimeTested": verification.runtime_tested,
                    "diagnostics": verification.diagnostics,
                    "units": verification.units,
                })
            })
            .unwrap_or_default(),
        verified_extraction: if matches!(status, AdaptationStatus::Activated) {
            state
                .verification
                .as_mut()
                .filter(|verification| {
                    verification.passed && verification.original_roundtrip_passed
                })
                .and_then(|verification| verification.verified_extraction.take())
        } else {
            None
        },
    }
}
async fn save_session(path: &Path, state: &mut Session) -> Result<(), EngineError> {
    session_store::save(path, state).await
}
pub(crate) fn file_error(error: impl std::fmt::Display) -> EngineError {
    EngineError::Protocol(error.to_string())
}
fn trace(request: &AdaptationRequest<'_>, mut entry: Value) -> Result<(), EngineError> {
    let directory = request.project_directory.join("logs");
    fs::create_dir_all(&directory)?;
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(directory.join(format!(
            "adaptation-{}.jsonl",
            chrono::Local::now().format("%Y%m%d")
        )))?;
    let run_id = entry
        .as_object_mut()
        .and_then(|entry| entry.remove("runId"));
    serde_json::to_writer(
        &mut file,
        &json!({"schemaVersion":2,"runId":run_id,"at":chrono::Utc::now().to_rfc3339(),"turn":entry.get("turn").cloned().unwrap_or(json!(0)),"entry":entry}),
    )?;
    file.write_all(b"\n")?;
    Ok(())
}
pub(crate) fn text<'a>(args: &'a Value, key: &str) -> Result<&'a str, EngineError> {
    args.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| EngineError::Protocol(format!("缺少字符串参数：{key}")))
}
fn number(args: &Value, key: &str, default: u64, min: u64, max: u64) -> Result<u64, EngineError> {
    let value = args
        .get(key)
        .map(|value| {
            value
                .as_u64()
                .ok_or_else(|| EngineError::Protocol(format!("{key} 必须是整数。")))
        })
        .transpose()?
        .unwrap_or(default);
    if !(min..=max).contains(&value) {
        return Err(EngineError::Protocol(format!("{key} 超出允许范围。")));
    }
    Ok(value)
}
fn boolean(args: &Value, key: &str, default: bool) -> Result<bool, EngineError> {
    args.get(key)
        .map(|value| {
            value
                .as_bool()
                .ok_or_else(|| EngineError::Protocol(format!("{key} 必须为布尔值。")))
        })
        .transpose()
        .map(|value| value.unwrap_or(default))
}
fn write_source_file(path: &Path, content: &[u8]) -> Result<(), EngineError> {
    let parent = path
        .parent()
        .ok_or_else(|| EngineError::Protocol("文件路径缺少目录。".into()))?;
    fs::create_dir_all(parent)?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary.write_all(content)?;
    temporary
        .persist(path)
        .map_err(|error| EngineError::Io(error.error))?;
    Ok(())
}
fn strings(args: &Value, key: &str) -> Result<Vec<String>, EngineError> {
    let values: Vec<String> = serde_json::from_value(
        args.get(key)
            .cloned()
            .ok_or_else(|| EngineError::Protocol(format!("缺少数组参数：{key}")))?,
    )?;
    if values.len() > 1024 {
        return Err(EngineError::Protocol("数组参数过长。".into()));
    }
    Ok(values)
}
fn tool_root(
    request: &AdaptationRequest<'_>,
    workspace: &Path,
    args: &Value,
) -> Result<PathBuf, EngineError> {
    match args
        .get("root")
        .and_then(Value::as_str)
        .unwrap_or("workspace")
    {
        "game" | "installed" => Ok(request.game_directory.clone()),
        "workspace" => Ok(workspace.into()),
        _ => Err(EngineError::Protocol(
            "root 必须是 game、installed 或 workspace。".into(),
        )),
    }
}
fn tool_file(
    request: &AdaptationRequest<'_>,
    workspace: &Path,
    args: &Value,
    relative: &str,
) -> Result<PathBuf, EngineError> {
    let root = tool_root(request, workspace, args)?;
    if args["root"] == "game" {
        request.inputs.file(&root, relative)
    } else {
        resolve_relative(&root, relative)
    }
}
fn write_path(root: &Path, relative: &str) -> Result<PathBuf, EngineError> {
    let normalized = relative.replace('\\', "/");
    let relative = normalized.as_str();
    if !relative.starts_with("src/") && !relative.starts_with("work/") {
        return Err(EngineError::Protocol("只能修改 src/ 或 work/。".into()));
    }
    if [".cs", ".csproj"]
        .iter()
        .any(|extension| relative.to_ascii_lowercase().ends_with(extension))
    {
        return Err(EngineError::Rejected(
            "新适配器使用 Python，C# 只保留作参考资料。".into(),
        ));
    }
    resolve_relative(root, relative)
}
fn workspace_file(relative: &str) -> String {
    if relative.starts_with("tools/") {
        format!("src/{relative}")
    } else {
        relative.into()
    }
}
fn written_selection(
    state: &Session,
    relative: &str,
    args: &Value,
) -> Result<Vec<ReleaseFile>, EngineError> {
    let mut next = state.release_files.clone();
    if args.get("releaseRole").is_some() {
        if !relative.starts_with("src/") {
            return Err(EngineError::Protocol(
                "只有 src/ 中的正式实现或依赖可以登记发布；分析材料保留在 work/。".into(),
            ));
        }
        select_release(
            &mut next,
            ReleaseFile {
                path: if relative.starts_with("src/tools/") {
                    relative.trim_start_matches("src/").into()
                } else {
                    relative.into()
                },
                role: text(args, "releaseRole")?.into(),
                purpose: text(args, "purpose")?.into(),
            },
        )?;
    } else if relative.starts_with("src/")
        && !relative.starts_with("src/tools/")
        && relative.ends_with(".py")
    {
        select_release(
            &mut next,
            ReleaseFile {
                path: relative.into(),
                role: "source".into(),
                purpose: "可复用 Python 适配器源码。".into(),
            },
        )?;
    }
    Ok(next)
}
fn register_python(state: &mut Session, relative: &str) -> Result<(), EngineError> {
    register_release(
        state,
        ReleaseFile {
            path: relative.into(),
            role: "source".into(),
            purpose: "可复用 Python 适配器源码。".into(),
        },
    )
}
fn register_release(state: &mut Session, file: ReleaseFile) -> Result<(), EngineError> {
    select_release(&mut state.release_files, file)
}
fn select_release(files: &mut Vec<ReleaseFile>, file: ReleaseFile) -> Result<(), EngineError> {
    resolve_relative(Path::new("."), &file.path)?;
    if file.purpose.trim().is_empty()
        || file.purpose.encode_utf16().count() > 240
        || !matches!(
            file.role.as_str(),
            "source" | "runtime" | "asset" | "license"
        )
        || if file.role == "source" {
            !file.path.starts_with("src/")
                || file.path.starts_with("src/tools/")
                || !file.path.ends_with(".py")
        } else {
            !file.path.starts_with("tools/")
        }
    {
        return Err(EngineError::Protocol("发布路径、用途或角色无效。".into()));
    }
    if let Some(previous) = files
        .iter_mut()
        .find(|previous| previous.path.eq_ignore_ascii_case(&file.path))
    {
        *previous = file;
    } else {
        if files.len() >= 512 {
            return Err(EngineError::Protocol("最多选择 512 个发布输入。".into()));
        }
        files.push(file);
    }
    Ok(())
}
fn copy_unmodified(source: &Path, destination: &Path) -> Result<(), EngineError> {
    let content = fs::read(source)?;
    if destination.is_file() {
        if fs::read(destination)? != content {
            return Err(EngineError::Rejected(format!(
                "目标已有不同内容，请先检查现有实现：{}",
                destination.display()
            )));
        }
        return Ok(());
    }
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(destination, content)?;
    Ok(())
}
fn preflight_unmodified(path: &Path, content: &[u8]) -> Result<(), EngineError> {
    if path.is_dir() || (path.is_file() && fs::read(path)? != content) {
        return Err(EngineError::Rejected(format!(
            "目标已有不同内容，请先检查现有实现：{}",
            path.display()
        )));
    }
    let mut parent = path.parent();
    while let Some(directory) = parent {
        if directory.is_file() {
            return Err(EngineError::Rejected(format!(
                "目标目录被文件占用：{}",
                directory.display()
            )));
        }
        parent = directory.parent();
    }
    Ok(())
}
fn write_unmodified(path: &Path, content: &str) -> Result<(), EngineError> {
    if path.is_file() && fs::read(path)? != content.as_bytes() {
        return Err(EngineError::Rejected(
            "已有不同源码，复制操作未覆盖修改。".into(),
        ));
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, content)?;
    Ok(())
}
fn describe_engine_components(args: &Value) -> Result<Value, EngineError> {
    let mut components = crate::adaptation_assets::engine_components();
    if args.get("id").is_some() {
        let id = text(args, "id")?;
        components.retain(|component| component["id"] == id);
        if components.is_empty() {
            return Err(EngineError::Rejected(format!("未知引擎组件：{id}")));
        }
    }
    Ok(json!({
        "components":components,
        "dependencies":["Shared Python 3.14.10","Hash-pinned shared GalTransl library; standard library only, no pip or runtime downloads."],
        "limits":{"rpaArchiveMiB":2048,"rpaSelectedMemberMiB":64,"rpaSelectedTotalMiB":256,"xp3ArchiveMiB":512,"xp3MemberMiB":64,"xp3TotalDecodedMiB":256,"entries":10000,"memberNamesMiB":2},
        "input":"run_engine_component: id, operation, archive (resources/ or work/), output (new work/ directory). Optional members or lowercase extensions select extraction; rebuild replacements map exact member names to work/ files. XP3 requires filter=none.",
        "output":"Bounded summary plus work/<output>/manifest.json containing every member's name and length. RPA inspect reads only the index; extract reads selected payloads. Unread member SHA256 is null. verifyContents=true hashes every member with bounded buffers. readBytes records actual source reads. extract writes files/; rebuild writes rebuilt/<original filename>, verifies all members, and preserves original bytes when unchanged.",
        "usage":"Stage a real archive, then call the component directly; no Python wrapper or model-generated parser is needed. Use try_builtin_adapter on extracted scripts. Component success is structural evidence, not runtime or adapter acceptance."
    }))
}
fn validate_family(kind: &str, key: &str, name: &str) -> Result<(), EngineError> {
    if !matches!(kind, "engine" | "template")
        || key.is_empty()
        || key.len() > 80
        || !key
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || b".-".contains(&byte))
        || name.trim().is_empty()
    {
        return Err(EngineError::Protocol(
            "引擎家族必须使用 engine/template 和稳定格式键，不能使用游戏名。".into(),
        ));
    }
    Ok(())
}
fn workspace_fingerprint(workspace: &Path, state: &Session) -> Result<String, EngineError> {
    let files: BTreeMap<_, _> = state
        .release_files
        .iter()
        .map(|file| {
            Ok((
                file.path.clone(),
                hash_file(&resolve_relative(workspace, &workspace_file(&file.path))?)
                    .map_err(file_error)?,
            ))
        })
        .collect::<Result<_, EngineError>>()?;
    Ok(hash_text(&serde_json::to_string(
        &json!({"hostRevision":1,"files":files,"selection":state.release_files,"patterns":state.resource_patterns,"recognition":state.recognition,"family":state.family}),
    )?))
}
fn verification_context(request: &AdaptationRequest<'_>) -> String {
    hash_text(&json!({"objective":request.objective,"baseline":request.baseline_units}).to_string())
}
fn safe_call_id(id: &str) -> String {
    hash_text(id)[..32].into()
}
pub(crate) fn read_tool_result(
    workspace: &Path,
    state: &Session,
    args: &Value,
) -> Result<Value, EngineError> {
    let id = text(args, "toolCallId")?;
    let stored = resolve_relative(
        &workspace.join("results"),
        &format!("{}.json", safe_call_id(id)),
    )?;
    if stored.is_file() {
        let value: Value = read_json(&stored).map_err(file_error)?;
        let mut page = readback_page(&value.to_string(), args, 0, true)?;
        page["toolCallId"] = json!(id);
        return Ok(page);
    }
    let content = state.messages.iter().rev().find_map(|message| {
        let call_id = message
            .get("tool_call_id")
            .or_else(|| message.get("toolCallId"))
            .or_else(|| message.get("call_id"))
            .and_then(Value::as_str);
        if call_id != Some(id) {
            return None;
        }
        message
            .get("content")
            .or_else(|| message.get("output"))
            .and_then(Value::as_str)
    });
    let mut archived = None;
    if content.is_none() {
        session_store::visit(state, |_, message| {
            let call_id = message
                .get("tool_call_id")
                .or_else(|| message.get("toolCallId"))
                .or_else(|| message.get("call_id"))
                .and_then(Value::as_str);
            if call_id == Some(id) {
                archived = message
                    .get("content")
                    .or_else(|| message.get("output"))
                    .and_then(Value::as_str)
                    .map(str::to_owned);
            }
            Ok(true)
        })?;
    }
    let content = content.or(archived.as_deref()).ok_or_else(|| {
        EngineError::Rejected(
            "找不到所选工具结果；请从 read_session_history 核对原调用 ID。".into(),
        )
    })?;
    let mut page = readback_page(content, args, 0, true)?;
    page["toolCallId"] = json!(id);
    Ok(page)
}

fn read_session_history(state: &Session, args: &Value) -> Result<Value, EngineError> {
    let total = session_store::base(state) + state.messages.len();
    let start = number(args, "messageIndex", 0, 0, total as u64)? as usize;
    let query = optional_text(args, "query", "")?;
    if query.encode_utf16().count() > 256 {
        return Err(EngineError::Protocol("查询内容过长。".into()));
    }
    let mut result = None;
    session_store::visit(state, |index, message| {
        if index < start {
            return Ok(true);
        }
        let content = readable_message(message);
        let Some(found) = find_ordinal_ignore_case(&content, query, 0) else {
            return Ok(true);
        };
        let mut page = readback_page(&content, args, found.saturating_sub(160), false)?;
        page["found"] = json!(true);
        page["messageIndex"] = json!(index);
        page["nextMessageIndex"] = json!(index + 1);
        page["role"] = message.get("role").cloned().unwrap_or_else(|| {
            json!(if message["type"] == "function_call_output" {
                "tool"
            } else {
                "assistant"
            })
        });
        page["toolCallId"] = message
            .get("toolCallId")
            .or_else(|| message.get("tool_call_id"))
            .or_else(|| message.get("call_id"))
            .cloned()
            .unwrap_or(Value::Null);
        result = Some(page);
        Ok(false)
    })?;
    if let Some(page) = result {
        return Ok(page);
    }
    Ok(json!({"found":false,"nextMessageIndex":total,"totalMessages":total}))
}

fn optional_text<'a>(args: &'a Value, key: &str, default: &'a str) -> Result<&'a str, EngineError> {
    args.get(key)
        .map(|value| {
            value
                .as_str()
                .ok_or_else(|| EngineError::Protocol(format!("{key} 必须为文本。")))
        })
        .transpose()
        .map(|value| value.unwrap_or(default))
}

fn readable_message(message: &Value) -> String {
    fn append(output: &mut String, value: &Value) {
        match value {
            Value::Object(object) => {
                for (key, value) in object {
                    if key != "encrypted_content" {
                        output.push('\n');
                        output.push_str(key);
                        output.push(':');
                        append(output, value);
                    }
                }
            }
            Value::Array(values) => {
                for value in values {
                    append(output, value);
                }
            }
            Value::String(value) => {
                output.push('\n');
                output.push_str(value);
            }
            _ => output.push_str(&value.to_string()),
        }
    }
    let mut output = message
        .get("content")
        .or_else(|| message.get("output"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    if let Some(parts) = message.get("content").and_then(Value::as_array) {
        for part in parts {
            if let Some(text) = part.get("text").and_then(Value::as_str) {
                output.push_str(text);
            }
        }
    }
    if let Some(calls) = message
        .get("toolCalls")
        .or_else(|| message.get("tool_calls"))
        .filter(|calls| calls.as_array().is_some_and(|calls| !calls.is_empty()))
    {
        output.push('\n');
        output.push_str(&visible_json(calls).to_string());
    }
    if let Some(data) = message.get("providerData") {
        append(&mut output, data);
    }
    if message
        .get("type")
        .is_some_and(|kind| kind != "message" && kind != "function_call_output")
    {
        append(&mut output, message);
    }
    output
}

fn github_tree_page(
    document: &crate::research::ResearchDocument,
    url: &reqwest::Url,
    args: &Value,
    query: &str,
) -> Result<Value, EngineError> {
    let (owner, repository) = crate::research::github_git_repository(url, "trees")
        .ok_or_else(|| EngineError::Protocol("不是 GitHub Git tree 地址。".into()))?;
    let data: Value = serde_json::from_slice(&document.content)?;
    let tree = data["tree"]
        .as_array()
        .ok_or_else(|| EngineError::Protocol("GitHub 返回的资料不包含目录树。".into()))?;
    let upstream_truncated = data["truncated"]
        .as_bool()
        .ok_or_else(|| EngineError::Protocol("GitHub 目录树缺少完整性标记。".into()))?;
    let mut matches = Vec::new();
    for item in tree {
        let path = text(item, "path")?;
        if query.is_empty() || find_ordinal_ignore_case(path, query, 0).is_some() {
            matches.push(item);
        }
    }
    let offset = number(args, "offset", 0, 0, matches.len() as u64)? as usize;
    let length = number(args, "length", 50, 1, 100)? as usize;
    let next = matches.len().min(offset + length);
    let mut entries = Vec::new();
    for item in &matches[offset..next] {
        let kind = text(item, "type")?;
        let mut entry = json!({"path":item["path"],"type":kind,"size":item["size"]});
        if matches!(kind, "blob" | "tree") {
            let sha = text(item, "sha")?;
            if sha.len() != 40 || !sha.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return Err(EngineError::Protocol("GitHub 目录条目的 SHA 无效。".into()));
            }
            if kind == "blob" {
                // Git object IDs avoid ambiguous branch names and paths in raw URLs.
                entry["rawUrl"] = json!(format!(
                    "https://api.github.com/repos/{owner}/{repository}/git/blobs/{sha}"
                ));
            } else {
                entry["treeUrl"] = json!(format!(
                    "https://api.github.com/repos/{owner}/{repository}/git/trees/{sha}"
                ));
            }
        }
        entries.push(entry);
    }
    let more = next < matches.len();
    let mut result = json!({"url":document.url,"mode":"github_tree","paginationUnit":"entries","contentSha256":document.sha256,"retrievedAt":document.retrieved_at,"reused":document.reused,"stale":document.stale,"treeSha":data["sha"],"upstreamTruncated":upstream_truncated,"totalEntries":tree.len(),"matchedEntries":matches.len(),"query":query,"offset":offset,"nextOffset":next,"more":more,"complete":!more&&!upstream_truncated,"entries":entries});
    if upstream_truncated {
        result["guidance"] = json!(
            "GitHub truncated this tree; absent paths are not evidence of absence. Read subtree treeUrl values (omit recursive=1 to enumerate immediate children) and filter those trees. rawUrl reads raw blob bytes through fetch_url/download_tool."
        );
    }
    Ok(result)
}

fn find_ordinal_ignore_case(content: &str, query: &str, start: usize) -> Option<usize> {
    fn fold(value: char) -> char {
        let mut upper = value.to_uppercase();
        let first = upper.next().unwrap_or(value);
        if upper.next().is_none() { first } else { value }
    }
    if query.is_empty() {
        return Some(start);
    }
    let query: Vec<_> = query.chars().map(fold).collect();
    let mut offset = 0;
    for (byte, character) in content.char_indices() {
        if offset >= start {
            let mut remaining = content[byte..].chars();
            if query.iter().all(|expected| {
                remaining
                    .next()
                    .is_some_and(|actual| fold(actual) == *expected)
            }) {
                return Some(offset);
            }
        }
        offset += character.len_utf16();
    }
    None
}

fn readback_page(
    content: &str,
    args: &Value,
    default_offset: usize,
    search: bool,
) -> Result<Value, EngineError> {
    let units: Vec<_> = content.encode_utf16().collect();
    let mut offset = number(args, "offset", default_offset as u64, 0, units.len() as u64)? as usize;
    let length = number(args, "length", 4096, 1, 16384)? as usize;
    let query = optional_text(args, "query", "")?;
    if query.encode_utf16().count() > 256 {
        return Err(EngineError::Protocol("查询内容过长。".into()));
    }
    if search && !query.is_empty() {
        let Some(found) = find_ordinal_ignore_case(content, query, offset) else {
            return Ok(json!({"found":false,"totalCharacters":units.len()}));
        };
        offset = offset.max(found.saturating_sub(160));
    }
    let end = units.len().min(offset + length);
    Ok(
        json!({"offset":offset,"length":end-offset,"totalCharacters":units.len(),"more":end<units.len(),"content":String::from_utf16_lossy(&units[offset..end])}),
    )
}

#[cfg(test)]
fn preview(content: &str, maximum: usize, success: bool) -> String {
    if content.chars().count() <= maximum {
        content.into()
    } else {
        let tail: String = content
            .chars()
            .rev()
            .take(512)
            .collect::<String>()
            .chars()
            .rev()
            .collect();
        json!({
            "success":success,
            "truncated":true,
            "originalCharacters":content.chars().count(),
            "preview":content.chars().take(maximum.saturating_sub(1536)).collect::<String>(),
            "tail":tail,
            "readBack":"read_tool_result(toolCallId,offset,length,query) retrieves the retained original without rerunning the operation."
        }).to_string()
    }
}
pub(crate) fn page_text(content: &str, args: &Value) -> Value {
    let offset = args.get("offset").and_then(Value::as_u64).unwrap_or(0) as usize;
    let length = args
        .get("length")
        .and_then(Value::as_u64)
        .unwrap_or(6000)
        .clamp(1, 16000) as usize;
    let mut byte_offset = content
        .char_indices()
        .nth(offset)
        .map(|(index, _)| index)
        .unwrap_or(content.len());
    if let Some(query) = args.get("query").and_then(Value::as_str) {
        if let Some(index) = content[byte_offset..]
            .to_lowercase()
            .find(&query.to_lowercase())
        {
            byte_offset += index;
            while !content.is_char_boundary(byte_offset) {
                byte_offset = byte_offset.saturating_sub(1);
            }
        } else {
            return json!({"queryFound":false,"content":"","more":false});
        }
    }
    let offset = content[..byte_offset].chars().count();
    let text: String = content[byte_offset..].chars().take(length).collect();
    let next = offset + text.chars().count();
    json!({"content":text,"offset":offset,"nextOffset":next,"more":byte_offset+text.len()<content.len(),"queryFound":true})
}
#[cfg(test)]
fn replay(state: &Session) -> Vec<Value> {
    history::flatten(&history::for_request(&state.messages[state.history_start..]).messages)
}
fn normalize_messages(messages: &mut Vec<Value>) {
    for message in messages {
        let Some(object) = message.as_object_mut() else {
            continue;
        };
        if let Some(calls) = object.remove("toolCalls")
            && let Some(calls) = calls.as_array()
        {
            object.insert("tool_calls".into(),json!(calls.iter().map(|call|json!({"id":call["id"],"type":"function","function":{"name":call["name"],"arguments":call["arguments"]}})).collect::<Vec<_>>()));
        }
        if let Some(id) = object.remove("toolCallId")
            && !id.is_null()
        {
            object.insert("tool_call_id".into(), id);
        }
    }
}
fn coverage_list(state: &Session, key: &str) -> Vec<String> {
    state
        .text_coverage
        .get(key)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}
fn apply_coverage(state: &mut Session, args: &Value) -> Result<(), EngineError> {
    let mut coverage = if state.text_coverage.is_object() {
        state.text_coverage.clone()
    } else {
        json!({"samples":[],"pending":[],"runtimeChecks":[],"corrections":[]})
    };
    for key in ["samples", "pending", "runtimeChecks", "corrections"] {
        if coverage[key].is_null() {
            coverage[key] = json!([]);
        }
        if !coverage[key].is_array() {
            return Err(EngineError::Protocol("文字覆盖记录必须是数组。".into()));
        }
    }
    let original_samples = coverage["samples"].clone();
    if let Some(value) = args.get("coverageSampleCorrections") {
        let corrections = value
            .as_array()
            .ok_or_else(|| EngineError::Protocol("样本纠正必须是数组。".into()))?;
        if corrections.is_empty() || corrections.len() > 4 {
            return Err(EngineError::Protocol(
                "每次纠正 1 到 4 个明确误标的样本。".into(),
            ));
        }
        for correction in corrections {
            let reason = text(correction, "reason")?;
            let evidence = text(correction, "evidence")?;
            if reason.trim().is_empty()
                || reason.encode_utf16().count() > 600
                || evidence.trim().is_empty()
                || evidence.encode_utf16().count() > 2000
            {
                return Err(EngineError::Protocol(
                    "样本纠正需要明确的原样本、替代样本、原因和独立资源证据。".into(),
                ));
            }
            let original_key = coverage_sample_key(&correction["original"])?;
            let replacement_key = coverage_sample_key(&correction["replacement"])?;
            let samples = coverage["samples"]
                .as_array()
                .ok_or_else(|| EngineError::Protocol("覆盖锚点无效。".into()))?;
            if coverage["corrections"]
                .as_array()
                .is_some_and(|history| history.contains(correction))
                && samples.iter().any(|sample| {
                    coverage_sample_key(sample).ok().as_ref() == Some(&replacement_key)
                        && sample["sourceText"] == correction["replacement"]["sourceText"]
                })
            {
                continue;
            }
            let original_index = samples
                .iter()
                .position(|sample| {
                    coverage_sample_key(sample).ok().as_ref() == Some(&original_key)
                        && sample["sourceText"] == correction["original"]["sourceText"]
                })
                .ok_or_else(|| {
                    EngineError::Rejected("待纠正样本已变化或不存在，未修改验收记录。".into())
                })?;
            let target_index = samples.iter().position(|sample| {
                coverage_sample_key(sample).ok().as_ref() == Some(&replacement_key)
            });
            if let Some(index) = target_index.filter(|index| *index != original_index)
                && samples[index]["sourceText"] != correction["replacement"]["sourceText"]
            {
                return Err(EngineError::Rejected(
                    "替代位置已有不同的样本，未覆盖其他验收记录。".into(),
                ));
            }
            let samples = coverage["samples"]
                .as_array_mut()
                .ok_or_else(|| EngineError::Protocol("覆盖锚点无效。".into()))?;
            samples.remove(original_index);
            if target_index.is_none() || target_index == Some(original_index) {
                samples.push(correction["replacement"].clone());
            }
            let history = coverage["corrections"]
                .as_array_mut()
                .ok_or_else(|| EngineError::Protocol("样本纠正历史无效。".into()))?;
            if history.len() >= 8 {
                history.drain(..history.len() - 7);
            }
            history.push(correction.clone());
        }
    }
    for (arg, key) in [
        ("pendingCoverage", "pending"),
        ("runtimeChecks", "runtimeChecks"),
    ] {
        if let Some(value) = args.get(arg) {
            let list: Vec<String> = serde_json::from_value(value.clone())?;
            if list.len() > 24
                || list
                    .iter()
                    .any(|item| item.trim().is_empty() || item.encode_utf16().count() > 600)
                || list
                    .iter()
                    .map(|item| item.encode_utf16().count())
                    .sum::<usize>()
                    > 6000
            {
                return Err(EngineError::Protocol("覆盖或运行检查最多 24 项。".into()));
            }
            coverage[key] = json!(list);
        }
    }
    if let Some(value) = args.get("coverageSamples") {
        let samples = value
            .as_array()
            .ok_or_else(|| EngineError::Protocol("coverageSamples 必须是数组。".into()))?;
        for sample in samples {
            let location = coverage_sample_key(sample)?;
            let stored = coverage["samples"]
                .as_array_mut()
                .ok_or_else(|| EngineError::Protocol("已保存覆盖锚点无效。".into()))?;
            if let Some(existing) = stored
                .iter_mut()
                .find(|item| coverage_sample_key(item).ok().as_ref() == Some(&location))
            {
                *existing = sample.clone();
            } else {
                if stored.len() >= 64 {
                    return Err(EngineError::Protocol("覆盖锚点最多 64 项。".into()));
                }
                stored.push(sample.clone());
            }
        }
    }
    let samples = coverage["samples"]
        .as_array()
        .ok_or_else(|| EngineError::Protocol("覆盖锚点无效。".into()))?;
    let mut locations = BTreeSet::new();
    let mut total = 0;
    for sample in samples {
        if !locations.insert(coverage_sample_key(sample)?) {
            return Err(EngineError::Protocol("覆盖样本位置不能重复。".into()));
        }
        total += ["resourcePath", "locator", "sourceText"]
            .iter()
            .map(|key| {
                sample[*key]
                    .as_str()
                    .map(|value| value.encode_utf16().count())
                    .unwrap_or(0)
            })
            .sum::<usize>();
    }
    if samples.len() > 64 || total > 16000 {
        return Err(EngineError::Protocol(
            "覆盖锚点超过数量或总长度限制。".into(),
        ));
    }
    for key in ["pending", "runtimeChecks"] {
        let list: Vec<String> = serde_json::from_value(coverage[key].clone())?;
        if list.len() > 24
            || list
                .iter()
                .any(|item| item.trim().is_empty() || item.encode_utf16().count() > 600)
            || list
                .iter()
                .map(|item| item.encode_utf16().count())
                .sum::<usize>()
                > 6000
        {
            return Err(EngineError::Protocol(
                "覆盖或运行检查超过数量或长度限制。".into(),
            ));
        }
    }
    let corrections = coverage["corrections"]
        .as_array()
        .ok_or_else(|| EngineError::Protocol("样本纠正历史无效。".into()))?;
    if corrections.len() > 8 {
        return Err(EngineError::Protocol("样本纠正历史超过数量限制。".into()));
    }
    for correction in corrections {
        coverage_sample_key(&correction["original"])?;
        coverage_sample_key(&correction["replacement"])?;
        for (key, maximum) in [("reason", 600), ("evidence", 2000)] {
            let value = text(correction, key)?;
            if value.trim().is_empty() || value.encode_utf16().count() > maximum {
                return Err(EngineError::Protocol("样本纠正原因或证据无效。".into()));
            }
        }
    }
    if coverage["samples"] != original_samples {
        state.verified_fingerprint = None;
        state.verified_package = None;
        state.extra.remove("rustVerifiedCoverageFingerprint");
    }
    state.text_coverage = coverage;
    Ok(())
}
fn coverage_sample_key(sample: &Value) -> Result<(String, String), EngineError> {
    for (key, maximum) in [
        ("resourcePath", 512),
        ("locator", 512),
        ("sourceText", 2000),
    ] {
        let value = text(sample, key)?;
        if value.trim().is_empty() || value.encode_utf16().count() > maximum {
            return Err(EngineError::Protocol(
                "覆盖锚点的路径、定位与原文不能为空或超长。".into(),
            ));
        }
    }
    Ok((
        text(sample, "resourcePath")?.replace('\\', "/"),
        text(sample, "locator")?.into(),
    ))
}
fn require_builtin_companions(
    format: &str,
    source: &Path,
    files: &[String],
) -> Result<(), EngineError> {
    let catalog = crate::adaptation_context::builtin_catalog();
    let Some(definition) = catalog
        .iter()
        .find(|definition| definition["format"] == format)
    else {
        return Ok(());
    };
    let extensions = definition["extensions"]
        .as_array()
        .ok_or_else(|| EngineError::Protocol("内置格式定义无效。".into()))?;
    let companions = definition["companions"]
        .as_array()
        .ok_or_else(|| EngineError::Protocol("内置伴随文件定义无效。".into()))?;
    for file in files {
        let extension = Path::new(file)
            .extension()
            .and_then(|extension| extension.to_str())
            .map(|extension| format!(".{extension}"))
            .unwrap_or_default();
        if !extensions.iter().any(|candidate| {
            candidate
                .as_str()
                .is_some_and(|candidate| candidate.eq_ignore_ascii_case(&extension))
        }) {
            continue;
        }
        let mut required = format == "Softpal";
        if format == "Yuris" {
            use std::io::Read;
            let mut input = fs::File::open(resolve_relative(source, file)?)?;
            let mut signature = [0_u8; 4];
            required = input.read(&mut signature)? == 4 && signature == *b"YSTB";
            if !required {
                continue;
            }
        }
        for name in companions.iter().filter_map(Value::as_str) {
            let relative = Path::new(file)
                .parent()
                .unwrap_or_else(|| Path::new(""))
                .join(name)
                .to_string_lossy()
                .replace('\\', "/");
            let present = resolve_relative(source, &relative)?.is_file();
            if required && !present {
                return Err(EngineError::Rejected(format!(
                    "{file} 缺少同目录必需伴随文件 {relative}。"
                )));
            }
            if present
                && !files
                    .iter()
                    .any(|selected| selected.eq_ignore_ascii_case(&relative))
            {
                return Err(EngineError::Rejected(format!(
                    "解析 {file} 会读取伴随文件 {relative}；请将它一并加入 files，确保样本范围和大小可核验。"
                )));
            }
        }
    }
    Ok(())
}
fn validate_batch(calls: &[ToolCall]) -> Result<(), String> {
    let work: Vec<_> = calls
        .iter()
        .filter(|call| call.name != "report_progress")
        .collect();
    if work.len() <= 1 {
        return Ok(());
    }
    let reads = [
        "list_files",
        "read_file",
        "search_text",
        "find_game_file",
        "inspect_existing_text",
        "inspect_game_launch",
        "inspect_pe_imports",
        "inspect_pe_code",
        "inspect_unity_schema",
        "list_adapters",
        "inspect_builtin_adapter",
        "inspect_adapter_source",
        "inspect_adapter_component",
        "adapter_protocol",
        "inspect_engine_knowledge",
        "inspect_engine_component",
        "read_tool_result",
        "read_session_history",
        "search_engine_references",
        "search_repositories",
        "fetch_url",
    ];
    if work.len() <= 6 && work.iter().all(|call| reads.contains(&call.name.as_str())) {
        return Ok(());
    }
    if work.len() <= 8
        && work[..work.len() - 1]
            .iter()
            .all(|call| ["write_file", "replace_text"].contains(&call.name.as_str()))
        && [
            "write_file",
            "replace_text",
            "run_engine_python",
            "run_tool",
            "test_adapter",
        ]
        .contains(&work[work.len() - 1].name.as_str())
    {
        return Ok(());
    }
    let names: Vec<_> = work.iter().map(|call| call.name.as_str()).collect();
    if matches!(
        names.as_slice(),
        ["stage_resources", "compose_adapter_template"]
            | ["compose_adapter_template", "test_adapter"]
            | [
                "stage_resources",
                "compose_adapter_template",
                "test_adapter"
            ]
    ) {
        return Ok(());
    }
    Err("本批未执行：可批量最多 6 项独立读取；或顺序提交最多 8 项编辑，末项可为一次 run_engine_python/run_tool/test_adapter；或按 stage_resources → compose_adapter_template → test_adapter 顺序组合（首末可省略）。所有参数须已明确，前置失败即跳过后续。发布、checkpoint 和停止请单独调用。".into())
}

pub(crate) fn definition(
    name: &str,
    description: &str,
    properties: Value,
    required: &[&str],
) -> Value {
    json!({"type":"function","function":{"name":name,"description":description,"parameters":{"type":"object","properties":properties,"required":required,"additionalProperties":false}}})
}
#[derive(Deserialize)]
struct ToolCatalog {
    groups: Vec<ToolGroup>,
    tools: Vec<Value>,
}

#[derive(Deserialize)]
struct ToolGroup {
    name: String,
    tools: Vec<String>,
}

fn tool_catalog() -> Result<ToolCatalog, EngineError> {
    Ok(serde_json::from_str(include_str!(
        "../prompts/adaptation-tools.json"
    ))?)
}

fn tool_definitions() -> Result<Vec<Value>, EngineError> {
    Ok(tool_catalog()?.tools)
}

fn filter_tools(tools: &[Value], groups: &[Value]) -> Result<Vec<Value>, EngineError> {
    if groups.iter().any(|group| group.as_str() == Some("all")) {
        return Ok(tools.to_vec());
    }
    let catalog = tool_catalog()?;
    let requested: Vec<_> = groups.iter().filter_map(Value::as_str).collect();
    Ok(tools
        .iter()
        .filter(|tool| {
            let name = tool["function"]["name"].as_str().unwrap_or("");
            catalog
                .groups
                .iter()
                .find(|group| group.tools.iter().any(|tool| tool == name))
                .is_none_or(|group| requested.contains(&group.name.as_str()))
        })
        .cloned()
        .collect())
}

fn tool_group_context() -> &'static str {
    include_str!("../prompts/adaptation-tool-groups.md")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cache_reference(
        service: &EngineService,
        url: &str,
        content: &[u8],
        media_type: &str,
    ) -> chrono::DateTime<chrono::Utc> {
        let directory = service.shared_directory.join("downloads").join(
            crate::research::public_download_cache_key(
                &reqwest::Url::parse(url).expect("reference URL"),
            ),
        );
        fs::create_dir_all(&directory).expect("cache");
        let sha256 = gamel10n_translation::files::hash_bytes(content);
        fs::write(directory.join(format!("{sha256}.bin")), content).expect("cached content");
        let retrieved_at = chrono::Utc::now() - chrono::Duration::hours(1);
        let receipt = crate::research::DownloadReceipt {
            url: url.into(),
            sha256,
            bytes: content.len() as u64,
            retrieved_at,
            media_type: Some(media_type.into()),
        };
        write_json(&directory.join("source.json"), &receipt).expect("receipt");
        retrieved_at
    }

    #[tokio::test]
    async fn fetch_url_preserves_utf16_paging_search_and_upstream_metadata() {
        let root = tempfile::tempdir().expect("fixture");
        let (service, _, _, _) = fixture(root.path());
        let cancellation = CancellationToken::new();
        let url = "https://example.com/reference.txt";
        let retrieved = cache_reference(&service, url, "A😀B".as_bytes(), "text/plain");
        let page = service
            .fetch_url(&json!({"url":url,"offset":3,"length":1}), &cancellation)
            .await
            .expect("UTF16 page");
        assert_eq!(page["content"], "B");
        assert_eq!(page["totalCharacters"], 4);
        assert_eq!(page["retrievedAt"], json!(retrieved));
        assert_eq!(page["contentSha256"], hash_text("A😀B"));
        assert_eq!(page["reused"], true);
        for args in [
            json!({"offset":5}),
            json!({"length":24001}),
            json!({"query":"x".repeat(257)}),
            json!({"query":"long","length":3}),
        ] {
            let mut args = args;
            args["url"] = json!(url);
            assert!(service.fetch_url(&args, &cancellation).await.is_err());
        }
        cache_reference(&service, url, "a".repeat(30000).as_bytes(), "text/plain");
        let default_page = service
            .fetch_url(&json!({"url":url}), &cancellation)
            .await
            .expect("default page");
        assert_eq!(default_page["nextOffset"], 12000);
        let maximum_page = service
            .fetch_url(&json!({"url":url,"length":24000}), &cancellation)
            .await
            .expect("maximum page");
        assert_eq!(maximum_page["nextOffset"], 24000);
        let content = format!("😀{}", format!("Needle{}", "x".repeat(1000)).repeat(10));
        cache_reference(&service, url, content.as_bytes(), "text/plain");
        let matches = service
            .fetch_url(&json!({"url":url,"query":"needle"}), &cancellation)
            .await
            .expect("matches");
        assert_eq!(matches["matches"].as_array().expect("matches").len(), 8);
        assert_eq!(matches["matches"][0]["matchOffset"], 2);
        assert_eq!(matches["more"], true);
        let total: usize = matches["matches"]
            .as_array()
            .expect("matches")
            .iter()
            .map(|item| {
                item["content"]
                    .as_str()
                    .expect("excerpt")
                    .encode_utf16()
                    .count()
            })
            .sum();
        assert!(total <= 12000);
        let github = "https://api.github.com/repos/example/repo/git/trees/main";
        cache_reference(
            &service,
            github,
            br#"{"truncated":true,"tree":[]}"#,
            "application/json",
        );
        let page = service
            .fetch_url(&json!({"url":github}), &cancellation)
            .await
            .expect("GitHub page");
        assert_eq!(page["upstreamTruncated"], true);
        cache_reference(&service, github, b"invalid JSON", "application/json");
        assert!(
            service
                .fetch_url(&json!({"url":github}), &cancellation)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn fetch_url_tree_filters_paths_before_paging_and_returns_fixed_object_urls() {
        let root = tempfile::tempdir().expect("fixture");
        let (service, _, _, _) = fixture(root.path());
        let url = "https://api.github.com/repos/example/repo/git/trees/feature/source?recursive=1";
        let blob_sha = "a".repeat(40);
        let subtree_sha = "b".repeat(40);
        let tree = json!({"sha":"c".repeat(40),"truncated":false,"tree":[
            {"path":"other.txt","type":"blob","size":5,"sha":blob_sha,"description":"Yuris"},
            {"path":"src/Yuris.cs","type":"blob","size":12,"sha":blob_sha},
            {"path":"src/YURIS/Reader.cs","type":"blob","size":13,"sha":blob_sha},
            {"path":"src/Yuris","type":"tree","sha":subtree_sha}
        ]});
        let retrieved = cache_reference(
            &service,
            url,
            tree.to_string().as_bytes(),
            "application/json",
        );
        let cancellation = CancellationToken::new();
        let first = service
            .fetch_url(
                &json!({"url":url,"mode":"github_tree","query":"yuris","length":1}),
                &cancellation,
            )
            .await
            .expect("first matching path");
        assert_eq!(
            first["entries"],
            json!([{"path":"src/Yuris.cs","type":"blob","size":12,"rawUrl":format!("https://api.github.com/repos/example/repo/git/blobs/{blob_sha}")}])
        );
        assert_eq!(first["paginationUnit"], "entries");
        assert_eq!(first["matchedEntries"], 3);
        assert_eq!(first["totalEntries"], 4);
        assert_eq!(first["nextOffset"], 1);
        assert_eq!(first["more"], true);
        assert_eq!(first["retrievedAt"], json!(retrieved));
        assert_eq!(first["reused"], true);
        let last = service
            .fetch_url(
                &json!({"url":url,"mode":"github_tree","query":"yuris","offset":1,"length":2}),
                &cancellation,
            )
            .await
            .expect("last matching paths");
        assert_eq!(last["entries"][0]["path"], "src/YURIS/Reader.cs");
        assert_eq!(
            last["entries"][1]["treeUrl"],
            format!("https://api.github.com/repos/example/repo/git/trees/{subtree_sha}")
        );
        assert_eq!(last["nextOffset"], 3);
        assert_eq!(last["more"], false);
        assert_eq!(last["complete"], true);

        let raw_url = first["entries"][0]["rawUrl"].as_str().expect("raw URL");
        cache_reference(&service, raw_url, b"// raw source, not JSON", "text/plain");
        let source = service
            .fetch_url(&json!({"url":raw_url}), &cancellation)
            .await
            .expect("raw blob");
        assert_eq!(source["content"], "// raw source, not JSON");
        assert_eq!(source["upstreamTruncated"], false);
    }

    #[tokio::test]
    async fn fetch_url_tree_keeps_upstream_truncation_on_empty_last_page_and_rejects_wrong_urls() {
        let root = tempfile::tempdir().expect("fixture");
        let (service, _, _, _) = fixture(root.path());
        let url = "https://api.github.com/repos/example/repo/git/trees/main?recursive=1";
        cache_reference(
            &service,
            url,
            br#"{"truncated":true,"tree":[]}"#,
            "application/json",
        );
        let cancellation = CancellationToken::new();
        let result = service
            .fetch_url(
                &json!({"url":url,"mode":"github_tree","query":"missing"}),
                &cancellation,
            )
            .await
            .expect("truncated tree");
        assert_eq!(result["entries"], json!([]));
        assert_eq!(result["more"], false);
        assert_eq!(result["upstreamTruncated"], true);
        assert_eq!(result["complete"], false);
        assert!(
            result["guidance"]
                .as_str()
                .expect("guidance")
                .contains("subtree")
        );
        for wrong_url in [
            "https://example.com/repos/example/repo/git/trees/main",
            "https://api.github.com/repos/example/repo/contents/main",
            "https://api.github.com/repos/example/repo/git/trees/",
        ] {
            let error = service
                .fetch_url(
                    &json!({"url":wrong_url,"mode":"github_tree"}),
                    &cancellation,
                )
                .await
                .expect_err("tree URL required");
            assert!(error.to_string().contains("github_tree 仅支持"));
        }
    }

    #[tokio::test]
    async fn fetch_url_decodes_html_before_hashing_and_paging() {
        let root = tempfile::tempdir().expect("fixture");
        let (service, _, _, _) = fixture(root.path());
        let url = "https://example.com/reference.html";
        cache_reference(&service, url, "<style>discard</style><p>A&amp;😀 &#x4E2D;</p><script>ignore</script>\n<p>&unknown; &#x110000;</p>".as_bytes(), "text/html");
        let page = service
            .fetch_url(&json!({"url":url}), &CancellationToken::new())
            .await
            .expect("decoded HTML");
        let expected = " A&😀 中 \n &unknown; &#x110000; ";
        assert_eq!(page["content"], expected);
        assert_eq!(page["contentSha256"], hash_text(expected));
        assert_eq!(page["totalCharacters"], expected.encode_utf16().count());
    }

    #[tokio::test]
    async fn editor_tools_normalize_backslashes_and_remove_case_insensitive_release_entries() {
        let root = tempfile::tempdir().expect("fixture");
        let (service, request, workspace, mut state) = fixture(root.path());
        let cancellation = CancellationToken::new();
        state.total_turns = 7;
        service
            .adaptation_tool(
                &request,
                &workspace,
                &mut state,
                "write_file",
                &json!({"path":"src\\nested\\file.py","content":"old"}),
                &cancellation,
                None,
            )
            .await
            .expect("write");
        assert!(
            state
                .release_files
                .iter()
                .any(|file| file.path == "src/nested/file.py")
        );
        assert_eq!(state.extra["lastImplementationTurn"], 7);
        service
            .adaptation_tool(
                &request,
                &workspace,
                &mut state,
                "replace_text",
                &json!({"path":"src\\nested\\file.py","oldText":"old","newText":"new"}),
                &cancellation,
                None,
            )
            .await
            .expect("replace");
        assert_eq!(
            fs::read_to_string(workspace.join("src/nested/file.py")).expect("content"),
            "new"
        );
        service
            .adaptation_tool(
                &request,
                &workspace,
                &mut state,
                "remove_file",
                &json!({"path":"src\\NESTED\\FILE.py"}),
                &cancellation,
                None,
            )
            .await
            .expect("remove");
        assert!(!workspace.join("src/nested/file.py").exists());
        assert!(
            !state
                .release_files
                .iter()
                .any(|file| file.path.eq_ignore_ascii_case("src/nested/file.py"))
        );
        assert!(write_path(&workspace, "src\\..\\escape.py").is_err());
    }

    #[tokio::test]
    async fn dependency_binding_preserves_an_available_old_native_pin_after_tool_upgrade() {
        let root = tempfile::tempdir().expect("fixture");
        let assets = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        let paths = crate::ToolPaths::new(root.path().join("tools"));
        fs::create_dir_all(paths.root.join("python")).expect("runtime dir");
        let native = paths.root.join("vntextpatch");
        fs::create_dir_all(&native).expect("native dir");
        fs::write(paths.vntextpatch(), b"old native").expect("old native");
        fs::write(native.join("capstone.dll"), b"decoder").expect("decoder");
        write_json(
            &paths.root.join("python/runtime.json"),
            &json!({"archiveSha256":"fixture"}),
        )
        .expect("runtime");
        let service = EngineService::new(assets, paths);
        let pin = json!({"schemaVersion":2,"runtime":"native","files":{"GameL10n.VnTextPatch.exe":hash_file(&service.tools.vntextpatch()).expect("old hash")}});
        service
            .retain_native_tools(&native, &pin)
            .expect("retain old");
        fs::write(service.tools.vntextpatch(), b"new native").expect("upgrade");
        let workspace = root.path().join("workspace");
        let pin_path = workspace.join("src/tools/vntextpatch-reference.json");
        write_json(&pin_path, &pin).expect("old workspace pin");
        let original = fs::read(&pin_path).expect("original pin");
        let mut state = Session::default();
        service
            .bind_reference(
                &workspace,
                &mut state,
                Some(json!({"kind":"siglus","revision":"fixture"})),
                &CancellationToken::new(),
            )
            .await
            .expect("bind retained dependency");
        assert_eq!(fs::read(pin_path).expect("preserved pin"), original);
        assert!(
            state
                .release_files
                .iter()
                .any(|file| file.path == "tools/vntextpatch-reference.json")
        );
        assert!(workspace.join("src/tools/gamel10n_host.py").is_file());
    }

    #[tokio::test]
    async fn dependency_binding_preflights_every_file_and_preserves_release_selection_on_conflict()
    {
        let root = tempfile::tempdir().expect("fixture");
        let assets = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        let paths = crate::ToolPaths::new(root.path().join("tools"));
        fs::create_dir_all(paths.root.join("python")).expect("runtime dir");
        fs::create_dir_all(paths.vntextpatch().parent().expect("native dir")).expect("native dir");
        write_json(
            &paths.root.join("python/runtime.json"),
            &json!({"archiveSha256":"fixture"}),
        )
        .expect("runtime");
        fs::write(paths.vntextpatch(), b"fixture native bytes").expect("native pin input");
        fs::write(
            paths.root.join("vntextpatch/capstone.dll"),
            b"fixture decoder",
        )
        .expect("native companion");
        let service = EngineService::new(assets, paths);
        let workspace = root.path().join("workspace");
        fs::create_dir_all(workspace.join("src/tools")).expect("workspace");
        let conflicting = workspace.join("src/tools/vntextpatch-reference.json");
        write_json(&conflicting, &json!({"different":true})).expect("conflict");
        let mut state = Session::default();
        let reference = json!({"kind":"siglus","revision":"fixture"});
        assert!(
            service
                .bind_reference(
                    &workspace,
                    &mut state,
                    Some(reference.clone()),
                    &CancellationToken::new()
                )
                .await
                .is_err()
        );
        assert!(!workspace.join("src/tools/engine-reference.json").exists());
        assert!(!workspace.join("src/tools/gamel10n_host.py").exists());
        assert!(state.release_files.is_empty());
        fs::remove_file(conflicting).expect("clear conflict");
        fs::write(
            workspace.join("src/tools/gamel10n_host.py"),
            "custom bridge",
        )
        .expect("bridge conflict");
        assert!(
            service
                .bind_reference(
                    &workspace,
                    &mut state,
                    Some(reference),
                    &CancellationToken::new()
                )
                .await
                .is_err()
        );
        assert!(!workspace.join("src/tools/engine-reference.json").exists());
        assert!(
            !workspace
                .join("src/tools/vntextpatch-reference.json")
                .exists()
        );
        assert!(state.release_files.is_empty());
    }
    use crate::ToolPaths;

    #[test]
    fn knowledge_helpers_use_revision_paths_and_check_every_conflict_before_copying() {
        let root = tempfile::tempdir().expect("fixture");
        let files = json!({"a.py":"first","b.py":"second"});
        let revision = "0123456789abcdef";
        let target = root
            .path()
            .join("work/engine-knowledge/engine.fixture.variant/0123456789ab");
        fs::create_dir_all(&target).expect("fixture");
        fs::write(target.join("b.py"), "custom").expect("local source");
        assert!(
            copy_knowledge_sources(
                root.path(),
                "engine.fixture.variant",
                revision,
                files.as_object().expect("sources")
            )
            .is_err()
        );
        assert!(!target.join("a.py").exists());
        assert_eq!(
            fs::read_to_string(target.join("b.py")).expect("source"),
            "custom"
        );
        fs::write(target.join("b.py"), "second").expect("matching source");
        let copied = copy_knowledge_sources(
            root.path(),
            "engine.fixture.variant",
            revision,
            files.as_object().expect("sources"),
        )
        .expect("copy");
        assert_eq!(
            copied[0],
            "work/engine-knowledge/engine.fixture.variant/0123456789ab/a.py"
        );
    }

    #[test]
    fn archive_component_inspection_selects_one_component_and_keeps_the_budget_contract() {
        let description =
            describe_engine_components(&json!({"id":"archive.xp3"})).expect("component");
        assert_eq!(
            description["components"]
                .as_array()
                .expect("components")
                .len(),
            1
        );
        assert_eq!(
            description["components"][0]["module"],
            "python.archives.xp3"
        );
        assert!(
            description["components"][0]["limitations"]
                .as_array()
                .expect("limitations")
                .iter()
                .any(|item| item
                    .as_str()
                    .is_some_and(|text| text.contains("zero flags")))
        );
        assert_eq!(description["limits"]["rpaArchiveMiB"], 2048);
        assert!(describe_engine_components(&json!({"id":"unknown"})).is_err());
    }

    #[tokio::test]
    async fn published_source_reuse_preserves_existing_code_identity_and_pins() {
        let root = tempfile::tempdir().expect("fixture");
        let (service, mut request, workspace, mut state) = fixture(root.path());
        let package = published_reuse_fixture(root.path());
        request.package_directories.push(package.clone());
        state.family = json!({"kind":"engine","key":"current-variant"});
        state
            .extra
            .insert("dependencyPins".into(), json!({"parser":"current"}));
        let before = serde_json::to_value(&state).expect("state");
        let entry = fs::read(workspace.join("src/adapter.py")).expect("current entry");
        let cancellation = CancellationToken::new();
        let base = json!({"id":"engine.reuse-fixture","version":"1"});
        let mut page_args = base.clone();
        page_args["query"] = json!("src/");
        page_args["length"] = json!(1);
        let index = service
            .adaptation_tool(
                &request,
                &workspace,
                &mut state,
                "inspect_adapter_source",
                &page_args,
                &cancellation,
                None,
            )
            .await
            .expect("index");
        assert_eq!(index["fileCount"], 2);
        assert_eq!(index["more"], true);
        assert!(index.get("content").is_none());
        page_args["offset"] = index["nextOffset"].clone();
        let next = published_reuse::inspect(&request, &page_args).expect("next index page");
        assert_eq!(next["files"][0]["path"], "src/text.py");
        let mut source_args = base.clone();
        source_args["path"] = json!("src/text.py");
        source_args["query"] = json!("def decode");
        source_args["length"] = json!(256);
        let source = service
            .adaptation_tool(
                &request,
                &workspace,
                &mut state,
                "inspect_adapter_source",
                &source_args,
                &cancellation,
                None,
            )
            .await
            .expect("source slice");
        assert!(source["offset"].as_u64().expect("offset") > 1000);
        assert!(
            source["content"]
                .as_str()
                .expect("source")
                .contains("def decode")
        );
        assert!(source["length"].as_u64().expect("length") <= 256);
        let mut copy_args = base;
        copy_args["files"] = json!(["src/text.py"]);
        let copied = service
            .adaptation_tool(
                &request,
                &workspace,
                &mut state,
                "copy_adapter",
                &copy_args,
                &cancellation,
                None,
            )
            .await
            .expect("selected source");
        let relative = copied["files"][0]["path"].as_str().expect("reference path");
        assert!(relative.starts_with("work/adapter-references/"));
        assert_eq!(
            fs::read(workspace.join(relative)).expect("copy"),
            fs::read(package.join("src/text.py")).expect("source")
        );
        assert_eq!(copied["referenceOnly"], true);
        service
            .copy_adapter(&request, &workspace, &mut state, &copy_args, &cancellation)
            .await
            .expect("repeat copy");
        assert_eq!(serde_json::to_value(&state).expect("state"), before);
        assert_eq!(
            fs::read(workspace.join("src/adapter.py")).expect("entry"),
            entry
        );
        assert!(!workspace.join("src/text.py").exists());
        let options = service
            .knowledge_reuse_options(
                &request,
                &json!({"family":{"kind":"engine","key":"reuse-fixture"}}),
            )
            .expect("reuse options");
        assert_eq!(
            options["publishedAdapters"][0]["inspectCall"]["tool"],
            "inspect_adapter_source"
        );
    }

    fn published_reuse_fixture(root: &Path) -> PathBuf {
        let package = root.join("published");
        fs::create_dir_all(package.join("src")).expect("package");
        fs::write(package.join("src/adapter.py"), "# published entry\n").expect("entry");
        fs::write(
            package.join("src/text.py"),
            format!(
                "{}def decode(data):\n    return data # 中文😀\n",
                "# context\n".repeat(2048)
            ),
        )
        .expect("module");
        write_json(&package.join("package.json"), &json!({
            "id":"engine.reuse-fixture","version":"1","manifestVersion":4,"protocolVersion":1,
            "family":{"kind":"engine","key":"reuse-fixture"},
            "packageFiles":{
                "src/adapter.py":hash_file(&package.join("src/adapter.py")).expect("hash"),
                "src/text.py":hash_file(&package.join("src/text.py")).expect("hash"),
                "tools/unselected-font.otf":hash_text("not needed to inspect or reference this module")
            }
        })).expect("manifest");
        package
    }

    #[tokio::test]
    async fn published_reference_copy_checks_selection_before_writing_and_preserves_edits() {
        let root = tempfile::tempdir().expect("fixture");
        let (service, mut request, workspace, mut state) = fixture(root.path());
        let package = published_reuse_fixture(root.path());
        request.package_directories.push(package.clone());
        let cancellation = CancellationToken::new();
        let base = json!({"id":"engine.reuse-fixture","version":"1"});
        for path in ["../outside.py", "src/not-registered.py"] {
            let mut args = base.clone();
            args["files"] = json!(["src/adapter.py", path]);
            assert!(
                service
                    .copy_adapter(&request, &workspace, &mut state, &args, &cancellation)
                    .await
                    .is_err()
            );
            assert!(!workspace.join("work/adapter-references").exists());
        }
        let original = fs::read(package.join("src/text.py")).expect("source");
        fs::write(package.join("src/text.py"), "changed published source").expect("change");
        let mut args = base.clone();
        args["files"] = json!(["src/adapter.py", "src/text.py"]);
        let error = service
            .copy_adapter(&request, &workspace, &mut state, &args, &cancellation)
            .await
            .expect_err("changed source");
        assert!(error.to_string().contains("适配器文件已变化"));
        assert!(!workspace.join("work/adapter-references").exists());
        let mut read_args = base.clone();
        read_args["path"] = json!("src/text.py");
        assert!(published_reuse::inspect(&request, &read_args).is_err());
        fs::write(package.join("src/text.py"), original).expect("restore fixture");
        args["files"] = json!(["src/text.py"]);
        let first = service
            .copy_adapter(&request, &workspace, &mut state, &args, &cancellation)
            .await
            .expect("initial copy");
        let path = workspace.join(first["files"][0]["path"].as_str().expect("path"));
        fs::write(&path, "local reference edits").expect("local edit");
        args["files"] = json!(["src/adapter.py", "src/text.py"]);
        assert!(
            service
                .copy_adapter(&request, &workspace, &mut state, &args, &cancellation)
                .await
                .is_err()
        );
        assert_eq!(
            fs::read_to_string(&path).expect("reference"),
            "local reference edits"
        );
        assert!(!path.with_file_name("adapter.py").exists());
    }

    #[tokio::test]
    async fn adapter_copy_rejects_existing_source_and_tampered_dependencies_before_writing() {
        let root = tempfile::tempdir().expect("fixture");
        let (service, mut request, workspace, mut state) = fixture(root.path());
        fs::remove_dir(workspace.join("src/tools")).expect("empty fixture tools");
        let cancellation = CancellationToken::new();
        for (id, version) in [
            ("engine.siglus", "1.0.0"),
            ("engine.vntextpatch-renpy", "1.0.1"),
        ] {
            let error = service
                .copy_adapter(
                    &request,
                    &workspace,
                    &mut state,
                    &json!({"id":id,"version":version}),
                    &cancellation,
                )
                .await
                .expect_err("existing source");
            assert!(error.to_string().contains("已有不同内容"));
            assert!(!workspace.join("src/tools").exists());
        }
        let package = root.path().join("published");
        fs::create_dir_all(package.join("src")).expect("package source");
        fs::create_dir_all(package.join("tools")).expect("package tools");
        fs::write(package.join("src/adapter.py"), "print('{}')").expect("entry");
        fs::write(package.join("tools/runtime.py"), "tampered").expect("dependency");
        write_json(&package.join("package.json"), &json!({"id":"engine.copy-fixture","version":"1","manifestVersion":4,"protocolVersion":1,"packageFiles":{"src/adapter.py":hash_file(&package.join("src/adapter.py")).expect("hash"),"tools/runtime.py":hash_text("expected")}})).expect("manifest");
        request.package_directories = vec![package];
        let args = json!({"id":"engine.copy-fixture","version":"1"});
        assert!(
            service
                .copy_adapter(&request, &workspace, &mut state, &args, &cancellation)
                .await
                .expect_err("nonempty src")
                .to_string()
                .contains("src 已包含代码")
        );
        fs::remove_file(workspace.join("src/adapter.py")).expect("clear fixture source");
        assert!(
            service
                .copy_adapter(&request, &workspace, &mut state, &args, &cancellation)
                .await
                .expect_err("tamper")
                .to_string()
                .contains("适配器文件已变化")
        );
        assert!(!workspace.join("src/adapter.py").exists());
        assert!(!workspace.join("src/tools").exists());
    }

    #[tokio::test]
    async fn downloaded_tools_preserve_local_changes_and_register_source_provenance() {
        let root = tempfile::tempdir().expect("fixture");
        let (service, request, workspace, mut state) = fixture(root.path());
        let path = workspace.join("src/tools/probe.py");
        fs::write(&path, "print('original')").expect("tool");
        let digest = hash_file(&path).expect("hash");
        write_json(
            &workspace.join("src/tools/sources.json"),
            &json!({"probe.py":{"url":"https://example.com/probe.py","sha256":digest}}),
        )
        .expect("provenance");
        let args = json!({"path":"src/tools/probe.py","url":"https://example.com/probe.py","releaseRole":"runtime","purpose":"test helper"});
        let cancellation = CancellationToken::new();
        let events = std::sync::Mutex::new(Vec::new());
        let report = |event| events.lock().expect("events").push(event);
        let result = service
            .adaptation_tool(
                &request,
                &workspace,
                &mut state,
                "download_tool",
                &args,
                &cancellation,
                Some(&report),
            )
            .await
            .expect("local reuse");
        assert_eq!(result["reused"], true);
        assert_eq!(result["source"], "project");
        assert_eq!(result["releaseRegistered"], true);
        {
            let events = events.lock().expect("events");
            assert_eq!(events.len(), 1);
            assert_eq!(events[0].stage, "复用本地工具");
            assert!(events[0].message.contains("src/tools/probe.py"));
            assert!(events[0].message.contains("https://example.com/probe.py"));
        }
        assert!(
            state
                .release_files
                .iter()
                .any(|file| file.path == "tools/sources.json" && file.role == "license")
        );
        fs::write(&path, "print('local changes')").expect("edit");
        let error = service
            .adaptation_tool(
                &request,
                &workspace,
                &mut state,
                "download_tool",
                &args,
                &cancellation,
                Some(&report),
            )
            .await
            .expect_err("protect local source");
        assert!(error.to_string().contains("已有本地修改"));
        assert_eq!(events.lock().expect("events").len(), 1);
        assert_eq!(
            fs::read_to_string(path).expect("source"),
            "print('local changes')"
        );
    }

    #[tokio::test]
    async fn downloaded_tools_report_shared_cache_once_and_never_report_cancelled_success() {
        let root = tempfile::tempdir().expect("fixture");
        let (service, request, workspace, mut state) = fixture(root.path());
        let url = "https://example.com/shared-probe.py";
        cache_reference(&service, url, b"print('cached')", "text/plain");
        let args = json!({"url":url,"path":"work/shared-probe.py"});
        let events = std::sync::Mutex::new(Vec::new());
        let report = |event| events.lock().expect("events").push(event);
        let cancellation = CancellationToken::new();
        let result = service
            .adaptation_tool(
                &request,
                &workspace,
                &mut state,
                "download_tool",
                &args,
                &cancellation,
                Some(&report),
            )
            .await
            .expect("shared cache restore");
        assert_eq!(result["source"], "shared_cache");
        assert_eq!(result["reused"], true);
        assert_eq!(
            fs::read_to_string(workspace.join("work/shared-probe.py")).expect("restored file"),
            "print('cached')"
        );
        {
            let events = events.lock().expect("events");
            assert_eq!(events.len(), 1);
            assert_eq!(events[0].stage, "命中下载缓存");
            assert!(events[0].message.contains("work/shared-probe.py"));
            assert!(events[0].message.contains(url));
        }
        cancellation.cancel();
        let error = service
            .adaptation_tool(
                &request,
                &workspace,
                &mut state,
                "download_tool",
                &args,
                &cancellation,
                Some(&report),
            )
            .await
            .expect_err("cancelled reuse");
        assert!(matches!(error, EngineError::Cancelled));
        assert_eq!(events.lock().expect("events").len(), 1);
    }

    #[test]
    fn readback_uses_utf16_offsets_and_keeps_original_function_arguments() {
        let state = Session {
            messages: vec![
                json!({"role":"assistant","content":"earlier"}),
                json!({"type":"function_call","name":"run_engine_python","call_id":"call-original","arguments":"{\"script\":\"work/inspect.py\"}","encrypted_content":"private"}),
            ],
            ..Session::default()
        };
        let page = read_session_history(&state, &json!({"query":"work/inspect.py"}))
            .expect("history read");
        assert_eq!(page["messageIndex"], 1);
        let content = page["content"].as_str().expect("content");
        assert!(
            content.contains("call-original")
                && content.contains("run_engine_python")
                && content.contains("work/inspect.py")
        );
        assert!(!content.contains("private") && !content.contains("encrypted_content"));
        let page = readback_page("a😀b中", &json!({"offset":3,"length":2}), 0, true).expect("page");
        assert_eq!(
            page,
            json!({"offset":3,"length":2,"totalCharacters":5,"more":false,"content":"b中"})
        );
        assert!(readback_page("abc", &json!({"offset":4}), 0, true).is_err());
        assert!(readback_page("abc", &json!({"length":16385}), 0, true).is_err());
    }

    #[test]
    fn coverage_corrections_are_transactional_idempotent_and_update_locations() {
        let mut state = Session::default();
        let original = json!({"resourcePath":"a.bin","locator":"1","sourceText":"old"});
        let replacement = json!({"resourcePath":"a.bin","locator":"2","sourceText":"correct"});
        apply_coverage(&mut state,&json!({"coverageSamples":[original.clone(),{"resourcePath":"a.bin","locator":"2","sourceText":"conflict"}]})).expect("coverage");
        let correction = json!({"original":original,"replacement":replacement,"reason":"wrong anchor","evidence":"independent resource bytes"});
        let before = state.text_coverage.clone();
        assert!(
            apply_coverage(
                &mut state,
                &json!({"coverageSampleCorrections":[correction.clone()]})
            )
            .is_err()
        );
        assert_eq!(state.text_coverage, before);
        apply_coverage(&mut state, &json!({"coverageSamples":[replacement]}))
            .expect("update by location");
        apply_coverage(
            &mut state,
            &json!({"coverageSampleCorrections":[correction.clone()]}),
        )
        .expect("correction");
        let corrected = state.text_coverage.clone();
        apply_coverage(
            &mut state,
            &json!({"coverageSampleCorrections":[correction]}),
        )
        .expect("idempotent");
        assert_eq!(state.text_coverage, corrected);
        assert_eq!(
            state.text_coverage["samples"]
                .as_array()
                .expect("samples")
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn source_registration_errors_preserve_files_and_noop_writes_keep_verification() {
        let root = tempfile::tempdir().expect("fixture");
        let (service, request, workspace, mut state) = fixture(root.path());
        let cancellation = CancellationToken::new();
        let path = workspace.join("src/adapter.py");
        let original = fs::read_to_string(&path).expect("source");
        state.verified_fingerprint = Some("verified".into());
        let result = service
            .adaptation_tool(
                &request,
                &workspace,
                &mut state,
                "write_file",
                &json!({"path":"src/adapter.py","content":original}),
                &cancellation,
                None,
            )
            .await
            .expect("noop");
        assert_eq!(result["changed"], false);
        assert_eq!(state.verified_fingerprint.as_deref(), Some("verified"));
        let failed=service.adaptation_tool(&request,&workspace,&mut state,"write_file",&json!({"path":"src/adapter.py","content":"changed","releaseRole":"runtime","purpose":"bad source role"}),&cancellation,None).await;
        assert!(failed.is_err());
        assert_eq!(fs::read_to_string(&path).expect("source"), original);
        assert!(write_path(&workspace, "work/probe.CSPROJ").is_err());
    }

    fn batch_calls(names: &[&str]) -> Vec<ToolCall> {
        names
            .iter()
            .enumerate()
            .map(|(index, name)| ToolCall {
                id: format!("call-{index}"),
                name: (*name).into(),
                arguments: "{}".into(),
            })
            .collect()
    }

    #[test]
    fn native_inspection_and_reference_research_can_share_read_batches() {
        for names in [
            vec![
                "inspect_pe_imports",
                "inspect_pe_code",
                "inspect_unity_schema",
                "read_file",
            ],
            vec![
                "search_repositories",
                "fetch_url",
                "search_engine_references",
                "adapter_protocol",
            ],
        ] {
            assert!(validate_batch(&batch_calls(&names)).is_ok(), "{names:?}");
        }
    }

    #[test]
    fn read_batches_stop_at_six_operations_and_allow_progress_reports() {
        let mut names = vec!["fetch_url"; 6];
        names.push("report_progress");
        assert!(validate_batch(&batch_calls(&names)).is_ok());
        names.push("read_file");
        assert!(validate_batch(&batch_calls(&names)).is_err());
    }

    #[test]
    fn write_batches_count_the_final_run_or_verification_toward_eight_operations() {
        for run in ["run_engine_python", "run_tool", "test_adapter"] {
            let mut names = vec!["write_file"; 7];
            names.push(run);
            assert!(validate_batch(&batch_calls(&names)).is_ok(), "{run}");
            names.insert(0, "replace_text");
            assert!(validate_batch(&batch_calls(&names)).is_err(), "{run}");
        }
        assert!(validate_batch(&batch_calls(&["replace_text"; 8])).is_ok());
    }

    #[test]
    fn dependent_reads_publication_and_checkpoints_cannot_mix_with_writes() {
        for names in [
            vec!["write_file", "read_file"],
            vec!["run_engine_python", "write_file"],
            vec!["write_file", "run_engine_python", "run_tool"],
            vec!["stage_resources", "read_file"],
            vec!["write_file", "finish"],
            vec!["read_file", "checkpoint"],
            vec!["fetch_url", "inspect_engine_reference"],
            vec!["inspect_siglus_game", "read_file"],
        ] {
            assert!(validate_batch(&batch_calls(&names)).is_err(), "{names:?}");
        }
    }

    #[test]
    fn template_batches_allow_only_explicit_preparation_composition_and_verification() {
        for names in [
            vec!["stage_resources", "compose_adapter_template"],
            vec!["compose_adapter_template", "test_adapter"],
            vec![
                "stage_resources",
                "compose_adapter_template",
                "test_adapter",
            ],
        ] {
            assert!(validate_batch(&batch_calls(&names)).is_ok(), "{names:?}");
        }
        for names in [
            vec!["stage_resources", "test_adapter"],
            vec!["compose_adapter_template", "stage_resources"],
            vec!["compose_adapter_template", "finish"],
            vec!["test_adapter", "write_file"],
        ] {
            assert!(validate_batch(&batch_calls(&names)).is_err(), "{names:?}");
        }
    }

    #[test]
    fn tool_catalog_keeps_legacy_argument_contracts_and_optional_groups() {
        let tools = tool_definitions().expect("embedded catalog");
        let properties = |name| {
            &tools
                .iter()
                .find(|tool| tool["function"]["name"] == name)
                .expect("tool")["function"]["parameters"]["properties"]
        };
        assert_eq!(properties("list_files")["limit"]["maximum"], 200);
        assert!(properties("list_files").get("path").is_some());
        assert!(
            properties("try_builtin_adapter")
                .get("checkRoundtrip")
                .is_some()
        );
        assert!(properties("run_engine_component").get("output").is_some());
        assert!(
            properties("copy_engine_knowledge_tools")
                .get("tool")
                .is_some()
        );
        assert_eq!(
            properties("checkpoint")["coverageSampleCorrections"]["maxItems"],
            4
        );
        let names = |selected: Vec<Value>| {
            selected
                .into_iter()
                .map(|tool| tool["function"]["name"].as_str().expect("name").to_owned())
                .collect::<Vec<_>>()
        };
        let basic = names(filter_tools(&tools, &[]).expect("basic tools"));
        assert!(basic.contains(&"list_adapters".into()));
        assert!(!basic.contains(&"run_tool".into()));
        let development =
            names(filter_tools(&tools, &[json!("development")]).expect("development tools"));
        assert!(development.contains(&"run_tool".into()));
        assert!(!development.contains(&"inspect_pe_code".into()));
        assert_eq!(
            filter_tools(&tools, &[json!("all")]).expect("all tools"),
            tools
        );
    }

    fn fixture(root: &Path) -> (EngineService, AdaptationRequest<'static>, PathBuf, Session) {
        let assets = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        let service = EngineService::new(&assets, ToolPaths::development())
            .with_shared_directory(root.join("shared"));
        let game = root.join("game");
        fs::create_dir_all(&game).expect("fixture");
        fs::write(
            game.join("text.json"),
            "[\"原文\",\"second\",\"third\",\"fourth\"]\n",
        )
        .expect("fixture");
        let workspace = root.join("project/adaptation/current");
        fs::create_dir_all(workspace.join("src/tools")).expect("fixture");
        fs::create_dir_all(workspace.join("resources")).expect("fixture");
        fs::copy(
            game.join("text.json"),
            workspace.join("resources/text.json"),
        )
        .expect("staged fixture");
        fs::write(
            workspace.join("src/adapter.py"),
            include_str!("../tests/fixtures/json_adapter.py"),
        )
        .expect("fixture");
        let mut session = Session::default();
        session.extra.insert(
            "resourceHashes".into(),
            json!({"text.json":hash_file(&game.join("text.json")).expect("hash")}),
        );
        register_python(&mut session, "src/adapter.py").expect("selection");
        let request = AdaptationRequest {
            game_id: "fixture".into(),
            game_directory: game,
            inputs: AdaptationInputs::default(),
            project_directory: root.join("project"),
            package_directory: root.join("extensions"),
            package_directories: vec![],
            user_instruction: String::new(),
            objective: "Develop".into(),
            improve_existing: false,
            require_model_analysis: false,
            reuse_failures: vec![],
            builtin_probes: vec![],
            discovery_inventory: None,
            limits: AdaptationLimits::default(),
            baseline_units: &[],
        };
        (service, request, workspace, session)
    }

    #[tokio::test]
    async fn siglus_inspection_uses_bound_resources_when_installed_files_are_missing() {
        let root = tempfile::tempdir().expect("fixture");
        let (service, mut request, workspace, mut state) = fixture(root.path());
        if !service.tools.python().is_file() {
            return;
        }
        let source = root.path().join("sources");
        fs::create_dir(&source).expect("source directory");
        for file in ["Scene.pck", "Gameexe.dat", "SiglusEngine.exe"] {
            let path = source.join(file);
            fs::write(&path, b"fixture without embedded sources").expect("source");
            request.inputs.files.insert(file.into(), path);
        }
        let result = service
            .adaptation_tool(
                &request,
                &workspace,
                &mut state,
                "inspect_siglus_game",
                &json!({"source":"missing.ss"}),
                &CancellationToken::new(),
                None,
            )
            .await
            .expect("read bound package without installed files");
        assert_eq!(result["sources"], json!([]));
        assert!(!request.game_directory.join("Scene.pck").exists());
        assert!(!workspace.join("resources/Scene.pck").exists());
    }

    #[tokio::test]
    async fn invalid_release_replacement_preserves_the_saved_selection() {
        let root = tempfile::tempdir().expect("fixture");
        let (service, request, workspace, mut state) = fixture(root.path());
        let original = serde_json::to_value(&state.release_files).expect("selection");
        let error = service.test_adapter(&request, &workspace, &mut state, &json!({"kind":"engine","key":"fixture","name":"Fixture","resourcePatterns":["*.json"],"releaseFiles":[{"path":"src/adapter.py","role":"source","purpose":"entry"},{"path":"tools/invalid.py","role":"source","purpose":"invalid role"}]}), &CancellationToken::new(), None).await.expect_err("invalid release");
        assert!(error.to_string().contains("发布"));
        assert_eq!(
            serde_json::to_value(&state.release_files).expect("selection"),
            original
        );
    }

    #[tokio::test]
    async fn full_verification_requires_the_selected_executables_actual_locale_profile() {
        let root = tempfile::tempdir().expect("fixture");
        let (service, request, workspace, mut state) = fixture(root.path());
        fs::write(request.game_directory.join("game.exe"), b"MZ fixture").expect("executable");
        fs::write(
            request.game_directory.join("game.exe.le.config"),
            b"profile",
        )
        .expect("profile");
        fs::copy(
            request.game_directory.join("game.exe"),
            workspace.join("resources/game.exe"),
        )
        .expect("staged executable");
        state.extra.get_mut("resourceHashes").expect("hashes")["game.exe"] =
            json!(hash_file(&request.game_directory.join("game.exe")).expect("hash"));
        state.extra.insert("launchInspection".into(), json!({"localeProfiles":[{"path":"game.exe.le.config","executable":"game.exe","executableExists":true}]}));
        let error = service.test_adapter(&request, &workspace, &mut state, &json!({"kind":"engine","key":"fixture","name":"Fixture","resourcePatterns":["*.json","*.exe"]}), &CancellationToken::new(), None).await.expect_err("unstaged locale profile");
        assert!(error.to_string().contains("game.exe.le.config"));
        assert!(!workspace.join("build").exists());
    }

    #[tokio::test]
    async fn verification_caches_keep_full_results_separate_from_trials_and_recheck_artifacts() {
        let root = tempfile::tempdir().expect("fixture");
        let (service, request, workspace, mut state) = fixture(root.path());
        if !service.tools.python().is_file() || !service.tools.vntextpatch().is_file() {
            return;
        }
        let args =
            json!({"kind":"engine","key":"fixture","name":"Fixture","resourcePatterns":["*.json"]});
        let cancellation = CancellationToken::new();
        state.total_turns = 3;
        state.update_progress("breakthrough", "已定位文本表。", None, None);
        let events = std::sync::Mutex::new(Vec::new());
        let report = |event| events.lock().expect("events").push(event);
        let first = service
            .test_adapter(
                &request,
                &workspace,
                &mut state,
                &args,
                &cancellation,
                Some(&report),
            )
            .await
            .expect("full");
        assert_eq!(first["stage"], "finish-ready");
        assert_eq!(state.extra["highestVerificationMilestone"], 4);
        let events = events.into_inner().expect("events");
        let extraction = events
            .iter()
            .find(|event| event.stage == "验收文本提取")
            .expect("extraction progress");
        assert_eq!(extraction.progress_note["message"], "已定位文本表。");
        assert_eq!(extraction.turn, 3);
        assert_eq!(
            events.last().expect("verification progress").progress_note["kind"],
            "verified"
        );
        assert_eq!(state.extra["lastBreakthrough"], state.progress_note);
        state.total_turns = 9;
        let mut notes = args.clone();
        notes["pendingCoverage"] = json!(["菜单尚未支持"]);
        notes["runtimeChecks"] = json!(["检查游戏显示"]);
        let cached = service
            .test_adapter(
                &request,
                &workspace,
                &mut state,
                &notes,
                &cancellation,
                None,
            )
            .await
            .expect("cache");
        assert_eq!(cached["reused"], true);
        assert_eq!(cached["pendingCoverage"][0], "菜单尚未支持");
        assert_eq!(state.extra["lastVerificationProgressTurn"], 3);
        let mut trial = args.clone();
        trial["trialFiles"] = json!(["text.json"]);
        let scoped = service
            .test_adapter(
                &request,
                &workspace,
                &mut state,
                &trial,
                &cancellation,
                None,
            )
            .await
            .expect("trial");
        assert_eq!(scoped["stage"], "trial");
        assert!(state.verified_package.is_none());
        assert!(
            state.progress_note["message"]
                .as_str()
                .expect("trial note")
                .contains("小范围")
        );
        let restored = service
            .test_adapter(&request, &workspace, &mut state, &args, &cancellation, None)
            .await
            .expect("full cache");
        assert_eq!(restored["reused"], true);
        assert!(state.verified_package.is_some());
        assert!(
            state.progress_note["message"]
                .as_str()
                .expect("full note")
                .contains("资源验收已通过")
        );
        let package = PathBuf::from(
            state.verified_package.as_ref().expect("package")["directoryPath"]
                .as_str()
                .expect("path"),
        );
        fs::write(package.join("src/adapter.py"), "invalid artifact").expect("damage package");
        let rebuilt = service
            .test_adapter(&request, &workspace, &mut state, &args, &cancellation, None)
            .await
            .expect("recheck");
        assert_eq!(rebuilt["reused"], false);
        assert_eq!(rebuilt["passed"], true);
    }

    #[tokio::test]
    async fn repair_trials_check_selected_baselines_without_promoting_full_verification() {
        let root = tempfile::tempdir().expect("fixture");
        let (service, mut request, workspace, mut state) = fixture(root.path());
        if !service.tools.python().is_file() || !service.tools.vntextpatch().is_file() {
            return;
        }
        request.objective = "Repair".into();
        let baseline_units = [
            TranslationUnit {
                id: "0".into(),
                resource_path: "text.json".into(),
                locator: "0".into(),
                source_text: "原文".into(),
                context: "stable".into(),
                speaker: None,
                kind: "dialogue".into(),
                protected_tokens: None,
            },
            TranslationUnit {
                id: "outside".into(),
                resource_path: "outside.json".into(),
                locator: "0".into(),
                source_text: "outside".into(),
                context: "stable".into(),
                speaker: None,
                kind: "dialogue".into(),
                protected_tokens: None,
            },
        ];
        let request = AdaptationRequest {
            baseline_units: &baseline_units,
            ..request
        };
        let args = json!({"kind":"engine","key":"fixture","name":"Fixture","resourcePatterns":["*.json"],"trialFiles":["text.json"]});
        let cancellation = CancellationToken::new();
        state.total_turns = 5;
        let passed = service
            .test_adapter(&request, &workspace, &mut state, &args, &cancellation, None)
            .await
            .expect("trial");
        assert_eq!(passed["passed"], true);
        assert_eq!(state.extra["highestVerificationMilestone"], 1);
        let mut changed_baseline = baseline_units.clone();
        changed_baseline[0].context = "changed baseline".into();
        let request = AdaptationRequest {
            baseline_units: &changed_baseline,
            ..request
        };
        state.total_turns = 9;
        let failed = service
            .test_adapter(&request, &workspace, &mut state, &args, &cancellation, None)
            .await
            .expect("trial mismatch");
        assert_eq!(failed["passed"], false);
        assert!(failed["diagnostics"].to_string().contains("已有文本"));
        assert_eq!(state.extra["lastVerificationProgressTurn"], 5);
    }

    #[tokio::test]
    async fn discovery_rejects_missing_staged_required_resources_before_resource_verification() {
        let root = tempfile::tempdir().expect("fixture");
        let (service, request, workspace, mut state) = fixture(root.path());
        if !service.tools.python().is_file() || !service.tools.vntextpatch().is_file() {
            return;
        }
        fs::write(request.game_directory.join("extra.json"), "[]").expect("extra candidate");
        let entry = workspace.join("src/adapter.py");
        fs::write(
            &entry,
            fs::read_to_string(&entry).expect("source").replace(
                "requiredFiles=['text.json']",
                "requiredFiles=sorted(r['resourceFiles'])",
            ),
        )
        .expect("dynamic probe");
        let result=service.test_adapter(&request,&workspace,&mut state,&json!({"kind":"engine","key":"fixture","name":"Fixture","resourcePatterns":["*.json"]}),&CancellationToken::new(),None).await.expect("discovery failure result");
        assert_eq!(result["stage"], "discovery");
        assert_eq!(result["passed"], false);
        assert!(result["diagnostics"].to_string().contains("必需资源不一致"));
        assert_eq!(state.extra["highestVerificationMilestone"], 1);
    }

    #[tokio::test]
    async fn independent_coverage_mismatches_stop_before_original_roundtrip() {
        let root = tempfile::tempdir().expect("fixture");
        let (service, request, workspace, mut state) = fixture(root.path());
        if !service.tools.python().is_file() || !service.tools.vntextpatch().is_file() {
            return;
        }
        let result=service.test_adapter(&request,&workspace,&mut state,&json!({"kind":"engine","key":"fixture","name":"Fixture","resourcePatterns":["*.json"],"coverageSamples":[{"resourcePath":"text.json","locator":"0","sourceText":"wrong anchor"}]}),&CancellationToken::new(),None).await.expect("coverage failure result");
        assert_eq!(result["stage"], "verification");
        assert_eq!(result["passed"], false);
        assert!(
            !state
                .verification
                .as_ref()
                .expect("verification")
                .original_roundtrip_passed
        );
        assert_eq!(state.extra["highestVerificationMilestone"], 2);
    }

    #[tokio::test]
    async fn only_unchanged_fully_verified_packages_can_publish() {
        let root = tempfile::tempdir().expect("fixture");
        let (service, request, workspace, mut session) = fixture(root.path());
        if !service.tools.python().is_file() || !service.tools.vntextpatch().is_file() {
            return;
        }
        let entry = workspace.join("src/adapter.py");
        let source = fs::read_to_string(&entry).expect("fixture source");
        fs::write(&entry,source.replace("requiredFiles=['text.json']","requiredFiles=['text.json']+(['extra.json'] if (root / 'extra.json').is_file() else [])")).expect("discovery fixture");
        let args =
            json!({"kind":"engine","key":"fixture","name":"Fixture","resourcePatterns":["*.json"]});
        let verified = service
            .test_adapter(
                &request,
                &workspace,
                &mut session,
                &args,
                &CancellationToken::new(),
                None,
            )
            .await
            .expect("full verification");
        assert_eq!(verified["passed"], true, "{verified}");
        let published = service
            .finish_adapter(
                &request,
                &workspace,
                &mut session,
                &json!({"summary":"静态验收通过；游戏显示未测试。"}),
                &CancellationToken::new(),
            )
            .await
            .expect("publish");
        assert_eq!(published["success"], true);
        fs::write(request.game_directory.join("extra.json"), "[]").expect("new resource");
        let changed = service
            .finish_adapter(
                &request,
                &workspace,
                &mut session,
                &json!({"summary":"stale discovery"}),
                &CancellationToken::new(),
            )
            .await
            .expect_err("discovery changed");
        assert!(changed.to_string().contains("必需资源集合发生变化"));
        fs::remove_file(request.game_directory.join("extra.json"))
            .expect("remove fixture resource");
        let mut changed_request = request.clone();
        changed_request.objective = "ExpandCoverage".into();
        assert!(
            service
                .finish_adapter(
                    &changed_request,
                    &workspace,
                    &mut session,
                    &json!({"summary":"changed objective"}),
                    &CancellationToken::new(),
                )
                .await
                .expect_err("changed objective requires verification")
                .to_string()
                .contains("目标或已有文本基线")
        );
        let changed_baseline = [TranslationUnit {
            id: "existing".into(),
            resource_path: "text.json".into(),
            locator: "0".into(),
            source_text: "different source".into(),
            context: String::new(),
            speaker: None,
            kind: "dialogue".into(),
            protected_tokens: None,
        }];
        let changed_request = AdaptationRequest {
            baseline_units: &changed_baseline,
            ..request.clone()
        };
        assert!(
            service
                .finish_adapter(
                    &changed_request,
                    &workspace,
                    &mut session,
                    &json!({"summary":"changed baseline"}),
                    &CancellationToken::new(),
                )
                .await
                .expect_err("changed baseline requires verification")
                .to_string()
                .contains("目标或已有文本基线")
        );
        fs::write(workspace.join("src/adapter.py"), "print('changed')").expect("edit");
        assert!(
            service
                .finish_adapter(
                    &request,
                    &workspace,
                    &mut session,
                    &json!({"summary":"stale"}),
                    &CancellationToken::new(),
                )
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn trial_validation_cannot_be_reused_as_full_publication() {
        let root = tempfile::tempdir().expect("fixture");
        let (service, request, workspace, mut session) = fixture(root.path());
        if !service.tools.python().is_file() || !service.tools.vntextpatch().is_file() {
            return;
        }
        let trial = json!({"kind":"engine","key":"fixture","name":"Fixture","resourcePatterns":["*.json"],"trialFiles":["text.json"]});
        let result = service
            .test_adapter(
                &request,
                &workspace,
                &mut session,
                &trial,
                &CancellationToken::new(),
                None,
            )
            .await
            .expect("trial");
        assert_eq!(result["stage"], "trial");
        assert!(
            service
                .finish_adapter(
                    &request,
                    &workspace,
                    &mut session,
                    &json!({"summary":"trial"}),
                    &CancellationToken::new(),
                )
                .await
                .is_err()
        );
        let result=service.test_adapter(&request,&workspace,&mut session,&json!({"kind":"engine","key":"fixture","name":"Fixture","resourcePatterns":["*.json"]}),&CancellationToken::new(),None).await.expect("full");
        assert_eq!(result["stage"], "finish-ready");
        assert_eq!(result["passed"], true);
    }

    #[tokio::test]
    async fn bundled_loose_template_reuses_actual_native_parser() {
        let root = tempfile::tempdir().expect("fixture");
        let (service, request, workspace, mut session) = fixture(root.path());
        if !service.tools.python().is_file() || !service.tools.vntextpatch().is_file() {
            return;
        }
        fs::remove_file(workspace.join("src/adapter.py")).expect("fixture");
        fs::write(
            request.game_directory.join("script.rpy"),
            "label start:\n    \"日本語の台詞\"\n",
        )
        .expect("fixture");
        service
            .adaptation_tool(
                &request,
                &workspace,
                &mut session,
                "stage_resources",
                &json!({"files":["script.rpy"]}),
                &CancellationToken::new(),
                None,
            )
            .await
            .expect("stage template sample");
        let composed = service
            .compose_template(
                &workspace,
                &mut session,
                &json!({"id":"loose-renpy"}),
                &CancellationToken::new(),
            )
            .await
            .expect("compose");
        let result = service
            .test_adapter(
                &request,
                &workspace,
                &mut session,
                &composed["testArguments"],
                &CancellationToken::new(),
                None,
            )
            .await
            .expect("verify template");
        assert_eq!(result["passed"], true, "{result}");
    }

    #[test]
    fn interrupted_conversations_keep_tool_order_and_unknown_session_fields() {
        let mut state:Session=serde_json::from_value(json!({"messages":[{"role":"assistant","content":"","toolCalls":[{"id":"c","name":"write_file","arguments":"{}"}],"toolCallId":null,"providerData":null}],"domainRule":"retained"})).expect("legacy session");
        normalize_messages(&mut state.messages);
        assert_eq!(
            state.messages[0]["tool_calls"][0]["function"]["name"],
            "write_file"
        );
        assert_eq!(
            serde_json::to_value(state).expect("state")["domainRule"],
            "retained"
        );
    }

    #[test]
    fn legacy_numeric_enums_and_provider_only_turns_remain_readable() {
        let directory = tempfile::tempdir().expect("fixture");
        let path = directory.path().join("session.json");
        let provider = json!([{"type":"reasoning","encrypted_content":"opaque","summary":[]},{"type":"function_call","id":"fc_1","call_id":"call_1","name":"read_file","arguments":"{}","status":"completed"}]);
        write_json(&path,&json!({"objective":0,"lastStatus":4,"checkpoint":null,"releaseFiles":null,"messages":[{"role":"assistant","content":"","toolCalls":null,"toolCallId":null,"providerData":provider}],"domainRule":"kept"})).expect("fixture");
        let state = load_session(&path).expect("legacy state");
        assert_eq!(state.objective, "Develop");
        assert_eq!(state.last_status, AdaptationStatus::BudgetExhausted);
        assert_eq!(state.messages[0]["providerData"], provider);
        let replay = replay(&state);
        assert_eq!(replay[1]["call_id"], "call_1");
        assert_eq!(replay[0]["encrypted_content"], "opaque");
        assert!(visible_json(&replay[0]).get("encrypted_content").is_none());
    }

    #[test]
    fn tool_readback_uses_retained_result_and_legacy_session_fallback() {
        let root = tempfile::tempdir().expect("fixture");
        let workspace = root.path();
        fs::create_dir(workspace.join("results")).expect("results");
        let value = json!({"success":true,"output":format!("{}末尾证据", "a".repeat(20000))});
        write_json(
            &workspace
                .join("results")
                .join(format!("{}.json", safe_call_id("full"))),
            &value,
        )
        .expect("stored result");
        let state = Session {
            messages: vec![
                json!({"role":"tool","tool_call_id":"full","content":preview(&value.to_string(),16000,true)}),
                json!({"role":"tool","toolCallId":"legacy","content":"旧会话完整结果"}),
                json!({"type":"function_call_output","call_id":"responses","output":"Responses 完整结果"}),
            ],
            ..Session::default()
        };
        let full = read_tool_result(
            workspace,
            &state,
            &json!({"toolCallId":"full","query":"末尾证据"}),
        )
        .expect("full readback");
        assert_eq!(full["toolCallId"], "full");
        assert!(full["content"].as_str().expect("page").contains("末尾证据"));
        for (id, expected) in [
            ("legacy", "旧会话完整结果"),
            ("responses", "Responses 完整结果"),
        ] {
            let page = read_tool_result(workspace, &state, &json!({"toolCallId":id}))
                .expect("legacy readback");
            assert_eq!(page["content"], expected);
        }
    }

    #[test]
    fn oversized_tool_result_keeps_failure_status_and_tail_in_valid_json() {
        let content = format!(
            r#"{{"success":false,"output":"{}","error":"末尾失败原因"}}"#,
            "a".repeat(20000)
        );
        let shortened: Value =
            serde_json::from_str(&preview(&content, 16000, false)).expect("valid preview JSON");
        assert_eq!(shortened["success"], false);
        assert_eq!(shortened["truncated"], true);
        assert!(
            shortened["tail"]
                .as_str()
                .expect("tail")
                .contains("末尾失败原因")
        );
    }

    #[test]
    fn explicit_readback_excerpts_survive_replay_for_both_saved_protocols() {
        let content = json!({"success":true,"content":"证据".repeat(5000)}).to_string();
        for call in [
            json!({"type":"function_call","call_id":"read","name":"read_tool_result"}),
            json!({"role":"assistant","providerData":[{"type":"function_call","call_id":"read","name":"read_tool_result"}]}),
            json!({"role":"assistant","providerData":{"role":"assistant","tool_calls":[{"id":"read","function":{"name":"read_tool_result"}}]}}),
            json!({"role":"assistant","toolCalls":[{"id":"read","name":"read_tool_result"}]}),
        ] {
            let state = Session {
                messages: vec![
                    call,
                    json!({"role":"tool","tool_call_id":"read","content":content}),
                ],
                ..Session::default()
            };
            assert_eq!(
                replay(&state).last().expect("tool output")["content"],
                content
            );
        }
    }

    #[tokio::test]
    async fn progress_reports_validate_kind_and_emit_chinese_titles() {
        let root = tempfile::tempdir().expect("fixture");
        let (service, request, workspace, mut session) = fixture(root.path());
        let events = std::sync::Mutex::new(Vec::new());
        let progress = |event| events.lock().expect("events").push(event);
        let cancellation = CancellationToken::new();
        service
            .adaptation_tool(
                &request,
                &workspace,
                &mut session,
                "report_progress",
                &json!({"kind":"breakthrough","message":" 已定位资源。 "}),
                &cancellation,
                Some(&progress),
            )
            .await
            .expect("progress");
        assert_eq!(events.lock().expect("events")[0].stage, "关键突破");
        assert_eq!(session.progress_note["message"], "已定位资源。");
        for args in [
            json!({"kind":"verified","message":"不能自称验证"}),
            json!({"kind":"exploring","message":" "}),
        ] {
            assert!(
                service
                    .adaptation_tool(
                        &request,
                        &workspace,
                        &mut session,
                        "report_progress",
                        &args,
                        &cancellation,
                        Some(&progress)
                    )
                    .await
                    .is_err()
            );
        }
        assert_eq!(events.lock().expect("events").len(), 1);
    }
}
