//! Bounded parser trials and first-turn composition of complete resource templates.
use super::*;
use crate::discovery::{glob_match, is_link};
use sha2::{Digest, Sha256};
use std::{io::Read, time::Duration};

const TRIAL_BYTES: u64 = 64 * 1024 * 1024;
const TEMPLATE_BYTES: u64 = 256 * 1024 * 1024;
const RPA_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const RUNTIME_CHECK: &str = "游戏启动、中文显示、字体及排版尚未实机验证。";

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct SampleCheckMetrics {
    pub checks: u64,
    pub sample_count: u64,
    pub sample_bytes: u64,
    pub cache_hits: u64,
    pub elapsed_ms: u64,
}

impl SampleCheckMetrics {
    pub fn since(&self, before: &Self) -> Self {
        Self {
            checks: self.checks.saturating_sub(before.checks),
            sample_count: self.sample_count.saturating_sub(before.sample_count),
            sample_bytes: self.sample_bytes.saturating_sub(before.sample_bytes),
            cache_hits: self.cache_hits.saturating_sub(before.cache_hits),
            elapsed_ms: self.elapsed_ms.saturating_sub(before.elapsed_ms),
        }
    }

    pub fn record(&mut self, started: Instant, count: usize, bytes: u64, cached: bool) {
        self.checks += 1;
        self.sample_count += count as u64;
        self.sample_bytes += bytes;
        self.cache_hits += u64::from(cached);
        self.elapsed_ms += started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
    }

    pub fn include_elapsed(&mut self, started: Instant, before: &Self) {
        let elapsed = started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
        self.elapsed_ms +=
            elapsed.saturating_sub(self.elapsed_ms.saturating_sub(before.elapsed_ms));
    }
}

struct Budget {
    token: CancellationToken,
    timer: tokio::task::JoinHandle<()>,
}
impl Budget {
    fn new(parent: &CancellationToken, duration: Duration) -> Self {
        let token = parent.child_token();
        let deadline = token.clone();
        let timer = tokio::spawn(async move {
            tokio::time::sleep(duration).await;
            deadline.cancel();
        });
        Self { token, timer }
    }
}
impl Drop for Budget {
    fn drop(&mut self) {
        self.timer.abort();
    }
}
fn cancelled(token: &CancellationToken) -> Result<(), EngineError> {
    if token.is_cancelled() {
        Err(EngineError::Cancelled)
    } else {
        Ok(())
    }
}
fn reject(message: impl Into<String>) -> EngineError {
    EngineError::Rejected(message.into())
}
fn limit(value: &str, length: usize) -> String {
    value.chars().take(length).collect()
}
fn extension(path: &str) -> String {
    Path::new(path)
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
}

fn fresh(
    request: &AdaptationRequest<'_>,
    workspace: &Path,
    state: &Session,
) -> Result<bool, EngineError> {
    Ok(!request.require_model_analysis
        && request.inputs.files.is_empty()
        && request.inputs.excluded.is_empty()
        && request.objective.eq_ignore_ascii_case("develop")
        && !request.improve_existing
        && request.baseline_units.is_empty()
        && state.total_turns == 0
        && state.pending_calls.is_empty()
        && state.verified_package.is_none()
        && state.release_files.is_empty()
        && state
            .extra
            .get("reuseFailures")
            .and_then(Value::as_array)
            .is_none_or(Vec::is_empty)
        && (!workspace.join("src").is_dir()
            || fs::read_dir(workspace.join("src"))?.next().is_none()))
}

fn candidates(state: &Session) -> Vec<Value> {
    state
        .extra
        .get("builtinDiscovery")
        .and_then(|report| report["candidates"].as_array())
        .cloned()
        .unwrap_or_default()
}
async fn record(
    state: &mut Session,
    path: &Path,
    key: &str,
    value: Value,
) -> Result<(), EngineError> {
    state.extra.insert(key.into(), value);
    save_session(path, state).await
}

fn checked_hash(
    path: &Path,
    expected: u64,
    token: &CancellationToken,
) -> Result<String, EngineError> {
    cancelled(token)?;
    let mut input = fs::File::open(path)?;
    if input.metadata()?.len() != expected {
        return Err(reject("自动试用样本大小发生变化。"));
    }
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 128 * 1024];
    let mut total = 0_u64;
    loop {
        cancelled(token)?;
        let count = input.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        total += count as u64;
        if total > expected {
            return Err(reject("自动试用样本超出预算。"));
        }
        digest.update(&buffer[..count]);
    }
    if total != expected {
        return Err(reject("自动试用样本被截断。"));
    }
    Ok(format!("{:x}", digest.finalize()))
}
fn sample_hashes(
    root: &Path,
    lengths: &BTreeMap<String, u64>,
    token: &CancellationToken,
) -> Result<BTreeMap<String, String>, EngineError> {
    lengths
        .iter()
        .map(|(relative, length)| {
            Ok((
                relative.clone(),
                checked_hash(&resolve_relative(root, relative)?, *length, token)?,
            ))
        })
        .collect()
}

fn stage(
    game: &Path,
    _workspace: &Path,
    state: &mut Session,
    lengths: &BTreeMap<String, u64>,
    expected: &BTreeMap<String, String>,
    token: &CancellationToken,
) -> Result<(), EngineError> {
    if sample_hashes(game, lengths, token)? != *expected {
        return Err(reject("自动试用期间原始资源发生变化。"));
    }
    for (relative, hash) in expected {
        cancelled(token)?;
        let path = resolve_relative(game, relative)?;
        state
            .extra
            .entry("resourceHashes".into())
            .or_insert_with(|| json!({}))[relative] = json!(hash);
        state
            .extra
            .entry("resourcePaths".into())
            .or_insert_with(|| json!({}))[relative] = json!(path);
    }
    state.extra.insert("resourceRoot".into(), json!(game));
    Ok(())
}

fn sample_group(
    definition: &Value,
    root: &Path,
    script: &str,
    inputs: &AdaptationInputs,
) -> Result<Vec<String>, EngineError> {
    let script = script.replace('\\', "/");
    let source = inputs.file(root, &script)?;
    if !source.is_file() {
        return Err(reject(format!("候选样本不存在：{script}")));
    }
    let format = definition["format"].as_str().unwrap_or("");
    let mut required = format == "Softpal";
    if format == "Yuris" {
        let mut signature = [0; 4];
        required = fs::File::open(source)?.read(&mut signature)? == 4 && signature == *b"YSTB";
        if !required {
            return Ok(vec![script]);
        }
    }
    let mut group = vec![script.clone()];
    for companion in definition["companions"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        let relative = Path::new(&script)
            .parent()
            .unwrap_or(Path::new(""))
            .join(companion)
            .to_string_lossy()
            .replace('\\', "/");
        if inputs.file(root, &relative)?.is_file() {
            group.push(relative);
        } else if required {
            return Err(reject(format!(
                "{script} 缺少同目录必需伴随文件 {relative}。"
            )));
        }
    }
    Ok(group)
}
fn parser_fingerprint(
    service: &EngineService,
    token: &CancellationToken,
) -> Result<String, EngineError> {
    let executable = service.tools.vntextpatch();
    if !executable.is_file() {
        return Err(EngineError::ToolMissing(executable));
    }
    let assembly = executable.with_extension("dll");
    let runtime = executable.with_extension("runtimeconfig.json");
    let mut files = vec![executable];
    if assembly.is_file() {
        files.push(assembly);
    }
    if runtime.is_file() {
        files.push(runtime);
    }
    let capstone = service.tools.vntextpatch().with_file_name("capstone.dll");
    if capstone.is_file() {
        files.push(capstone);
    }
    let hashes: Vec<_> = files
        .iter()
        .map(|path| checked_hash(path, path.metadata()?.len(), token))
        .collect::<Result<_, EngineError>>()?;
    Ok(hash_text(&serde_json::to_string(&hashes)?))
}
fn deterministic(diagnostic: &Value) -> bool {
    let mut diagnostic = diagnostic;
    while let Some(inner) = diagnostic.get("inner").filter(|inner| inner.is_object()) {
        diagnostic = inner;
    }
    matches!(
        diagnostic["errorType"].as_str(),
        Some(
            "System.IO.InvalidDataException"
                | "System.IO.EndOfStreamException"
                | "System.Text.DecoderFallbackException"
                | "System.Text.EncoderFallbackException"
                | "System.ArgumentOutOfRangeException"
                | "System.IndexOutOfRangeException"
                | "System.OverflowException"
        )
    )
}

pub(super) async fn trial_response(
    service: &EngineService,
    request: &AdapterRequest,
    format: &str,
    result: &mut Value,
    cancellation: &CancellationToken,
) -> Result<crate::protocol::AdapterResponse, EngineError> {
    let temporary = tempfile::tempdir()?;
    let mut request = request.clone();
    request.format = Some(format.into());
    request.extra.insert(
        "responseFile".into(),
        json!(temporary.path().join("response.json")),
    );
    let response: Value = crate::ToolProcess::new(service.tools.vntextpatch())
        .with_response_file(temporary.path().join("response.json"))
        .with_failure_response()
        .execute(&request, cancellation)
        .await?;
    if response.get("diagnostic").is_some() {
        result["diagnostic"] = response["diagnostic"].clone();
    }
    serde_json::from_value::<crate::protocol::AdapterResponse>(response)?.checked()
}

pub(super) async fn check_translation(
    service: &EngineService,
    source: &Path,
    format: &str,
    files: &[String],
    units: &[TranslationUnit],
    result: &mut Value,
    cancellation: &CancellationToken,
) -> Result<(), EngineError> {
    let trial = units.iter().enumerate().find_map(|(index, unit)| {
        let text = gamel10n_translation::text::verification_text(unit, index, true);
        (text != unit.source_text).then_some((unit, text))
    });
    let Some((unit, text)) = trial else {
        result["translationStatus"] = json!("inconclusive");
        return Ok(());
    };
    let output = tempfile::tempdir()?;
    result["stage"] = json!("translation-build");
    result["translationStatus"] = json!("failed");
    let mut build = AdapterRequest::new("build", source);
    build.resource_files = Some(files.to_vec());
    build.output_directory = Some(output.path().into());
    build.translations = Some(BTreeMap::from([(unit.id.clone(), text.clone())]).into());
    let built = trial_response(service, &build, format, result, cancellation)
        .await?
        .build
        .ok_or_else(|| reject("内置解析器未返回小样中文回填结果。"))?;
    let selected: BTreeSet<_> = files.iter().map(|file| file.to_lowercase()).collect();
    let inventory = Inventory::scan(output.path())?;
    if !inventory.complete
        || inventory
            .files
            .iter()
            .map(|file| file.path.to_lowercase())
            .collect::<BTreeSet<_>>()
            != selected
        || built
            .files
            .iter()
            .map(|file| file.to_lowercase())
            .collect::<BTreeSet<_>>()
            != selected
        || built.files.len() != files.len()
        || built.launch_executable.is_some()
        || built.launch.is_some()
    {
        return Err(reject("小样中文回填的文件集合与所选样本不一致。"));
    }
    result["stage"] = json!("translation-readback");
    let mut extract = AdapterRequest::new("extract", output.path());
    extract.resource_files = Some(files.to_vec());
    let extracted = trial_response(service, &extract, format, result, cancellation)
        .await?
        .extraction
        .ok_or_else(|| reject("内置解析器未返回小样中文复提结果。"))?;
    let after: BTreeMap<_, _> = extracted
        .units
        .iter()
        .map(|unit| (unit.id.as_str(), unit))
        .collect();
    if after.len() != units.len() || after.len() != extracted.units.len() {
        result["issues"] = json!([crate::VerificationIssue::new(
            crate::VerificationIssueCode::TextCount,
            "translation-readback"
        )
        .difference(&units.len().to_string(), &extracted.units.len().to_string())]);
        return Err(reject("小样中文复提改变了文本数量或产生重复 ID。"));
    }
    for original in units {
        let expected = if original.id == unit.id {
            &text
        } else {
            &original.source_text
        };
        let mut issue = crate::VerificationIssue::new(
            crate::VerificationIssueCode::TextMismatch,
            "translation-readback",
        )
        .at(&original.resource_path, Some(&original.id))
        .unit_difference(
            original,
            after.get(original.id.as_str()).copied(),
            expected,
            false,
            true,
        );
        if !after.contains_key(original.id.as_str()) || !issue.field_differences.is_empty() {
            issue.code = if !after.contains_key(original.id.as_str()) {
                crate::VerificationIssueCode::MissingUnit
            } else if issue
                .field_differences
                .iter()
                .any(|difference| difference.field == "sourceText")
            {
                crate::VerificationIssueCode::TextMismatch
            } else if issue
                .field_differences
                .iter()
                .any(|difference| difference.field == "protectedTokens")
            {
                crate::VerificationIssueCode::ProtectedTokens
            } else {
                crate::VerificationIssueCode::IdentityMismatch
            };
            let fields = issue.field_names();
            result["issues"] = json!([issue]);
            return Err(reject(format!(
                "小样中文复提不一致：{}（变化字段：{fields}）。",
                original.id
            )));
        }
    }
    result["translationStatus"] = json!("passed");
    result["translatedUnitId"] = json!(unit.id);
    Ok(())
}

pub(super) async fn run(
    service: &EngineService,
    request: &AdaptationRequest<'_>,
    workspace: &Path,
    state: &mut Session,
    path: &Path,
    cancellation: &CancellationToken,
    progress: ProgressCallback<'_>,
) -> Result<Option<AdapterHandle>, EngineError> {
    run_trials(
        service,
        request,
        workspace,
        state,
        path,
        cancellation,
        progress,
    )
    .await?;
    run_template(
        service,
        request,
        workspace,
        state,
        path,
        cancellation,
        progress,
    )
    .await
}

pub(super) fn sample_files(workspace: &Path, args: &Value) -> Result<Vec<String>, EngineError> {
    if args.get("sampleFiles").is_none() {
        return Ok(Vec::new());
    }
    let files: Vec<_> = strings(args, "sampleFiles")?
        .into_iter()
        .map(|file| file.replace('\\', "/"))
        .collect();
    if files.len() > 12 {
        return Err(EngineError::Protocol("单次最多提交 12 个样本文件。".into()));
    }
    let mut known = BTreeSet::new();
    for file in &files {
        if !matches!(file.split('/').next(), Some("work" | "resources"))
            || !known.insert(file.to_lowercase())
        {
            return Err(EngineError::Protocol(
                "样本须为不重复的 work/ 或 resources/ 相对文件路径。".into(),
            ));
        }
        resolve_relative(workspace, file)?;
    }
    Ok(files)
}

pub(super) fn sample_extensions() -> Vec<String> {
    let mut extensions: BTreeSet<_> = crate::adaptation_context::builtin_catalog()
        .into_iter()
        .filter(|format| {
            crate::vntextpatch::FORMATS.contains(&format["format"].as_str().unwrap_or(""))
        })
        .flat_map(|format| format["extensions"].as_array().cloned().unwrap_or_default())
        .filter_map(|extension| extension.as_str().map(str::to_owned))
        .collect();
    extensions.extend([".scn".into(), ".psb".into()]);
    extensions.into_iter().collect()
}

// Supplementary checks must not discard a completed extraction or experiment.
pub(super) fn sample_result(outcome: Result<Value, EngineError>) -> Value {
    outcome.unwrap_or_else(|error| {
        json!({
            "status":if matches!(error, EngineError::Cancelled) {"cancelled"} else {"incomplete"},
            "error":limit(&error.to_string(),500),"runtimeStatus":"not-tested"
        })
    })
}

fn parser_reuse(workspace: &Path, format: &str, result: &mut Value) {
    if (result["parseStatus"] != "passed" && result["status"] != "passed")
        || result["unitCount"].as_u64().is_none_or(|count| count == 0)
    {
        return;
    }
    if !crate::vntextpatch::FORMATS.contains(&format) {
        result["nextAction"] = json!(
            "复用此组件中已通过的步骤，仅修复失败阶段；封包、过滤器、编码与完整适配器仍须单独验收。"
        );
        return;
    }
    if workspace.join("src/adapter.py").exists() {
        if let Some(result) = result.as_object_mut() {
            result.remove("reuseCall");
        }
        result["nextAction"] = json!(
            "保留当前适配器入口与固定依赖，通过 tools.gamel10n_host.parser 复用已通过的解析步骤；按各阶段结果补齐回填、编码等缺口，仍须完整 test_adapter。"
        );
    } else {
        result["reuseCall"] = json!({"tool":"copy_adapter","arguments":{"id":format!("engine.vntextpatch-{}",format.to_ascii_lowercase()),"version":"1.0.1"}});
    }
}

pub(super) fn remember_trial(
    state: &mut Session,
    directory: &str,
    files: &[String],
    result: &Value,
) {
    let mut trials = state
        .extra
        .get("builtinTrials")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    trials.retain(|trial| {
        !(trial["format"] == result["format"]
            && trial["directory"] == directory
            && trial["files"] == json!(files))
    });
    let mut observation = result.clone();
    observation["directory"] = json!(directory);
    observation["files"] = json!(files);
    observation["observedAt"] = json!(chrono::Utc::now().to_rfc3339());
    trials.push(observation);
    if trials.len() > 6 {
        trials.remove(0);
    }
    state.extra.insert("builtinTrials".into(), json!(trials));
}

async fn try_component_sample(
    service: &EngineService,
    workspace: &Path,
    state: &mut Session,
    format: &str,
    file: &str,
    checks: (bool, bool),
    token: &CancellationToken,
) -> Result<Value, EngineError> {
    let source =
        components::sample_source(format)?.ok_or_else(|| reject("该组件未提供小样检查入口。"))?;
    let input = resources::file(workspace, state, file)?;
    let _input_handles =
        crate::source::lock_inputs(&BTreeMap::from([(file.to_string(), input.clone())]), token)?;
    let size = input.metadata()?.len();
    if size > TRIAL_BYTES {
        return Err(reject("小样超过 64 MiB 预算。"));
    }
    let started = Instant::now();
    let outcome = async {
    let digest = checked_hash(&input, size, token)?;
    let reference = service.adaptation_reference(workspace, state)?;
    let library = service.reference_library(&reference)?;
    let fingerprint = hash_text(&json!({"revision":1,"format":format,"file":file,"hash":digest,"component":gamel10n_translation::files::hash_bytes(source),"reference":reference,"checks":checks}).to_string());
    if let Some(trial) = state
        .extra
        .get("builtinTrials")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|trial| trial["fingerprint"] == fingerprint)
    {
        let mut result = trial.clone();
        result["cached"] = json!(true);
        parser_reuse(workspace, format, &mut result);
        return Ok(result);
    }
    let mut result = components::check_sample(service, &library, source,
        &json!({"operation":"verify","input":input,"checkRoundtrip":checks.0,"checkTranslation":checks.1}), token).await?;
    if checked_hash(&input, size, token)? != digest {
        return Err(reject("小样检查期间输入发生变化，结果未保存。"));
    }
    let (directory, relative) = file
        .split_once('/')
        .ok_or_else(|| reject("小样路径无效。"))?;
    result["format"] = json!(format);
    result["status"] = json!(if result["success"] == true {
        if result["fieldCount"] == 0 {
            "no-text"
        } else {
            "passed"
        }
    } else {
        "failed"
    });
    result["unitCount"] = result["fieldCount"].clone();
    result["sourceHashes"] = json!({relative:digest});
    result["runtimeStatus"] = json!("not-tested");
    result["cached"] = json!(false);
    if result["success"] == true
        || matches!(
            result["diagnostic"]["errorType"].as_str(),
            Some(
                "ValueError"
                    | "IndexError"
                    | "KeyError"
                    | "UnicodeDecodeError"
                    | "UnicodeEncodeError"
                    | "struct.error"
            )
        )
    {
        result["fingerprint"] = json!(fingerprint);
    }
    parser_reuse(workspace, format, &mut result);
    remember_trial(state, directory, &[relative.into()], &result);
    Ok(result)
    }.await;
    state.sample_checks.record(
        started,
        1,
        size,
        outcome
            .as_ref()
            .is_ok_and(|result| result["cached"] == true),
    );
    outcome
}

pub(super) async fn component_trials(
    service: &EngineService,
    workspace: &Path,
    state: &mut Session,
    args: &Value,
    cancellation: &CancellationToken,
) -> Result<Value, EngineError> {
    let directory = text(args, "directory")?.replace('\\', "/");
    let files: Vec<_> = strings(args, "files")?
        .iter()
        .map(|file| format!("{directory}/{}", file.replace('\\', "/")))
        .collect();
    let files = sample_files(workspace, &json!({"sampleFiles":files}))?;
    if files.is_empty() {
        return Err(reject("请明确选择要检查的完整样本。"));
    }
    let total = files.iter().try_fold(0_u64, |sum, file| {
        Ok::<_, EngineError>(
            sum.saturating_add(resources::file(workspace, state, file)?.metadata()?.len()),
        )
    })?;
    if total > TRIAL_BYTES {
        return Err(reject("小样合计超过 64 MiB 预算。"));
    }
    let checks = (
        boolean(args, "checkRoundtrip", true)?,
        boolean(args, "checkTranslation", true)?,
    );
    let budget = Budget::new(cancellation, Duration::from_secs(60));
    let mut trials = Vec::new();
    for file in files {
        trials.push(
            try_component_sample(
                service,
                workspace,
                state,
                text(args, "format")?,
                &file,
                checks,
                &budget.token,
            )
            .await?,
        );
    }
    if trials.len() == 1 {
        return Ok(trials.remove(0));
    }
    let mut result = json!({"format":args["format"],"runtimeStatus":"not-tested","unitCount":trials.iter().filter_map(|trial|trial["unitCount"].as_u64()).sum::<u64>()});
    result["status"] = json!(if trials.iter().any(|trial| trial["status"] == "failed") {
        "failed"
    } else if result["unitCount"].as_u64().unwrap_or(0) > 0 {
        "passed"
    } else {
        "no-text"
    });
    for field in ["parseStatus", "roundtripStatus", "translationStatus"] {
        result[field] = trials
            .iter()
            .map(|trial| &trial[field])
            .find(|value| value.as_str() == Some("failed"))
            .or_else(|| {
                trials
                    .iter()
                    .map(|trial| &trial[field])
                    .find(|value| value.as_str() == Some("inconclusive"))
            })
            .unwrap_or(&trials[0][field])
            .clone();
    }
    result["trials"] = json!(trials);
    Ok(result)
}

async fn try_cached_sample(
    service: &EngineService,
    workspace: &Path,
    state: &mut Session,
    format: &str,
    directory: &str,
    files: &[String],
    token: &CancellationToken,
) -> Result<Value, EngineError> {
    let paths: BTreeMap<_, _> = files
        .iter()
        .map(|file| {
            Ok((
                file.clone(),
                resources::file(workspace, state, &format!("{directory}/{file}"))?,
            ))
        })
        .collect::<Result<_, EngineError>>()?;
    let lengths: BTreeMap<_, _> = files
        .iter()
        .map(|file| Ok((file.clone(), paths[file].metadata()?.len())))
        .collect::<Result<_, EngineError>>()?;
    if files.is_empty()
        || files.len() > 12
        || lengths
            .values()
            .try_fold(0_u64, |sum, length| sum.checked_add(*length))
            .is_none_or(|bytes| bytes > TRIAL_BYTES)
    {
        return Err(reject("试用样本超过 12 文件／64 MiB 预算。"));
    }
    let started = Instant::now();
    let before = state.sample_checks.clone();
    let hash_samples = || {
        paths
            .iter()
            .map(|(file, path)| Ok((file.clone(), checked_hash(path, lengths[file], token)?)))
            .collect::<Result<BTreeMap<_, _>, EngineError>>()
    };
    let outcome = async {
    let hashes = hash_samples()?;
    let parser = parser_fingerprint(service, token)?;
    let fingerprint = hash_text(&json!({"trialRevision":3,"format":format,"directory":directory,"parser":parser,"files":hashes}).to_string());
    if let Some(trial) = state
        .extra
        .get("builtinTrials")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|trial| trial["fingerprint"] == fingerprint)
    {
        let mut result = trial.clone();
        result["cached"] = json!(true);
        parser_reuse(workspace, format, &mut result);
        return Ok(result);
    }
    let result = service
        .try_builtin_sample(
            workspace,
            state,
            &json!({"format":format,"directory":directory,"files":files,"checkRoundtrip":true}),
            token,
        )
        .await?;
    if hash_samples()? != hashes
        || result["sourceHashes"] != json!(hashes)
        || parser_fingerprint(service, token)? != parser
    {
        return Err(reject("试用期间样本或解析工具发生变化，结果未缓存。"));
    }
    let mut result = result;
    if let Some(samples) = result["samples"].as_array_mut() {
        samples.truncate(2);
        for sample in samples {
            if let Some(text) = sample["text"].as_str() {
                sample["text"] = json!(limit(text, 120));
            }
        }
    }
    if let Some(trial) = state
        .extra
        .get_mut("builtinTrials")
        .and_then(Value::as_array_mut)
        .and_then(|trials| {
            trials.iter_mut().rev().find(|trial| {
                trial["format"] == format
                    && trial["directory"] == directory
                    && trial["files"] == json!(files)
            })
        })
    {
        let observed_at = trial["observedAt"].clone();
        *trial = result.clone();
        trial["directory"] = json!(directory);
        trial["files"] = json!(files);
        trial["observedAt"] = observed_at;
        trial["fingerprint"] = if matches!(result["status"].as_str(), Some("passed" | "no-text"))
            || deterministic(&result["diagnostic"])
        {
            json!(fingerprint)
        } else {
            Value::Null
        };
    }
    result["cached"] = json!(false);
    parser_reuse(workspace, format, &mut result);
    Ok(result)
    }.await;
    let nested = state.sample_checks.since(&before);
    if nested.checks == 0 {
        state.sample_checks.record(
            started,
            files.len(),
            lengths.values().sum(),
            outcome
                .as_ref()
                .is_ok_and(|result| result["cached"] == true),
        );
    } else {
        // A cache miss executes the native trial, whose samples are already counted.
        state.sample_checks.include_elapsed(started, &before);
    }
    outcome
}

pub(super) async fn inspect_samples(
    service: &EngineService,
    workspace: &Path,
    state: &mut Session,
    files: &[String],
    cancellation: &CancellationToken,
) -> Result<Value, EngineError> {
    let started = Instant::now();
    let before = state.sample_checks.clone();
    let budget = Budget::new(cancellation, Duration::from_secs(60));
    let token = &budget.token;
    let mut result = json!({"status":"incomplete","trials":[],"runtimeStatus":"not-tested"});
    let outcome = async {
        let bindings = AdaptationInputs {
            files: resources::inputs(workspace, state)?
                .files
                .into_iter()
                .map(|(relative, path)| (format!("resources/{relative}"), path))
                .collect(),
            ..Default::default()
        };
        let report =
            crate::adaptation_context::inspect_builtin_samples(workspace, files, &bindings, token)?;
        result["candidates"] = report["candidates"].clone();
        result["components"] = report["components"].clone();
        let catalog = crate::adaptation_context::builtin_catalog();
        let mut selected_bytes = 0_u64;
        for candidate in report["candidates"].as_array().into_iter().flatten() {
            let Some(definition) = catalog.iter().find(|definition| {
                definition["format"] == candidate["format"]
                    && crate::vntextpatch::FORMATS
                        .contains(&definition["format"].as_str().unwrap_or(""))
            }) else {
                continue;
            };
            if service.disabled_families.contains(text(definition, "id")?) {
                continue;
            }
            let Some(first) = candidate["sampleFiles"]
                .as_array()
                .and_then(|files| files.first())
                .and_then(Value::as_str)
            else {
                continue;
            };
            let Some((directory, _)) = first.split_once('/') else {
                continue;
            };
            if !matches!(directory, "work" | "resources") {
                continue;
            }
            let mut selected = BTreeSet::new();
            for file in candidate["sampleFiles"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .filter_map(|file| file.strip_prefix(&format!("{directory}/")))
            {
                if definition["extensions"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .any(|ext| {
                        ext.as_str().is_some_and(|ext| {
                            ext.trim_start_matches('.')
                                .eq_ignore_ascii_case(&extension(file))
                        })
                    })
                {
                    for path in sample_group(
                        definition,
                        workspace,
                        &format!("{directory}/{file}"),
                        &bindings,
                    )? {
                        if let Some(relative) = path.strip_prefix(&format!("{directory}/")) {
                            selected.insert(relative.to_owned());
                        }
                    }
                }
            }
            if selected.is_empty() {
                continue;
            }
            let lengths = selected
                .iter()
                .map(|file| {
                    Ok(bindings
                        .file(workspace, &format!("{directory}/{file}"))?
                        .metadata()?
                        .len())
                })
                .collect::<Result<Vec<_>, EngineError>>()?;
            let bytes = lengths
                .iter()
                .try_fold(selected_bytes, |sum, bytes| sum.checked_add(*bytes));
            if selected.len() > 12 || bytes.is_none_or(|bytes| bytes > TRIAL_BYTES) {
                continue;
            }
            selected_bytes = bytes.unwrap_or(0);
            let trial_budget = Budget::new(token, Duration::from_secs(20));
            let trial = try_cached_sample(
                service,
                workspace,
                state,
                text(definition, "format")?,
                directory,
                &selected.into_iter().collect::<Vec<_>>(),
                &trial_budget.token,
            )
            .await?;
            result["trials"]
                .as_array_mut()
                .ok_or_else(|| reject("试用结果列表无效。"))?
                .push(trial);
            if result["trials"]
                .as_array()
                .is_some_and(|trials| trials.len() >= 3)
            {
                break;
            }
        }
        for component in report["components"].as_array().into_iter().flatten() {
            let id = text(component, "id")?;
            if components::sample_source(id)?.is_none() {
                continue;
            }
            for file in component["sampleFiles"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .take(2)
            {
                if result["trials"]
                    .as_array()
                    .is_some_and(|trials| trials.len() >= 3)
                {
                    break;
                }
                let size = bindings.file(workspace, file)?.metadata()?.len();
                if selected_bytes.saturating_add(size) > TRIAL_BYTES {
                    continue;
                }
                selected_bytes += size;
                let trial =
                    try_component_sample(service, workspace, state, id, file, (true, true), token)
                        .await?;
                result["trials"]
                    .as_array_mut()
                    .ok_or_else(|| reject("试用结果列表无效。"))?
                    .push(trial);
            }
        }
        Ok::<_, EngineError>(())
    }
    .await;
    state.sample_checks.include_elapsed(started, &before);
    match outcome {
        Ok(()) => result["status"] = json!("completed"),
        Err(EngineError::Cancelled) if cancellation.is_cancelled() => {
            return Err(EngineError::Cancelled);
        }
        Err(error) => {
            if matches!(error, EngineError::ToolMissing(_)) {
                result["status"] = json!("unavailable");
            }
            result["error"] = json!(limit(&error.to_string(), 500));
            result["nextAction"] = json!(
                "检查所选样本、伴随文件和本地解析工具；超时或工具故障不代表脚本格式不兼容。已有实验输出仍可复用。"
            );
        }
    }
    Ok(result)
}

pub(super) async fn inspect_directory(
    service: &EngineService,
    workspace: &Path,
    state: &mut Session,
    directory: &str,
    cancellation: &CancellationToken,
) -> Result<Value, EngineError> {
    let started = Instant::now();
    let before = state.sample_checks.clone();
    let outcome = (|| {
        let root = resolve_relative(workspace, directory)?;
        let report = crate::adaptation_context::inspect_builtin_discovery(&root, cancellation)?;
        let files: Vec<_> = ["candidates", "components"]
            .into_iter()
            .flat_map(|key| report[key].as_array().into_iter().flatten())
            .flat_map(|candidate| candidate["sampleFiles"].as_array().into_iter().flatten())
            .filter_map(Value::as_str)
            .map(|file| format!("{directory}/{file}"))
            .take(12)
            .collect();
        sample_files(workspace, &json!({"sampleFiles":files}))
    })();
    let outcome = match outcome {
        Ok(files) => inspect_samples(service, workspace, state, &files, cancellation).await,
        Err(EngineError::Cancelled) => Err(EngineError::Cancelled),
        Err(error) => Ok(
            json!({"status":"incomplete","error":limit(&error.to_string(),500),"runtimeStatus":"not-tested"}),
        ),
    };
    state.sample_checks.include_elapsed(started, &before);
    outcome
}

pub(super) async fn inspect_workspace(
    service: &EngineService,
    workspace: &Path,
    state: &mut Session,
    cancellation: &CancellationToken,
) -> Result<(), EngineError> {
    let mut components = Vec::new();
    let mut issues = Vec::new();
    let mut trials = 0;
    let budget = Budget::new(cancellation, Duration::from_secs(60));
    for directory in ["work", "resources"] {
        if !workspace.join(directory).is_dir() {
            continue;
        }
        let result =
            match inspect_directory(service, workspace, state, directory, &budget.token).await {
                Ok(result) => result,
                Err(EngineError::Cancelled) if !cancellation.is_cancelled() => {
                    issues.push("已有样本检查达到 60 秒预算，未完成部分仍需确认。".into());
                    break;
                }
                Err(error) => return Err(error),
            };
        trials += result["trials"].as_array().map_or(0, Vec::len);
        components.extend(
            result["components"]
                .as_array()
                .into_iter()
                .flatten()
                .cloned(),
        );
        if let Some(error) = result["error"].as_str() {
            issues.push(format!("{directory}：{}", limit(error, 500)));
        }
    }
    if trials > 0 || !components.is_empty() || !issues.is_empty() {
        state.extra.insert("workspaceSamples".into(), json!({"trialCount":trials,"components":components,"issues":issues,"runtimeStatus":"not-tested"}));
    } else {
        state.extra.remove("workspaceSamples");
    }
    Ok(())
}

pub(super) async fn run_trials(
    service: &EngineService,
    request: &AdaptationRequest<'_>,
    workspace: &Path,
    state: &mut Session,
    path: &Path,
    cancellation: &CancellationToken,
    progress: ProgressCallback<'_>,
) -> Result<(), EngineError> {
    // Bound originals may differ from installed files. Explicit workspace samples
    // remain available for these tasks without replacing their production inputs.
    if !request.inputs.files.is_empty() || !request.inputs.excluded.is_empty() {
        return Ok(());
    }
    let mut candidates = candidates(state);
    if candidates.is_empty() {
        return Ok(());
    }
    candidates
        .sort_by_key(|candidate| usize::from(candidate["evidenceKind"] != "script-signature"));
    let budget = Budget::new(cancellation, Duration::from_secs(60));
    let token = &budget.token;
    let mut skipped = Vec::<String>::new();
    let mut reused = 0_u32;
    let mut attempted = 0;
    let mut selected_bytes = 0_u64;
    record(
        state,
        path,
        "automaticTrials",
        json!({"revision":1,"status":"running","reused":0,"skipped":[]}),
    )
    .await?;
    let outcome=async{
        for candidate in candidates{
            if attempted>=3{break;}cancelled(token)?;
            let catalog=crate::adaptation_context::builtin_catalog();let Some(definition)=catalog.iter().find(|definition|definition["format"]==candidate["format"])else{continue;};
            if service.disabled_families.contains(definition["id"].as_str().unwrap_or("")){continue;}
            let format=text(definition,"format")?;let mut lengths=BTreeMap::new();let mut scripts=0;
            for script in candidate["sampleFiles"].as_array().into_iter().flatten().take(4).filter_map(Value::as_str){
                if scripts>=2{break;}
                let ext=extension(script);let filename=Path::new(script).file_name().unwrap_or_default().to_string_lossy();
                if !definition["extensions"].as_array().into_iter().flatten().any(|extension|extension.as_str().is_some_and(|extension|extension.trim_start_matches('.').eq_ignore_ascii_case(&ext)))
                    ||definition["companions"].as_array().into_iter().flatten().any(|name|name.as_str().is_some_and(|name|name.eq_ignore_ascii_case(&filename))){continue;}
                let extra=(||->Result<BTreeMap<String,u64>,EngineError>{sample_group(definition,&request.game_directory,script,&request.inputs)?.into_iter().filter(|file|!lengths.keys().any(|known:&String|known.eq_ignore_ascii_case(file))).map(|file|{let length=resolve_relative(&request.game_directory,&file)?.metadata()?.len();Ok((file,length))}).collect()})();
                let extra=match extra{Ok(extra)=>extra,Err(error)=>{if skipped.len()<6{skipped.push(format!("{format}：{}",limit(&error.to_string(),240)));}continue;}};
                let bytes=extra.values().try_fold(0_u64,|sum,length|sum.checked_add(*length));
                if lengths.len()+extra.len()>12||bytes.is_none_or(|bytes|bytes>TRIAL_BYTES.saturating_sub(selected_bytes)){
                    if skipped.len()<6{skipped.push(format!("{format}：{} 及伴随文件超过自动试用的 64 MiB／12 文件预算。",limit(script,80)));}continue;}
                selected_bytes+=bytes.unwrap_or(0);lengths.extend(extra);scripts+=1;
            }
            if scripts==0{if skipped.len()<6{skipped.push(format!("{format}：没有可直接试用的裸脚本样本；封包、编译脚本和私有过滤器需要另行确认。"));}continue;}
            attempted+=1;EngineService::report(progress,"自动试用内置解析器",format!("正在用 {} 个原始资源检查 {format} 的提取与原文回填。",lengths.len()));
            let candidate_budget=Budget::new(token,Duration::from_secs(20));let token=&candidate_budget.token;
            let hashes=sample_hashes(&request.game_directory,&lengths,token)?;
            let conflict=lengths.iter().find(|(file,length)|{
                resolve_relative(&workspace.join("resources"),file).is_ok_and(|copy|copy.exists()&&!checked_hash(&copy,**length,token).is_ok_and(|hash|hash==hashes[*file]))
            });
            if let Some((file,_))=conflict {if skipped.len()<6{skipped.push(format!("{format}：已有工作副本与游戏文件不同，未替换 {file}；可直接提交工作区样本试用。"));}continue;}
            stage(&request.game_directory,workspace,state,&lengths,&hashes,token)?;
            let files:Vec<_>=lengths.keys().cloned().collect();
            let result=try_cached_sample(service,workspace,state,format,"resources",&files,token).await?;
            reused+=u32::from(result["cached"]==true);
            let after=sample_hashes(&request.game_directory,&lengths,token)?;
            if hashes!=after||result["sourceHashes"].as_object().into_iter().flatten().any(|(file,hash)|hashes.get(file).is_none_or(|expected|hash.as_str()!=Some(expected))){return Err(reject("自动试用期间原始资源或样本发生变化，结果未缓存，请重新确认资源。"));}
            save_session(path,state).await?;
        }Ok::<_,EngineError>(())
    }.await;
    match outcome {
        Ok(()) => record(
            state,
            path,
            "automaticTrials",
            json!({"revision":1,"status":"completed","reused":reused,"skipped":skipped}),
        ).await,
        Err(EngineError::Cancelled) if cancellation.is_cancelled() => Err(EngineError::Cancelled),
        Err(EngineError::Cancelled) => record(
            state,
            path,
            "automaticTrials",
            json!({"revision":1,"status":"timed-out","reused":reused,"skipped":skipped,"error":"自动试用达到时间预算，未完成的结果未缓存；可继续定向分析，不代表格式不受支持。"}),
        ).await,
        Err(error) => record(
            state,
            path,
            "automaticTrials",
            json!({"revision":1,"status":"incomplete","reused":reused,"skipped":skipped,"error":limit(&error.to_string(),500)}),
        ).await,
    }
}

fn scan(root: &Path, token: &CancellationToken) -> Result<BTreeMap<String, u64>, EngineError> {
    if !root.is_dir() {
        return Err(reject("游戏目录不存在。"));
    }
    let mut pending = vec![root.to_path_buf()];
    let mut paths = BTreeMap::new();
    let mut seen = BTreeSet::new();
    let mut entries = 0;
    while let Some(directory) = pending.pop() {
        cancelled(token)?;
        for entry in fs::read_dir(directory)? {
            cancelled(token)?;
            entries += 1;
            if entries > 20000 {
                return Err(reject("目录扫描达到 20,000 项上限，资源列表不完整。"));
            }
            let entry = entry?;
            let metadata = entry.path().symlink_metadata()?;
            if is_link(&metadata) {
                return Err(reject("资源目录包含重解析点，不能自动确认完整范围。"));
            }
            if metadata.is_dir() {
                pending.push(entry.path());
            } else if metadata.is_file() {
                let relative = entry
                    .path()
                    .strip_prefix(root)
                    .map_err(file_error)?
                    .to_string_lossy()
                    .replace('\\', "/");
                if !seen.insert(relative.to_lowercase()) {
                    return Err(reject("资源目录存在大小写冲突，资源列表不完整。"));
                }
                paths.insert(relative, metadata.len());
            }
        }
    }
    Ok(paths)
}

fn supported(
    paths: impl IntoIterator<Item = impl AsRef<str>>,
    template: &Value,
    allow_archives: bool,
    allow_compiled: bool,
) -> Result<(), EngineError> {
    let other: BTreeSet<_> = crate::adaptation_context::BUILTIN_FORMATS
        .iter()
        .flat_map(|format| format.extensions.iter().copied())
        .filter(|extension| !matches!(*extension, "" | "txt" | "bin" | "dat" | "map"))
        .chain([
            "rpyc", "rpym", "rpymc", "scn", "xp3", "rpa", "arc", "pack", "pak", "ypf", "int",
            "pfs", "pck",
        ])
        .collect();
    let unsupported: Vec<_> = paths
        .into_iter()
        .filter_map(|path| {
            let path = path.as_ref();
            let extension = extension(path);
            let selected = template["scriptExtensions"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|item| {
                    item.as_str().is_some_and(|item| {
                        item.trim_start_matches('.')
                            .eq_ignore_ascii_case(&extension)
                    })
                });
            ((other.contains(extension.as_str())
                && !selected
                && !(allow_compiled && extension == "rpyc")
                && !(allow_archives && extension == "rpa"))
                || path.to_lowercase().ends_with("_ren.py"))
            .then(|| limit(path, 200))
        })
        .take(8)
        .collect();
    if unsupported.is_empty() {
        Ok(())
    } else {
        Err(reject(format!(
            "发现模板未覆盖的封包或脚本，需要继续确认，未自动发布：{}",
            unsupported.join("、")
        )))
    }
}

pub(super) async fn run_template(
    service: &EngineService,
    request: &AdaptationRequest<'_>,
    workspace: &Path,
    state: &mut Session,
    path: &Path,
    cancellation: &CancellationToken,
    progress: ProgressCallback<'_>,
) -> Result<Option<AdapterHandle>, EngineError> {
    if !fresh(request, workspace, state)?
        || !state.messages.is_empty()
        || !request.user_instruction.trim().is_empty()
        || !state.user_instruction.trim().is_empty()
        || state.extra.contains_key("automaticTemplate")
        || !candidates(state)
            .iter()
            .any(|candidate| matches!(candidate["format"].as_str(), Some("Renpy" | "KirikiriKs")))
    {
        return Ok(None);
    }
    let budget = Budget::new(cancellation, Duration::from_secs(300));
    let token = &budget.token;
    let mut report = json!({"revision":3,"status":"running","templateId":null,"stage":"resources","diagnostics":[],"evidenceFiles":[],"resourceBytes":null,"selectedBytes":null,"archiveReadBytes":null,"looseScripts":null,"archivedScripts":null,"mediaOnlyArchives":null});
    let outcome=async{
        record(state,path,"automaticTemplate",report.clone()).await?;EngineService::report(progress,"检查自动组合条件","正在检查完整资源列表及模板未覆盖的脚本、封包和启动线索。");
        let paths=scan(&request.game_directory,token)?;let by_extension=|ext:&str|paths.keys().filter(|path|extension(path)==ext).cloned().collect::<Vec<_>>();
        let xp3=by_extension("xp3");if !xp3.is_empty(){return Err(reject(format!("XP3 缺少独立的无过滤器证据，不能根据零标记或可解包推断 filter=none；保留模型流程确认：{}",xp3.iter().take(8).cloned().collect::<Vec<_>>().join("、"))));}
        let archives=by_extension("rpa");let renpy=by_extension("rpy");let compiled=by_extension("rpyc");let kirikiri=by_extension("ks");
        if !kirikiri.is_empty()&&(!archives.is_empty()||!renpy.is_empty()||!compiled.is_empty()){return Err(reject("发现不同引擎的资源混用，需先确认加载范围。"));}
        let mixed=!renpy.is_empty()&&!compiled.is_empty()||!archives.is_empty()&&(!renpy.is_empty()||!compiled.is_empty());
        let id=if mixed{"renpy-mixed"}else if !archives.is_empty(){"rpa3-renpy"}else if !compiled.is_empty(){"loose-renpy-rpyc"}else if !renpy.is_empty(){"loose-renpy"}else{"loose-kirikiri-ks"};
        let mut template=crate::adaptation_assets::template_catalog().into_iter().find(|template|template["id"]==id).unwrap();report["templateId"]=json!(id);
        let format=text(&template,"format")?;let parser_id=format!("engine.vntextpatch-{}",format.to_ascii_lowercase());
        if archives.is_empty()&&service.disabled_families.contains(&format!("template.{id}"))||service.disabled_families.contains(&parser_id){return Err(reject(format!("模板或其内置解析器已禁用，未自动组合：{id}")));}
        let patterns:Vec<String>=serde_json::from_value(template["resourcePatterns"].clone())?;
        let lengths:BTreeMap<_,_>=paths.iter().filter(|(path,_)|patterns.iter().any(|pattern|glob_match(pattern,path))).map(|(path,length)|(path.clone(),*length)).collect();
        if lengths.is_empty()||lengths.len()>1024||archives.len()>16{return Err(reject("自动组合需要 1～1,024 个资源，且最多 16 个封包；当前资源数量超出范围。"));}
        supported(paths.keys(),&template,template["archiveComponent"].is_string(),false)?;
        if let Some(other)=candidates(state).into_iter().find(|candidate|candidate["format"]!=format&&matches!(candidate["evidenceKind"].as_str(),Some("script-signature"|"container-signature"|"format-signature"|"container-header"))){return Err(reject(format!("另有未被模板覆盖的格式证据：{}；{}",other["format"],other["evidence"])));}
        if let Some(launch)=state.extra.get("launchInspection")&& (launch["partial"]==true||["localeProfiles","launchScripts","errorLogs"].iter().any(|field|launch[*field].as_array().is_some_and(|items|!items.is_empty()))){return Err(reject("存在尚需解释的启动配置、启动脚本、错误日志或不完整启动检查；先交给模型核对。"));}
        if !coverage_list(state,"pending").is_empty()||!coverage_list(state,"runtimeChecks").is_empty(){return Err(reject("已有覆盖或运行待办需要继续处理，未自动完成任务。"));}
        let resource_bytes=lengths.values().try_fold(0_u64,|sum,length|sum.checked_add(*length)).ok_or_else(||reject("完整资源超过输入预算。"))?;
        let mut selected_bytes=lengths.iter().filter(|(path,_)|extension(path)!="rpa").map(|(_,length)|*length).sum::<u64>();let input_budget=if archives.is_empty(){TEMPLATE_BYTES}else{RPA_BYTES};
        report["resourceBytes"]=json!(resource_bytes);report["selectedBytes"]=json!(selected_bytes);report["looseScripts"]=json!(renpy.len()+compiled.len()+kirikiri.len());report["archivedScripts"]=json!(0);report["mediaOnlyArchives"]=json!(0);
        if selected_bytes>TEMPLATE_BYTES{return Err(reject("松散脚本合计超过自动组合的 256 MiB 脚本预算。"));}
        if resource_bytes>input_budget||lengths.values().any(|length|*length>input_budget){return Err(reject(format!("完整资源超过自动组合的 {} MiB 输入预算，未缩小为少量样本发布。",input_budget/(1024*1024))));}
        let hashes=sample_hashes(&request.game_directory,&lengths,token)?;report["stage"]=json!("stage");record(state,path,"automaticTemplate",report.clone()).await?;
        stage(&request.game_directory,workspace,state,&lengths,&hashes,token)?;
        if !archives.is_empty(){
            report["stage"]=json!("archive-inspection");report["archiveReadBytes"]=json!(0);record(state,path,"automaticTemplate",report.clone()).await?;
            let mut entries=0_u64;let mut scripts=0_u64;let mut sources=renpy.len();let mut compiled=compiled.len();let mut media=0_u64;let mut read=0_u64;
            let inspection=format!("work/automatic-template-{}",chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default());
            for(index,resource)in archives.iter().enumerate(){
                EngineService::report(progress,"检查自动组合封包",format!("正在检查 {resource} 的全部成员及支持条件。"));
                let output=format!("{inspection}/{index:02}");let evidence=format!("{output}/manifest.json");
                let inspected=service.run_component(workspace,state,&json!({"id":"archive.rpa3","operation":"inspect","archive":format!("resources/{resource}"),"output":output,"extensions":[".rpy",".rpyc"]}),token).await?;
                report["evidenceFiles"].as_array_mut().unwrap().push(json!(evidence));record(state,path,"automaticTemplate",report.clone()).await?;
                if inspected["success"]!=true{return Err(reject(format!("{resource}：{}",inspected["error"])));}
                let manifest_path=resolve_relative(workspace,&evidence)?;if manifest_path.metadata()?.len()>8*1024*1024{return Err(reject("封包成员清单超过 8 MiB。"));}
                let manifest:Value=serde_json::from_slice(&fs::read(manifest_path)?)?;let members=manifest["entries"].as_array().ok_or_else(||reject("封包清单未返回完整成员。"))?;
                entries+=members.len() as u64;selected_bytes=selected_bytes.checked_add(manifest["selectedBytes"].as_u64().ok_or_else(||reject("封包清单缺少脚本大小。"))?).ok_or_else(||reject("封包脚本超出预算。"))?;
                read+=manifest["readBytes"].as_u64().unwrap_or(0);if entries>10000||selected_bytes>TEMPLATE_BYTES{return Err(reject("全部封包合计超过自动组合的 10,000 成员／256 MiB 脚本预算。"));}
                let mut names=Vec::new();for member in members{let name=text(member,"name")?;let length=member["length"].as_u64().ok_or_else(||reject("封包清单成员长度无效。"))?;
                    match extension(name).as_str(){"rpyc"=>{if length>32*1024*1024{return Err(reject("单个 RPYC2 脚本超过 32 MiB 读取预算。"));}compiled+=1;},"rpy"=>{if length>64*1024*1024{return Err(reject("单个 .rpy 脚本超过 64 MiB 读取预算。"));}sources+=1;},_=>{}}names.push(name);}
                supported(names,&template,false,true)?;let count=manifest["selectedCount"].as_u64().ok_or_else(||reject("封包清单缺少脚本数量。"))?;scripts+=count;if count==0{media+=1;}
                report["selectedBytes"]=json!(selected_bytes);report["archiveReadBytes"]=json!(read);report["archivedScripts"]=json!(scripts);report["mediaOnlyArchives"]=json!(media);
            }
            let id=if sources>0&&compiled>0{"renpy-mixed"}else if scripts==0&&renpy.is_empty()&&compiled==0{return Err(reject("全部资源中都没有模板支持的 .rpy 或 RPYC2 编译脚本；未生成或发布适配器。"));}else if !mixed&&compiled>0{"rpa3-renpy-rpyc"}else{text(&template,"id")?};
            template=crate::adaptation_assets::template_catalog().into_iter().find(|template|template["id"]==id).unwrap();report["templateId"]=template["id"].clone();
            if service.disabled_families.contains(&format!("template.{}",text(&template,"id")?)){return Err(reject("模板已禁用，未自动组合。"));}
        }
        report["stage"]=json!("compose");record(state,path,"automaticTemplate",report.clone()).await?;EngineService::report(progress,"自动组合适配器",format!("资源符合 {} 的条件，正在连接现有组件。",template["id"]));
        let composed=service.compose_template(workspace,state,&json!({"id":template["id"]}),token).await?;
        if !state.text_coverage.is_object(){state.text_coverage=json!({});}let mut checks=coverage_list(state,"runtimeChecks");checks.push(RUNTIME_CHECK.into());state.text_coverage["runtimeChecks"]=json!(checks);
        report["stage"]=json!("verification");record(state,path,"automaticTemplate",report.clone()).await?;EngineService::report(progress,"验证自动组合适配器","正在检查原字节往返，先做少量中文试写，通过后再验收全量中文回填。");
        let tested=service.test_adapter(request,workspace,state,&composed["testArguments"],token,progress).await?;report["diagnostics"]=tested["diagnostics"].clone();
        if tested["passed"]!=true||tested["stage"]!="finish-ready"{return Ok(None);}
        report["stage"]=json!("scope-recheck");record(state,path,"automaticTemplate",report.clone()).await?;
        let current=scan(&request.game_directory,token)?;if paths.keys().ne(current.keys()){state.verified_package=None;state.verified_fingerprint=None;return Err(reject("自动组合期间游戏目录发生变化或未完整读取，需重新确认模板适用范围；未自动发布。"));}
        report["stage"]=json!("publish");record(state,path,"automaticTemplate",report.clone()).await?;
        let summary=format!("已自动组合 {}，全部匹配资源已通过提取、原字节往返、少量中文试写及全量中文回填验收。支持范围仅为此模板选中的脚本文本；独立文字覆盖{}，不代表整款游戏文字已完整覆盖。",template["family"]["name"].as_str().unwrap_or(""),if state.text_coverage["samples"].as_array().is_none_or(Vec::is_empty){"尚未评估"}else{"仅核对已保留样本"});
        let published=service.finish_adapter(request,workspace,state,&json!({"summary":summary}),token).await?;
        let directory=PathBuf::from(text(&published,"packageDirectory")?);Ok(Some(PackageManifest::load(&directory)?.handle(&directory)?))
    }.await;
    match outcome {
        Ok(adapter) => {
            report["status"] = json!(if adapter.is_some() {
                "activated"
            } else {
                "incomplete"
            });
            record(state, path, "automaticTemplate", report).await?;
            Ok(adapter)
        }
        Err(error) => {
            report["status"] = json!(if cancellation.is_cancelled() {
                "cancelled"
            } else if matches!(error, EngineError::Cancelled) {
                "timed-out"
            } else {
                "incomplete"
            });
            let diagnostics = report["diagnostics"]
                .as_array_mut()
                .ok_or_else(|| reject("自动组合诊断无效。"))?;
            diagnostics.push(json!(if matches!(error,EngineError::Cancelled)&&!cancellation.is_cancelled(){"自动组合达到 5 分钟预算；保留现有文件与证据，未完成的步骤交给模型继续，不代表格式不受支持。".into()}else{limit(&error.to_string(),2000)}));
            if diagnostics.len() > 12 {
                diagnostics.drain(..diagnostics.len() - 12);
            }
            record(state, path, "automaticTemplate", report).await?;
            if matches!(&error, EngineError::Isolation(_)) {
                Err(error)
            } else if cancellation.is_cancelled() {
                Err(EngineError::Cancelled)
            } else {
                Ok(None)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(
        root: &Path,
        format: &str,
        resource: &str,
    ) -> (EngineService, AdaptationRequest<'static>, PathBuf, Session) {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let workspace = root.join("adaptation/current");
        fs::create_dir_all(workspace.join("work")).unwrap();
        fs::create_dir_all(root.join("game")).unwrap();
        let request = AdaptationRequest {
            game_id: "automatic-fixture".into(),
            game_directory: root.join("game"),
            inputs: AdaptationInputs::default(),
            project_directory: root.into(),
            package_directory: root.join("extensions"),
            package_directories: vec![],
            user_instruction: String::new(),
            objective: "Develop".into(),
            improve_existing: false,
            require_model_analysis: false,
            limits: AdaptationLimits::default(),
            baseline_units: &[],
            reuse_failures: vec![],
            builtin_probes: vec![],
            discovery_inventory: None,
        };
        let mut state = Session {
            game_id: request.game_id.clone(),
            game_directory: request.game_directory.clone(),
            objective: "Develop".into(),
            ..Session::default()
        };
        state.extra.insert("builtinDiscovery".into(),json!({"revision":6,"candidates":[{"format":format,"evidenceKind":"script-signature","sampleFiles":[resource]}]}));
        state.extra.insert(
            "launchInspection".into(),
            json!({"partial":false,"localeProfiles":[],"launchScripts":[],"errorLogs":[]}),
        );
        (
            EngineService::new(&repo, crate::ToolPaths::development()),
            request,
            workspace,
            state,
        )
    }

    #[tokio::test]
    async fn existing_implementation_preserves_source_during_local_trials() {
        let root = tempfile::tempdir().unwrap();
        let (service, request, workspace, mut state) = fixture(root.path(), "Renpy", "script.rpy");
        fs::create_dir_all(workspace.join("src")).unwrap();
        fs::write(workspace.join("src/adapter.py"), "existing code").unwrap();
        assert!(
            run(
                &service,
                &request,
                &workspace,
                &mut state,
                &workspace.join("session.json"),
                &CancellationToken::new(),
                None
            )
            .await
            .unwrap()
            .is_none()
        );
        assert_eq!(state.extra["automaticTrials"]["status"], "completed");
        assert!(!state.extra.contains_key("automaticTemplate"));
        assert_eq!(
            fs::read_to_string(workspace.join("src/adapter.py")).unwrap(),
            "existing code"
        );
    }

    #[tokio::test]
    async fn saved_messages_prevent_template_creation_before_first_turn() {
        let root = tempfile::tempdir().unwrap();
        let (service, request, workspace, mut state) = fixture(root.path(), "Renpy", "script.rpy");
        state
            .messages
            .push(json!({"role":"user","content":"saved context"}));
        assert!(fresh(&request, &workspace, &state).unwrap());
        assert!(
            run_template(
                &service,
                &request,
                &workspace,
                &mut state,
                &workspace.join("session.json"),
                &CancellationToken::new(),
                None
            )
            .await
            .unwrap()
            .is_none()
        );
        assert!(!state.extra.contains_key("automaticTemplate"));
        assert!(!workspace.join("src").exists());
    }

    #[tokio::test]
    async fn required_model_analysis_prevents_local_template_publication() {
        let root = tempfile::tempdir().unwrap();
        let (service, mut request, workspace, mut state) =
            fixture(root.path(), "Renpy", "script.rpy");
        request.require_model_analysis = true;
        assert!(
            run(
                &service,
                &request,
                &workspace,
                &mut state,
                &workspace.join("session.json"),
                &CancellationToken::new(),
                None
            )
            .await
            .unwrap()
            .is_none()
        );
        assert!(!state.extra.contains_key("automaticTemplate"));
        assert_eq!(state.extra["automaticTrials"]["status"], "completed");
        assert!(!workspace.join("src").exists());
    }

    #[tokio::test]
    async fn template_checks_complete_scope_and_launch_before_writing_source() {
        for resource in ["archive.xp3", "other.ybn", "locale.config"] {
            let root = tempfile::tempdir().unwrap();
            let (service, request, workspace, mut state) =
                fixture(root.path(), "Renpy", "script.rpy");
            fs::write(
                request.game_directory.join("script.rpy"),
                "label start:\n    \"こんにちは\"\n",
            )
            .unwrap();
            fs::write(
                request.game_directory.join(resource),
                b"additional resource",
            )
            .unwrap();
            if resource == "locale.config" {
                state.extra.get_mut("launchInspection").unwrap()["localeProfiles"] =
                    json!([{"path":resource}]);
            }
            assert!(
                run_template(
                    &service,
                    &request,
                    &workspace,
                    &mut state,
                    &workspace.join("session.json"),
                    &CancellationToken::new(),
                    None
                )
                .await
                .unwrap()
                .is_none()
            );
            assert_eq!(state.extra["automaticTemplate"]["status"], "incomplete");
            assert!(!workspace.join("src").exists());
            assert!(state.verified_package.is_none());
        }
    }

    #[tokio::test]
    async fn deterministic_native_failure_keeps_diagnostics_and_reuses_fingerprint() {
        let root = tempfile::tempdir().unwrap();
        let (service, request, workspace, mut state) = fixture(root.path(), "Majiro", "bad.mjo");
        if !service.tools.vntextpatch().is_file() {
            return;
        }
        state.total_turns = 42;
        state.extra.insert(
            "reuseFailures".into(),
            json!([{"id":"engine.old","failedStage":"extraction"}]),
        );
        fs::write(
            request.game_directory.join("bad.mjo"),
            b"invalid format bytes",
        )
        .unwrap();
        let path = workspace.join("session.json");
        let token = CancellationToken::new();
        run_trials(
            &service, &request, &workspace, &mut state, &path, &token, None,
        )
        .await
        .unwrap();
        assert_eq!(state.extra["automaticTrials"]["status"], "completed");
        let first = &state.extra["builtinTrials"][0];
        assert_eq!(first["status"], "failed");
        assert!(deterministic(&first["diagnostic"]));
        assert!(first["fingerprint"].as_str().is_some());
        let fingerprint = first["fingerprint"].clone();
        assert_eq!(state.sample_checks.checks, 1);
        assert_eq!(state.sample_checks.sample_count, 1);
        assert_eq!(state.sample_checks.sample_bytes, 20);
        assert_eq!(state.sample_checks.cache_hits, 0);
        run_trials(
            &service, &request, &workspace, &mut state, &path, &token, None,
        )
        .await
        .unwrap();
        assert_eq!(state.extra["automaticTrials"]["reused"], 1);
        assert_eq!(state.sample_checks.checks, 2);
        assert_eq!(state.sample_checks.sample_bytes, 40);
        assert_eq!(state.sample_checks.cache_hits, 1);
        assert_eq!(state.extra["builtinTrials"].as_array().unwrap().len(), 1);
        assert_eq!(state.extra["builtinTrials"][0]["fingerprint"], fingerprint);
        assert_eq!(
            fs::read(request.game_directory.join("bad.mjo")).unwrap(),
            b"invalid format bytes"
        );
    }

    #[tokio::test]
    async fn resumed_trials_do_not_replace_existing_working_resources() {
        let root = tempfile::tempdir().unwrap();
        let (service, request, workspace, mut state) =
            fixture(root.path(), "KirikiriKs", "scene.ks");
        state.total_turns = 118;
        fs::write(request.game_directory.join("scene.ks"), b"installed patch").unwrap();
        fs::create_dir_all(workspace.join("resources")).unwrap();
        fs::write(workspace.join("resources/scene.ks"), b"original snapshot").unwrap();
        run_trials(
            &service,
            &request,
            &workspace,
            &mut state,
            &workspace.join("session.json"),
            &CancellationToken::new(),
            None,
        )
        .await
        .unwrap();
        assert_eq!(
            fs::read(workspace.join("resources/scene.ks")).unwrap(),
            b"original snapshot"
        );
        assert!(
            state.extra["automaticTrials"]["skipped"][0]
                .as_str()
                .unwrap()
                .contains("未替换")
        );
        assert!(!state.extra.contains_key("builtinTrials"));
    }

    #[tokio::test]
    async fn psb_samples_keep_component_failures_without_accepting_adapters() {
        let root = tempfile::tempdir().unwrap();
        let (service, _, workspace, mut state) = fixture(root.path(), "KirikiriKs", "archive.xp3");
        fs::write(
            workspace.join("work/scene.ks"),
            b"PSB\0\x03\0\0\0partial header",
        )
        .unwrap();
        fs::write(
            workspace.join("work/unrelated.rpy"),
            "label start:\n    \"無関係\"\n",
        )
        .unwrap();
        let result = inspect_samples(
            &service,
            &workspace,
            &mut state,
            &["work/scene.ks".into()],
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(result["components"][0]["id"], "kirikiri-scn-text");
        assert_eq!(
            result["components"][0]["sampleFiles"],
            json!(["work/scene.ks"])
        );
        assert_eq!(result["candidates"], json!([]));
        assert_eq!(result["status"], "completed", "{result}");
        assert_eq!(result["trials"][0]["parseStatus"], "failed", "{result}");
        assert_eq!(result["trials"][0]["roundtripStatus"], "not-run");
        assert_eq!(result["trials"][0]["translationStatus"], "not-run");
        let repeated = inspect_samples(
            &service,
            &workspace,
            &mut state,
            &["work/scene.ks".into()],
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(repeated["trials"][0]["cached"], true, "{repeated}");
        assert_eq!(repeated["trials"][0]["parseStatus"], "failed");
        assert!(state.verified_package.is_none());
        assert!(!workspace.join("src").exists());
    }

    #[test]
    fn sample_paths_are_optional_bounded_and_confined_to_working_copies() {
        let root = tempfile::tempdir().unwrap();
        assert!(sample_files(root.path(), &json!({})).unwrap().is_empty());
        for files in [
            json!(["src/adapter.py"]),
            json!(["work/../../outside.ks"]),
            json!(["work/a.ks", "work/A.ks"]),
            json!(vec!["work/a.ks"; 13]),
        ] {
            assert!(sample_files(root.path(), &json!({"sampleFiles":files})).is_err());
        }
        assert_eq!(
            sample_files(
                root.path(),
                &json!({"sampleFiles":["work/unpacked/scene.ks"]})
            )
            .unwrap(),
            ["work/unpacked/scene.ks"]
        );
    }

    #[tokio::test]
    async fn workspace_trials_reuse_content_hashes_and_preserve_existing_adapter() {
        let root = tempfile::tempdir().unwrap();
        let (service, _, workspace, mut state) = fixture(root.path(), "KirikiriKs", "archive.xp3");
        if !service.tools.vntextpatch().is_file() {
            return;
        }
        fs::write(
            workspace.join("work/scene.ks"),
            "\u{feff}[cm]\nこんにちは[p]\n",
        )
        .unwrap();
        state.total_turns = 118;
        state.resource_patterns = vec!["**/*.xp3".into()];
        let verified = json!({"id":"engine.existing","version":"1.0.0"});
        state.verified_package = Some(verified.clone());
        let files = ["work/scene.ks".into()];
        let token = CancellationToken::new();
        let requests_before = state.api_requests();
        let first = inspect_samples(&service, &workspace, &mut state, &files, &token)
            .await
            .unwrap();
        assert_eq!(first["trials"][0]["status"], "passed", "{first}");
        assert_eq!(first["trials"][0]["roundtripStatus"], "passed");
        assert_eq!(first["trials"][0]["cached"], false);
        assert_eq!(first["trials"][0]["reuseCall"]["tool"], "copy_adapter");
        fs::create_dir_all(workspace.join("src")).unwrap();
        fs::write(workspace.join("src/adapter.py"), "existing adapter").unwrap();
        let second = inspect_samples(&service, &workspace, &mut state, &files, &token)
            .await
            .unwrap();
        assert_eq!(second["trials"][0]["cached"], true);
        assert!(second["trials"][0]["reuseCall"].is_null());
        assert!(
            second["trials"][0]["nextAction"]
                .as_str()
                .unwrap()
                .contains("保留当前适配器入口")
        );
        fs::write(
            workspace.join("work/scene.ks"),
            "\u{feff}[cm]\nこんにちは[p]\n[cm]\n次の台詞[p]\n",
        )
        .unwrap();
        let changed = inspect_samples(&service, &workspace, &mut state, &files, &token)
            .await
            .unwrap();
        assert_eq!(changed["trials"][0]["cached"], false);
        assert!(
            changed["trials"][0]["unitCount"].as_u64().unwrap()
                > first["trials"][0]["unitCount"].as_u64().unwrap()
        );
        assert_eq!(
            fs::read_to_string(workspace.join("src/adapter.py")).unwrap(),
            "existing adapter"
        );
        assert_eq!(state.resource_patterns, ["**/*.xp3"]);
        assert_eq!(state.verified_package, Some(verified));
        assert_eq!(state.api_requests(), requests_before);
    }

    #[tokio::test]
    #[cfg(windows)]
    async fn oversized_backup_samples_are_rejected_before_opening_or_copying() {
        use std::os::windows::fs::OpenOptionsExt;
        let root = tempfile::tempdir().unwrap();
        let (service, request, workspace, mut state) =
            fixture(root.path(), "KirikiriKs", "scene.ks");
        let backup = request.game_directory.join("scene.ks.bak");
        let exclusive = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .share_mode(0)
            .open(&backup)
            .unwrap();
        exclusive.set_len(TRIAL_BYTES + 1).unwrap();
        state
            .extra
            .insert("resourceRoot".into(), json!(request.game_directory));
        state
            .extra
            .insert("resourcePaths".into(), json!({"scene.ks":backup}));
        state
            .extra
            .insert("resourceHashes".into(), json!({"scene.ks":"unused"}));
        let args = json!({"format":"KirikiriKs","directory":"resources","files":["scene.ks"]});
        let result = service
            .try_builtin_sample(&workspace, &mut state, &args, &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(result["status"], "limit-exceeded", "{result}");
        let error = try_cached_sample(
            &service,
            &workspace,
            &mut state,
            "KirikiriKs",
            "resources",
            &["scene.ks".into()],
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("64 MiB"), "{error}");
        assert!(!workspace.join("resources/scene.ks").exists());
    }

    #[tokio::test]
    async fn analysis_reads_original_backup_paths_without_materializing_resources() {
        let root = tempfile::tempdir().unwrap();
        let (service, mut request, workspace, mut state) =
            fixture(root.path(), "KirikiriKs", "scene.bin");
        if !service.tools.python().is_file() {
            return;
        }
        let backup = request.game_directory.join("scene.bin.bak");
        fs::write(&backup, b"original").unwrap();
        fs::write(request.game_directory.join("scene.bin"), b"installed patch").unwrap();
        request
            .inputs
            .files
            .insert("scene.bin".into(), backup.clone());
        request
            .inputs
            .hashes
            .insert("scene.bin".into(), hash_file(&backup).unwrap());
        let token = CancellationToken::new();
        resources::stage(
            &request,
            &workspace,
            &mut state,
            &["scene.bin".into()],
            false,
            &token,
            None,
        )
        .unwrap();
        let result = service
            .adaptation_tool(
                &request,
                &workspace,
                &mut state,
                "run_engine_python",
                &json!({
            "script":"work/inspect.py", "content":r#"from pathlib import Path
assert resource_paths['scene.bin'].name == 'scene.bin.bak'
assert resource_paths['scene.bin'].read_bytes() == b'original'
for path in [resource_paths['scene.bin'], resource_directory / 'new.bin']:
    try:
        path.write_bytes(b'changed')
    except PermissionError:
        pass
    else:
        raise AssertionError('input write succeeded')
Path('work/result.json').write_text('{}', encoding='utf8')
"#}),
                &token,
                None,
            )
            .await
            .unwrap();
        assert_eq!(result["success"], true, "{result}");
        assert!(
            fs::read_dir(workspace.join("resources"))
                .unwrap()
                .next()
                .is_none()
        );
        assert!(!request.game_directory.join("new.bin").exists());
        assert_eq!(fs::read(&backup).unwrap(), b"original");
        assert_eq!(
            fs::read(request.game_directory.join("scene.bin")).unwrap(),
            b"installed patch"
        );
        fs::write(&backup, b"unlocked after analysis").unwrap();
    }

    #[tokio::test]
    async fn python_output_samples_are_tried_in_the_same_tool_call() {
        let root = tempfile::tempdir().unwrap();
        let (service, request, workspace, mut state) =
            fixture(root.path(), "KirikiriKs", "archive.xp3");
        if !service.tools.vntextpatch().is_file() || !service.tools.python().is_file() {
            return;
        }
        let args = json!({"script":"work/decode.py","content":"from pathlib import Path\nPath('work/scene.ks').write_text('[cm]\\nこんにちは[p]\\n', encoding='utf-8-sig')\n"});
        let result = service
            .adaptation_tool(
                &request,
                &workspace,
                &mut state,
                "run_engine_python",
                &args,
                &CancellationToken::new(),
                None,
            )
            .await
            .unwrap();
        assert_eq!(
            result["builtinSamples"]["trials"][0]["status"], "passed",
            "{result}"
        );
        assert_eq!(state.api_requests(), 0);
        assert!(!workspace.join("src").exists());
    }

    #[tokio::test]
    async fn memory_samples_receive_native_stage_checks_without_model_requests() {
        let root = tempfile::tempdir().unwrap();
        let (service, request, workspace, mut state) =
            fixture(root.path(), "KirikiriKs", "game.xp3");
        if !service.tools.vntextpatch().is_file() || !service.tools.python().is_file() {
            return;
        }
        let result = service.adaptation_tool(&request, &workspace, &mut state, "run_engine_python",
            &json!({"script":"work/decode.py","workingDirectory":"work","content":"save_sample('work/scene.ks', '[cm]\\nこんにちは[p]\\n[cm]\\n保持原文[p]\\n'.encode('utf-8-sig'))"}),
            &CancellationToken::new(), None).await.unwrap();
        let trial = &result["builtinSamples"]["trials"][0];
        assert_eq!(result["success"], true, "{result}");
        assert_eq!(trial["parseStatus"], "passed", "{result}");
        assert_eq!(trial["roundtripStatus"], "passed");
        assert_eq!(trial["translationStatus"], "passed");
        assert_eq!(trial["runtimeStatus"], "not-tested");
        assert_eq!(state.api_requests(), 0);
        assert!(state.verified_package.is_none());
        assert!(!workspace.join("src").exists());
    }

    #[tokio::test]
    async fn component_samples_preserve_checks_across_recovery_and_invalidate_changed_bytes() {
        let root = tempfile::tempdir().unwrap();
        let (service, request, workspace, mut state) =
            fixture(root.path(), "KirikiriKs", "game.xp3");
        if !service.tools.python().is_file() || service.current_reference().is_err() {
            return;
        }
        fs::write(
            workspace.join("work/scn_fixture.py"),
            include_str!("../tests/fixtures/kirikiri_scn_fixture.py"),
        )
        .unwrap();
        let script = "import runpy\npsb=runpy.run_path('work/scn_fixture.py')['psb']\ntree={'scenes':[{'title':'Opening','texts':[['actor',[[None,'Original',8,'Original','Original']],None,None,None]]}]}\nsave_sample('work/scene.scn', psb(tree))\n";
        let token = CancellationToken::new();
        let result = service
            .adaptation_tool(
                &request,
                &workspace,
                &mut state,
                "run_engine_python",
                &json!({"script":"work/decode.py","content":script}),
                &token,
                None,
            )
            .await
            .unwrap();
        let trial = &result["builtinSamples"]["trials"][0];
        assert_eq!(trial["format"], "kirikiri-scn-text", "{result}");
        for field in ["parseStatus", "roundtripStatus", "translationStatus"] {
            assert_eq!(trial[field], "passed", "{field}: {result}");
        }
        let path = workspace.join("session.json");
        save_session(&path, &mut state).await.unwrap();
        let mut restored: Session = read_json(&path).unwrap();
        let result = inspect_samples(
            &service,
            &workspace,
            &mut restored,
            &["work/scene.scn".into()],
            &token,
        )
        .await
        .unwrap();
        assert_eq!(result["trials"][0]["cached"], true, "{result}");
        let result = service
            .adaptation_tool(
                &request,
                &workspace,
                &mut restored,
                "run_engine_python",
                &json!({"script":"work/decode.py","content":script.replace("Opening", "Changed")}),
                &token,
                None,
            )
            .await
            .unwrap();
        assert_eq!(
            result["builtinSamples"]["trials"][0]["cached"], false,
            "{result}"
        );
        assert!(restored.verified_package.is_none());
        assert!(restored.workflow.verification_args.is_none());
        assert_eq!(restored.api_requests(), 0);
        assert!(!workspace.join("src").exists());
    }

    #[tokio::test]
    async fn translated_sample_readback_rejects_changes_to_untouched_metadata() {
        let root = tempfile::tempdir().unwrap();
        let (service, _, workspace, _) = fixture(root.path(), "KirikiriKs", "game.xp3");
        if !service.tools.vntextpatch().is_file() {
            return;
        }
        let source = workspace.join("work");
        fs::write(
            source.join("scene.ks"),
            "\u{feff}[cm]\nこんにちは[p]\n[cm]\n保持原文[p]\n",
        )
        .unwrap();
        let token = CancellationToken::new();
        let mut response = json!({});
        let mut request = AdapterRequest::new("extract", &source);
        request.resource_files = Some(vec!["scene.ks".into()]);
        let mut units = trial_response(&service, &request, "KirikiriKs", &mut response, &token)
            .await
            .unwrap()
            .extraction
            .unwrap()
            .units;
        assert!(units.len() >= 2);
        units[1].speaker = Some("unexpected speaker".into());
        let result = check_translation(
            &service,
            &source,
            "KirikiriKs",
            &["scene.ks".into()],
            &units,
            &mut response,
            &token,
        )
        .await;
        assert!(result.is_err(), "{response}");
        assert_eq!(response["stage"], "translation-readback");
        assert_eq!(response["translationStatus"], "failed");
        assert_eq!(
            response["issues"][0]["fieldDifferences"][0]["field"],
            "speaker"
        );
    }

    #[tokio::test]
    async fn eagls_component_reuses_parse_and_roundtrip_when_encoding_is_not_ready() {
        let root = tempfile::tempdir().unwrap();
        let (service, request, workspace, mut state) = fixture(root.path(), "Eagls", "game.pak");
        if !service.tools.python().is_file() || service.current_reference().is_err() {
            return;
        }
        let script = "from python.engines.eagls import crypt_script\nheader=b'START'.ljust(32,b'\\0')+bytes(3600-32)\nplain=header+'$START\\r\\n#太郎&1\"本文\"\\r\\n'.encode('cp932')+b'\\x83\\x83'\nsave_sample('work/sample.dat',crypt_script(plain,text_offset=3600,key=b'EAGLS_SYSTEM',version=2))\n";
        let token = CancellationToken::new();
        let result = service
            .adaptation_tool(
                &request,
                &workspace,
                &mut state,
                "run_engine_python",
                &json!({"script":"work/sample.py","content":script}),
                &token,
                None,
            )
            .await
            .unwrap();
        assert_eq!(result["success"], true, "{result}");
        let args = json!({"format":"eagls-script","directory":"work","files":["sample.dat"]});
        let result = service
            .try_builtin_sample(&workspace, &mut state, &args, &token)
            .await
            .unwrap();
        assert_eq!(result["parseStatus"], "passed", "{result}");
        assert_eq!(result["roundtripStatus"], "passed");
        assert_eq!(result["translationStatus"], "failed");
        assert_eq!(result["diagnostic"]["errorType"], "UnicodeEncodeError");
        assert_eq!(result["profile"]["version"], 2);
        assert_eq!(result["profile"]["text_offset"], 3600);
        let before = state.sample_checks.clone();
        let repeated = service
            .try_builtin_sample(&workspace, &mut state, &args, &token)
            .await
            .unwrap();
        assert_eq!(repeated["cached"], true);
        let cached_metrics = state.sample_checks.since(&before);
        assert_eq!(cached_metrics.checks, 1);
        assert_eq!(cached_metrics.sample_count, 1);
        assert_eq!(
            cached_metrics.sample_bytes,
            fs::metadata(workspace.join("work/sample.dat"))
                .unwrap()
                .len()
        );
        assert_eq!(cached_metrics.cache_hits, 1);
        fs::write(workspace.join("work/sample.dat"), b"changed sample").unwrap();
        let changed = service
            .try_builtin_sample(&workspace, &mut state, &args, &token)
            .await
            .unwrap();
        assert_eq!(changed["cached"], false);
        assert_ne!(changed["sourceHashes"], repeated["sourceHashes"]);
        assert_eq!(state.api_requests(), 0);
        assert!(state.verified_package.is_none());
        assert!(!workspace.join("src").exists());
    }

    #[tokio::test]
    async fn continuation_discovers_existing_samples_without_a_declared_file_list() {
        let root = tempfile::tempdir().unwrap();
        let (service, _, workspace, mut state) = fixture(root.path(), "Renpy", "game.rpa");
        if !service.tools.vntextpatch().is_file() {
            return;
        }
        fs::create_dir_all(workspace.join("work/decoded")).unwrap();
        fs::write(workspace.join("work/decoded/ReAdMe.TxT"), b"YSTBpayload").unwrap();
        for (file, text) in [
            ("a.rpy", "label a:\n    return\n"),
            ("b.rpy", "label b:\n    return\n"),
            ("c.rpy", "label c:\n    \"こんにちは\"\n"),
        ] {
            fs::write(workspace.join("work/decoded").join(file), text).unwrap();
        }
        fs::write(
            workspace.join("work/decoded/scene.scn"),
            b"PSB\0\x03\0\0\0partial header",
        )
        .unwrap();
        state.total_turns = 118;
        let requests = state.api_requests();
        inspect_workspace(&service, &workspace, &mut state, &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(state.extra["workspaceSamples"]["trialCount"], 2);
        assert_eq!(state.sample_checks.checks, 2);
        let expected_samples = state.extra["builtinTrials"]
            .as_array()
            .unwrap()
            .iter()
            .map(|trial| trial["files"].as_array().unwrap().len() as u64)
            .sum::<u64>();
        assert_eq!(state.sample_checks.sample_count, expected_samples);
        let before = state.sample_checks.clone();
        inspect_workspace(&service, &workspace, &mut state, &CancellationToken::new())
            .await
            .unwrap();
        let cached_metrics = state.sample_checks.since(&before);
        assert_eq!(cached_metrics.checks, 2);
        assert_eq!(cached_metrics.cache_hits, 2);
        assert_eq!(cached_metrics.sample_count, before.sample_count);
        assert_eq!(cached_metrics.sample_bytes, before.sample_bytes);
        assert_eq!(state.extra["builtinTrials"][0]["status"], "passed");
        assert_eq!(state.extra["builtinTrials"][0]["unitCount"], 1);
        assert_eq!(state.api_requests(), requests);
        assert!(!workspace.join("src").exists());
        assert!(state.verified_package.is_none());
        let context = decision::build(&mut state, &workspace, &BTreeSet::new()).unwrap();
        let visible = context
            .replay
            .messages
            .iter()
            .filter_map(|message| message["content"].as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(visible.contains("workspaceSamples"));
        assert!(visible.contains("kirikiri-scn-text"));
        assert!(visible.contains("builtinTrials"));
    }

    #[tokio::test]
    async fn python_automatic_samples_skip_documents_but_explicit_samples_remain_available() {
        for registered in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let (service, request, workspace, mut state) =
                fixture(root.path(), "Yuris", "game.ypf");
            if !service.tools.python().is_file() || service.current_reference().is_err() {
                return;
            }
            let script = if registered {
                "save_sample('work/ReAdMe.TxT', b'YSTBpayload')"
            } else {
                "from pathlib import Path\nPath('work/ReAdMe.TxT').write_bytes(b'YSTBpayload')"
            };
            let token = CancellationToken::new();
            let result = service
                .adaptation_tool(
                    &request,
                    &workspace,
                    &mut state,
                    "run_engine_python",
                    &json!({"script":"work/docs.py","content":script}),
                    &token,
                    None,
                )
                .await
                .unwrap();
            assert_eq!(result["success"], true, "{result}");
            assert!(result.get("builtinSamples").is_none(), "{result}");
            assert_eq!(
                fs::read(workspace.join("work/ReAdMe.TxT")).unwrap(),
                b"YSTBpayload"
            );
            let result = service.adaptation_tool(&request,&workspace,&mut state,"run_engine_python",
                &json!({"script":"work/inspect.py","content":"print('sample selected')","sampleFiles":["work/ReAdMe.TxT"]}),&token,None).await.unwrap();
            assert_eq!(
                result["builtinSamples"]["candidates"][0]["sampleFiles"],
                json!(["work/ReAdMe.TxT"]),
                "{result}"
            );
        }
    }

    #[tokio::test]
    async fn failed_experiment_retains_automatic_sample_checks_without_becoming_successful() {
        let root = tempfile::tempdir().unwrap();
        let (service, request, workspace, mut state) =
            fixture(root.path(), "KirikiriKs", "game.xp3");
        if !service.tools.vntextpatch().is_file() || !service.tools.python().is_file() {
            return;
        }
        let result = service.adaptation_tool(&request, &workspace, &mut state, "run_engine_python",
            &json!({"script":"work/decode.py","content":"from pathlib import Path\nPath('work/scene.ks').write_text('[cm]\\nこんにちは[p]\\n', encoding='utf-8-sig')\nraise ValueError('later probe failed')\n"}),
            &CancellationToken::new(), None).await.unwrap();
        assert_eq!(result["success"], false, "{result}");
        assert_eq!(result["executionSucceeded"], false);
        assert_eq!(result["builtinSamples"]["trials"][0]["status"], "passed");
        assert_eq!(state.sample_checks.checks, 1);
        assert_eq!(state.sample_checks.sample_count, 1);
        assert_eq!(
            state.sample_checks.sample_bytes,
            fs::metadata(workspace.join("work/scene.ks")).unwrap().len()
        );
        assert!(!result.as_object().unwrap().contains_key("writtenFiles"));
        assert!(state.verified_package.is_none());
    }

    #[tokio::test]
    #[ignore = "Requires GAMEL10N_PARSER_SAMPLE_DIRECTORY with existing script samples and native runtime tools."]
    async fn stored_scripts_are_discovered_and_parsed_with_pinned_native_tools() {
        let samples = PathBuf::from(
            std::env::var_os("GAMEL10N_PARSER_SAMPLE_DIRECTORY").expect("sample directory"),
        );
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let root = tempfile::Builder::new()
            .prefix("parser-samples-")
            .tempdir_in(repo.join("target"))
            .unwrap();
        let (service, _, workspace, mut state) = fixture(root.path(), "KirikiriKs", "game.xp3");
        assert!(service.tools.vntextpatch().is_file());
        let inventory = Inventory::scan(&samples).unwrap();
        let mut hashes = BTreeMap::new();
        let mut bytes = 0;
        for file in inventory
            .files
            .iter()
            .filter(|file| extension(&file.path) == "ks")
            .take(12)
        {
            bytes += file.length;
            assert!(bytes <= 8 * 1024 * 1024);
            let source = resolve_relative(&samples, &file.path).unwrap();
            hashes.insert(file.path.clone(), hash_file(&source).unwrap());
            let destination =
                resolve_relative(&workspace.join("work/samples"), &file.path).unwrap();
            fs::create_dir_all(destination.parent().unwrap()).unwrap();
            fs::copy(source, destination).unwrap();
        }
        assert!(!hashes.is_empty());
        state.total_turns = 118;
        let requests = state.api_requests();
        inspect_workspace(&service, &workspace, &mut state, &CancellationToken::new())
            .await
            .unwrap();
        let trials = state.extra["builtinTrials"]
            .as_array()
            .expect("local parser trials");
        assert!(
            trials.iter().any(|trial| trial["status"] == "passed"
                && trial["unitCount"].as_u64().is_some_and(|count| count > 0)),
            "{trials:?}"
        );
        for (file, hash) in &hashes {
            assert_eq!(
                hash_file(&resolve_relative(&samples, file).unwrap()).unwrap(),
                *hash
            );
        }
        assert_eq!(state.api_requests(), requests);
        assert!(state.verified_package.is_none());
        println!(
            "{} source files preserved; local trial units: {:?}",
            hashes.len(),
            trials
                .iter()
                .map(|trial| &trial["unitCount"])
                .collect::<Vec<_>>()
        );
    }

    #[tokio::test]
    async fn unavailable_parser_assets_do_not_reject_the_sample_format() {
        let root = tempfile::tempdir().unwrap();
        let (mut service, _, workspace, mut state) =
            fixture(root.path(), "KirikiriKs", "archive.xp3");
        service.tools.root = root.path().join("missing-runtime");
        fs::write(
            workspace.join("work/scene.ks"),
            "\u{feff}[cm]\nこんにちは[p]\n",
        )
        .unwrap();
        let files = ["work/scene.ks".into()];
        let result = inspect_samples(
            &service,
            &workspace,
            &mut state,
            &files,
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(result["status"], "unavailable", "{result}");
        assert!(!state.extra.contains_key("builtinTrials"));
        let stopped = CancellationToken::new();
        stopped.cancel();
        assert!(matches!(
            inspect_samples(&service, &workspace, &mut state, &files, &stopped).await,
            Err(EngineError::Cancelled)
        ));
    }

    #[tokio::test]
    async fn complete_loose_template_verifies_and_publishes_without_model() {
        let root = tempfile::tempdir().unwrap();
        let (service, request, workspace, mut state) = fixture(root.path(), "Renpy", "script.rpy");
        if !service.tools.vntextpatch().is_file() || !service.tools.python().is_file() {
            return;
        }
        fs::write(
            request.game_directory.join("script.rpy"),
            "label start:\n    \"こんにちは世界。\"\n",
        )
        .unwrap();
        let original = fs::read(request.game_directory.join("script.rpy")).unwrap();
        let adapter = run(
            &service,
            &request,
            &workspace,
            &mut state,
            &workspace.join("session.json"),
            &CancellationToken::new(),
            None,
        )
        .await
        .unwrap();
        assert!(adapter.is_some(), "{}", state.extra["automaticTemplate"]);
        assert_eq!(state.extra["automaticTemplate"]["status"], "activated");
        assert_eq!(state.usage.input_tokens, 0);
        assert_eq!(state.usage.output_tokens, 0);
        assert_eq!(
            fs::read(request.game_directory.join("script.rpy")).unwrap(),
            original
        );
        assert_eq!(coverage_list(&state, "runtimeChecks"), vec![RUNTIME_CHECK]);
    }

    #[tokio::test]
    async fn complete_archive_template_inspects_all_members_before_publication() {
        let root = tempfile::tempdir().unwrap();
        let (service, request, workspace, mut state) = fixture(root.path(), "Renpy", "scripts.rpa");
        if !service.tools.vntextpatch().is_file() || !service.tools.python().is_file() {
            return;
        }
        let archive = request.game_directory.join("scripts.rpa");
        let script = r#"import pathlib, pickle, sys, zlib
key = 0x12345678
body = bytearray(b'RPA-3.0 ' + b'0' * 16 + b' 12345678\n')
index = {}
for name, data in {'script.rpy': 'label start:\n    "こんにちは世界。"\n'.encode(), 'image.png': b'fixture-media'}.items():
    index[name] = [(len(body) ^ key, len(data) ^ key, b'')]
    body.extend(data)
body[8:24] = f'{len(body):016x}'.encode()
body.extend(zlib.compress(pickle.dumps(index, protocol=3)))
pathlib.Path(sys.argv[1]).write_bytes(body)
"#;
        assert!(
            std::process::Command::new(service.tools.python())
                .args(["-I", "-X", "utf8", "-c", script])
                .arg(&archive)
                .status()
                .unwrap()
                .success()
        );
        let original = fs::read(&archive).unwrap();
        fs::create_dir_all(workspace.join("resources")).unwrap();
        fs::copy(&archive, workspace.join("resources/game.rpa")).unwrap();
        let extracted = service.adaptation_tool(&request, &workspace, &mut state, "run_engine_component",
            &json!({"id":"archive.rpa3","operation":"extract","archive":"resources/game.rpa","output":"work/unpacked","extensions":[".rpy"]}),
            &CancellationToken::new(), None).await.unwrap();
        assert_eq!(
            extracted["builtinSamples"]["trials"][0]["status"], "passed",
            "{extracted}"
        );
        assert!(state.verified_package.is_none());
        let adapter = run(
            &service,
            &request,
            &workspace,
            &mut state,
            &workspace.join("session.json"),
            &CancellationToken::new(),
            None,
        )
        .await
        .unwrap();
        let report = &state.extra["automaticTemplate"];
        assert!(adapter.is_some(), "{report}");
        assert_eq!(report["templateId"], "rpa3-renpy");
        assert_eq!(report["archivedScripts"], 1);
        let evidence = report["evidenceFiles"][0].as_str().unwrap();
        let manifest: Value =
            serde_json::from_slice(&fs::read(workspace.join(evidence)).unwrap()).unwrap();
        assert_eq!(manifest["entries"].as_array().unwrap().len(), 2);
        assert_eq!(fs::read(&archive).unwrap(), original);
        assert_eq!(state.usage.input_tokens, 0);
        assert_eq!(state.usage.output_tokens, 0);
    }
}
