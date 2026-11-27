//! Conditional shared engine knowledge from cited project evidence.

mod evidence;

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Read,
    path::{Path, PathBuf},
};

use gamel10n_translation::{
    TokenUsage,
    files::{hash_file, hash_text, read_json, write_json},
    provider::{ESTIMATED_BYTES_PER_TOKEN, OpenAiProvider},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    AdaptationLimits, CancellationToken, EngineError, EngineProgress, EngineService,
    ProgressCallback,
    adaptation::{
        Session, add_usage, completion_usage, definition, file_error, page_text, text, tokens,
    },
    discovery::{Inventory, InventoryFile, resolve_relative},
};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineReviewResult {
    #[serde(default)]
    pub cache_version: u32,
    #[serde(default)]
    pub knowledge_dependencies: Vec<String>,
    pub evidence_fingerprint: String,
    pub library_fingerprint: String,
    pub summary: String,
    pub entry_id: Option<String>,
    pub revision: Option<String>,
    pub usage: TokenUsage,
    #[serde(default)]
    pub api_requests: Option<u64>,
    pub main_program_suggestions: Vec<String>,
    pub reused: bool,
}

type SourceHashes = Vec<(String, String)>;

pub const ADAPTATION_SESSION_PATH: &str = "adaptation/current/session.json";

const ADAPTATION_WORKSPACE_DIR: &str = "adaptation/current";
const ADAPTATION_SRC_DIR: &str = "adaptation/current/src";
const SESSION_FILE: &str = "session.json";
const REVIEW_SESSION_LOG_PATH: &str = "logs/engine-review-session.json";
const OVERVIEW_EVIDENCE_PATH: &str = "overview";
const KNOWLEDGE_INSPECTION_PREFIX: &str = "knowledge:";
const FAMILY_MATCH_FIELDS: [&str; 3] = ["key", "name", "adapterId"];
const REVIEW_CACHE_VERSION: u32 = 2;
const MAX_KNOWLEDGE_OFFSET: u64 = 100_000;
const KNOWLEDGE_NAME_LIMIT: usize = 160;
const KNOWLEDGE_KEY_LIMIT: usize = 80;
const MAX_KNOWLEDGE_TOOLS: usize = 4;
const MAX_KNOWLEDGE_ALIASES: usize = 12;
const MAX_KNOWLEDGE_TOOL_FILES: usize = 12;
const MAX_KNOWLEDGE_PAGE_ITEMS: usize = 12;
const REVIEW_SUMMARY_LIMIT: usize = 4000;
const KNOWLEDGE_SUMMARY_LIMIT: usize = 400;
const CONDITIONS_LIMIT: usize = 1600;
const GUIDANCE_LIMIT: usize = 8000;

fn review_evidence(directory: &Path) -> Result<(Vec<InventoryFile>, SourceHashes), EngineError> {
    let source = directory.join(ADAPTATION_SRC_DIR);
    let mut evidence = Vec::new();
    let mut hashes = Vec::new();
    if source.is_dir() {
        let inventory = Inventory::scan(&source)?;
        if !inventory.complete {
            return Err(EngineError::Protocol(
                "适配器源码清单不完整，无法核对总结证据。".into(),
            ));
        }
        for file in inventory.files {
            let path = format!("{ADAPTATION_SRC_DIR}/{}", file.path);
            hashes.push((
                path.clone(),
                hash_file(&resolve_relative(directory, &path)?).map_err(file_error)?,
            ));
            evidence.push(InventoryFile {
                path,
                length: file.length,
            });
        }
    }
    // Logs are readable evidence, but routine logging and review output do not invalidate a review.
    let logs = directory.join("logs");
    if logs.is_dir() {
        for entry in fs::read_dir(logs)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if entry.file_type()?.is_file() && !name.contains("review") {
                evidence.push(InventoryFile {
                    path: format!("logs/{name}"),
                    length: entry.metadata()?.len(),
                });
            }
        }
    }
    evidence.sort_by(|a, b| a.path.cmp(&b.path));
    Ok((evidence, hashes))
}

fn review_library_fingerprint(
    library: &[Value],
    state: &Session,
    dependencies: &[String],
) -> Result<String, EngineError> {
    let mut keys = crate::adaptation_context::knowledge_candidates(state);
    for field in FAMILY_MATCH_FIELDS {
        if let Some(value) = state.family[field].as_str() {
            keys.push(value.into());
        }
    }
    let relevant: Vec<_> = library.iter().filter(|entry| {
        knowledge_match_score(entry, &keys) > 0
            || knowledge_id(entry).is_some_and(|id| dependencies.contains(&id))
    }).map(|entry| json!({"id":knowledge_id(entry),"revision":entry["revision"],"enabled":entry["enabled"]})).collect();
    Ok(hash_text(&serde_json::to_string(&relevant)?))
}

fn review_fingerprint(
    state: &Session,
    source_hashes: &[(String, String)],
) -> Result<String, EngineError> {
    Ok(hash_text(&serde_json::to_string(&json!({
        "objective":state.objective,"instruction":state.user_instruction,"summary":state.summary,
        "checkpoint":state.checkpoint,"verification":state.verification,"coverage":state.text_coverage,
        "family":state.family,"messages":state.messages,"history":state.history_storage,"sources":source_hashes,
        "verifiedFingerprint":state.verified_fingerprint,"package":state.verified_package,"releaseFiles":state.release_files,
    }))?))
}

pub(crate) fn read_knowledge(
    directory: &Path,
    args: &Value,
    candidate_keys: Option<&[String]>,
) -> Result<Value, EngineError> {
    if let Some(id) = args
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
    {
        let path = resolve_relative(directory, &format!("{id}.json"))?;
        let entry = match read_knowledge_entry(&path, id) {
            Err(EngineError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(EngineError::Rejected("没有找到所选引擎经验。".into()));
            }
            result => result?,
        };
        if entry["enabled"].as_bool() == Some(false) {
            return Err(EngineError::Rejected("所选引擎经验已停用。".into()));
        }
        return Ok(entry);
    }
    let entries = knowledge_entries(directory)?;
    let query = args
        .get("query")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_lowercase();
    let offset = match args.get("offset") {
        None => 0,
        Some(value) => value
            .as_u64()
            .filter(|offset| *offset <= MAX_KNOWLEDGE_OFFSET)
            .ok_or_else(|| {
                EngineError::Protocol(format!("offset 必须为 0–{MAX_KNOWLEDGE_OFFSET} 的整数。"))
            })? as usize,
    };
    let mut matched: Vec<_> = entries
        .iter()
        .filter(|entry| {
            entry
                .get("enabled")
                .and_then(Value::as_bool)
                .unwrap_or(true)
                && if query.is_empty() {
                    candidate_keys.is_none_or(|keys| knowledge_match_score(entry, keys) > 0)
                } else {
                    knowledge_names(entry)
                        .iter()
                        .any(|name| name.to_lowercase().contains(&query))
                        || knowledge_id(entry).is_some_and(|id| id.to_lowercase().contains(&query))
                        || entry["summary"]
                            .as_str()
                            .is_some_and(|summary| summary.to_lowercase().contains(&query))
                }
        })
        .collect();
    if let Some(keys) = candidate_keys.filter(|_| query.is_empty()) {
        matched.sort_by_key(|entry| std::cmp::Reverse(knowledge_match_score(entry, keys)));
    }
    let page = knowledge_page(matched.iter().skip(offset).map(|entry| {
        let mut preview = knowledge_preview(entry);
        let family = &entry["family"];
        preview["family"] =
            json!({"kind":family["kind"],"key":family["key"],"name":family["name"]});
        preview
    }));
    let next_offset = offset + page.len();
    Ok(
        json!({"entries":page,"totalMatches":matched.len(),"nextOffset":next_offset,"more":next_offset<matched.len(),"note":"列表仅含截短预览，按 id 读取完整条件和指导；按 nextOffset 翻页。同引擎仅表示调查线索，无匹配时按正常分析继续。"}),
    )
}

pub(crate) fn knowledge_preview(entry: &Value) -> Value {
    json!({"id":knowledge_id(entry),"revision":entry["revision"],
        "conditions":entry["conditions"].as_str().unwrap_or("").chars().take(240).collect::<String>(),
        "summary":entry["summary"].as_str().unwrap_or("").chars().take(200).collect::<String>()})
}

pub(crate) fn knowledge_page(items: impl Iterator<Item = Value>) -> Vec<Value> {
    let mut page = Vec::new();
    let mut bytes = 0;
    for item in items.take(MAX_KNOWLEDGE_PAGE_ITEMS) {
        let size = item.to_string().len() + 1;
        // Leave room for metadata below the history's 8 KiB preview threshold.
        // A single oversized source remains retrievable through read_tool_result.
        if !page.is_empty() && bytes + size > 6 * 1024 {
            break;
        }
        bytes += size;
        page.push(item);
    }
    page
}

fn knowledge_names(entry: &Value) -> Vec<&str> {
    FAMILY_MATCH_FIELDS
        .iter()
        .filter_map(|key| entry["family"][key].as_str())
        .chain(
            entry["aliases"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str),
        )
        .collect()
}

fn knowledge_match_score(entry: &Value, keys: &[String]) -> u8 {
    crate::adaptation_context::knowledge_match_score(entry, keys)
}

impl EngineService {
    pub async fn review(
        &self,
        project_directory: &Path,
        knowledge_directory: &Path,
        provider: &OpenAiProvider,
        limits: &AdaptationLimits,
        cancellation: &CancellationToken,
        progress: ProgressCallback<'_>,
    ) -> Result<EngineReviewResult, EngineError> {
        limits.validate()?;
        let workspace = project_directory.join(ADAPTATION_WORKSPACE_DIR);
        let session_path = workspace.join(SESSION_FILE);
        let project_root = project_directory.to_owned();
        let knowledge_root = knowledge_directory.to_owned();
        let (state, evidence, evidence_fingerprint, library) =
            tokio::task::spawn_blocking(move || -> Result<_, EngineError> {
                let state = crate::adaptation::load_session(&session_path)?;
                let (evidence, hashes) = review_evidence(&project_root)?;
                let fingerprint = review_fingerprint(&state, &hashes)?;
                let library = knowledge_entries(&knowledge_root)?;
                Ok((state, evidence, fingerprint, library))
            })
            .await
            .map_err(file_error)??;
        let report = project_directory.join("engine-review.json");
        if let Ok(mut previous) = read_json::<EngineReviewResult>(&report)
            && previous.cache_version == REVIEW_CACHE_VERSION
            && previous.evidence_fingerprint == evidence_fingerprint
            && previous.library_fingerprint
                == review_library_fingerprint(&library, &state, &previous.knowledge_dependencies)?
        {
            previous.reused = true;
            Self::report(
                progress,
                "已有总结可用",
                "项目证据和相关经验未变化，复用上次总结，本次不调用模型。",
            );
            return Ok(previous);
        }
        let overview = evidence::overview(&state, &evidence);
        let mut messages = vec![
            json!({"role":"system","content":format!("{}\nRust host: read_evidence is bounded/read-only. Existing Python helpers may be copied only from inspected sources with attribution. Never execute scripts or the game.\n",include_str!("../prompts/review.md"))}),
            json!({"role":"user","content":json!({"task":"总结当前项目对后续同引擎/格式变体的可靠经验，无有用改进时 improvement=null。","overview":overview,"existingKnowledge":read_knowledge(knowledge_directory,&json!({}),None)?}).to_string()}),
        ];
        let tools = review_tools();
        let mut usage = TokenUsage::default();
        let requests_before = provider.request_count();
        let mut inspected = BTreeMap::new();
        let mut turn = 0;
        let mut latest_exchange = 0;
        let submission = 'review: loop {
            if cancellation.is_cancelled() {
                return Err(EngineError::Cancelled);
            }
            let request_messages = evidence::request_messages(&messages, latest_exchange);
            let estimated = (serde_json::to_vec(&request_messages)?.len()
                + serde_json::to_vec(&tools)?.len()) as u64
                / ESTIMATED_BYTES_PER_TOKEN
                + 1;
            let remaining = limits.maximum_tokens.saturating_sub(tokens(&usage));
            if remaining < estimated + 128 {
                break None;
            }
            turn += 1;
            if let Some(progress) = progress {
                progress(EngineProgress {
                    stage: "总结引擎经验".into(),
                    message: "正在核对可复用结论。".into(),
                    usage: usage.clone(),
                    turn,
                    api_requests: Some(provider.request_count().saturating_sub(requests_before)),
                    tool_name: None,
                    status_only: true,
                    ..EngineProgress::default()
                });
            }
            let response = provider
                .complete_review(
                    &request_messages,
                    &tools,
                    limits
                        .maximum_output_tokens
                        .min((remaining - estimated).min(u64::from(u32::MAX)) as u32),
                    cancellation,
                    &|error, retry| {
                        if let Some(progress) = progress {
                            progress(EngineProgress {
                                stage: "重试总结请求".into(),
                                message: format!(
                                    "{}，正在进行第 {retry} 次重试",
                                    error.failure_summary()
                                ),
                                usage: usage.clone(),
                                turn,
                                api_requests: Some(
                                    provider.request_count().saturating_sub(requests_before),
                                ),
                                ..EngineProgress::default()
                            });
                        }
                    },
                )
                .await;
            let completion = match response {
                Ok(completion) => completion,
                Err(error) if error.is_cancelled() => return Err(EngineError::Cancelled),
                Err(error) => return Err(EngineError::Api(Box::new(error))),
            };
            add_usage(&mut usage, &completion_usage(&completion, estimated)?);
            if tokens(&usage) > limits.maximum_tokens {
                break None;
            }
            latest_exchange = messages.len();
            if completion.output_items.is_empty() {
                messages.push(json!({"role":"assistant","content":completion.text,"tool_calls":completion.tool_calls.iter().map(|call|json!({"id":call.id,"type":"function","function":{"name":call.name,"arguments":call.arguments}})).collect::<Vec<_>>()}));
            } else {
                messages.extend(completion.output_items);
            }
            if completion.tool_calls.is_empty() {
                messages.push(json!({"role":"user","content":"核实已有证据后调用 finish_review 结束；没有新的可靠优化时 improvement=null。"}));
                continue;
            }
            let finish_alone = completion.tool_calls.len() == 1;
            for call in completion.tool_calls {
                let mut args = serde_json::from_str::<Value>(&call.arguments)?;
                if matches!(call.name.as_str(), "read_evidence" | "read_tool_result") {
                    let length = args["length"].as_u64().unwrap_or(2400).min(4096);
                    args["length"] = json!(length);
                }
                if let Some(progress) = progress {
                    progress(EngineProgress {
                        stage: "执行总结工具".into(),
                        message: crate::tool_activity::message(&call.name, &args),
                        tool_name: Some(call.name.clone()),
                        usage: usage.clone(),
                        turn,
                        api_requests: Some(
                            provider.request_count().saturating_sub(requests_before),
                        ),
                        ..EngineProgress::default()
                    });
                }
                let result = match call.name.as_str() {
                    "read_evidence" => {
                        let relative = text(&args, "path")?;
                        if relative == OVERVIEW_EVIDENCE_PATH {
                            Ok(json!({"evidencePath":OVERVIEW_EVIDENCE_PATH,"result":overview}))
                        } else if relative.ends_with(SESSION_FILE) {
                            Err(EngineError::Rejected(
                                "会话正文请按工具 ID 使用 read_tool_result，避免读取全部会话。"
                                    .into(),
                            ))
                        } else if !evidence.iter().any(|file| file.path == relative) {
                            Err(EngineError::Rejected(
                                "请从 overview.evidenceFiles 选择项目证据。".into(),
                            ))
                        } else {
                            let path = resolve_relative(project_directory, relative)?;
                            let content = fs::read_to_string(path)?;
                            inspected.insert(relative.into(), content.clone());
                            let mut value = page_text(&content, &args);
                            value["evidencePath"] = json!(relative);
                            Ok(value)
                        }
                    }
                    "read_tool_result" => evidence::tool_result(
                        &workspace, &state, &messages, &args,
                    )
                    .inspect(|value| {
                        if value["evidencePath"] == ADAPTATION_SESSION_PATH
                            && value["found"] != false
                        {
                            inspected.insert(ADAPTATION_SESSION_PATH.into(), String::new());
                        }
                    }),
                    "read_knowledge" => {
                        if let Some(id) = args.get("id").and_then(Value::as_str) {
                            inspected.insert(
                                format!("{KNOWLEDGE_INSPECTION_PREFIX}{id}"),
                                String::new(),
                            );
                        }
                        read_knowledge(knowledge_directory, &args, None)
                    }
                    "finish_review" if !finish_alone => {
                        Err(EngineError::Rejected("finish_review 必须单独调用。".into()))
                    }
                    "finish_review" => match submit_review(
                        &args,
                        knowledge_directory,
                        project_directory,
                        &state,
                        &evidence_fingerprint,
                        &inspected,
                        &evidence,
                    ) {
                        Err(
                            error @ (EngineError::Protocol(_)
                            | EngineError::Rejected(_)
                            | EngineError::Json(_)),
                        ) => Err(error),
                        Err(error) => return Err(error),
                        Ok(submission) => break 'review Some(submission),
                    },
                    _ => Err(EngineError::Rejected(
                        "总结仅允许读取证据、读取经验和 finish_review。".into(),
                    )),
                };
                let value = match result {
                    Ok(value) => value,
                    Err(error) if call.name == "finish_review" => {
                        let reason = match error {
                            EngineError::Protocol(reason) | EngineError::Rejected(reason) => reason,
                            other => other.to_string(),
                        };
                        if let Some(progress) = progress {
                            progress(EngineProgress {
                                stage: "修正经验总结".into(),
                                message: format!("总结提交未通过，正在请求模型修正。{reason}"),
                                usage: usage.clone(),
                                turn,
                                api_requests: Some(
                                    provider.request_count().saturating_sub(requests_before),
                                ),
                                ..EngineProgress::default()
                            });
                        }
                        json!({"success":false,"error":reason,"instruction":"沿用已读证据，修正后重新提交。"})
                    }
                    Err(error) => json!({"success":false,"error":error.to_string()}),
                };
                messages.push(
                    json!({"role":"tool","tool_call_id":call.id,"content":value.to_string()}),
                );
                write_json(
                    &project_directory.join(REVIEW_SESSION_LOG_PATH),
                    &json!({"messages":messages,"usage":usage,"finished":false}),
                )
                .map_err(file_error)?;
            }
        };
        let submission = submission
            .ok_or_else(|| EngineError::Rejected("本次总结的 Token 预算不足。".into()))?;
        let entry_id = submission.entry_id;
        let mut knowledge_dependencies: Vec<_> = inspected
            .keys()
            .filter_map(|key| key.strip_prefix(KNOWLEDGE_INSPECTION_PREFIX).map(str::to_owned))
            .collect();
        if let Some(id) = &entry_id {
            knowledge_dependencies.push(id.clone());
        }
        knowledge_dependencies.sort();
        knowledge_dependencies.dedup();
        let result = EngineReviewResult {
            cache_version: REVIEW_CACHE_VERSION,
            library_fingerprint: review_library_fingerprint(
                &knowledge_entries(knowledge_directory)?,
                &state,
                &knowledge_dependencies,
            )?,
            knowledge_dependencies,
            evidence_fingerprint,
            summary: submission.summary,
            entry_id,
            revision: submission.revision,
            usage,
            api_requests: Some(provider.request_count().saturating_sub(requests_before)),
            main_program_suggestions: submission.suggestions,
            reused: false,
        };
        write_json(&report, &result).map_err(file_error)?;
        write_json(
            &project_directory.join(REVIEW_SESSION_LOG_PATH),
            &json!({"messages":messages,"usage":result.usage,"finished":true}),
        )
        .map_err(file_error)?;
        Ok(result)
    }
}

pub(crate) fn knowledge_entries(directory: &Path) -> Result<Vec<Value>, EngineError> {
    knowledge_entries_with_warning(directory, |_| {})
}

pub(crate) fn knowledge_entries_with_warning(
    directory: &Path,
    mut warning: impl FnMut(String),
) -> Result<Vec<Value>, EngineError> {
    if !directory.is_dir() {
        return Ok(vec![]);
    }
    let mut entries = Vec::new();
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        if !path
            .extension()
            .and_then(|value| value.to_str())
            .is_some_and(|value| value.eq_ignore_ascii_case("json"))
        {
            continue;
        }
        let result = path
            .file_stem()
            .and_then(|name| name.to_str())
            .ok_or_else(|| EngineError::Protocol("无效的引擎经验标识。".into()))
            .and_then(|id| {
                let resolved = resolve_relative(directory, &entry.file_name().to_string_lossy())?;
                read_knowledge_entry(&resolved, id)
            });
        match result {
            Ok(value) => entries.push(value),
            Err(error) => warning(format!(
                "已跳过不可用的引擎经验 {}：{error}",
                entry.file_name().to_string_lossy()
            )),
        }
    }
    entries.sort_by_key(knowledge_id);
    Ok(entries)
}

const MAX_KNOWLEDGE_BYTES: u64 = 512 * 1024;

fn read_knowledge_entry(path: &Path, id: &str) -> Result<Value, EngineError> {
    if !stable_key(id, KNOWLEDGE_NAME_LIMIT) || id.len() < 2 {
        return Err(EngineError::Protocol("无效的引擎经验标识。".into()));
    }
    let file = fs::File::open(path)?;
    if file.metadata()?.len() > MAX_KNOWLEDGE_BYTES {
        return Err(EngineError::Protocol("经验文件过大。".into()));
    }
    let mut bytes = Vec::new();
    file.take(MAX_KNOWLEDGE_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_KNOWLEDGE_BYTES {
        return Err(EngineError::Protocol("经验文件过大。".into()));
    }
    let mut value: Value =
        serde_json::from_slice(bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&bytes))?;
    validate_knowledge(&value)?;
    if knowledge_id(&value).as_deref() != Some(id)
        || value["revision"].as_str() != Some(&knowledge_revision(&value))
    {
        return Err(EngineError::Protocol("经验内容或标识与版本不一致。".into()));
    }
    if value.get("enabled").is_none() {
        value["enabled"] = json!(true);
    }
    Ok(value)
}

fn stable_key(value: &str, maximum: usize) -> bool {
    !value.is_empty()
        && value.len() <= maximum
        && value.as_bytes()[0].is_ascii_alphanumeric()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || b".-".contains(&byte))
}

fn require_knowledge_text<'a>(
    entry: &'a Value,
    key: &str,
    maximum: usize,
) -> Result<&'a str, EngineError> {
    let value = text(entry, key)?;
    check_text_length(
        format_args!("经验 {key}"),
        value,
        value.encode_utf16().count(),
        maximum,
    )?;
    Ok(value)
}

fn check_text_length(
    field: impl std::fmt::Display,
    value: &str,
    length: usize,
    maximum: usize,
) -> Result<(), EngineError> {
    if value.trim().is_empty() {
        return Err(EngineError::Protocol(format!(
            "{field} 需要填写非空白文本。"
        )));
    }
    if length > maximum {
        return Err(EngineError::Protocol(format!(
            "{field} 长度为 {length}，上限为 {maximum}。"
        )));
    }
    Ok(())
}

fn validate_knowledge(entry: &Value) -> Result<(), EngineError> {
    validate_knowledge_fields(entry)?;
    validate_knowledge_attachments(entry)
}

fn validate_knowledge_fields(entry: &Value) -> Result<(), EngineError> {
    let family = &entry["family"];
    let key = text(family, "key")?;
    let name = require_knowledge_text(family, "name", KNOWLEDGE_NAME_LIMIT)?;
    if !matches!(family["kind"].as_str(), Some("engine" | "template"))
        || !stable_key(key, KNOWLEDGE_KEY_LIMIT)
        || name.chars().any(char::is_control)
        || !stable_key(text(entry, "key")?, 64)
    {
        return Err(EngineError::Protocol(
            "经验的引擎分类或变体标识无效。".into(),
        ));
    }
    for (key, maximum) in [
        ("conditions", CONDITIONS_LIMIT),
        ("summary", KNOWLEDGE_SUMMARY_LIMIT),
        ("guidance", GUIDANCE_LIMIT),
    ] {
        require_knowledge_text(entry, key, maximum)?;
    }
    let invalid_aliases = || {
        EngineError::Protocol(format!(
            "经验的 aliases 必须为最多 {MAX_KNOWLEDGE_ALIASES} 项的数组，每项须为非空白文本，长度上限为 {KNOWLEDGE_NAME_LIMIT}。"
        ))
    };
    let aliases = entry["aliases"].as_array().ok_or_else(invalid_aliases)?;
    if aliases.len() > MAX_KNOWLEDGE_ALIASES
        || aliases.iter().any(|alias| {
            alias.as_str().is_none_or(|alias| {
                alias.trim().is_empty() || alias.encode_utf16().count() > KNOWLEDGE_NAME_LIMIT
            })
        })
    {
        return Err(invalid_aliases());
    }
    Ok(())
}

fn validate_knowledge_attachments(entry: &Value) -> Result<(), EngineError> {
    let invalid = || EngineError::Protocol("经验的工具或证据来源无效。".into());
    let origins = entry["origins"].as_array().ok_or_else(invalid)?;
    if origins.is_empty()
        || origins.iter().any(|origin| {
            origin["gameId"]
                .as_str()
                .is_none_or(|id| id.trim().is_empty())
                || origin["evidence"].as_array().is_none_or(Vec::is_empty)
        })
    {
        return Err(invalid());
    }
    if entry
        .get("enabled")
        .is_some_and(|enabled| !enabled.is_boolean())
    {
        return Err(invalid());
    }
    let tools = entry["tools"].as_array().ok_or_else(invalid)?;
    let mut names = BTreeSet::new();
    let mut source_bytes = 0usize;
    if tools.len() > MAX_KNOWLEDGE_TOOLS {
        return Err(invalid());
    }
    for tool in tools {
        let name = require_knowledge_text(tool, "name", KNOWLEDGE_KEY_LIMIT)?;
        require_knowledge_text(tool, "description", KNOWLEDGE_SUMMARY_LIMIT)?;
        require_knowledge_text(tool, "usage", CONDITIONS_LIMIT)?;
        if !names.insert(name) {
            return Err(invalid());
        }
        let files = tool["files"].as_object().ok_or_else(invalid)?;
        let mut paths = BTreeSet::new();
        if files.is_empty() || files.len() > MAX_KNOWLEDGE_TOOL_FILES {
            return Err(invalid());
        }
        for (path, source) in files {
            resolve_relative(Path::new("."), path)?;
            let source = source.as_str().ok_or_else(invalid)?;
            if !matches!(
                Path::new(path)
                    .extension()
                    .and_then(|extension| extension.to_str()),
                Some("cs" | "py")
            ) || source.encode_utf16().count() > 64 * 1024
                || !paths.insert(path.to_lowercase())
            {
                return Err(invalid());
            }
            source_bytes += source.len();
        }
    }
    if source_bytes > 128 * 1024 {
        return Err(invalid());
    }
    Ok(())
}

// Match System.Text.Json's default encoder and the original content-only revision.
// Origins, enabled state and stored revision do not change the content fingerprint.
fn knowledge_revision(entry: &Value) -> String {
    let family = &entry["family"];
    let mut aliases = entry["aliases"].as_array().cloned().unwrap_or_default();
    aliases.sort_by(|a, b| {
        a.as_str()
            .unwrap_or("")
            .encode_utf16()
            .cmp(b.as_str().unwrap_or("").encode_utf16())
    });
    let mut tools = entry["tools"].as_array().cloned().unwrap_or_default();
    tools.sort_by(|a, b| {
        a["name"]
            .as_str()
            .unwrap_or("")
            .encode_utf16()
            .cmp(b["name"].as_str().unwrap_or("").encode_utf16())
    });
    let tools:Vec<_>=tools.iter().map(|tool| {
        let mut files:Vec<_>=tool["files"].as_object().into_iter().flatten().collect();
        files.sort_by(|a,b|a.0.encode_utf16().cmp(b.0.encode_utf16()));
        json!({"Name":tool["name"],"Description":tool["description"],"Usage":tool["usage"],"files":files.into_iter().map(|(path,source)|json!({"Key":path,"Value":source})).collect::<Vec<_>>()})
    }).collect();
    let content = json!({"Key":entry["key"],"Family":{"Kind":family["kind"],"Key":family["key"],"Name":family["name"],"AdapterId":format!("{}.{}",family["kind"].as_str().unwrap_or(""),family["key"].as_str().unwrap_or(""))},"aliases":aliases,"Conditions":entry["conditions"],"Summary":entry["summary"],"Guidance":entry["guidance"],"tools":tools});
    hash_text(&csharp_json(&content))
}

fn csharp_json(value: &Value) -> String {
    match value {
        Value::String(value) => {
            use std::fmt::Write;
            let mut output = String::from("\"");
            for unit in value.encode_utf16() {
                match unit {
                    8 => output.push_str("\\b"),
                    9 => output.push_str("\\t"),
                    10 => output.push_str("\\n"),
                    12 => output.push_str("\\f"),
                    13 => output.push_str("\\r"),
                    92 => output.push_str("\\\\"),
                    32..=126 if ![34, 38, 39, 43, 60, 62, 96].contains(&unit) => {
                        output.push(char::from_u32(u32::from(unit)).unwrap())
                    }
                    _ => {
                        write!(output, "\\u{unit:04X}").expect("writing to String");
                    }
                }
            }
            output.push('"');
            output
        }
        Value::Array(values) => format!(
            "[{}]",
            values.iter().map(csharp_json).collect::<Vec<_>>().join(",")
        ),
        Value::Object(values) => format!(
            "{{{}}}",
            values
                .iter()
                .map(|(key, value)| format!("{}:{}", csharp_json(&json!(key)), csharp_json(value)))
                .collect::<Vec<_>>()
                .join(",")
        ),
        _ => value.to_string(),
    }
}
fn knowledge_id(value: &Value) -> Option<String> {
    Some(format!(
        "{}.{}.{}",
        value["family"]["kind"].as_str()?,
        value["family"]["key"].as_str()?,
        value["key"].as_str()?
    ))
}

struct SubmittedReview {
    summary: String,
    suggestions: Vec<String>,
    entry_id: Option<String>,
    revision: Option<String>,
}

fn submit_review(
    args: &Value,
    directory: &Path,
    project: &Path,
    state: &Session,
    fingerprint: &str,
    inspected: &BTreeMap<String, String>,
    evidence: &[InventoryFile],
) -> Result<SubmittedReview, EngineError> {
    let summary = text(args, "summary")?;
    check_text_length(
        "项目总结 summary",
        summary,
        summary.chars().count(),
        REVIEW_SUMMARY_LIMIT,
    )?;
    let suggestions = args
        .get("mainProgramSuggestions")
        .map(Vec::<String>::deserialize)
        .transpose()?
        .unwrap_or_default();
    let (entry_id, revision) = match args.get("improvement").filter(|value| !value.is_null()) {
        Some(draft) => save_improvement(
            directory,
            project,
            state,
            fingerprint,
            draft,
            inspected,
            evidence,
        )?,
        None => (None, None),
    };
    Ok(SubmittedReview {
        summary: summary.to_owned(),
        suggestions,
        entry_id,
        revision,
    })
}

fn save_improvement(
    directory: &Path,
    project: &Path,
    state: &Session,
    fingerprint: &str,
    draft: &Value,
    inspected: &BTreeMap<String, String>,
    evidence_files: &[InventoryFile],
) -> Result<(Option<String>, Option<String>), EngineError> {
    validate_knowledge_fields(draft)?;
    let id = knowledge_id(draft)
        .ok_or_else(|| EngineError::Protocol("经验缺少稳定的引擎分类。".into()))?;
    let path = resolve_relative(directory, &format!("{id}.json"))?;
    let citations: Vec<String> = serde_json::from_value(draft["evidence"].clone())?;
    if citations.is_empty()
        || citations.len() > 8
        || citations.iter().any(|path| {
            path != OVERVIEW_EVIDENCE_PATH
                && (!inspected.contains_key(path)
                    || path != ADAPTATION_SESSION_PATH
                        && !evidence_files.iter().any(|file| file.path == *path))
        })
    {
        return Err(EngineError::Rejected(
            "经验必须引用 overview 或实际读取过的项目证据，最多 8 项。".into(),
        ));
    }
    let previous = if path.try_exists()? {
        Some(read_knowledge_entry(&path, &id).map_err(|error| {
            match error {
                EngineError::Io(_) => error,
                _ => std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("读取已有引擎经验 {id} 失败：{error}"),
                )
                .into(),
            }
        })?)
    } else {
        None
    };
    if previous.is_some()
        && !inspected.contains_key(&format!("{KNOWLEDGE_INSPECTION_PREFIX}{id}"))
    {
        return Err(EngineError::Rejected(
            "更新已有变体经验前必须 read_knowledge(id) 核对完整内容。".into(),
        ));
    }
    let mut entry = draft.clone();
    let mut origins = previous
        .as_ref()
        .and_then(|value| value["origins"].as_array())
        .cloned()
        .unwrap_or_default();
    origins.retain(|origin| origin["gameId"].as_str() != Some(&state.game_id));
    origins.push(json!({"gameId":state.game_id,"title":project.file_name().map(|value|value.to_string_lossy()).unwrap_or_default(),"evidenceFingerprint":fingerprint,"evidence":citations}));
    entry["origins"] = json!(origins);
    entry["enabled"] = previous
        .as_ref()
        .map(|entry| entry["enabled"].clone())
        .unwrap_or(json!(true));
    let mut tools = previous
        .as_ref()
        .and_then(|value| value["tools"].as_array())
        .cloned()
        .unwrap_or_default();
    if let Some(captured) = draft.get("tools").and_then(Value::as_array) {
        if captured.len() > MAX_KNOWLEDGE_TOOLS {
            return Err(EngineError::Protocol(format!(
                "经验最多保存 {MAX_KNOWLEDGE_TOOLS} 个参数化 Python 工具。"
            )));
        }
        for tool in captured {
            let name = text(tool, "name")?;
            let source_files: Vec<String> = serde_json::from_value(tool["sourceFiles"].clone())?;
            let mut files = BTreeMap::new();
            for relative in source_files {
                if !relative.ends_with(".py") || !inspected.contains_key(&relative) {
                    return Err(EngineError::Rejected(
                        "只能保存已读取并确认可复用的 Python 源码。".into(),
                    ));
                }
                let content = fs::read_to_string(resolve_relative(project, &relative)?)?;
                if content.contains(&state.game_directory.to_string_lossy().to_string()) {
                    return Err(EngineError::Rejected(
                        "共享经验工具不能包含当前游戏绝对路径。".into(),
                    ));
                }
                files.insert(
                    PathBuf::from(relative)
                        .file_name()
                        .ok_or_else(|| EngineError::Protocol("经验源码文件名无效。".into()))?
                        .to_string_lossy()
                        .into_owned(),
                    content,
                );
            }
            let stored = json!({"name":name,"description":tool["description"],"usage":tool["usage"],"files":files});
            tools.retain(|previous| previous["name"].as_str() != Some(name));
            tools.push(stored);
        }
    }
    entry["tools"] = json!(tools);
    entry
        .as_object_mut()
        .map(|object| object.remove("evidence"));
    validate_knowledge_attachments(&entry)?;
    let revision = knowledge_revision(&entry);
    entry["revision"] = json!(revision);
    write_json(&path, &entry).map_err(std::io::Error::other)?;
    Ok((Some(id), Some(revision)))
}

fn review_tools() -> Vec<Value> {
    let string = json!({"type":"string"});
    let integer = json!({"type":"integer","minimum":0});
    let knowledge_text = |maximum: usize, description: &str| {
        json!({
            "type":"string", "minLength":1, "maxLength":maximum,
            "description":format!("{description} Nonblank, at most {maximum} UTF-16 code units.")
        })
    };
    vec![
        definition(
            "read_evidence",
            "Read a source or log excerpt. Use a single exact symbol or diagnostic to locate what is missing. path=overview returns the host's facts. Cite returned evidencePath.",
            json!({"path":string,"query":string,"offset":integer,"length":integer}),
            &["path"],
        ),
        definition(
            "read_tool_result",
            "Read the retained original result by toolCallId. Use source=review for a truncated review result or source=adaptation for adaptation evidence.",
            json!({"toolCallId":string,"query":string,"offset":integer,"length":integer,"source":{"type":"string","enum":["auto","review","adaptation"]}}),
            &["toolCallId"],
        ),
        definition(
            "read_knowledge",
            "Search short shared engine experience previews by query; use nextOffset to page. Read full id before merging. Entries are conditional references.",
            json!({"id":string,"query":string,"offset":{"type":"integer","minimum":0,"maximum":MAX_KNOWLEDGE_OFFSET}}),
            &[],
        ),
        definition(
            "finish_review",
            "Save concise conclusion and at most one cited conditional engine/variant improvement. improvement=null is correct when no supported useful finding remains. Preserve valid existing guidance and tools. Call alone.",
            json!({
                "summary":{"type":"string","minLength":1,"maxLength":REVIEW_SUMMARY_LIMIT,"description":"Nonblank project conclusion."},
                "mainProgramSuggestions":{"type":"array","items":string},
                "improvement":{"anyOf":[{"type":"null"}, {
                    "type":"object","properties":{
                        "key":string,
                        "family":{"type":"object","properties":{"kind":string,"key":string,"name":string},"required":["kind","key","name"]},
                        "aliases":{"type":"array","items":string},
                        "conditions":knowledge_text(CONDITIONS_LIMIT, "When the entry applies."),
                        "summary":knowledge_text(KNOWLEDGE_SUMMARY_LIMIT, "Short library preview. Put implementation details in guidance."),
                        "guidance":knowledge_text(GUIDANCE_LIMIT, "Reusable details for the next adaptation."),
                        "evidence":{"type":"array","items":string},
                        "tools":{"type":"array","items":{"type":"object","properties":{"name":string,"description":string,"usage":string,"sourceFiles":{"type":"array","items":string}},"required":["name","description","usage","sourceFiles"]}}
                    },"required":["key","family","aliases","conditions","summary","guidance","evidence","tools"]
                }]}
            }),
            &["summary", "mainProgramSuggestions", "improvement"],
        ),
    ]
}

#[cfg(test)]
mod knowledge_tests {
    use super::*;

    #[test]
    fn review_cache_ignores_own_logs_and_unrelated_knowledge_but_detects_same_size_source_changes()
    {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("adaptation/current/src")).unwrap();
        fs::create_dir(root.path().join("logs")).unwrap();
        let source = root.path().join("adaptation/current/src/adapter.py");
        fs::write(&source, "one").unwrap();
        let state: Session = serde_json::from_value(json!({"family":{"key":"fixture"}})).unwrap();
        let before = review_fingerprint(&state, &review_evidence(root.path()).unwrap().1).unwrap();
        fs::write(root.path().join("logs/20261107.log"), "task started").unwrap();
        fs::write(
            root.path().join("logs/engine-review-session.json"),
            "review output",
        )
        .unwrap();
        assert_eq!(
            before,
            review_fingerprint(&state, &review_evidence(root.path()).unwrap().1).unwrap()
        );
        fs::write(source, "two").unwrap();
        assert_ne!(
            before,
            review_fingerprint(&state, &review_evidence(root.path()).unwrap().1).unwrap()
        );
        let related = entry();
        let base = review_library_fingerprint(std::slice::from_ref(&related), &state, &[]).unwrap();
        let mut unrelated = entry();
        unrelated["family"]["key"] = json!("different");
        let mut library = vec![related, unrelated];
        assert_eq!(
            base,
            review_library_fingerprint(&library, &state, &[]).unwrap()
        );
        library[0]["revision"] = json!("updated");
        assert_ne!(
            base,
            review_library_fingerprint(&library, &state, &[]).unwrap()
        );
    }

    fn entry() -> Value {
        json!({"key":"variant","family":{"kind":"engine","key":"fixture","name":"测试 / \"safe\" <&>' +` \\ \n \u{8}\t\r\u{c}\u{7f} 😀"},"aliases":["a","B"],"conditions":"适用条件","summary":"概要","guidance":"指引","tools":[{"name":"probe","description":"说明","usage":"用法","files":{"a.py":"print(\"测试\")\n"}}],"origins":[{"gameId":"game","title":"Game","evidence":["overview"]}]})
    }

    #[test]
    fn revision_matches_original_csharp_serialization() {
        let mut entry = entry();
        assert_eq!(
            knowledge_revision(&entry),
            "40122b0ea3954643d096e2f880b987d486294b345045cbe603e34827826f303a"
        );
        let revision = knowledge_revision(&entry);
        entry["enabled"] = json!(false);
        entry["origins"] = json!([]);
        entry["revision"] = json!("ignored");
        assert_eq!(knowledge_revision(&entry), revision);
    }

    #[test]
    fn knowledge_search_pages_stay_readable_without_skipping_long_unicode_entries() {
        let root = tempfile::tempdir().unwrap();
        let mut expected_ids = Vec::new();
        for index in 0..25 {
            let mut value = entry();
            value["key"] = json!(format!("variant-{index:02}"));
            value["family"]["name"] = json!("测试引擎");
            value["conditions"] = json!("适用条件😀".repeat(250));
            value["summary"] = json!("概要😀".repeat(100));
            value["revision"] = json!(knowledge_revision(&value));
            let id = knowledge_id(&value).unwrap();
            write_json(&root.path().join(format!("{id}.json")), &value).unwrap();
            expected_ids.push(id);
        }

        let mut found_ids = Vec::new();
        let mut offset = 0;
        for _ in 0..25 {
            let page = read_knowledge(root.path(), &json!({"offset":offset}), None).unwrap();
            // The history layer must not replace a search page with a head/tail preview.
            assert!(page.to_string().len() < 8192);
            assert_eq!(page["totalMatches"], 25);
            for preview in page["entries"].as_array().unwrap() {
                assert!(preview["conditions"].as_str().unwrap().chars().count() <= 240);
                assert!(preview["summary"].as_str().unwrap().chars().count() <= 200);
                assert!(preview.get("guidance").is_none());
                found_ids.push(preview["id"].as_str().unwrap().to_owned());
            }
            if page["more"] == false {
                break;
            }
            let next = page["nextOffset"].as_u64().unwrap();
            assert!(next > offset);
            offset = next;
        }
        assert_eq!(found_ids, expected_ids);
        let full = read_knowledge(root.path(), &json!({"id":found_ids[0]}), None).unwrap();
        assert_eq!(full["conditions"], "适用条件😀".repeat(250));
        assert_eq!(full["tools"][0]["files"]["a.py"], "print(\"测试\")\n");
    }

    #[test]
    fn direct_knowledge_reads_preserve_identity_revision_and_enabled_checks() {
        let root = tempfile::tempdir().unwrap();
        let id = "engine.fixture.variant";
        let path = root.path().join(format!("{id}.json"));
        let mut valid = entry();
        valid["family"]["name"] = json!("测试引擎");
        valid["revision"] = json!(knowledge_revision(&valid));
        write_json(&path, &valid).unwrap();
        assert_eq!(
            read_knowledge(root.path(), &json!({"id":id}), None).unwrap()["guidance"],
            valid["guidance"]
        );
        write_json(&root.path().join("engine.fixture.wrong.json"), &valid).unwrap();
        for invalid in [
            "../engine.fixture.variant",
            "engine.fixture.wrong",
            "engine.fixture.missing",
        ] {
            assert!(read_knowledge(root.path(), &json!({"id":invalid}), None).is_err());
        }
        valid["enabled"] = json!(false);
        write_json(&path, &valid).unwrap();
        assert!(read_knowledge(root.path(), &json!({"id":id}), None).is_err());
        valid["enabled"] = json!(true);
        valid["guidance"] = json!("changed without updating revision");
        write_json(&path, &valid).unwrap();
        assert!(read_knowledge(root.path(), &json!({"id":id}), None).is_err());
    }

    #[test]
    fn skips_unusable_files_without_losing_valid_knowledge() {
        let root = tempfile::tempdir().unwrap();
        let mut valid = entry();
        valid["family"]["name"] = json!("测试 / Fixture");
        valid["revision"] = json!(knowledge_revision(&valid));
        write_json(&root.path().join("engine.fixture.variant.json"), &valid).unwrap();
        fs::write(root.path().join("broken.json"), "{").unwrap();
        fs::write(
            root.path().join("large.json"),
            vec![b' '; MAX_KNOWLEDGE_BYTES as usize + 1],
        )
        .unwrap();
        write_json(&root.path().join("wrong-id.json"), &valid).unwrap();
        valid["key"] = json!("tampered");
        write_json(&root.path().join("engine.fixture.tampered.json"), &valid).unwrap();
        valid["key"] = json!("invalid");
        valid["tools"][0]["files"] = json!({"../escape.py":"print('bad')"});
        valid["revision"] = json!(knowledge_revision(&valid));
        write_json(&root.path().join("engine.fixture.invalid.json"), &valid).unwrap();
        let mut warnings = Vec::new();
        let entries =
            knowledge_entries_with_warning(root.path(), |warning| warnings.push(warning)).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(
            knowledge_id(&entries[0]).as_deref(),
            Some("engine.fixture.variant")
        );
        assert_eq!(warnings.len(), 5);
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("经验文件过大"))
        );
    }
}
