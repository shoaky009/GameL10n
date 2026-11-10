//! Packaging and verification shared by model-driven adapter tools.
use super::*;
use crate::protocol::ProbeResult;

pub(super) const VERIFIER_REVISION: u32 = 10;

fn verified_public_dependency(
    service: &EngineService,
    state: &Session,
    path: &str,
    digest: &str,
) -> Result<bool, EngineError> {
    if !path.starts_with("tools/") {
        return Ok(false);
    }
    let relative = workspace_file(path);
    if components::matches_shipped_dependency(service, &relative, digest)? {
        return Ok(true);
    }
    let Some(source) = state
        .extra
        .get("toolSources")
        .and_then(|sources| sources.get(&relative))
    else {
        return Ok(false);
    };
    if source["sha256"] != digest {
        return Ok(false);
    }
    let Some(url) = source["url"].as_str() else {
        return Ok(false);
    };
    service.has_verified_public_download(url, digest)
}

fn input_file_error(path: &Path, error: std::io::Error) -> EngineError {
    EngineError::Io(std::io::Error::new(
        error.kind(),
        format!("无法读取验收资源 {}：{error}", path.display()),
    ))
}

fn samples(coverage: &Value) -> Value {
    coverage
        .get("samples")
        .cloned()
        .unwrap_or_else(|| json!([]))
}

fn validate_release_selection(selection: &[ReleaseFile]) -> Result<Vec<ReleaseFile>, EngineError> {
    if selection.is_empty() || selection.len() > 512 {
        return Err(EngineError::Rejected("发布清单需包含 1 到 512 项。".into()));
    }
    let mut selected = Vec::with_capacity(selection.len());
    let mut paths = BTreeSet::new();
    for file in selection {
        if file.path.encode_utf16().count() > 512
            || file.path.contains('\\')
            || file
                .path
                .split('/')
                .any(|part| matches!(part, "" | "." | ".."))
            || !paths.insert(file.path.to_lowercase())
        {
            return Err(EngineError::Rejected(format!(
                "发布文件需要不重复的规范相对路径：{}",
                file.path
            )));
        }
        select_release(&mut selected, file.clone())?;
    }
    if !selected
        .iter()
        .any(|file| file.path == "src/adapter.py" && file.role == "source")
    {
        return Err(EngineError::Rejected(
            "发布选择必须包含 src/adapter.py。".into(),
        ));
    }
    selected.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(selected)
}

fn require_launch_resources(state: &Session, inputs: &AdaptationInputs) -> Result<(), EngineError> {
    let profiles = state
        .extra
        .get("launchInspection")
        .and_then(|inspection| inspection["localeProfiles"].as_array());
    let approved = state.extra.get("resourceHashes").and_then(Value::as_object);
    let staged = |path: &str| {
        approved.is_some_and(|files| files.keys().any(|file| file.eq_ignore_ascii_case(path)))
    };
    for profile in profiles.into_iter().flatten() {
        if profile["executable"]
            .as_str()
            .is_some_and(|path| !inputs.excludes(path) && staged(path))
            && let Some(path) = profile["path"].as_str()
            && !inputs.excludes(path)
            && !staged(path)
        {
            return Err(EngineError::Rejected(format!(
                "所选程序有转区配置尚未纳入验证，请 stage_resources 并声明资源发现规则：{path}"
            )));
        }
    }
    Ok(())
}

fn progress(state: &mut Session, milestone: u32) {
    let previous = state
        .extra
        .get("highestVerificationMilestone")
        .and_then(|value| {
            value.as_u64().or_else(|| match value.as_str()? {
                "None" => Some(0),
                "Compiled" => Some(1),
                "Discovery" => Some(2),
                "OriginalRoundtrip" => Some(3),
                "Complete" => Some(4),
                _ => None,
            })
        })
        .unwrap_or(0);
    if u64::from(milestone) > previous || milestone == 4 {
        if u64::from(milestone) > previous {
            state
                .extra
                .insert("highestVerificationMilestone".into(), json!(milestone));
        }
        state.extra.insert(
            "lastVerificationProgressTurn".into(),
            json!(state.total_turns),
        );
    }
}

fn report_verification_progress(
    state: &mut Session,
    stage: &str,
    passed: bool,
    original_roundtrip_passed: bool,
    callback: ProgressCallback<'_>,
) {
    let message = match (stage, passed) {
        ("discovery", true) => "已确认适配器能识别当前游戏资源；提取和中文回填尚未验收。",
        ("finish-ready", true) => {
            "原文回填、中文回填和局部试译的资源验收已通过；游戏启动与显示仍需实际验证。"
        }
        ("trial", true) => "所选资源的小范围试验已通过；仍需完整资源验收，游戏启动与显示尚未验证。",
        ("verification", false) if original_roundtrip_passed => {
            "原文回填已通过逐字节验证；中文或局部试译验收尚未通过，正在修复。"
        }
        _ => return,
    };
    state.update_progress("verified", message, Some("test_adapter"), callback);
}

fn evidence(
    request: &AdaptationRequest<'_>,
    state: &Session,
    trial: Option<&[String]>,
) -> (Value, Vec<TranslationUnit>) {
    let mut coverage = state.text_coverage.clone();
    let included = |path: &str| {
        trial.is_none_or(|files| {
            files
                .iter()
                .any(|file| file.eq_ignore_ascii_case(&path.replace('\\', "/")))
        })
    };
    coverage["samples"] = json!(
        samples(&coverage)
            .as_array()
            .into_iter()
            .flatten()
            .filter(|sample| sample["resourcePath"].as_str().is_some_and(&included))
            .collect::<Vec<_>>()
    );
    let baseline = if request.objective.eq_ignore_ascii_case("repair") {
        request
            .baseline_units
            .iter()
            .filter(|unit| included(&unit.resource_path))
            .cloned()
            .collect()
    } else {
        vec![]
    };
    (coverage, baseline)
}

#[derive(Default)]
pub(super) struct FingerprintStats {
    fingerprint_ms: u128,
    discovery_ms: u128,
    passes: u64,
    hashed_files: u64,
    hashed_bytes: u64,
    identity_files: u64,
    game_files: BTreeSet<String>,
    staged_files: BTreeSet<String>,
    candidate_files: usize,
    candidate_bytes: u64,
    discovery_read_files: u64,
    discovery_read_bytes: u64,
}

impl FingerprintStats {
    fn record_probe(&mut self, evidence: &Value) {
        self.discovery_read_files += evidence["readFiles"].as_u64().unwrap_or(0);
        self.discovery_read_bytes += evidence["readBytes"].as_u64().unwrap_or(0);
    }

    fn apply(&self, result: &mut Value) {
        result["fingerprintMs"] = json!(self.fingerprint_ms);
        result["discoveryMs"] = json!(self.discovery_ms);
        result["fingerprintStats"] = json!({
            "passes": self.passes,
            "hashedFiles": self.hashed_files,
            "hashedBytes": self.hashed_bytes,
            "identityFiles": self.identity_files,
            "gameFiles": self.game_files.len(),
            "stagedFiles": self.staged_files.len(),
            "gamePaths": self.game_files.iter().take(8).collect::<Vec<_>>(),
            "stagedPaths": self.staged_files.iter().take(8).collect::<Vec<_>>(),
            "pathsTruncated": self.game_files.len() > 8 || self.staged_files.len() > 8,
            "candidateFiles": self.candidate_files,
            "candidateBytes": self.candidate_bytes,
            "discoveryReadFiles": self.discovery_read_files,
            "discoveryReadBytes": self.discovery_read_bytes,
        });
    }
}

struct DiscoveryInputs {
    evidence: Value,
    probe: ProbeResult,
    resources: Vec<String>,
    game_launch_files: Vec<String>,
    staged_launch_files: Vec<String>,
    complete: bool,
}

struct InputFingerprint {
    digest: String,
    staged_hashes: BTreeMap<String, String>,
}

fn file_identities(
    root: &Path,
    files: &BTreeSet<String>,
    inputs: &AdaptationInputs,
) -> Result<BTreeMap<String, Value>, EngineError> {
    files
        .iter()
        .map(|file| {
            let path = inputs.file(root, file)?;
            let metadata = path.metadata().map_err(|error| input_file_error(&path, error))?;
            Ok((
                file.clone(),
                json!({
                    "length": metadata.len(),
                    "modified": format!("{:?}", metadata.modified().map_err(|error| input_file_error(&path, error))?),
                    "created": metadata.created().ok().map(|time| format!("{time:?}")),
                }),
            ))
        })
        .collect()
}

pub(super) fn input_hashes(
    root: &Path,
    files: &BTreeSet<String>,
    inputs: &AdaptationInputs,
    stats: &mut FingerprintStats,
) -> Result<BTreeMap<String, String>, EngineError> {
    files
        .iter()
        .map(|file| {
            let path = inputs.file(root, file)?;
            stats.hashed_bytes += path
                .metadata()
                .map_err(|error| input_file_error(&path, error))?
                .len();
            stats.hashed_files += 1;
            let digest = hash_file(&path).map_err(|error| match error {
                gamel10n_translation::files::FileError::Io { source, .. } => {
                    input_file_error(&path, source)
                }
                error => file_error(error),
            })?;
            if inputs
                .expected_hash(file)
                .is_some_and(|expected| !digest.eq_ignore_ascii_case(expected))
            {
                return Err(EngineError::Rejected(format!(
                    "已绑定的制作输入发生变化：{file}；已有文本基线保留。"
                )));
            }
            Ok((file.clone(), digest))
        })
        .collect()
}

fn candidate_inventory(inventory: &Inventory, patterns: &[String]) -> BTreeMap<String, u64> {
    inventory
        .files
        .iter()
        .filter(|file| {
            patterns
                .iter()
                .any(|pattern| crate::discovery::glob_match(pattern, &file.path))
        })
        .map(|file| (file.path.clone(), file.length))
        .collect()
}

#[expect(
    clippy::too_many_arguments,
    reason = "Discovery checks the existing package against independent game and staged inputs."
)]
async fn discover_inputs(
    service: &EngineService,
    request: &AdaptationRequest<'_>,
    workspace: &Path,
    state: &Session,
    package: &PackageManifest,
    handle: &AdapterHandle,
    trial: Option<&[String]>,
    cancellation: &CancellationToken,
    stats: &mut FingerprintStats,
) -> Result<DiscoveryInputs, EngineError> {
    let staged_root = workspace.join("resources");
    if let Some(files) = trial {
        let (probe, evidence) = service
            .probe_bounded_with_evidence(handle, &staged_root, files, cancellation)
            .await?;
        stats.record_probe(&evidence);
        return Ok(DiscoveryInputs {
            evidence,
            probe: probe
                .ok_or_else(|| EngineError::Rejected("适配器未识别所选完整试验资源。".into()))?,
            resources: files.to_vec(),
            game_launch_files: vec![],
            staged_launch_files: vec![],
            complete: true,
        });
    }
    let game = request
        .inputs
        .inventory(Inventory::scan(&request.game_directory)?)?;
    let staged = Inventory::scan(&staged_root)?;
    let mut patterns = state.resource_patterns.clone();
    if let Some(recognition) = &state.recognition {
        for rule in recognition.required.iter().chain(&recognition.hints) {
            patterns.extend(rule.patterns.iter().cloned());
        }
    }
    patterns.extend(["**/*.exe.le.config".into(), "*.exe.le.config".into()]);
    let game_candidates = candidate_inventory(&game, &patterns);
    let staged_candidates = candidate_inventory(&staged, &patterns);
    stats.candidate_files = game_candidates.len() + staged_candidates.len();
    stats.candidate_bytes = game_candidates
        .values()
        .chain(staged_candidates.values())
        .sum();
    let recognition = game.recognize_with(package.recognition.as_ref(), &|relative| {
        request.inputs.file(&request.game_directory, relative).ok()
    });
    if package.recognition.is_some() && recognition.0 != crate::discovery::RecognitionStatus::Match
    {
        return Err(EngineError::Rejected(
            "声明的必要识别条件不符合实际游戏。".into(),
        ));
    }
    let (sample, sample_evidence) = service
        .probe_bounded_with_evidence(
            handle,
            &staged_root,
            &package.resources(&staged),
            cancellation,
        )
        .await?;
    stats.record_probe(&sample_evidence);
    let sample = sample.ok_or_else(|| {
        EngineError::Rejected("适配器未能识别暂存样本，请检查候选规则与 64 KiB 探测边界。".into())
    })?;
    let (actual, actual_evidence) = service
        .probe_inputs_with_evidence(
            handle,
            &request.game_directory,
            &package.resources(&game),
            &request.inputs,
            cancellation,
        )
        .await?;
    stats.record_probe(&actual_evidence);
    let actual =
        actual.ok_or_else(|| EngineError::Rejected("适配器未能识别真实游戏资源。".into()))?;
    let paths = |files: &[String]| {
        files
            .iter()
            .map(|file| file.replace('\\', "/").to_lowercase())
            .collect::<BTreeSet<_>>()
    };
    if paths(&sample.required_files) != paths(&actual.required_files) {
        return Err(EngineError::Rejected(
            "真实游戏与暂存样本的必需资源不一致，请补暂存缺少的必需资源或修正 probe。".into(),
        ));
    }
    let executable = |probe: &ProbeResult| {
        probe
            .original_executable
            .as_ref()
            .map(|path| path.replace('\\', "/").to_lowercase())
    };
    if executable(&sample) != executable(&actual) {
        return Err(EngineError::Rejected(
            "真实游戏与暂存样本的原启动入口不一致。".into(),
        ));
    }
    let launch_files = |inventory: &Inventory| {
        inventory
            .files
            .iter()
            .filter(|file| file.path.to_lowercase().ends_with(".exe.le.config"))
            .map(|file| file.path.clone())
            .collect()
    };
    // Probe only sees bounded copies; its actual reads and candidate set cover discovery
    // changes without reading the unrelated bodies of unselected archives.
    Ok(DiscoveryInputs {
        evidence: json!({
            "gameCandidates":game_candidates,"stagedCandidates":staged_candidates,
            "gameComplete":game.complete,"stagedComplete":staged.complete,
            "recognition":recognition,"sample":sample,"actual":actual,
            "sampleReads":sample_evidence,"actualReads":actual_evidence,
        }),
        resources: sample.required_files.clone(),
        // Output naming uses the real candidate inventory, including unstaged media volumes.
        probe: ProbeResult {
            build_hints: actual.build_hints,
            ..sample
        },
        game_launch_files: launch_files(&game),
        staged_launch_files: launch_files(&staged),
        complete: game.complete && staged.complete,
    })
}

fn fingerprint(
    request: &AdaptationRequest<'_>,
    workspace: &Path,
    state: &Session,
    discovery_only: bool,
    trial: Option<&[String]>,
    discovery: &DiscoveryInputs,
    stats: &mut FingerprintStats,
) -> Result<InputFingerprint, EngineError> {
    let approved: BTreeMap<String, String> = serde_json::from_value(
        state
            .extra
            .get("resourceHashes")
            .cloned()
            .unwrap_or_else(|| json!({})),
    )?;
    if approved.is_empty() {
        return Err(EngineError::Rejected(
            "请先 stage_resources 选择用于验证的真实资源。".into(),
        ));
    }
    let staged_root = workspace.join("resources");
    let mut game_files: BTreeSet<_> = discovery.resources.iter().cloned().collect();
    let mut staged_files = game_files.clone();
    if let Some(files) = trial {
        game_files.extend(files.iter().cloned());
        staged_files.extend(files.iter().cloned());
    } else {
        game_files.extend(discovery.game_launch_files.iter().cloned());
        staged_files.extend(discovery.staged_launch_files.iter().cloned());
        game_files.extend(approved.keys().cloned());
        staged_files.extend(approved.keys().cloned());
    }
    stats.passes += 1;
    stats.game_files.extend(game_files.iter().cloned());
    stats.staged_files.extend(staged_files.iter().cloned());
    let (game_inputs, staged_inputs, staged_hashes) = if discovery_only {
        // Bounded probe evidence plus file metadata is sufficient for discovery reuse.
        // Full and trial verification still establish byte-for-byte source integrity.
        let game_inputs = file_identities(&request.game_directory, &game_files, &request.inputs)?;
        let staged_inputs =
            file_identities(&staged_root, &staged_files, &AdaptationInputs::default())?;
        stats.identity_files += (game_inputs.len() + staged_inputs.len()) as u64;
        (json!(game_inputs), json!(staged_inputs), BTreeMap::new())
    } else {
        let game_hashes =
            input_hashes(&request.game_directory, &game_files, &request.inputs, stats)?;
        let staged_hashes = input_hashes(
            &staged_root,
            &staged_files,
            &AdaptationInputs::default(),
            stats,
        )?;
        for (path, hash) in &approved {
            if trial.is_some_and(|files| !files.iter().any(|file| file.eq_ignore_ascii_case(path)))
            {
                continue;
            }
            if game_hashes.get(path) != Some(hash) || staged_hashes.get(path) != Some(hash) {
                return Err(EngineError::Rejected(format!(
                    "原资源或工作副本发生变化，请重新 stage_resources 并验证：{path}"
                )));
            }
        }
        for path in &discovery.resources {
            if game_hashes.get(path) != staged_hashes.get(path) {
                return Err(EngineError::Rejected(format!(
                    "真实游戏与暂存必需资源内容不一致，请重新 stage_resources：{path}"
                )));
            }
        }
        (json!(game_hashes), json!(staged_hashes), staged_hashes)
    };
    let (coverage, baseline) = evidence(request, state, trial);
    let digest = hash_text(&json!({"cacheSchema":4,"verifierRevision":VERIFIER_REVISION,"host":"rust-python-v2","implementation":workspace_fingerprint(workspace,state)?,"gameDirectory":request.game_directory,"discovery":discovery.evidence,"gameResources":game_inputs,"stagedResources":staged_inputs,"approvedResources":approved.iter().filter(|(path,_)|trial.is_none_or(|files|files.iter().any(|file|file.eq_ignore_ascii_case(path)))).collect::<BTreeMap<_,_>>(),"objective":request.objective,"baseline":baseline,"coverage":samples(&coverage),"launchInspection":state.extra.get("launchInspection"),"discoveryOnly":discovery_only,"trialFiles":trial}).to_string());
    Ok(InputFingerprint {
        digest,
        staged_hashes,
    })
}

fn refresh_result(mut result: Value, state: &Session, reused: bool) -> Value {
    result["reused"] = json!(reused);
    result["runtimeTested"] = json!(false);
    result["pendingCoverage"] = json!(coverage_list(state, "pending"));
    result["runtimeChecks"] = json!(coverage_list(state, "runtimeChecks"));
    if reused {
        for key in ["compileMs", "discoveryMs", "verificationMs"] {
            result[key] = json!(0);
        }
    }
    result
}

fn accept(
    state: &mut Session,
    package: Value,
    implementation: &str,
    verification: VerificationResult,
    request: &AdaptationRequest<'_>,
) {
    state.verified_package = Some(package);
    state.verified_fingerprint = Some(implementation.into());
    state.verification = Some(verification);
    state.extra.insert(
        "rustVerifiedContextFingerprint".into(),
        json!(verification_context(request)),
    );
    state.extra.insert(
        "rustVerifiedCoverageFingerprint".into(),
        json!(hash_text(&samples(&state.text_coverage).to_string())),
    );
}

#[expect(
    clippy::too_many_arguments,
    reason = "The two fingerprint layers share the package, resource scope and execution controls."
)]
async fn inspect_inputs(
    service: &EngineService,
    request: &AdaptationRequest<'_>,
    workspace: &Path,
    state: &Session,
    package: &PackageManifest,
    handle: &AdapterHandle,
    discovery_only: bool,
    trial: Option<&[String]>,
    cancellation: &CancellationToken,
    stats: &mut FingerprintStats,
) -> Result<(DiscoveryInputs, InputFingerprint), EngineError> {
    let started = Instant::now();
    let discovery = discover_inputs(
        service,
        request,
        workspace,
        state,
        package,
        handle,
        trial,
        cancellation,
        stats,
    )
    .await;
    stats.discovery_ms += started.elapsed().as_millis();
    let discovery = discovery?;
    let started = Instant::now();
    let fingerprint = fingerprint(
        request,
        workspace,
        state,
        discovery_only,
        trial,
        &discovery,
        stats,
    );
    stats.fingerprint_ms += started.elapsed().as_millis();
    Ok((discovery, fingerprint?))
}

pub(super) async fn test_adapter(
    service: &EngineService,
    request: &AdaptationRequest<'_>,
    workspace: &Path,
    state: &mut Session,
    args: &Value,
    cancellation: &CancellationToken,
    callback: ProgressCallback<'_>,
) -> Result<Value, EngineError> {
    let changes = resources::reconcile(request, workspace, state, cancellation, callback)?;
    if !changes.is_empty() {
        EngineService::report(
            callback,
            "更新验证资源",
            format!(
                "已同步制作输入，更新 {} 个资源副本，移除 {} 个过期记录。",
                changes.rebound.len(),
                changes.removed.len()
            ),
        );
        state.extra.insert("resourceChanges".into(), json!(changes));
    }
    let kind = text(args, "kind")?;
    let key = text(args, "key")?;
    let name = text(args, "name")?;
    validate_family(kind, key, name)?;
    state.family = json!({"kind":kind,"key":key,"name":name});
    state.resource_patterns = strings(args, "resourcePatterns")?;
    for pattern in &state.resource_patterns {
        validate_pattern(pattern)?;
    }
    if let Some(recognition) = args.get("recognition") {
        state.recognition = if recognition.is_null() {
            None
        } else {
            Some(serde_json::from_value(recognition.clone())?)
        };
    }
    if let Some(recognition) = &state.recognition {
        recognition.validate()?;
    }
    if let Some(selection) = args.get("releaseFiles") {
        let selection: Vec<ReleaseFile> = serde_json::from_value(selection.clone())?;
        state.release_files = validate_release_selection(&selection)?;
    }
    apply_coverage(state, args)?;
    let discovery_only = args
        .get("discoveryOnly")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let trial = args
        .get("trialFiles")
        .map(|value| serde_json::from_value::<Vec<String>>(value.clone()))
        .transpose()?
        .map(|files| {
            files
                .into_iter()
                .map(|file| file.replace('\\', "/"))
                .collect::<Vec<_>>()
        });
    if let Some(files) = &trial {
        if discovery_only {
            return Err(EngineError::Rejected(
                "trialFiles 与 discoveryOnly 不能同时使用。".into(),
            ));
        }
        let unique: BTreeSet<_> = files.iter().map(|file| file.to_lowercase()).collect();
        if files.is_empty() || files.len() > 12 || unique.len() != files.len() {
            return Err(EngineError::Rejected(
                "trialFiles 需要 1–12 个不重复的完整资源路径。".into(),
            ));
        }
        let approved = state.extra.get("resourceHashes");
        for file in files {
            resolve_relative(&workspace.join("resources"), file)?;
            if approved.and_then(|value| value.get(file)).is_none() {
                return Err(EngineError::Rejected(format!(
                    "试验资源尚未 stage_resources：{file}"
                )));
            }
        }
    }
    let previous_full_verification = if !discovery_only && trial.is_none() {
        state
            .verification
            .take()
            .filter(|verification| {
                verification.passed
                    && verification.original_roundtrip_passed
                    && verification.verified_extraction.is_some()
            })
            .and_then(|verification| {
                Some((
                    state.verified_package.take()?,
                    state.verified_fingerprint.take()?,
                    state
                        .extra
                        .get("verificationCache")?
                        .get("fingerprint")?
                        .clone(),
                    verification,
                ))
            })
    } else {
        None
    };
    state.verified_package = None;
    state.verified_fingerprint = None;
    if trial.is_none() {
        state.verification = None;
        require_launch_resources(state, &request.inputs)?;
    }
    let preparation_started = Instant::now();
    service
        .bind_reference(workspace, state, None, cancellation)
        .await?;
    state.release_files = validate_release_selection(&state.release_files)?;
    if state
        .extra
        .get("resourceHashes")
        .and_then(Value::as_object)
        .is_none_or(|files| files.is_empty())
    {
        return Err(EngineError::Rejected(
            "请先 stage_resources 选择用于验证的真实资源。".into(),
        ));
    }
    let implementation = workspace_fingerprint(workspace, state)?;
    let mut stats = FingerprintStats::default();
    let cache_key = if discovery_only {
        "discoveryVerificationCache"
    } else if trial.is_some() {
        "trialVerificationCache"
    } else {
        "verificationCache"
    };
    let cached = state.extra.get(cache_key).cloned().filter(|cache| {
        cache["revision"] == VERIFIER_REVISION && cache["implementation"] == implementation
    });
    let prepared = if let Some(cached) = &cached {
        if let Some(error) = cached["compilationError"].as_str() {
            return Err(EngineError::Rejected(error.into()));
        }
        let package = &cached["package"];
        if let Some(path) = package["directoryPath"].as_str() {
            let path = Path::new(path);
            match PackageManifest::load(path) {
                Ok(manifest) => match manifest.handle(path) {
                    Ok(handle) if service.tool_paths(Some(path), cancellation).await.is_ok() => {
                        Some((path.to_path_buf(), manifest, handle))
                    }
                    _ => None,
                },
                Err(_) => None,
            }
        } else {
            None
        }
    } else {
        None
    };
    let package_was_valid = prepared.is_some();
    let directory = workspace.join("builds").join(&implementation[..20]);
    let id = format!("{kind}.{key}");
    let version = implementation[..16].to_string();
    let (package, parsed, handle) = if let Some(prepared) = prepared {
        prepared
    } else {
        let package = directory.join("package");
        fs::create_dir_all(&package)?;
        let approved: BTreeMap<String, String> = serde_json::from_value(
            state
                .extra
                .get("resourceHashes")
                .cloned()
                .unwrap_or_else(|| json!({})),
        )?;
        let mut package_files = BTreeMap::new();
        let mut python_files = vec![];
        for file in &state.release_files {
            let source = resolve_relative(workspace, &workspace_file(&file.path))?;
            let digest = hash_file(&source).map_err(file_error)?;
            if approved.values().any(|hash| hash == &digest)
                && !verified_public_dependency(service, state, &file.path, &digest)?
            {
                return Err(EngineError::Rejected(format!(
                    "发布选择包含游戏原始资源：{}。若为公共依赖，请用 download_tool 获取可核实的公开来源；不要通过重命名或打包绕过检查。",
                    file.path
                )));
            }
            let target = resolve_relative(&package, &file.path)?;
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(source, &target)?;
            if file.path.ends_with(".py") {
                python_files.push(target);
            }
            package_files.insert(file.path.clone(), digest);
        }
        if let Err(error) = service
            .helper(
                &json!({"operation":"syntax","files":python_files}),
                cancellation,
            )
            .await
        {
            if matches!(&error, EngineError::Rejected(_))
                && implementation == workspace_fingerprint(workspace, state)?
            {
                state.extra.insert(cache_key.into(),json!({"implementation":implementation,"revision":VERIFIER_REVISION,"compilationError":error.to_string()}));
            }
            return Err(error);
        }
        let sources = package_files
            .iter()
            .filter(|(path, _)| path.starts_with("src/"))
            .collect::<BTreeMap<_, _>>();
        let source_sha256 = hash_text(&serde_json::to_string(&sources)?);
        let assembly_sha256 = package_files
            .get("src/adapter.py")
            .ok_or_else(|| EngineError::Protocol("缺少适配器入口。".into()))?;
        let manifest = json!({"id":id,"version":version,"manifestVersion":4,"protocolVersion":1,"family":state.family,"resourceFiles":[],"resourcePatterns":state.resource_patterns,"recognition":state.recognition,"sourceSha256":source_sha256,"assemblySha256":assembly_sha256,"packageFiles":package_files,"releaseFiles":state.release_files});
        write_json(&package.join("package.json"), &manifest).map_err(file_error)?;
        let parsed = PackageManifest::load(&package)?;
        let handle = parsed.handle(&package)?;
        (package, parsed, handle)
    };
    let manifest = serde_json::to_value(&parsed)?;
    progress(state, 1);
    let compile_ms = preparation_started.elapsed().as_millis();
    let staged_root = workspace.join("resources");
    let mut stage = if trial.is_some() {
        "trial"
    } else {
        "discovery"
    };
    let mut verification = None;
    let mut verification_directory = None;
    let mut probe = None;
    let mut verification_ms = 0;
    let mut cacheable = true;
    let mut input = None;
    let mut inspected = false;
    let mut reused = false;
    state.extra.remove(cache_key);
    let checked = async {
        let (discovery, fingerprint) = inspect_inputs(
            service,
            request,
            workspace,
            state,
            &parsed,
            &handle,
            discovery_only,
            trial.as_deref(),
            cancellation,
            &mut stats,
        )
        .await?;
        if implementation != workspace_fingerprint(workspace, state)? {
            return Err(EngineError::Rejected(
                "验收准备期间源码或依赖发生变化，请重新验收。".into(),
            ));
        }
        input = Some(fingerprint.digest);
        inspected = true;
        cacheable &= discovery.complete;
        let resources = discovery.resources;
        probe = Some(discovery.probe);
        if let Some(cached) = cached.as_ref().filter(|cached| {
            package_was_valid
                && input
                    .as_ref()
                    .is_some_and(|input| cached["fingerprint"] == *input)
                && cached["result"].is_object()
        }) {
            reused = true;
            if !discovery_only {
                stage = cached["result"]["stage"].as_str().unwrap_or("discovery");
                verification = cached
                    .get("verification")
                    .filter(|value| !value.is_null())
                    .map(|value| serde_json::from_value::<VerificationResult>(value.clone()))
                    .transpose()?;
                if trial.is_none()
                    && stage == "finish-ready"
                    && cached["result"]["passed"] == true
                    && let Some(result) = verification.as_mut().filter(|verification| {
                        verification.passed && verification.original_roundtrip_passed
                    })
                    && let Some((package, implementation, input, previous)) =
                        &previous_full_verification
                    && cached["package"] == *package
                    && cached["implementation"] == *implementation
                    && cached["fingerprint"] == *input
                    && result.resource_hashes == previous.resource_hashes
                {
                    result
                        .verified_extraction
                        .clone_from(&previous.verified_extraction);
                }
            }
            return Ok(cached["result"]["passed"] == true);
        }
        if trial.is_none() {
            progress(state, 2);
        }
        if discovery_only {
            return Ok(true);
        }
        stage = if trial.is_some() {
            "trial"
        } else {
            "verification"
        };
        let (coverage, baseline) = evidence(request, state, trial.as_deref());
        let output = tempfile::Builder::new()
            .prefix("verification-")
            .tempdir_in(&directory)?
            .keep();
        verification_directory = Some(output.clone());
        let started = Instant::now();
        let forward = |event| {
            if let Some(callback) = callback {
                callback(state.progress(event));
            }
        };
        let result = service
            .verify_with_coverage(
                &handle,
                &staged_root,
                &resources,
                &output,
                &baseline,
                &coverage,
                Some(&fingerprint.staged_hashes),
                probe.as_ref(),
                cancellation,
                Some(&forward),
            )
            .await?;
        verification_ms = started.elapsed().as_millis();
        cacheable &= result.cacheable;
        if trial.is_none() {
            if result.original_roundtrip_passed {
                progress(state, 3);
            }
            if result.passed {
                stage = "finish-ready";
            }
        }
        let passed = result.passed;
        verification = Some(result);
        Ok(passed)
    }
    .await;
    let (mut passed, mut diagnostics) = match checked {
        Ok(passed) => (
            passed,
            verification
                .as_ref()
                .map(|result| result.diagnostics.clone())
                .unwrap_or_else(|| {
                    vec!["真实游戏与样本的探测结果一致；尚未执行完整资源哈希校验、提取及回填验收，不能发布。".into()]
                }),
        ),
        Err(EngineError::Cancelled) => return Err(EngineError::Cancelled),
        Err(error) => {
            cacheable &= !matches!(&error, EngineError::Io(_) | EngineError::Timeout);
            (false, vec![error.to_string()])
        }
    };
    if inspected && !reused {
        let final_input = inspect_inputs(
            service,
            request,
            workspace,
            state,
            &parsed,
            &handle,
            discovery_only,
            trial.as_deref(),
            cancellation,
            &mut stats,
        )
        .await;
        let changed = match final_input {
            Ok((discovery, final_input)) => {
                cacheable &= discovery.complete;
                input.as_ref() != Some(&final_input.digest)
                    || implementation != workspace_fingerprint(workspace, state)?
            }
            Err(EngineError::Cancelled) => return Err(EngineError::Cancelled),
            Err(error) => {
                diagnostics.push(format!("验收输入复核失败：{error}"));
                true
            }
        };
        if changed {
            passed = false;
            cacheable = false;
            if stage == "finish-ready" {
                stage = "verification";
            }
            diagnostics.push(
                "验收期间源码、资源或发现依据发生变化，请重新暂存变化资源并重新验收。".into(),
            );
        }
    }
    if let Some(verification) = &mut verification {
        verification.passed = passed;
        verification.cacheable = cacheable;
        verification.diagnostics.clone_from(&diagnostics);
        if let Some(directory) = &verification_directory {
            write_json(&directory.join("verification.json"), verification).map_err(file_error)?;
        }
        if trial.is_none() {
            state.verification = Some(verification.clone());
        }
    }
    if passed && stage == "finish-ready" && !reused {
        progress(state, 4);
    }
    report_verification_progress(
        state,
        stage,
        passed,
        verification
            .as_ref()
            .is_some_and(|result| result.original_roundtrip_passed),
        callback,
    );
    let selected = trial
        .as_ref()
        .or_else(|| probe.as_ref().map(|probe| &probe.required_files))
        .cloned()
        .unwrap_or_default();
    let package_info = json!({"directoryPath":package,"id":id,"version":version,"manifest":manifest,"resourceFiles":selected,"resourceHashes":verification.as_ref().map(|result|&result.resource_hashes)});
    if passed && stage == "finish-ready" {
        accept(
            state,
            package_info.clone(),
            &implementation,
            verification
                .clone()
                .ok_or_else(|| EngineError::Protocol("完整验收缺少结果。".into()))?,
            request,
        );
    }
    let mut result = json!({"passed":passed,"stage":stage,"diagnostics":diagnostics,"id":id,"version":version,"compileMs":compile_ms,"verificationMs":verification_ms,"verificationStagesMs":if reused {None}else{verification.as_ref().map(|result|&result.stage_timings_ms)},"trialFiles":trial,"probe":probe,"nextAction":if !passed{"修复 diagnostics 后重新验收。"}else if stage=="trial"{"小范围试验通过；省略 trialFiles 执行完整验收后才能 finish。"}else if stage=="discovery"{"资源发现通过；执行完整 test_adapter 后才能发布。"}else{"完整资源验收通过，当前目标完成后 finish；已知遗漏需要 allowPartial 并明确披露。"}});
    result["issues"] = json!(
        verification
            .as_ref()
            .map(|verification| verification.issues.clone())
            .unwrap_or_default()
    );
    result["evidenceFile"] = json!(verification_directory.as_ref().and_then(|directory| {
        directory
            .join("verification.json")
            .strip_prefix(workspace)
            .ok()
            .map(|path| path.to_string_lossy().replace('\\', "/"))
    }));
    if !passed && result["issues"].as_array().is_none_or(Vec::is_empty) {
        result["issues"] = json!([crate::VerificationIssue::new(
            crate::VerificationIssueCode::AdapterOperation,
            stage
        )
        .difference("verification passes", &diagnostics.join("; "))]);
    }
    if let Some(files) = &trial {
        result["trialBytes"] = json!(
            files
                .iter()
                .map(|file| resolve_relative(&staged_root, file)
                    .and_then(|path| Ok(path.metadata()?.len())))
                .collect::<Result<Vec<_>, _>>()?
                .into_iter()
                .sum::<u64>()
        );
    }
    result = refresh_result(result, state, reused);
    result["compileMs"] = json!(compile_ms);
    stats.apply(&mut result);
    if cacheable && input.is_some() {
        state.extra.insert(cache_key.into(),json!({"fingerprint":input,"implementation":implementation,"revision":VERIFIER_REVISION,"package":package_info,"verification":verification,"result":result}));
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(root: &Path) -> (EngineService, AdaptationRequest<'static>, PathBuf, Session) {
        let assets = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        let service = EngineService::new(&assets, crate::ToolPaths::development())
            .with_shared_directory(root.join("shared"));
        let game = root.join("game");
        let workspace = root.join("project/adaptation/current");
        fs::create_dir_all(&game).expect("game");
        fs::create_dir_all(workspace.join("resources")).expect("resources");
        fs::create_dir_all(workspace.join("src/tools")).expect("source");
        fs::write(game.join("text.json"), "[\"原文\",\"second\"]\n").expect("resource");
        fs::copy(
            game.join("text.json"),
            workspace.join("resources/text.json"),
        )
        .expect("staged resource");
        let adapter = include_str!("../tests/fixtures/json_adapter.py").replace(
            "requiredFiles=['text.json']",
            "requiredFiles=[p for p in r['resourceFiles'] if p.endswith('.json')]",
        );
        fs::write(workspace.join("src/adapter.py"), adapter).expect("adapter");
        let mut state = Session::default();
        state.extra.insert(
            "resourceHashes".into(),
            json!({"text.json":hash_file(&game.join("text.json")).expect("hash")}),
        );
        register_python(&mut state, "src/adapter.py").expect("selection");
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
        (service, request, workspace, state)
    }

    fn arguments() -> Value {
        json!({"kind":"engine","key":"fixture","name":"Fixture","resourcePatterns":["*.json","*.bin"]})
    }

    #[test]
    fn launch_validation_distinguishes_original_profiles_from_patch_added_profiles() {
        let mut state = Session::default();
        state
            .extra
            .insert("resourceHashes".into(), json!({"game.exe":"selected"}));
        state.extra.insert(
            "launchInspection".into(),
            json!({"localeProfiles":[
                {"path":"game.exe.le.config","executable":"game.exe","executableExists":false}
            ]}),
        );
        let mut inputs = AdaptationInputs::default();
        assert!(require_launch_resources(&state, &inputs).is_err());
        inputs.excluded.insert("game.exe.le.config".into());
        require_launch_resources(&state, &inputs)
            .expect("patch-added profile is not original input");
        inputs.files.insert(
            "game.exe.le.config".into(),
            PathBuf::from("original-profile"),
        );
        assert!(require_launch_resources(&state, &inputs).is_err());
        state.extra.get_mut("resourceHashes").expect("hashes")["game.exe.le.config"] =
            json!("selected original");
        require_launch_resources(&state, &inputs).expect("original profile is staged");
    }

    #[tokio::test]
    async fn repair_rebinds_staged_inputs_and_publishes_against_the_same_baseline() {
        let root = tempfile::tempdir().expect("fixture");
        let (service, mut request, workspace, mut state) = fixture(root.path());
        if !service.tools.python().is_file() || !service.tools.vntextpatch().is_file() {
            return;
        }
        let source = request.project_directory.join("sources/baseline");
        fs::create_dir_all(&source).expect("source");
        fs::copy(
            request.game_directory.join("text.json"),
            source.join("text.json"),
        )
        .expect("baseline");
        let original_hash = hash_file(&source.join("text.json")).expect("hash");
        request
            .inputs
            .files
            .insert("text.json".into(), source.join("text.json"));
        request
            .inputs
            .hashes
            .insert("text.json".into(), original_hash.clone());
        request.inputs.excluded.insert("runtime.json".into());
        request.objective = "Repair".into();
        request.improve_existing = true;
        let baseline_units: Vec<_> = ["原文", "second"]
            .into_iter()
            .enumerate()
            .map(|(index, text)| TranslationUnit {
                id: index.to_string(),
                resource_path: "text.json".into(),
                locator: index.to_string(),
                source_text: text.into(),
                context: "stable".into(),
                speaker: None,
                kind: "dialogue".into(),
                protected_tokens: None,
            })
            .collect();
        let request = AdaptationRequest {
            baseline_units: &baseline_units,
            ..request
        };
        for directory in [&request.game_directory, &workspace.join("resources")] {
            fs::write(
                directory.join("text.json"),
                b"installed patch is not the original JSON",
            )
            .expect("installed data");
            fs::write(directory.join("runtime.json"), b"patch-only dependency").expect("sidecar");
        }
        state.extra.insert("resourceHashes".into(), json!({
            "text.json":hash_file(&request.game_directory.join("text.json")).expect("installed hash"),
            "runtime.json":hash_file(&request.game_directory.join("runtime.json")).expect("sidecar hash"),
        }));
        let token = CancellationToken::new();
        let read = service
            .adaptation_tool(
                &request,
                &workspace,
                &mut state,
                "read_file",
                &json!({"root":"game","path":"text.json","encoding":"utf8"}),
                &token,
                None,
            )
            .await
            .expect("read original input");
        assert_eq!(
            serde_json::from_str::<Vec<String>>(read["content"].as_str().expect("content"))
                .expect("original JSON"),
            ["原文", "second"]
        );
        let installed = service
            .adaptation_tool(
                &request,
                &workspace,
                &mut state,
                "read_file",
                &json!({"root":"installed","path":"text.json","encoding":"utf8"}),
                &token,
                None,
            )
            .await
            .expect("read installed bytes for diagnosis");
        assert_eq!(
            installed["content"],
            "installed patch is not the original JSON"
        );
        let tested = service
            .test_adapter(&request, &workspace, &mut state, &arguments(), &token, None)
            .await
            .expect("repair verification");
        assert_eq!(tested["stage"], "finish-ready", "{tested}");
        assert_eq!(
            state.extra["resourceHashes"],
            json!({"text.json":original_hash})
        );
        assert!(!workspace.join("resources/runtime.json").exists());
        let published = service
            .finish_adapter(
                &request,
                &workspace,
                &mut state,
                &json!({"summary":"Repair verified against the saved text baseline."}),
                &token,
            )
            .await
            .expect("publish using bound inputs");
        assert_eq!(published["success"], true);
        assert_eq!(
            fs::read(request.game_directory.join("text.json")).expect("installed data"),
            b"installed patch is not the original JSON"
        );
        assert!(request.game_directory.join("runtime.json").is_file());
        fs::write(source.join("text.json"), "[\"原文\", \"second\"]")
            .expect("changed input with identical text");
        let error = resources::stage(
            &request,
            &workspace,
            &mut state,
            &["text.json".into()],
            false,
            &token,
            None,
        )
        .expect_err("bound bytes cannot change even when the text is unchanged");
        assert!(error.to_string().contains("已绑定的制作输入发生变化"));
        assert_eq!(
            hash_file(&workspace.join("resources/text.json")).expect("staged hash"),
            request.inputs.hashes["text.json"]
        );
    }

    #[tokio::test]
    async fn staged_public_dependencies_require_host_verified_provenance() {
        let root = tempfile::tempdir().expect("fixture");
        let (service, request, workspace, mut state) = fixture(root.path());
        if !service.tools.python().is_file() || !service.tools.vntextpatch().is_file() {
            return;
        }
        let content = b"public font fixture";
        let digest = gamel10n_translation::files::hash_bytes(content);
        let url = "https://example.com/font.otf";
        for path in [
            request.game_directory.join("font.otf"),
            workspace.join("resources/font.otf"),
            workspace.join("src/tools/font.otf"),
        ] {
            fs::write(path, content).expect("font");
        }
        state.extra.get_mut("resourceHashes").expect("hashes")["font.otf"] = json!(digest);
        state.extra.insert(
            "toolSources".into(),
            json!({"src/tools/font.otf":{"url":url,"sha256":digest}}),
        );
        select_release(
            &mut state.release_files,
            ReleaseFile {
                path: "tools/font.otf".into(),
                role: "asset".into(),
                purpose: "runtime font".into(),
            },
        )
        .expect("font selection");
        let cancellation = CancellationToken::new();
        let error = service
            .test_adapter(
                &request,
                &workspace,
                &mut state,
                &arguments(),
                &cancellation,
                None,
            )
            .await
            .expect_err("model-authored provenance alone is insufficient");
        assert!(error.to_string().contains("游戏原始资源"));

        let directory = service.shared_directory.join("downloads").join(
            crate::research::public_download_cache_key(&reqwest::Url::parse(url).expect("URL")),
        );
        fs::create_dir_all(&directory).expect("download directory");
        fs::write(directory.join(format!("{digest}.bin")), content).expect("download content");
        write_json(
            &directory.join(format!("{digest}.json")),
            &crate::research::DownloadReceipt {
                url: url.into(),
                sha256: digest,
                bytes: content.len() as u64,
                retrieved_at: chrono::Utc::now(),
                media_type: None,
            },
        )
        .expect("host receipt");
        let tested = service
            .test_adapter(
                &request,
                &workspace,
                &mut state,
                &arguments(),
                &cancellation,
                None,
            )
            .await
            .expect("verified public font can be packaged");
        assert_eq!(tested["stage"], "finish-ready");
    }

    #[test]
    fn missing_verification_inputs_identify_the_path_and_keep_io_error_kind() {
        let root = tempfile::tempdir().expect("fixture");
        let path = "missing.dat";
        let error = file_identities(
            root.path(),
            &BTreeSet::from([path.into()]),
            &AdaptationInputs::default(),
        )
        .expect_err("missing input");
        assert!(
            matches!(&error, EngineError::Io(error) if error.kind() == std::io::ErrorKind::NotFound)
        );
        assert!(error.to_string().contains(path));
    }

    fn archive_fixture(
        root: &Path,
    ) -> (
        AdaptationRequest<'static>,
        PathBuf,
        Session,
        DiscoveryInputs,
    ) {
        let (_, request, workspace, mut state) = fixture(root);
        let archive = request.game_directory.join("archive.bin");
        fs::File::create(&archive)
            .expect("archive")
            .set_len(256 * 1024)
            .expect("archive length");
        fs::copy(&archive, workspace.join("resources/archive.bin")).expect("staged archive");
        state.extra.insert(
            "resourceHashes".into(),
            json!({"archive.bin":hash_file(&archive).expect("archive hash")}),
        );
        let discovery = DiscoveryInputs {
            evidence: json!({"actualReads":{"ranges":[{"path":"archive.bin","offset":0,"length":65536,"sha256":"header"}]}}),
            probe: serde_json::from_value(json!({
                "engineId":"fixture","adapterId":"engine.fixture","adapterVersion":"1",
                "description":"fixture","requiredFiles":["archive.bin"],
            }))
            .expect("probe"),
            resources: vec!["archive.bin".into()],
            game_launch_files: vec![],
            staged_launch_files: vec![],
            complete: true,
        };
        (request, workspace, state, discovery)
    }

    #[test]
    fn discovery_fingerprint_avoids_hashing_selected_archive_bodies() {
        let root = tempfile::tempdir().expect("fixture");
        let (request, workspace, state, discovery) = archive_fixture(root.path());
        let mut stats = FingerprintStats::default();
        let input = fingerprint(
            &request, &workspace, &state, true, None, &discovery, &mut stats,
        )
        .expect("discovery fingerprint");
        assert_eq!((stats.hashed_files, stats.hashed_bytes), (0, 0));
        assert_eq!(stats.identity_files, 2);
        assert!(input.staged_hashes.is_empty());
    }

    #[test]
    fn discovery_fingerprint_changes_when_file_metadata_changes() {
        let root = tempfile::tempdir().expect("fixture");
        let (request, workspace, state, discovery) = archive_fixture(root.path());
        let first = fingerprint(
            &request,
            &workspace,
            &state,
            true,
            None,
            &discovery,
            &mut FingerprintStats::default(),
        )
        .expect("first fingerprint");
        let file = OpenOptions::new()
            .write(true)
            .open(request.game_directory.join("archive.bin"))
            .expect("archive");
        let modified = file
            .metadata()
            .expect("metadata")
            .modified()
            .expect("mtime");
        file.set_times(
            fs::FileTimes::new().set_modified(modified + std::time::Duration::from_secs(2)),
        )
        .expect("change metadata");
        let changed = fingerprint(
            &request,
            &workspace,
            &state,
            true,
            None,
            &discovery,
            &mut FingerprintStats::default(),
        )
        .expect("changed fingerprint");
        assert_ne!(first.digest, changed.digest);
    }

    #[test]
    fn full_and_trial_fingerprints_detect_body_changes_with_unchanged_metadata() {
        let root = tempfile::tempdir().expect("fixture");
        let (request, workspace, state, discovery) = archive_fixture(root.path());
        let files = ["archive.bin".into()];
        for root in [&request.game_directory, &workspace.join("resources")] {
            let path = root.join("archive.bin");
            let mut file = OpenOptions::new().write(true).open(path).expect("archive");
            let modified = file
                .metadata()
                .expect("metadata")
                .modified()
                .expect("mtime");
            std::io::Seek::seek(&mut file, std::io::SeekFrom::Start(128 * 1024))
                .expect("body offset");
            file.write_all(b"changed body").expect("change body");
            file.set_times(fs::FileTimes::new().set_modified(modified))
                .expect("restore metadata");
        }
        for trial in [None, Some(files.as_slice())] {
            let result = fingerprint(
                &request,
                &workspace,
                &state,
                false,
                trial,
                &discovery,
                &mut FingerprintStats::default(),
            );
            assert!(
                matches!(result, Err(EngineError::Rejected(message)) if message.contains("stage_resources"))
            );
        }
    }

    #[test]
    fn full_fingerprint_rejects_required_source_mismatch_outside_approved_resources() {
        let root = tempfile::tempdir().expect("fixture");
        let (request, workspace, mut state, discovery) = archive_fixture(root.path());
        state.extra.insert(
            "resourceHashes".into(),
            json!({"text.json":hash_file(&request.game_directory.join("text.json")).expect("hash")}),
        );
        let mut file = OpenOptions::new()
            .write(true)
            .open(workspace.join("resources/archive.bin"))
            .expect("staged archive");
        std::io::Seek::seek(&mut file, std::io::SeekFrom::Start(128 * 1024)).expect("body offset");
        file.write_all(b"changed staged body").expect("change body");
        drop(file);
        let result = fingerprint(
            &request,
            &workspace,
            &state,
            false,
            None,
            &discovery,
            &mut FingerprintStats::default(),
        );
        assert!(
            matches!(result, Err(EngineError::Rejected(message)) if message.contains("必需资源内容不一致"))
        );
    }

    #[test]
    fn incomplete_discovery_still_fingerprints_observed_changes() {
        let root = tempfile::tempdir().expect("fixture");
        let (_, request, workspace, state) = fixture(root.path());
        let probe = serde_json::from_value(json!({
            "engineId":"fixture","adapterId":"engine.fixture","adapterVersion":"1",
            "description":"fixture","requiredFiles":["text.json"],
        }))
        .expect("probe");
        let mut discovery = DiscoveryInputs {
            evidence: json!({"gameComplete":false,"gameCandidates":{"text.json":21}}),
            probe,
            resources: vec!["text.json".into()],
            game_launch_files: vec![],
            staged_launch_files: vec![],
            complete: false,
        };
        let first = fingerprint(
            &request,
            &workspace,
            &state,
            true,
            None,
            &discovery,
            &mut FingerprintStats::default(),
        )
        .expect("fingerprint");
        discovery.evidence["gameCandidates"]["new.bin"] = json!(128);
        let changed = fingerprint(
            &request,
            &workspace,
            &state,
            true,
            None,
            &discovery,
            &mut FingerprintStats::default(),
        )
        .expect("changed fingerprint");
        assert_ne!(first.digest, changed.digest);
    }

    #[tokio::test]
    async fn discovery_cache_uses_bounded_candidates_and_detects_new_required_resources() {
        let root = tempfile::tempdir().expect("fixture");
        let (service, request, workspace, mut state) = fixture(root.path());
        if !service.tools.python().is_file() || !service.tools.vntextpatch().is_file() {
            return;
        }
        let media = request.game_directory.join("media.bin");
        fs::File::create(&media)
            .expect("media")
            .set_len(256 * 1024)
            .expect("length");
        let full_cache = json!({"result":{"passed":true,"stage":"finish-ready"}});
        state
            .extra
            .insert("verificationCache".into(), full_cache.clone());
        let trial_cache = json!({"result":{"passed":true,"stage":"trial"}});
        state
            .extra
            .insert("trialVerificationCache".into(), trial_cache.clone());
        let mut args = arguments();
        args["discoveryOnly"] = json!(true);
        let cancellation = CancellationToken::new();
        let first = test_adapter(
            &service,
            &request,
            &workspace,
            &mut state,
            &args,
            &cancellation,
            None,
        )
        .await
        .expect("discovery");
        assert_eq!(first["passed"], true);
        assert_eq!(first["stage"], "discovery");
        assert_eq!(first["fingerprintStats"]["hashedBytes"], 0);
        assert_eq!(state.extra["verificationCache"], full_cache);
        assert_eq!(state.extra["trialVerificationCache"], trial_cache);
        assert!(state.extra.contains_key("discoveryVerificationCache"));
        assert!(state.verified_package.is_none());
        assert!(state.verification.is_none());
        assert!(
            first["fingerprintStats"]["discoveryReadBytes"]
                .as_u64()
                .expect("reads")
                < 256 * 1024
        );
        let cached = test_adapter(
            &service,
            &request,
            &workspace,
            &mut state,
            &args,
            &cancellation,
            None,
        )
        .await
        .expect("cached discovery");
        assert_eq!(cached["reused"], true);
        assert_eq!(cached["fingerprintStats"]["passes"], 1);
        assert_eq!(cached["stage"], "discovery");
        assert_eq!(cached["fingerprintStats"]["hashedBytes"], 0);
        assert!(state.verified_package.is_none());
        let mut file = OpenOptions::new().write(true).open(&media).expect("media");
        file.write_all(b"changed header").expect("change header");
        drop(file);
        let changed = test_adapter(
            &service,
            &request,
            &workspace,
            &mut state,
            &args,
            &cancellation,
            None,
        )
        .await
        .expect("changed candidate");
        assert_eq!(changed["reused"], false);
        fs::remove_file(media).expect("remove media");
        let removed = test_adapter(
            &service,
            &request,
            &workspace,
            &mut state,
            &args,
            &cancellation,
            None,
        )
        .await
        .expect("removed candidate");
        assert_eq!(removed["reused"], false);
        fs::write(request.game_directory.join("new.json"), "[]").expect("new script");
        let added = test_adapter(
            &service,
            &request,
            &workspace,
            &mut state,
            &args,
            &cancellation,
            None,
        )
        .await
        .expect("new required resource");
        assert_eq!(added["passed"], false);
        assert!(added["diagnostics"].to_string().contains("必需资源不一致"));
        assert!(state.verified_package.is_none());
    }

    #[tokio::test]
    async fn selected_same_length_changes_reject_cached_verification() {
        let root = tempfile::tempdir().expect("fixture");
        let (service, request, workspace, mut state) = fixture(root.path());
        if !service.tools.python().is_file() || !service.tools.vntextpatch().is_file() {
            return;
        }
        let args = arguments();
        let cancellation = CancellationToken::new();
        let first = test_adapter(
            &service,
            &request,
            &workspace,
            &mut state,
            &args,
            &cancellation,
            None,
        )
        .await
        .expect("full verification");
        assert_eq!(first["passed"], true);
        let extraction = state
            .verification
            .as_ref()
            .and_then(|verification| verification.verified_extraction.clone())
            .expect("in-memory extraction");
        let cached = test_adapter(
            &service,
            &request,
            &workspace,
            &mut state,
            &args,
            &cancellation,
            None,
        )
        .await
        .expect("cached full verification");
        assert_eq!(cached["reused"], true);
        assert!(std::sync::Arc::ptr_eq(
            &extraction,
            state
                .verification
                .as_ref()
                .and_then(|verification| verification.verified_extraction.as_ref())
                .expect("preserved extraction"),
        ));
        let mut restored: Session =
            serde_json::from_value(serde_json::to_value(&state).expect("saved session"))
                .expect("restored session");
        let restarted = test_adapter(
            &service,
            &request,
            &workspace,
            &mut restored,
            &args,
            &cancellation,
            None,
        )
        .await
        .expect("cached verification after restart");
        assert_eq!(restarted["reused"], true);
        assert!(
            restored
                .verification
                .as_ref()
                .expect("restored verification")
                .verified_extraction
                .is_none()
        );
        fs::write(
            request.game_directory.join("text.json"),
            "[\"原文\",\"SECOND\"]\n",
        )
        .expect("same length change");
        let changed = test_adapter(
            &service,
            &request,
            &workspace,
            &mut state,
            &args,
            &cancellation,
            None,
        )
        .await
        .expect("changed input");
        assert_eq!(changed["passed"], false);
        assert!(
            changed["diagnostics"]
                .to_string()
                .contains("stage_resources")
        );
        assert!(state.verified_package.is_none());
        assert!(state.verification.is_none());
    }

    #[tokio::test]
    async fn final_input_changes_revoke_success_and_the_persisted_report() {
        let root = tempfile::tempdir().expect("fixture");
        let (service, request, workspace, mut state) = fixture(root.path());
        if !service.tools.python().is_file() || !service.tools.vntextpatch().is_file() {
            return;
        }
        let changed = std::sync::atomic::AtomicBool::new(false);
        let callback = |event: EngineProgress| {
            if event.stage == "验收资源回填"
                && !changed.swap(true, std::sync::atomic::Ordering::SeqCst)
            {
                fs::write(
                    request.game_directory.join("text.json"),
                    "[\"原文\",\"SECOND\"]\n",
                )
                .expect("change during verification");
            }
        };
        let result = test_adapter(
            &service,
            &request,
            &workspace,
            &mut state,
            &arguments(),
            &CancellationToken::new(),
            Some(&callback),
        )
        .await
        .expect("verification");
        assert!(changed.load(std::sync::atomic::Ordering::SeqCst));
        assert_eq!(result["passed"], false);
        assert_eq!(result["stage"], "verification");
        assert!(state.verified_package.is_none());
        assert!(!state.verification.as_ref().expect("verification").passed);
        assert!(!state.extra.contains_key("verificationCache"));
        let build = fs::read_dir(workspace.join("builds"))
            .expect("builds")
            .next()
            .expect("build")
            .expect("entry")
            .path();
        let report = fs::read_dir(build)
            .expect("build outputs")
            .map(|entry| entry.expect("entry").path().join("verification.json"))
            .find(|path| path.is_file())
            .expect("report");
        assert_eq!(
            read_json::<Value>(&report).expect("report JSON")["passed"],
            false
        );
    }

    #[test]
    fn fresh_complete_verification_refreshes_progress_but_repeated_partial_stages_do_not() {
        let mut state = Session {
            total_turns: 4,
            ..Session::default()
        };
        progress(&mut state, 4);
        state.total_turns = 12;
        progress(&mut state, 3);
        assert_eq!(state.extra["lastVerificationProgressTurn"], 4);
        progress(&mut state, 4);
        assert_eq!(state.extra["lastVerificationProgressTurn"], 12);
        assert_eq!(state.extra["highestVerificationMilestone"], 4);
    }
}
