//! Bounded first-turn reuse of built-in parsers and complete, known resource templates.
use super::*;
use crate::discovery::{glob_match, is_link};
use sha2::{Digest, Sha256};
use std::{
    io::{Read, Write},
    time::Duration,
};

const TRIAL_BYTES: u64 = 64 * 1024 * 1024;
const TEMPLATE_BYTES: u64 = 256 * 1024 * 1024;
const RPA_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const RUNTIME_CHECK: &str = "游戏启动、中文显示、字体及排版尚未实机验证。";

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
    workspace: &Path,
    state: &mut Session,
    lengths: &BTreeMap<String, u64>,
    expected: &BTreeMap<String, String>,
    token: &CancellationToken,
) -> Result<(), EngineError> {
    fs::create_dir_all(workspace.join("work"))?;
    let temporary = tempfile::Builder::new()
        .prefix(".auto-stage-")
        .tempdir_in(workspace.join("work"))?;
    for (relative, length) in lengths {
        cancelled(token)?;
        let destination = resolve_relative(&workspace.join("resources"), relative)?;
        if destination.is_file()
            && checked_hash(&destination, *length, token)
                .is_ok_and(|hash| hash == expected[relative])
        {
            continue;
        }
        let source = resolve_relative(game, relative)?;
        let copy = resolve_relative(temporary.path(), relative)?;
        if let Some(parent) = copy.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut input = fs::File::open(source)?;
        let mut output = fs::File::create(&copy)?;
        let mut buffer = [0_u8; 128 * 1024];
        let mut total = 0_u64;
        loop {
            cancelled(token)?;
            let count = input.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            total += count as u64;
            if total > *length {
                return Err(reject(format!("自动试用复制超出样本预算：{relative}")));
            }
            output.write_all(&buffer[..count])?;
        }
        drop(output);
        if checked_hash(&copy, *length, token)? != expected[relative] {
            return Err(reject("自动试用复制的样本与原始资源不一致。"));
        }
    }
    if sample_hashes(game, lengths, token)? != *expected {
        return Err(reject("自动试用暂存期间原始资源发生变化。"));
    }
    for (relative, hash) in expected {
        cancelled(token)?;
        let destination = resolve_relative(&workspace.join("resources"), relative)?;
        let copy = resolve_relative(temporary.path(), relative)?;
        if copy.is_file() {
            if let Some(parent) = destination.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::rename(copy, destination)?;
        }
        let hashes = state
            .extra
            .entry("resourceHashes".into())
            .or_insert_with(|| json!({}));
        hashes[relative] = json!(hash);
    }
    Ok(())
}

fn sample_group(definition: &Value, root: &Path, script: &str) -> Result<Vec<String>, EngineError> {
    let script = script.replace('\\', "/");
    let source = resolve_relative(root, &script)?;
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
        if resolve_relative(root, &relative)?.is_file() {
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
    let assembly = executable.with_extension("dll");
    let runtime = executable.with_extension("runtimeconfig.json");
    let mut files = vec![executable];
    if assembly.is_file() {
        files.push(assembly);
    }
    if runtime.is_file() {
        files.push(runtime);
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

pub(super) async fn run(
    service: &EngineService,
    request: &AdaptationRequest<'_>,
    workspace: &Path,
    state: &mut Session,
    path: &Path,
    cancellation: &CancellationToken,
    progress: ProgressCallback<'_>,
) -> Result<Option<AdapterHandle>, EngineError> {
    if !fresh(request, workspace, state)? {
        return Ok(None);
    }
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

pub(super) async fn run_trials(
    service: &EngineService,
    request: &AdaptationRequest<'_>,
    workspace: &Path,
    state: &mut Session,
    path: &Path,
    cancellation: &CancellationToken,
    progress: ProgressCallback<'_>,
) -> Result<(), EngineError> {
    if !fresh(request, workspace, state)? {
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
    let mut parser = None::<String>;
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
                let extra=(||->Result<BTreeMap<String,u64>,EngineError>{sample_group(definition,&request.game_directory,script)?.into_iter().filter(|file|!lengths.keys().any(|known:&String|known.eq_ignore_ascii_case(file))).map(|file|{let length=resolve_relative(&request.game_directory,&file)?.metadata()?.len();Ok((file,length))}).collect()})();
                let extra=match extra{Ok(extra)=>extra,Err(error)=>{if skipped.len()<6{skipped.push(format!("{format}：{}",limit(&error.to_string(),240)));}continue;}};
                let bytes=extra.values().try_fold(0_u64,|sum,length|sum.checked_add(*length));
                if lengths.len()+extra.len()>12||bytes.is_none_or(|bytes|bytes>TRIAL_BYTES.saturating_sub(selected_bytes)){
                    if skipped.len()<6{skipped.push(format!("{format}：{} 及伴随文件超过自动试用的 64 MiB／12 文件预算。",limit(script,80)));}continue;}
                selected_bytes+=bytes.unwrap_or(0);lengths.extend(extra);scripts+=1;
            }
            if scripts==0{if skipped.len()<6{skipped.push(format!("{format}：没有可直接试用的裸脚本样本；封包、编译脚本和私有过滤器需要另行确认。"));}continue;}
            if parser.is_none(){match parser_fingerprint(service,token){Ok(fingerprint)=>parser=Some(fingerprint),Err(EngineError::Cancelled)=>return Err(EngineError::Cancelled),Err(error)=>{record(state,path,"automaticTrials",json!({"revision":1,"status":"unavailable","reused":reused,"skipped":skipped,"error":format!("本地内置解析工具不可用，未执行自动试用；这是工具资产问题，不是格式不兼容：{}",limit(&error.to_string(),300))})).await?;return Ok(false);}}}
            attempted+=1;EngineService::report(progress,"自动试用内置解析器",format!("正在用 {} 个工作副本检查 {format} 的提取与原文回填。",lengths.len()));
            let candidate_budget=Budget::new(token,Duration::from_secs(20));let token=&candidate_budget.token;
            let hashes=sample_hashes(&request.game_directory,&lengths,token)?;
            let file_hashes:Vec<_>=hashes.iter().map(|(key,value)|json!({"key":key,"value":value})).collect();
            let fingerprint=hash_text(&json!({"revision":1,"trialRevision":1,"format":format,"parserFingerprint":parser,"files":file_hashes}).to_string());
            let cached=state.extra.get("builtinTrials").and_then(Value::as_array).into_iter().flatten().any(|trial|trial["format"]==format&&trial["fingerprint"]==fingerprint);
            if !cached&&let Some(trials)=state.extra.get_mut("builtinTrials").and_then(Value::as_array_mut){trials.retain(|trial|trial["format"]!=format);}
            stage(&request.game_directory,workspace,state,&lengths,&hashes,token)?;
            if parser.as_deref()!=Some(&parser_fingerprint(service,token)?){return Err(reject("自动试用期间解析工具发生变化，结果未复用。"));}
            if cached{reused+=1;continue;}
            let files:Vec<_>=lengths.keys().cloned().collect();
            let result=service.try_builtin_sample(workspace,state,&json!({"format":format,"directory":"resources","files":files,"checkRoundtrip":true}),token).await?;
            let after=sample_hashes(&request.game_directory,&lengths,token)?;
            if hashes!=after||result["sourceHashes"].as_object().into_iter().flatten().any(|(file,hash)|hashes.get(file).is_none_or(|expected|hash.as_str()!=Some(expected))){return Err(reject("自动试用期间原始资源或样本发生变化，结果未缓存，请重新确认资源。"));}
            if parser.as_deref()!=Some(&parser_fingerprint(service,token)?){return Err(reject("自动试用期间解析工具发生变化，结果未缓存。"));}
            if let Some(trial)=state.extra.get_mut("builtinTrials").and_then(Value::as_array_mut).and_then(|trials|trials.iter_mut().find(|trial|trial["format"]==format)){
                trial["fingerprint"]=if matches!(result["status"].as_str(),Some("passed"|"no-text"))||deterministic(&result["diagnostic"]){json!(fingerprint)}else{Value::Null};
                if let Some(samples)=trial["samples"].as_array_mut(){samples.truncate(2);for sample in samples{if let Some(text)=sample["text"].as_str(){sample["text"]=json!(limit(text,120));}}}
            }save_session(path,state).await?;
        }Ok::<_,EngineError>(true)
    }.await;
    match outcome {
        Ok(false) => Ok(()),
        Ok(true) => record(
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
            if cancellation.is_cancelled() {
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
    async fn existing_implementation_skips_automatic_work() {
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
        assert!(!state.extra.contains_key("automaticTrials"));
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
        assert!(!state.extra.contains_key("automaticTrials"));
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
        run_trials(
            &service, &request, &workspace, &mut state, &path, &token, None,
        )
        .await
        .unwrap();
        assert_eq!(state.extra["automaticTrials"]["reused"], 1);
        assert_eq!(state.extra["builtinTrials"].as_array().unwrap().len(), 1);
        assert_eq!(state.extra["builtinTrials"][0]["fingerprint"], fingerprint);
        assert_eq!(
            fs::read(request.game_directory.join("bad.mjo")).unwrap(),
            b"invalid format bytes"
        );
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
