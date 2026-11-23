//! Handlers for the adaptation tool registry.
use super::*;

pub(super) fn load_tool_groups(state: &mut Session, args: &Value) -> Result<Value, EngineError> {
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
    let classified = filter_tools(&tool_definitions()?, &selected)?;
    Ok(
        json!({"groups":selected,"groupTools":classified.iter().filter_map(|tool|tool["function"]["name"].as_str()).collect::<Vec<_>>(),"toolDefinitionsFixed":true,"success":true,"nextAction":"全部工具定义均已提供，请直接调用所需工具；工具组分类不改变 API 工具集。"}),
    )
}

pub(super) fn report_progress(
    state: &mut Session,
    args: &Value,
    progress: ProgressCallback<'_>,
) -> Result<Value, EngineError> {
    let kind = text(args, "kind")?;
    if !matches!(
        kind,
        "exploring" | "breakthrough" | "implementing" | "correction"
    ) {
        return Err(EngineError::Protocol(
            "进展类型必须是 exploring、breakthrough、implementing 或 correction。".into(),
        ));
    }
    let message = text(args, "message")?.trim();
    if message.is_empty() || message.chars().count() > 400 {
        return Err(EngineError::Protocol(
            "进度说明必须为 1 到 400 字符。".into(),
        ));
    }
    let repair_update = repair::approach(state, args)?;
    decision::update(state, args)?;
    if let Some((index, approach)) = repair_update {
        state.repairs[index].approach = Some(approach);
        state.repairs[index].status = repair::Status::Analyzing;
    }
    let changed = state.update_progress(kind, message, Some("report_progress"), true, progress);
    Ok(
        json!({"changed":changed,"note":state.progress_note,"problem":state.decision.current_problem,"validation":false}),
    )
}

pub(super) async fn inspect_workspace_files(
    service: &EngineService,
    request: &AdaptationRequest<'_>,
    workspace: &Path,
    state: &Session,
    name: &str,
    args: &Value,
    cancellation: &CancellationToken,
) -> Result<Value, EngineError> {
    let mut params = args.clone();
    params["operation"] = json!(name);
    params["root"] = json!(tool_root(request, workspace, args)?);
    let workspace_root = args["root"].as_str().unwrap_or("workspace") == "workspace";
    params["workspaceRoot"] = json!(workspace_root);
    if workspace_root {
        params["componentSourceDirectory"] =
            json!(workspace_file(components::LAUNCHER_SOURCE_DIRECTORY));
    }
    let inputs = args["root"] == "game";
    params["inputFiles"] = if inputs && name == "read_file" {
        let path = text(args, "path")?;
        json!({path:request.inputs.file(&request.game_directory, path)?})
    } else if inputs {
        json!(request.inputs.files)
    } else if args["root"].as_str().unwrap_or("workspace") == "workspace" {
        json!(
            resources::inputs(workspace, state)?
                .files
                .into_iter()
                .map(|(relative, path)| (format!("resources/{relative}"), path))
                .collect::<BTreeMap<_, _>>()
        )
    } else {
        json!({})
    };
    params["excludedFiles"] = if inputs {
        json!(request.inputs.excluded)
    } else {
        json!([])
    };
    service.helper(&params, cancellation).await
}

pub(super) fn find_game_file(
    request: &AdaptationRequest<'_>,
    args: &Value,
) -> Result<Value, EngineError> {
    let requested = text(args, "path")?.replace('\\', "/");
    let exact = resolve_relative(&request.game_directory, &requested)?;
    let query = requested.rsplit('/').next().unwrap_or("").to_lowercase();
    let inventory = Inventory::scan(&request.game_directory)?;
    let matches: Vec<_> = inventory
        .files
        .iter()
        .filter(|file| file.path.rsplit('/').next().unwrap_or("").to_lowercase() == query)
        .take(21)
        .map(|file| file.path.clone())
        .collect();
    Ok(
        json!({"requested":requested,"exactPathExists":exact.is_file(),"matches":matches.iter().take(20).collect::<Vec<_>>(),"partial":!inventory.complete||matches.len()>20,"note":"文件存在不代表游戏进程能够按相同路径访问；还需核对工作目录、转区环境与脚本引用的原始字节。"}),
    )
}

pub(super) fn inspect_existing_text(
    request: &AdaptationRequest<'_>,
    args: &Value,
) -> Result<Value, EngineError> {
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

pub(super) async fn inspect_native_resource(
    service: &EngineService,
    request: &AdaptationRequest<'_>,
    workspace: &Path,
    name: &str,
    args: &Value,
    cancellation: &CancellationToken,
) -> Result<Value, EngineError> {
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
            .ok_or_else(|| EngineError::Protocol("ranges 必须包含 1–8 个代码范围。".into()))?;
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
        payload["instructionLimit"] = json!(number(args, "instructionLimit", 80, 1, 200)?);
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
    let mut result = service.inspect_native(&payload, cancellation).await?;
    if name == "inspect_pe_imports" {
        crate::adaptation_context::annotate_font_imports(&mut result);
    }
    if name == "inspect_unity_schema" {
        result["components"] = json!(components::related(Some("unity"))?);
    }
    Ok(result)
}

pub(super) async fn inspect_siglus_game(
    service: &EngineService,
    request: &AdaptationRequest<'_>,
    args: &Value,
    cancellation: &CancellationToken,
) -> Result<Value, EngineError> {
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
        probe.resource_files = Some(inventory.files.into_iter().map(|file| file.path).collect());
        probe
            .extra
            .insert("inspectionInputs".into(), json!(request.inputs.files));
    }
    let response = service
        .execute(&AdapterHandle::BuiltinSiglus, &probe, cancellation)
        .await?;
    response
        .inspection
        .ok_or_else(|| EngineError::Protocol("Siglus 未返回检查结果。".into()))
}

pub(super) fn update_release_files(
    workspace: &Path,
    state: &mut Session,
    args: &Value,
) -> Result<Value, EngineError> {
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
    components::retain_release_sources(&state.release_files, &mut next)?;
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
            !components::is_release_source(path)
                && !next.iter().any(|file| file.path.eq_ignore_ascii_case(path))
        })
        .collect();
    state.release_files = next;
    Ok(
        json!({"releaseFiles":components::visible_release_files(&state.release_files).collect::<Vec<_>>(),"unregisteredCount":unregistered.len(),"unregisteredBytes":unregistered.iter().map(|(_,bytes)|bytes).sum::<u64>(),"unregistered":unregistered.iter().take(30).map(|(path,bytes)|json!({"path":path,"bytes":bytes})).collect::<Vec<_>>(),"nextAction":"请登记适配器实际使用的文件。组件的随附源码已由宿主管理，分析脚本与样本保留在 work/ 即可"}),
    )
}

pub(super) async fn query_engine_reference(
    service: &EngineService,
    workspace: &Path,
    state: &Session,
    name: &str,
    args: &Value,
    cancellation: &CancellationToken,
) -> Result<Value, EngineError> {
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
        return service.helper(&params, cancellation).await;
    }
    let reference = service.adaptation_reference(workspace, state);
    let library = reference
        .as_ref()
        .map_err(|error| EngineError::Rejected(error.to_string()))
        .and_then(|reference| service.reference_library(reference));
    match library {
        Ok(library) => {
            params["library"] = json!(library);
            params["revision"] = reference?["revision"].clone();
        }
        Err(error) if name == "inspect_engine_reference" => return Err(error),
        Err(_) => {
            let url = "https://raw.githubusercontent.com/GalTransl/GalTransl-GalGameReverse-Skill/main/catalog/engines.json";
            match service.fetch_reference(url, false, cancellation).await {
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
    service.helper(&params, cancellation).await
}

pub(super) async fn run_engine_python(
    service: &EngineService,
    request: &AdaptationRequest<'_>,
    workspace: &Path,
    state: &mut Session,
    args: &Value,
    cancellation: &CancellationToken,
) -> Result<Value, EngineError> {
    let mut samples = automatic::sample_files(workspace, args)?;
    if let Some(id) = args.get("questionId") {
        decision::validate_question_id(
            id.as_str()
                .ok_or_else(|| EngineError::Protocol("questionId 必须是字符串。".into()))?,
        )?;
    }
    let mut params = args.clone();
    if let Some(params) = params.as_object_mut() {
        params.remove("sampleFiles");
    }
    params["operation"] = json!("run");
    params["workspace"] = json!(workspace);
    let resource_inputs = resources::inputs(workspace, state)?;
    let _input_handles = crate::source::lock_inputs(&resource_inputs.files, cancellation)?;
    params["resourceDirectory"] = json!(request.game_directory);
    params["resourcePaths"] = json!(resource_inputs.files);
    params["resourceFiles"] = json!(resource_inputs.files.keys().collect::<Vec<_>>());
    params["sampleExtensions"] = json!(automatic::sample_extensions());
    if params.get("questionId").is_none()
        && let Some(problem) = &state.decision.current_problem
    {
        params["questionId"] = json!(problem.id);
    }
    params["toolPaths"] = service
        .tool_paths(Some(&workspace.join("src")), cancellation)
        .await?;
    let reference =
        if let Ok(pin) = read_json::<Value>(&workspace.join("src/tools/engine-reference.json")) {
            pin["reference"].clone()
        } else {
            service.adaptation_reference(workspace, state)?
        };
    params["library"] = if reference["kind"].as_str() == Some("siglus") {
        service.siglus(cancellation).await?["library"].clone()
    } else {
        json!(service.reference_library(&reference)?)
    };
    state
        .extra
        .insert("engineReference".into(), reference.clone());
    let mut result = service.helper(&params, cancellation).await?;
    result["revision"] = reference["revision"].clone();
    samples.extend(
        result["registeredSamples"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .filter(|file| !crate::discovery::is_documentation_file(file))
            .take(12)
            .map(str::to_owned),
    );
    if samples.is_empty() {
        samples.extend(
            result["writtenFiles"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .filter(|file| !crate::discovery::is_documentation_file(file))
                .take(128)
                .map(str::to_owned),
        );
    }
    if let Some(result) = result.as_object_mut() {
        result.remove("registeredSamples");
        result.remove("writtenFiles");
        if result.get("writtenFilesPartial") == Some(&json!(false)) {
            result.remove("writtenFilesPartial");
        }
    }
    let mut seen = BTreeSet::new();
    samples.retain(|file| seen.insert(file.to_lowercase()));
    if !samples.is_empty() {
        result["builtinSamples"] = automatic::sample_result(
            automatic::inspect_samples(service, workspace, state, &samples, cancellation).await,
        );
    }
    Ok(result)
}

pub(super) fn inspect_engine_knowledge(
    service: &EngineService,
    request: &AdaptationRequest<'_>,
    state: &mut Session,
    args: &Value,
    progress: ProgressCallback<'_>,
) -> Result<Value, EngineError> {
    if args.get("sourceOffset").is_some() && args["id"].as_str().is_none_or(str::is_empty) {
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
            let page = crate::review::knowledge_page(
                sources
                    .iter()
                    .skip(offset)
                    .map(|origin| json!({"title":origin["title"],"evidence":origin["evidence"]})),
            );
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
            json!({"id":id,"revision":value["revision"],"knowledgeName":family["name"],"family":family,"conditions":value["conditions"],"summary":value["summary"],"guidance":value["guidance"],"reuseOptions":service.knowledge_reuse_options(request,&value)?,"engineCandidateMatches":crate::adaptation_context::knowledge_match_score(&value,&knowledge_candidates(request,state)?)>0,"tools":value["tools"].as_array().into_iter().flatten().map(|tool|json!({"name":tool["name"],"description":tool["description"],"usage":tool["usage"],"files":tool["files"].as_object().map(|files|files.keys().collect::<Vec<_>>())})).collect::<Vec<_>>(),"sourceGameCount":sources.len(),"limitation":"先根据当前资源核对适用条件。来源游戏的结论不能代替本游戏的提取、回填、字形或运行验证；需要来源证据时，使用相同 id 和 sourceOffset=0 分页读取。"}),
        );
    }
    Ok(value)
}

pub(super) fn copy_engine_knowledge_tools(
    request: &AdaptationRequest<'_>,
    workspace: &Path,
    state: &mut Session,
    args: &Value,
    progress: ProgressCallback<'_>,
) -> Result<Value, EngineError> {
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

pub(super) async fn search_repositories(
    service: &EngineService,
    args: &Value,
    cancellation: &CancellationToken,
) -> Result<Value, EngineError> {
    let query = text(args, "query")?;
    if query.len() > 512 {
        return Err(EngineError::Protocol("检索条件过长。".into()));
    }
    let mut url =
        reqwest::Url::parse("https://api.github.com/search/repositories").map_err(file_error)?;
    url.query_pairs_mut()
        .append_pair("per_page", "8")
        .append_pair("q", query);
    let document = service
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

pub(super) async fn download_tool(
    service: &EngineService,
    workspace: &Path,
    state: &mut Session,
    args: &Value,
    cancellation: &CancellationToken,
    progress: ProgressCallback<'_>,
) -> Result<Value, EngineError> {
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
    let (sha256, bytes, reused, changed, source) = if !refresh && path.is_file() && pinned.is_some()
    {
        let digest = hash_file(&path).map_err(file_error)?;
        if Some(digest.as_str()) != pinned {
            return Err(EngineError::Rejected(format!(
                "目标工具已有本地修改，未覆盖：{relative}。请选择新路径，或用 refresh=true 明确重新下载。"
            )));
        }
        (digest, path.metadata()?.len(), true, false, "project")
    } else {
        let download = service
            .fetch_public_download(
                url,
                refresh,
                if refresh { None } else { pinned },
                cancellation,
            )
            .await?;
        let changed =
            !path.is_file() || hash_file(&path).map_err(file_error)? != download.source.sha256;
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
            tool_name: Some("download_tool".into()),
            ..EngineProgress::default()
        }));
    }
    Ok(
        json!({"path":relative,"sha256":sha256,"reused":reused,"source":source,"bytes":bytes,"releaseRegistered":state.release_files.iter().any(|file|workspace_file(&file.path)==relative)}),
    )
}

pub(super) fn report_blocker(state: &mut Session, args: &Value) -> Result<Value, EngineError> {
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
    let can_continue = match args.get("canContinueLocally") {
        Some(Value::Bool(value)) => Some(*value),
        None => None,
        _ => {
            return Err(EngineError::Protocol(
                "canContinueLocally 必须为布尔值".into(),
            ));
        }
    };
    let has_evidence = args["evidence"].as_str().is_some_and(|evidence| {
        !evidence.trim().is_empty() && evidence.encode_utf16().count() <= 2000
    });
    if can_continue == Some(true)
        || (kind == "unsupported" && (can_continue != Some(false) || !has_evidence))
    {
        return Ok(
            json!({"stopped":false,"nextAction":"请继续当前可执行的本地工作。确有无法适配的阻碍时，提供已尝试方案和结果，并确认本地工作无法继续"}),
        );
    }
    state.summary = text(args, "summary")?.into();
    state.next_action = text(args, "nextAction")?.into();
    state.extra.insert("blocker".into(), args.clone());
    Ok(json!({"stopped":true,"kind":kind,"success":true}))
}
