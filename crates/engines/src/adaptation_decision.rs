//! Append-only host context within bounded segments. Evidence locations reference the session archive.
use super::*;

const MAX_REFERENCES: usize = 48;

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct DecisionState {
    pub current_problem: Option<Problem>,
    pub hypotheses: Vec<Hypothesis>,
    pub evidence_refs: Vec<EvidenceRef>,
    pub problem_start: usize,
    pub references: String,
    sent_context: SentContext,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default)]
struct SentContext {
    snapshot: Value,
    evidence: BTreeMap<String, String>,
    protocol_sent: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Problem {
    pub id: String,
    pub layer: Layer,
    pub question: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) enum Layer {
    Workflow,
    EngineFamily,
    EngineVariant,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Hypothesis {
    pub statement: String,
    pub status: HypothesisStatus,
    #[serde(default)]
    pub evidence: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum HypothesisStatus {
    Open,
    Supported,
    Refuted,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct EvidenceRef {
    pub tool_call_id: String,
    pub tool: String,
    pub message_index: usize,
    pub question_id: Option<String>,
    /// Digest of the host's existing staged-input hashes, not another copy of those hashes.
    pub input_fingerprint: Option<String>,
}

pub(super) struct Context {
    pub replay: history::Replay,
    pub selected_start: usize,
    pub evidence_count: usize,
    pub components: Value,
}

pub(super) fn reset_context(state: &mut Session) {
    state.decision.sent_context = SentContext::default();
}

fn changes(before: &Value, after: &Value) -> Option<Value> {
    if before == after {
        return None;
    }
    let (Some(before), Some(after)) = (before.as_object(), after.as_object()) else {
        return Some(after.clone());
    };
    let mut updated = serde_json::Map::new();
    for (key, value) in after {
        if let Some(change) = changes(before.get(key).unwrap_or(&Value::Null), value) {
            updated.insert(key.clone(), change);
        }
    }
    for key in before.keys().filter(|key| !after.contains_key(*key)) {
        updated.insert(key.clone(), Value::Null);
    }
    Some(Value::Object(updated))
}

fn short(value: &str, limit: usize) -> String {
    value.chars().take(limit).collect()
}

fn bounded(value: &Value, limit: usize) -> Value {
    let text = value.to_string();
    if text.chars().count() <= limit {
        value.clone()
    } else {
        json!({"preview":short(&text,limit),"truncated":true,"readBack":"read_session_history or read_tool_result"})
    }
}

pub(super) fn input_fingerprint(state: &Session) -> Option<String> {
    state
        .extra
        .get("resourceHashes")
        .and_then(Value::as_object)
        .map(|value| hash_text(&json!(value.iter().collect::<BTreeMap<_, _>>()).to_string()))
}

fn message_id(message: &Value) -> Option<&str> {
    if message["role"] != "tool" && message["type"] != "function_call_output" {
        return None;
    }
    message["tool_call_id"]
        .as_str()
        .or_else(|| message["call_id"].as_str())
}

pub(super) fn validate_question_id(id: &str) -> Result<(), EngineError> {
    if id.is_empty()
        || id.len() > 80
        || !id.chars().all(|c| c.is_alphanumeric() || "-_.".contains(c))
    {
        return Err(EngineError::Protocol(
            "问题 ID 必须为 1～80 字节的字母、数字或 -_.。".into(),
        ));
    }
    Ok(())
}

/// Runs inside report_progress, so bookkeeping can accompany the actual experiment or edit.
pub(super) fn update(state: &mut Session, args: &Value) -> Result<(), EngineError> {
    if ["problem", "hypotheses", "evidenceRefs"]
        .iter()
        .all(|key| args.get(key).is_none())
    {
        return Ok(());
    }
    let problem: Option<Problem> = args
        .get("problem")
        .map(|p| serde_json::from_value(p.clone()))
        .transpose()?;
    if let Some(problem) = &problem {
        validate_question_id(&problem.id)?;
        if problem.question.trim().is_empty() || problem.question.chars().count() > 600 {
            return Err(EngineError::Protocol("当前问题必须为 1～600 字符。".into()));
        }
    }
    let hypotheses: Option<Vec<Hypothesis>> = args
        .get("hypotheses")
        .map(|p| serde_json::from_value(p.clone()))
        .transpose()?;
    if let Some(items) = &hypotheses
        && (items.len() > 4
            || items.iter().any(|h| {
                h.statement.trim().is_empty()
                    || h.statement.chars().count() > 600
                    || h.evidence.len() > 3
            }))
    {
        return Err(EngineError::Protocol(
            "假设最多 4 项，每项 1～600 字符及最多 3 个证据调用 ID。".into(),
        ));
    }
    let explicit: Vec<String> = args
        .get("evidenceRefs")
        .map(|p| serde_json::from_value(p.clone()))
        .transpose()?
        .unwrap_or_default();
    if explicit.len() > 8 {
        return Err(EngineError::Protocol("显式证据引用最多 8 项。".into()));
    }
    let requested: BTreeSet<_> = explicit
        .iter()
        .chain(hypotheses.iter().flatten().flat_map(|h| h.evidence.iter()))
        .cloned()
        .collect();
    if requested.iter().any(|id| id.is_empty() || id.len() > 200) {
        return Err(EngineError::Protocol("证据调用 ID 无效。".into()));
    }
    let mut refs = state.decision.evidence_refs.clone();
    let missing: BTreeSet<_> = requested
        .iter()
        .filter(|id| !refs.iter().any(|r| &r.tool_call_id == *id))
        .cloned()
        .collect();
    if !missing.is_empty() {
        let mut names = BTreeMap::new();
        session_store::visit(state, |index, message| {
            for (id, name) in history::calls(message) {
                if missing.contains(&id) {
                    names.insert(id, name);
                }
            }
            if let Some(id) = message_id(message)
                .filter(|id| missing.contains(*id) && !refs.iter().any(|r| r.tool_call_id == *id))
            {
                refs.push(EvidenceRef {
                    tool_call_id: id.into(),
                    tool: names.get(id).cloned().unwrap_or_default(),
                    message_index: index,
                    question_id: None,
                    input_fingerprint: None,
                });
            }
            Ok(true)
        })?;
        if missing
            .iter()
            .any(|id| !refs.iter().any(|r| &r.tool_call_id == id))
        {
            return Err(EngineError::Protocol(
                "证据 ID 必须对应已有工具结果；未执行步骤和模型说明不能作为宿主证据。".into(),
            ));
        }
    }
    let switched = problem.as_ref().is_some_and(|p| {
        state
            .decision
            .current_problem
            .as_ref()
            .is_none_or(|old| old.id != p.id)
    });
    if switched {
        state.decision.problem_start = session_store::base(state) + state.messages.len();
        state.decision.hypotheses.clear();
    }
    if let Some(problem) = problem {
        state.decision.current_problem = Some(problem);
    }
    if let Some(hypotheses) = hypotheses {
        state.decision.hypotheses = hypotheses;
    }
    for reference in &mut refs {
        if requested.contains(&reference.tool_call_id) {
            reference.question_id = state
                .decision
                .current_problem
                .as_ref()
                .map(|p| p.id.clone());
        }
    }
    refs.sort_by_key(|r| requested.contains(&r.tool_call_id));
    state.decision.evidence_refs = refs;
    trim_references(state);
    Ok(())
}

fn trim_references(state: &mut Session) {
    let pinned: BTreeSet<_> = state
        .decision
        .hypotheses
        .iter()
        .flat_map(|h| h.evidence.iter())
        .collect();
    while state.decision.evidence_refs.len() > MAX_REFERENCES {
        let Some(index) = state
            .decision
            .evidence_refs
            .iter()
            .position(|r| !pinned.contains(&r.tool_call_id))
        else {
            break;
        };
        state.decision.evidence_refs.remove(index);
    }
}

pub(super) fn record(state: &mut Session, call: &ToolCall, args: &Value) {
    if matches!(
        call.name.as_str(),
        "report_progress"
            | "checkpoint"
            | "load_tool_groups"
            | "read_tool_result"
            | "read_session_history"
            | "finish"
    ) {
        return;
    }
    let reference = EvidenceRef {
        tool_call_id: call.id.clone(),
        tool: call.name.clone(),
        message_index: session_store::base(state) + state.messages.len(),
        question_id: args["questionId"].as_str().map(str::to_owned).or_else(|| {
            state
                .decision
                .current_problem
                .as_ref()
                .map(|p| p.id.clone())
        }),
        input_fingerprint: input_fingerprint(state),
    };
    state.decision.evidence_refs.push(reference);
    trim_references(state);
}

pub(super) fn reset_problem(state: &mut Session) {
    state.decision.current_problem = None;
    state.decision.hypotheses.clear();
    state.decision.problem_start = session_store::base(state) + state.messages.len();
}

pub(super) fn bootstrap(state: &mut Session) {
    if !state.decision.evidence_refs.is_empty() {
        return;
    }
    let names: BTreeMap<_, _> = state.messages.iter().flat_map(history::calls).collect();
    let refs = state
        .messages
        .iter()
        .enumerate()
        .rev()
        .filter_map(|(index, message)| {
            let id = message_id(message)?;
            Some(EvidenceRef {
                tool_call_id: id.into(),
                tool: names.get(id).cloned().unwrap_or_default(),
                message_index: session_store::base(state) + index,
                question_id: None,
                input_fingerprint: None,
            })
        })
        .take(MAX_REFERENCES)
        .collect::<Vec<_>>();
    state.decision.evidence_refs = refs.into_iter().rev().collect();
}

fn verification_summary(state: &Session, key: &str) -> Option<Value> {
    let cache = state.extra.get(key)?;
    if let Some(error) = cache.get("compilationError") {
        return Some(json!({"stage":"compilation","passed":false,"error":bounded(error,2048)}));
    }
    let result = cache.get("result")?;
    let mut summary = serde_json::Map::new();
    for key in [
        "passed",
        "stage",
        "id",
        "version",
        "trialFiles",
        "trialBytes",
        "diagnostics",
        "issues",
        "nextAction",
    ] {
        if let Some(value) = result.get(key) {
            summary.insert(key.into(), bounded(value, 2048));
        }
    }
    Some(Value::Object(summary))
}

fn prune_empty(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for child in map.values_mut() {
                prune_empty(child);
            }
            map.retain(|_, child| match child {
                Value::Null => false,
                Value::String(text) => !text.is_empty(),
                Value::Array(items) => !items.is_empty(),
                Value::Object(entries) => !entries.is_empty(),
                _ => true,
            });
        }
        Value::Array(items) => {
            for child in items.iter_mut() {
                prune_empty(child);
            }
        }
        _ => {}
    }
}

fn snapshot(
    state: &Session,
    workspace: &Path,
    disabled: &BTreeSet<String>,
    has_recent_turns: bool,
) -> Value {
    use std::io::Read;
    let work_notes = resolve_relative(workspace, "work/status.md")
        .ok()
        .and_then(|path| fs::File::open(path).ok())
        .and_then(|file| {
            let mut text = Vec::new();
            file.take(32 * 1024).read_to_end(&mut text).ok()?;
            Some(short(&String::from_utf8_lossy(&text), 8000))
        });
    let discovery = state
        .extra
        .get("builtinDiscovery")
        .filter(|value| value.is_object())
        .map(|value| {
            let mut report = value.clone();
            if let Some(candidates) = report["candidates"].as_array_mut() {
                candidates.retain(|candidate| {
                    !candidate["adapterId"]
                        .as_str()
                        .is_some_and(|id| disabled.contains(id))
                });
            }
            bounded(&report, 4000)
        });
    let probes = (!has_recent_turns)
        .then(|| state.extra.get("successfulProbes"))
        .flatten()
        .map(|value| bounded(value, 6000));
    let notes_turn = state.extra.get("notesTurn").and_then(Value::as_u64);
    let host_state = json!({
            "recognition":bounded(&json!(state.recognition),2048),"family":bounded(&state.family,2048),
            "verification":verification_summary(state,"verificationCache"),
            "recordedVerification":state.verification.as_ref().filter(|_|!state.extra.contains_key("verificationCache")).map(|v| json!({"passed":v.passed,"originalRoundtripPassed":v.original_roundtrip_passed,"diagnostics":v.diagnostics,"units":v.units,"runtimeTested":v.runtime_tested})),
            "discovery":verification_summary(state,"discoveryVerificationCache"),
            "trial":verification_summary(state,"trialVerificationCache"),
            "verificationUse":"Cached results describe their recorded inputs. Only the existing verifier can reuse them after checking current fingerprints; script reports and model claims cannot activate support.",
            "verifiedPackage":state.verified_package.as_ref().map(|p| json!({"id":p["id"],"version":p["version"]})),
            "verifiedFingerprint":state.verified_fingerprint,
            "textCoverage":bounded(&state.text_coverage,6000),
            "inputBinding":state.extra.get("inputBinding"),"resourceChanges":state.extra.get("resourceChanges"),
            "stagedInputFingerprint":input_fingerprint(state),
            "stagedResources":state.extra.get("resourceHashes").and_then(Value::as_object).map(|h| json!({"count":h.len(),"paths":h.keys().take(24).collect::<Vec<_>>()})),
            "sourceFiles":super::flow::workspace_files(workspace,"src",32),
            "workFiles":super::flow::workspace_files(workspace,"work",8),
            "releaseFiles":state.release_files.iter().take(32).map(|f| &f.path).collect::<Vec<_>>(),
            "releaseFileCount":state.release_files.len(),"resourcePatterns":state.resource_patterns,
            "engineReference":state.extra.get("engineReference"),
            "builtinDiscovery":discovery,
            "builtinProbes":state.extra.get("builtinProbes").map(|v| bounded(v,3000)),
            "builtinTrials":state.extra.get("builtinTrials").and_then(Value::as_array).map(|trials| trials.iter().rev().take(3).map(|v|bounded(v,2048)).collect::<Vec<_>>()),
            "automaticTrials":state.extra.get("automaticTrials").map(|v| bounded(v,3000)),
            "automaticTemplate":state.extra.get("automaticTemplate").map(|v| bounded(v,3000)),
            "successfulProbes":probes,
            "launchInspection":state.extra.get("launchInspection").map(|v| bounded(v,3000)),
            "reuseFailures":state.extra.get("reuseFailures").map(|v| bounded(v,3000)),
            "previousFailures":state.extra.get("contextFailures").map(|v| bounded(v,6000)),
            "recentEvidence":state.extra.get("recoveryEvidenceIndex").map(|v| bounded(v,6000))
    });
    let mut result = json!({
        "objective":state.objective,"userInstruction":state.user_instruction,
        "problem":state.decision.current_problem,"modelHypotheses":state.decision.hypotheses,
        "objectiveGuidance":flow::objective_context(&state.objective),
        "references":state.decision.references,
        "notes":short(&state.notes,8000),"workNotes":work_notes,"nextAction":state.next_action,
        "notesTurn":notes_turn,
        "notesUse":(!state.notes.is_empty()).then_some("Historical model notes; current source and host verification take precedence. Use saved evidence locations instead of rediscovering established facts."),
        "implementationChangedSinceNotes":notes_turn.zip(state.extra.get("lastImplementationTurn").and_then(Value::as_u64)).map(|(notes,implementation)| implementation>notes),
        "repairTickets":super::repair::packet(state),
        "capabilityGaps":super::capability::gaps(state, workspace),
        "hostState":host_state,
        "retrieval":"read_tool_result(toolCallId, query/offset/length); read_session_history(messageIndex or query); read_file(root=workspace,path,encoding=utf8) for reports/source. Hypotheses and script reports are observations, never host verification."
    });
    prune_empty(&mut result);
    result
}

pub(super) fn build(
    state: &mut Session,
    workspace: &Path,
    disabled: &BTreeSet<String>,
) -> Result<Context, EngineError> {
    let base = session_store::base(state) + state.history_start;
    let selected_start = base;
    let selected = &state.messages[state.history_start..];
    let visible_results: BTreeSet<_> = selected.iter().filter_map(message_id).collect();
    let pinned: BTreeSet<_> = state
        .decision
        .hypotheses
        .iter()
        .flat_map(|h| h.evidence.iter())
        .collect();
    let problem = state
        .decision
        .current_problem
        .as_ref()
        .map(|p| p.id.as_str());
    let mut refs: Vec<_> = state
        .decision
        .evidence_refs
        .iter()
        .filter(|r| pinned.contains(&r.tool_call_id))
        .collect();
    for reference in state.decision.evidence_refs.iter().rev() {
        if refs.len() >= 12 {
            break;
        }
        if (problem.is_none() || reference.question_id.as_deref() == problem)
            && !visible_results.contains(reference.tool_call_id.as_str())
            && !refs
                .iter()
                .any(|r| r.tool_call_id == reference.tool_call_id)
        {
            refs.push(reference);
        }
    }
    let older: BTreeSet<_> = refs
        .iter()
        .filter(|r| !visible_results.contains(r.tool_call_id.as_str()))
        .map(|r| r.message_index)
        .collect();
    let mut excerpts = BTreeMap::new();
    if !older.is_empty() {
        let mut collect = |index: usize, message: &Value| {
            if older.contains(&index) {
                let content = message["content"]
                    .as_str()
                    .or_else(|| message["output"].as_str())
                    .unwrap_or_default();
                let result = serde_json::from_str::<Value>(content).unwrap_or(json!(content));
                let diagnostics: serde_json::Map<_, _> = [
                    "success",
                    "error",
                    "diagnostics",
                    "stage",
                    "phase",
                    "exitCode",
                    "timedOut",
                    "report",
                    "reportError",
                    "reportContract",
                    "executionSucceeded",
                    "issues",
                    "scriptSha256",
                    "revision",
                    "questionId",
                ]
                .into_iter()
                .filter_map(|key| result.get(key).map(|v| (key.into(), bounded(v, 1200))))
                .collect();
                excerpts.insert(
                    index,
                    json!({"diagnostics":diagnostics,"reportIdentity":result.get("report").map(|report| json!({"file":report["file"],"sha256":report["sha256"],"status":report["status"],"tested":report["tested"],"matched":report["matched"]})),"preview":short(content,1024)}),
                );
            }
        };
        let archive_base = session_store::base(state);
        for index in older.range(archive_base..) {
            if let Some(message) = state.messages.get(index - archive_base) {
                collect(*index, message);
            }
        }
        if let Some(last) = older.range(..archive_base).next_back().copied() {
            session_store::visit(state, |index, message| {
                collect(index, message);
                Ok(index < last)
            })?;
        }
    }
    let fingerprint = input_fingerprint(state);
    let evidence: Vec<_> = refs.iter()
        .filter(|reference| !visible_results.contains(reference.tool_call_id.as_str()))
        .map(|reference| json!({"reference":reference,"inputChanged":reference.input_fingerprint.as_ref().zip(fingerprint.as_ref()).map(|(old,new)|old!=new),"olderObservation":excerpts.get(&reference.message_index)}))
        .collect();
    let evidence_count = evidence.len();
    let has_recent_turns = !visible_results.is_empty();
    let snapshot = snapshot(state, workspace, disabled, has_recent_turns);
    let full = state.decision.sent_context.snapshot.is_null();
    let updates = changes(&state.decision.sent_context.snapshot, &snapshot);
    let retrieval_ids: BTreeSet<_> = selected
        .iter()
        .flat_map(history::calls)
        .filter(|(_, name)| matches!(name.as_str(), "read_tool_result" | "read_session_history"))
        .map(|(id, _)| id)
        .collect();
    let original_retrieval_bytes: usize = selected
        .iter()
        .filter(|message| message_id(message).is_some_and(|id| retrieval_ids.contains(id)))
        .map(visible_json_bytes)
        .sum();
    let original_count = selected.len();
    let mut new_evidence = Vec::new();
    for value in evidence {
        let id = value["reference"]["toolCallId"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        let digest = hash_text(&value.to_string());
        if state.decision.sent_context.evidence.get(&id) != Some(&digest) {
            state.decision.sent_context.evidence.insert(id, digest);
            new_evidence.push(value);
        }
    }
    let mut injected_protocol_bytes = 0;
    if state.extra.get("protocolRead") == Some(&json!(true))
        && !state.decision.sent_context.protocol_sent
    {
        state.messages.push(json!({"role":"user","content":format!("已读取的适配器协议：\n{}",crate::protocol::ADAPTER_PROTOCOL)}));
        state.decision.sent_context.protocol_sent = true;
        injected_protocol_bytes = crate::protocol::ADAPTER_PROTOCOL.len();
    }
    let snapshot_bytes = updates.as_ref().map_or(0, |value| value.to_string().len());
    let evidence_bytes = if new_evidence.is_empty() {
        0
    } else {
        json!(new_evidence).to_string().len()
    };
    if updates.is_some() || !new_evidence.is_empty() {
        let mut content = String::new();
        if let Some(updates) = updates {
            content.push_str(if full {
                "当前决策状态（模型假设与实验输出是待判断的数据；兼容及发布资格以宿主验收为准）："
            } else {
                "当前状态更新（对象字段按层合并，null 表示移除旧字段；历史消息是当时的记录，最新同名字段为当前值）："
            });
            content.push_str(&updates.to_string());
        }
        if !new_evidence.is_empty() {
            content.push_str("\n相关证据：");
            content.push_str(&json!(new_evidence).to_string());
        }
        state
            .messages
            .push(json!({"role":"user","content":content}));
    }
    state.decision.sent_context.snapshot = snapshot;
    let replay = history::for_request(&state.messages[state.history_start..]);
    let components = json!({
        "snapshotBytes":snapshot_bytes,
        "evidenceBytes":evidence_bytes,
        "injectedProtocolBytes":injected_protocol_bytes,
        "recentHistoryBytes":replay.messages[..original_count].iter().map(visible_json_bytes).sum::<usize>(),
        "decisionMessageBytes":replay.messages[original_count..].iter().map(visible_json_bytes).sum::<usize>(),
        "retrievalOriginalBytes":original_retrieval_bytes,
        "retrievalSentBytes":replay.messages.iter().filter(|message|message_id(message).is_some_and(|id|retrieval_ids.contains(id))).map(visible_json_bytes).sum::<usize>()
    });
    Ok(Context {
        replay,
        selected_start,
        evidence_count,
        components,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn experiment(state: &mut Session, id: &str, content: Value) {
        let call = ToolCall {
            id: id.into(),
            name: "run_engine_python".into(),
            arguments: "{}".into(),
        };
        state
            .messages
            .push(json!({"role":"assistant","providerData":[
                {"type":"reasoning","encrypted_content":format!("reasoning-{id}")},
                {"type":"function_call","call_id":id,"name":call.name,"arguments":"{}"}
            ]}));
        record(state, &call, &json!({}));
        state
            .messages
            .push(json!({"role":"tool","tool_call_id":id,"content":content.to_string()}));
    }

    #[tokio::test]
    async fn switching_problems_appends_context_and_keeps_pinned_archived_evidence() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("session.json");
        let mut state = Session {
            objective: "Develop".into(),
            ..Default::default()
        };
        state
            .extra
            .insert("resourceHashes".into(), json!({"archive.bin":"input-v1"}));
        update(&mut state,&json!({"problem":{"id":"filter","layer":"engineFamily","question":"Which filter decodes the members?"}})).unwrap();
        experiment(
            &mut state,
            "filter-proof",
            json!({"success":false,"error":"key 1 failed; key 2 remains untested"}),
        );
        session_store::save(&path, &mut state).await.unwrap();
        state.history_start = state.messages.len();
        session_store::save(&path, &mut state).await.unwrap();
        let mut state = load_session(&path).unwrap();
        for index in 0..8 {
            experiment(
                &mut state,
                &format!("unrelated-{index}"),
                json!({"success":true,"data":"unrelated".repeat(3000)}),
            );
        }
        update(&mut state,&json!({"problem":{"id":"script-layout","layer":"engineVariant","question":"Which stride locates the display field?"},
            "hypotheses":[{"statement":"The filter result constrains the layout experiment","status":"open","evidence":["filter-proof"]}]})).unwrap();
        experiment(
            &mut state,
            "current",
            json!({"success":true,"report":{"status":"completed","tested":24,"matched":2,"validation":false}}),
        );
        state
            .extra
            .insert("resourceHashes".into(), json!({"archive.bin":"input-v2"}));
        let before = state.messages.clone();
        let context = build(&mut state, root.path(), &BTreeSet::new()).unwrap();
        let text = context.replay.messages.last().unwrap()["content"]
            .as_str()
            .unwrap();
        assert!(text.contains("key 1 failed; key 2 remains untested"));
        assert!(text.contains("\"inputChanged\":true"));
        assert!(text.contains("script-layout"));
        assert!(!text.contains("unrelatedunrelated"));
        assert_eq!(
            &context.replay.messages[..before.len()],
            history::for_request(&before).messages
        );
        assert_eq!(before, state.messages[..before.len()]);
        assert!(
            read_tool_result(root.path(), &state, &json!({"toolCallId":"filter-proof"}))
                .unwrap()
                .to_string()
                .contains("key 1 failed")
        );
        assert!(state.verified_package.is_none());
        session_store::save(&path, &mut state).await.unwrap();
        let mut restored = load_session(&path).unwrap();
        assert_eq!(
            restored.decision.current_problem.as_ref().unwrap().id,
            "script-layout"
        );
        assert_eq!(restored.decision.hypotheses[0].evidence, ["filter-proof"]);
        let resumed = build(&mut restored, root.path(), &BTreeSet::new()).unwrap();
        assert_eq!(resumed.replay.messages, context.replay.messages);
        assert_eq!(resumed.components["decisionMessageBytes"], 0);
        restored.decision.hypotheses[0].status = HypothesisStatus::Refuted;
        let updated = build(&mut restored, root.path(), &BTreeSet::new()).unwrap();
        assert_eq!(
            updated.replay.messages[..resumed.replay.messages.len()],
            resumed.replay.messages
        );
        let delta = updated.replay.messages.last().unwrap()["content"]
            .as_str()
            .unwrap();
        assert!(delta.contains("refuted"));
        assert!(!delta.contains("相关证据"));
    }

    #[test]
    fn invalid_evidence_and_unexecuted_calls_cannot_update_decision_state() {
        let mut state = Session {
            messages: vec![
                json!({"type":"function_call","call_id":"not-executed","name":"test_adapter"}),
            ],
            ..Default::default()
        };
        let before = serde_json::to_value(&state.decision).unwrap();
        for id in ["unknown", "not-executed"] {
            assert!(update(&mut state,&json!({"problem":{"id":"new","layer":"workflow","question":"Can this publish?"},"hypotheses":[{"statement":"Already verified","status":"supported","evidence":[id]}]})).is_err());
            assert_eq!(before, serde_json::to_value(&state.decision).unwrap());
        }
    }

    #[test]
    fn legacy_sessions_bootstrap_real_results_without_promoting_model_claims() {
        let mut state = Session {
            summary: "Everything is verified".into(),
            messages: vec![
                json!({"type":"reasoning","encrypted_content":"keep"}),
                json!({"type":"function_call","call_id":"a","name":"run_engine_python"}),
                json!({"type":"function_call_output","call_id":"a","output":"{\"success\":false,\"error\":\"offset mismatch\"}"}),
            ],
            ..Default::default()
        };
        bootstrap(&mut state);
        assert_eq!(state.decision.evidence_refs.len(), 1);
        assert_eq!(state.decision.evidence_refs[0].message_index, 2);
        assert_eq!(state.decision.evidence_refs[0].tool, "run_engine_python");
        assert!(state.verified_fingerprint.is_none());
    }

    #[test]
    fn segment_keeps_previews_current_failure_and_raw_responses_reasoning() {
        let root = tempfile::tempdir().unwrap();
        let mut state = Session::default();
        for index in 0..30 {
            experiment(
                &mut state,
                &format!("old-{index}"),
                json!({"success":true,"items":"old investigation ".repeat(1000)}),
            );
        }
        experiment(
            &mut state,
            "latest",
            json!({"success":false,"phase":"build","stderr":"layout mismatch at offset 64"}),
        );
        let original: usize = state.messages.iter().map(visible_json_bytes).sum();
        let saved = state.messages.clone();
        let baseline = history::for_request(&saved).messages;
        let context = build(&mut state, root.path(), &BTreeSet::new()).unwrap();
        let visible: usize = context.replay.messages.iter().map(visible_json_bytes).sum();
        assert!(visible < original / 2, "{visible} / {original}");
        assert_eq!(context.replay.messages[..baseline.len()], baseline);
        assert_eq!(state.messages[..saved.len()], saved);
        assert_eq!(context.replay.messages.last(), state.messages.last());
        assert!(
            context
                .replay
                .messages
                .iter()
                .any(|m| m["providerData"][0]["encrypted_content"] == "reasoning-latest")
        );
        assert!(context.replay.messages.iter().any(|m| {
            m["content"]
                .as_str()
                .is_some_and(|v| v.contains("layout mismatch"))
        }));
    }

    #[test]
    fn recorded_input_versions_ignore_hash_map_insertion_order() {
        let mut state = Session::default();
        state
            .extra
            .insert("resourceHashes".into(), json!({"a.bin":"a","b.bin":"b"}));
        let first = input_fingerprint(&state);
        state
            .extra
            .insert("resourceHashes".into(), json!({"b.bin":"b","a.bin":"a"}));
        assert_eq!(first, input_fingerprint(&state));
        state.extra.insert(
            "resourceHashes".into(),
            json!({"b.bin":"changed","a.bin":"a"}),
        );
        assert_ne!(first, input_fingerprint(&state));
    }

    #[test]
    fn context_metrics_separate_protocol_and_retrieval_without_changing_saved_evidence() {
        let root = tempfile::tempdir().unwrap();
        let mut state = Session::default();
        state.extra.insert("protocolRead".into(), json!(true));
        state.messages = vec![
            json!({"role":"assistant","tool_calls":[{"id":"retrieve","function":{"name":"read_session_history","arguments":"{}"}}]}),
            json!({"role":"tool","tool_call_id":"retrieve","content":json!({"success":true,"content":"exact saved evidence ".repeat(2000)}).to_string()}),
        ];
        let saved = state.messages.clone();
        let context = build(&mut state, root.path(), &BTreeSet::new()).unwrap();
        let sizes = &context.components;
        assert_eq!(
            sizes["injectedProtocolBytes"].as_u64().unwrap() as usize,
            crate::protocol::ADAPTER_PROTOCOL.len()
        );
        assert!(sizes["retrievalOriginalBytes"].as_u64() > sizes["retrievalSentBytes"].as_u64());
        assert_eq!(
            sizes["recentHistoryBytes"].as_u64().unwrap() as usize,
            context.replay.messages[..saved.len()]
                .iter()
                .map(visible_json_bytes)
                .sum::<usize>()
        );
        assert_eq!(
            sizes["decisionMessageBytes"].as_u64().unwrap() as usize,
            context.replay.messages[saved.len()..]
                .iter()
                .map(visible_json_bytes)
                .sum::<usize>()
        );
        assert_eq!(state.messages[..saved.len()], saved);
    }

    #[test]
    fn decision_context_respects_disabled_builtin_candidates() {
        let root = tempfile::tempdir().unwrap();
        let mut state = Session::default();
        state.extra.insert("builtinDiscovery".into(),json!({"candidates":[{"format":"Age","adapterId":"engine.age"},{"format":"Renpy","adapterId":"engine.renpy"}]}));
        let context = build(
            &mut state,
            root.path(),
            &BTreeSet::from(["engine.age".into()]),
        )
        .unwrap();
        let text = context.replay.messages[0]["content"].as_str().unwrap();
        assert!(!text.contains("engine.age"));
        assert!(text.contains("engine.renpy"));
    }

    #[test]
    fn compaction_keeps_original_evidence_indices_without_resending_visible_results_as_excerpts() {
        let root = tempfile::tempdir().unwrap();
        let mut state = Session::default();
        experiment(
            &mut state,
            "kept",
            json!({"success":false,"error":"one canonical failure"}),
        );
        let original_index = state.decision.evidence_refs[0].message_index;
        state.history_start = state.messages.len();
        state.messages.extend(state.messages.clone());
        let context = build(&mut state, root.path(), &BTreeSet::new()).unwrap();
        let text = context
            .replay
            .messages
            .iter()
            .filter_map(|m| m["content"].as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(text.matches("one canonical failure").count(), 1);
        assert_eq!(
            state.decision.evidence_refs[0].message_index,
            original_index
        );
    }

    #[test]
    fn consecutive_turns_preserve_decision_and_history_prefix_and_inject_omitted_protocol() {
        let root = tempfile::tempdir().unwrap();
        let mut state = Session {
            objective: "Develop".into(),
            ..Default::default()
        };
        state.extra.insert("protocolRead".into(), json!(true));
        let protocol_call = ToolCall {
            id: "proto".into(),
            name: "adapter_protocol".into(),
            arguments: "{}".into(),
        };
        state.messages.push(json!({"role":"assistant","tool_calls":[{"id":"proto","function":{"name":"adapter_protocol","arguments":"{}"}}]}));
        record(&mut state, &protocol_call, &json!({}));
        state.messages.push(json!({"role":"tool","tool_call_id":"proto","content":json!({"success":true,"protocol":crate::protocol::ADAPTER_PROTOCOL}).to_string()}));
        let first = build(&mut state, root.path(), &BTreeSet::new()).unwrap();
        assert!(
            first.replay.messages[2]["content"]
                .as_str()
                .unwrap()
                .contains(crate::protocol::ADAPTER_PROTOCOL)
        );
        assert!(
            first.replay.messages[3]["content"]
                .as_str()
                .unwrap()
                .starts_with("当前决策状态（")
        );

        state.summary = "turn 2 transient note".into();
        state.progress_note = json!({"kind":"exploring","message":"transient progress"});
        experiment(&mut state, "step-2", json!({"success":true,"items":[1,2]}));
        let second = build(&mut state, root.path(), &BTreeSet::new()).unwrap();
        assert_eq!(second.components["injectedProtocolBytes"], 0);
        assert_eq!(second.components["snapshotBytes"], 0);
        assert!(!second.replay.messages.iter().any(|m| {
            m["content"]
                .as_str()
                .is_some_and(|v| v.contains("transient"))
        }));
        assert_eq!(
            &second.replay.messages[..first.replay.messages.len()],
            &first.replay.messages[..]
        );

        state.summary = "turn 3 transient note".into();
        experiment(&mut state, "step-3", json!({"success":true,"items":[3,4]}));
        let third = build(&mut state, root.path(), &BTreeSet::new()).unwrap();
        assert_eq!(third.components["injectedProtocolBytes"], 0);
        assert_eq!(
            &third.replay.messages[..second.replay.messages.len()],
            &second.replay.messages[..]
        );
    }
}
