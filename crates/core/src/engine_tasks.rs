//! Project-owned recognition, adaptation and verified patch operations.
use crate::{
    Application, CancellationToken, CoreError, StoredAnalysis,
    application::resolve_relative,
    catalog::{AdapterVersion, EngineCatalog, GameCompatibility},
    logs::FileProgress,
    patching::{PAYLOAD_DIRECTORY, PatchInstaller, build_manifest},
};
use gamel10n_engines::{
    AdapterHandle, EngineProgress, EngineService, PackageManifest, VerificationResult,
    VerifiedExtraction,
    adaptation::{AdaptationInputs, AdaptationLimits, AdaptationRequest, AdaptationResult},
    discovery::Inventory,
    protocol::{AdapterRequest, BuildMode, ProbeResult},
    service::GeneratedAdapterReuseFailure,
};
use gamel10n_translation::{
    files::{hash_file, hash_text, read_json, write_json},
    provider::{ApiProtocol, OpenAiProvider, ProviderSettings, TranslationFormat},
    validate_units,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PatchResult {
    pub patch_directory: PathBuf,
    #[serde(default)]
    pub launch_executable: Option<String>,
    #[serde(default)]
    pub total_units: usize,
    #[serde(default)]
    pub translated_units: usize,
    #[serde(default)]
    pub is_trial: bool,
    #[serde(default)]
    pub validation: Value,
    #[serde(default)]
    pub warnings: Vec<String>,
    #[serde(default)]
    pub source_fingerprint: String,
    #[serde(default, flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub enum AdaptationObjective {
    Develop,
    Repair,
    ExpandCoverage,
}

#[derive(Debug)]
pub enum RecognitionResult {
    Extracted(usize),
    Adapted(Box<AdaptationResult>),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RecognitionMode {
    #[default]
    Automatic,
    Intelligent,
}

const SMALL_EXTRACTION_THRESHOLD: usize = 1_000;

struct AdaptationStart {
    objective: AdaptationObjective,
    inventory: Option<Inventory>,
    require_model_analysis: bool,
}

struct ExistingRecognition {
    extracted: Option<usize>,
    inventory: Inventory,
    known_game: bool,
}

fn catalog_adapter(project: &crate::GameProject) -> Option<AdapterVersion> {
    crate::adapter_feedback::current_adapter(&project.extra).filter(|adapter| {
        adapter.adapter_id.starts_with("engine.") || adapter.adapter_id.starts_with("template.")
    })
}

impl RecognitionResult {
    fn task_outcome(&self) -> Result<String, String> {
        match self {
            Self::Extracted(count) => Ok(format!("识别完成，已提取 {count} 条文本。")),
            Self::Adapted(result) => adaptation_task_outcome(result),
        }
    }
}

impl AdaptationObjective {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Develop => "Develop",
            Self::Repair => "Repair",
            Self::ExpandCoverage => "ExpandCoverage",
        }
    }

    fn from_saved(value: &Value) -> Result<Option<Self>, CoreError> {
        if value.is_null() || value.as_str() == Some("") {
            return Ok(None);
        }
        match (value.as_u64(), value.as_str()) {
            (Some(0), _) | (_, Some("Develop")) => Ok(Some(Self::Develop)),
            (Some(1), _) | (_, Some("Repair")) => Ok(Some(Self::Repair)),
            (Some(2), _) | (_, Some("ExpandCoverage")) => Ok(Some(Self::ExpandCoverage)),
            _ => Err(CoreError::InvalidData("适配会话目标无效。".into())),
        }
    }
}

fn improving_existing(project: &crate::GameProject) -> bool {
    [
        project.extra.get("adapterId"),
        project
            .extra
            .get("analysis")
            .and_then(|value| value.get("adapterId")),
    ]
    .into_iter()
    .flatten()
    .filter_map(Value::as_str)
    .any(|id| !id.trim().is_empty())
}

fn saved_adaptation_objective(
    project: &crate::GameProject,
    directory: &Path,
) -> Result<Option<AdaptationObjective>, CoreError> {
    if let Some(value) = project.extra.get("adaptationObjective")
        && let Some(objective) = AdaptationObjective::from_saved(value)?
    {
        return Ok(Some(objective));
    }
    #[derive(Deserialize)]
    struct SavedObjective {
        #[serde(default)]
        objective: Value,
    }
    match read_json::<SavedObjective>(&directory.join("adaptation/current/session.json")) {
        Ok(saved) => AdaptationObjective::from_saved(&saved.objective),
        Err(gamel10n_translation::files::FileError::Io { source, .. })
            if source.kind() == std::io::ErrorKind::NotFound =>
        {
            Ok(None)
        }
        Err(gamel10n_translation::files::FileError::Json { source, .. }) if !source.is_io() => {
            Ok(None)
        }
        Err(error) => Err(error.into()),
    }
}

fn has_unfinished_adaptation(
    directory: &Path,
    previous_task: Option<&crate::TaskState>,
) -> Result<bool, CoreError> {
    if previous_task
        .is_some_and(|task| task.operation == "recognize" && task.succeeded == Some(true))
        && directory.join("extraction/native.json").is_file()
    {
        return Ok(false);
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct SavedStatus {
        #[serde(default)]
        last_status: Option<gamel10n_engines::AdaptationStatus>,
    }
    let saved = match read_json::<SavedStatus>(&directory.join("adaptation/current/session.json")) {
        Ok(saved) => saved,
        Err(gamel10n_translation::files::FileError::Io { source, .. })
            if source.kind() == std::io::ErrorKind::NotFound =>
        {
            return Ok(false);
        }
        // Let the adaptation recovery path diagnose the existing session and retain its files.
        Err(gamel10n_translation::files::FileError::Json { source, .. }) if !source.is_io() => {
            return Ok(true);
        }
        Err(error) => return Err(error.into()),
    };
    Ok(!matches!(
        saved.last_status,
        Some(
            gamel10n_engines::AdaptationStatus::Activated
                | gamel10n_engines::AdaptationStatus::Reused
        )
    ) || previous_task
        .is_some_and(|task| task.operation == "adapt" && task.succeeded != Some(true)))
}

fn resolve_adaptation_objective(
    explicit: Option<AdaptationObjective>,
    saved: Option<AdaptationObjective>,
    improving: bool,
    instruction: &str,
    has_error: bool,
) -> AdaptationObjective {
    explicit.or(saved).unwrap_or(if improving {
        if !instruction.trim().is_empty() || has_error {
            AdaptationObjective::Repair
        } else {
            AdaptationObjective::ExpandCoverage
        }
    } else {
        AdaptationObjective::Develop
    })
}

fn adaptation_task_outcome(result: &AdaptationResult) -> Result<String, String> {
    if matches!(
        result.status,
        gamel10n_engines::AdaptationStatus::Failed
            | gamel10n_engines::AdaptationStatus::Unavailable
    ) {
        let reason = result
            .failure
            .as_ref()
            .map(|failure| failure.message.clone())
            .unwrap_or_else(|| {
                if result.diagnostics.is_empty() {
                    result.summary.clone()
                } else {
                    result.diagnostics.join("\n")
                }
            });
        return Err(format!("{reason}\n{}", result.next_action));
    }
    Ok(format!("{}\n{}", result.summary, result.next_action))
}

impl Application {
    pub fn engine_service(&self) -> EngineService {
        EngineService::new(self.tools.asset_root(), self.tools.clone())
            .with_shared_directory(self.data_directory().join("extensions/engine-tools"))
    }
    pub async fn package_directories(&self) -> Result<Vec<PathBuf>, CoreError> {
        let root = self.data_directory().to_path_buf();
        tokio::task::spawn_blocking(move || -> Result<Vec<PathBuf>, CoreError> {
            EngineCatalog::open(&root)?.candidate_package_directories()
        })
        .await?
    }
    pub async fn analysis_provider(
        &self,
        id: &str,
    ) -> Result<(OpenAiProvider, AdaptationLimits), CoreError> {
        let settings = self.settings().await?;
        let project = self.store.get(id).await?;
        let profile_id = project
            .settings
            .get("analysisProfileId")
            .and_then(Value::as_str)
            .or(settings.api_configuration.analysis_profile_id.as_deref())
            .ok_or_else(|| CoreError::InvalidData("请先选择分析 API。".into()))?;
        let p = settings
            .api_configuration
            .profiles
            .iter()
            .find(|p| p.id == profile_id)
            .ok_or_else(|| CoreError::InvalidData("分析 API 配置不存在。".into()))?;
        let provider = OpenAiProvider::new(ProviderSettings {
            endpoint: p.endpoint.clone(),
            api_key: self.decrypt_key(p).await?,
            model: p.model.clone(),
            protocol: if p.protocol == "Responses" {
                ApiProtocol::Responses
            } else {
                ApiProtocol::ChatCompletions
            },
            translation_format: TranslationFormat::JsonBatch,
            analysis_reasoning_effort: p.analysis_reasoning_effort.clone(),
        })
        .map_err(|e| CoreError::InvalidData(e.to_string()))?
        .with_display_name(p.name.clone());
        let limits = AdaptationLimits {
            maximum_tokens: project
                .settings
                .get("adaptationTokenBudget")
                .and_then(Value::as_u64)
                .unwrap_or(settings.adaptation_token_budget),
            maximum_turns: project
                .settings
                .get("adaptationMaximumTurns")
                .and_then(Value::as_u64)
                .unwrap_or(settings.adaptation_maximum_turns.into())
                as u32,
            ..AdaptationLimits::default()
        };
        Ok((provider, limits))
    }
    pub async fn recognize(&self, id: &str) -> Result<RecognitionResult, CoreError> {
        self.recognize_with_instruction(id, String::new()).await
    }

    /// Resume unfinished adaptation; otherwise try existing adapters before intelligent adaptation.
    pub async fn recognize_with_instruction(
        &self,
        id: &str,
        instruction: String,
    ) -> Result<RecognitionResult, CoreError> {
        self.recognize_with_mode(id, instruction, RecognitionMode::Automatic)
            .await
    }

    pub async fn recognize_with_mode(
        &self,
        id: &str,
        instruction: String,
        mode: RecognitionMode,
    ) -> Result<RecognitionResult, CoreError> {
        let forced = mode == RecognitionMode::Intelligent;
        let continuing = if forced {
            false
        } else {
            self.open_project(id).await?;
            let directory = self.project_directory(id)?;
            let task = self.task_state(id).await;
            tokio::task::spawn_blocking(move || {
                has_unfinished_adaptation(&directory, task.as_ref())
            })
            .await??
        };
        let recover_missing = if continuing
            && !self
                .project_directory(id)?
                .join("extraction/native.json")
                .is_file()
        {
            let root = self.data_directory().to_owned();
            let game_id = id.to_owned();
            let known = tokio::task::spawn_blocking(move || -> Result<bool, CoreError> {
                Ok(EngineCatalog::open(&root)?
                    .discovery_candidates_for_game(&game_id, None)?
                    .iter()
                    .any(|candidate| candidate.is_known_game))
            })
            .await??;
            known && self.cached_analysis(id).await?.is_none()
        } else {
            false
        };
        let mut operation = if forced || (continuing && !recover_missing) {
            "adapt"
        } else {
            "recognize"
        };
        let cancellation = self
            .begin(
                id,
                operation,
                if recover_missing {
                    "正在常规适配：优先找回此游戏已有的适配器并重新提取文本。"
                } else if continuing {
                    "正在智能适配：恢复上次中断的会话和已有进度。"
                } else if forced {
                    "正在智能适配：跳过常规匹配，准备分析游戏资源。"
                } else {
                    "正在常规适配：扫描游戏目录并筛选已有适配器。"
                },
            )
            .await?;
        self.metrics_details(
            id,
            Some("recognize"),
            continuing.then_some("continued"),
            None,
        )
        .await;
        let result = async {
            if continuing {
                if recover_missing {
                    let recovered = self.recognize_existing(id, &cancellation, true).await?;
                    if let Some(count) = recovered.extracted.filter(|count| *count > 0) {
                        self.metrics_details(id, None, Some("reused"), None).await;
                        return Ok(RecognitionResult::Extracted(count));
                    }
                    operation = "adapt";
                    self.progress(id, operation, "本地复用未恢复文本，正在续接上次智能适配。")
                        .await;
                }
                let result = self.run_adaptation(
                    id,
                    instruction,
                    AdaptationStart {
                        objective: self.recognition_objective(id).await?,
                        inventory: None,
                        require_model_analysis: false,
                    },
                    &cancellation,
                ).await?;
                return Ok(RecognitionResult::Adapted(Box::new(result)));
            }
            if forced {
                let instruction = format!("本次选择强制智能识别。请核对实际游戏的引擎和主要文本来源；已有常规匹配、源码和经验仅作参考，不要仅因解析器能提取部分文本就认定已完成。复用仍有效的调查与实现，避免重复工作。\n\n{instruction}");
                let result = self.run_adaptation(
                    id,
                    instruction,
                    AdaptationStart {
                        objective: AdaptationObjective::Develop,
                        inventory: None,
                        require_model_analysis: true,
                    },
                    &cancellation,
                ).await?;
                return Ok(RecognitionResult::Adapted(Box::new(result)));
            }
            let recognized = self.recognize_existing(id, &cancellation, false).await?;
            let extracted = recognized.extracted;
            let inventory = recognized.inventory;
            if cancellation.is_cancelled() {
                return Err(gamel10n_engines::EngineError::Cancelled.into());
            }
            if let Some(count) = extracted.filter(|count| *count > 0 && (*count >= SMALL_EXTRACTION_THRESHOLD || recognized.known_game)) {
                if recognized.known_game { self.metrics_details(id, None, Some("reused"), None).await; }
                return Ok(RecognitionResult::Extracted(count));
            }
            operation = "adapt";
            let (message, instruction, objective) = if let Some(count) = extracted {
                let message = format!("常规适配仅提取 {count} 条文本，已保留结果；正在智能检查文字覆盖，确认是否遗漏主要脚本。");
                let instruction = format!("常规适配仅提取 {count} 条文本（少于 {SMALL_EXTRACTION_THRESHOLD} 条），尚不能确认文字覆盖。请核对已匹配解析器、提取来源和实际游戏资源；不要把说明文件、辅助文本或少量可解析文本误认为主要脚本已提取完成。数量只是检查线索；若确为小体量游戏，可根据资源证据确认，无需凑足文本数量。已有结果可用 inspect_existing_text 查看。\n\n{instruction}");
                (message, instruction, AdaptationObjective::ExpandCoverage)
            } else {
                ("常规适配未找到可用适配器，正在转入智能适配。".into(), instruction, AdaptationObjective::Develop)
            };
            self.progress(
                id,
                operation,
                &message,
            )
            .await;
            let result = self
                .run_adaptation(
                    id,
                    instruction,
                    AdaptationStart {
                        objective,
                        inventory: Some(inventory),
                        require_model_analysis: extracted.is_some(),
                    },
                    &cancellation,
                )
                .await?;
            Ok(RecognitionResult::Adapted(Box::new(result)))
        }
        .await;
        self.finish(
            id,
            operation,
            result
                .as_ref()
                .map_err(ToString::to_string)
                .and_then(RecognitionResult::task_outcome),
        )
        .await?;
        result
    }

    async fn recognize_existing(
        &self,
        id: &str,
        cancellation: &CancellationToken,
        known_only: bool,
    ) -> Result<ExistingRecognition, CoreError> {
        let project = self.store.get(id).await?;
        let root = self.data_directory().to_path_buf();
        let game = crate::game_directory::validate(&project.game_directory)?;
        let game_id = id.to_owned();
        let pin = catalog_adapter(&project).map(|adapter| (adapter.adapter_id, adapter.version));
        let (known_packages, packages, inventory, disabled, preferred_adapter, known_versions) =
            tokio::task::spawn_blocking(move || -> Result<_, CoreError> {
                let catalog = EngineCatalog::open(&root)?;
                let inventory = gamel10n_engines::discovery::Inventory::scan(&game)?;
                let mut candidates = catalog.discovery_candidates_for_game(
                    &game_id,
                    pin.as_ref()
                        .map(|(id, version)| (id.as_str(), version.as_str())),
                )?;
                if known_only {
                    candidates.retain(|candidate| candidate.is_known_game);
                }
                let preferred_adapter = pin.as_ref().map(|(id, _)| id.clone()).or_else(|| {
                    candidates
                        .iter()
                        .find(|candidate| candidate.is_known_game)
                        .map(|candidate| candidate.adapter_id.clone())
                });
                let known_versions = candidates
                    .iter()
                    .filter(|candidate| candidate.is_known_game)
                    .map(|candidate| (candidate.adapter_id.clone(), candidate.version.clone()))
                    .collect::<BTreeSet<_>>();
                let mut known_packages = Vec::new();
                let mut packages = Vec::new();
                for candidate in candidates {
                    if candidate.matches_inventory(&inventory) {
                        if candidate.is_known_game {
                            known_packages.push(candidate.directory);
                        } else {
                            packages.push(candidate.directory);
                        }
                    }
                }
                let mut disabled = catalog.disabled_family_ids()?;
                if let Some((id, _)) = pin {
                    disabled.remove(&id);
                }
                Ok((
                    known_packages,
                    packages,
                    inventory,
                    disabled,
                    preferred_adapter,
                    known_versions,
                ))
            })
            .await??;
        self.progress(
            id,
            "recognize",
            &format!(
                "已扫描 {} 个文件，筛选出 {} 个扩展适配器候选；接下来检查候选与内置解析器。",
                inventory.files.len(),
                known_packages.len() + packages.len()
            ),
        )
        .await;
        let mut excluded = BTreeSet::new();
        let mut reuse_failures = Vec::new();
        let mut builtin_probes = BTreeMap::new();
        let mut diagnostics = Vec::new();
        let mut known_phase = known_only || !known_packages.is_empty();
        loop {
            if cancellation.is_cancelled() {
                return Err(gamel10n_engines::EngineError::Cancelled.into());
            }
            let mut service = self
                .engine_service()
                .with_disabled_families(disabled.clone())
                .with_preferred_adapter(preferred_adapter.clone())
                .with_excluded_adapters(excluded.clone());
            if known_phase {
                service = service.with_ordered_packages_only();
            }
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<EngineProgress>();
            let report = move |progress| {
                let _ = tx.send(progress);
            };
            let discovered = {
                let discovery = service.discover_with_inventory(
                    &project.game_directory,
                    if known_phase {
                        &known_packages
                    } else {
                        &packages
                    },
                    &inventory,
                    cancellation,
                    Some(&report),
                );
                tokio::pin!(discovery);
                loop {
                    tokio::select! {
                        biased;
                        Some(progress) = rx.recv() => {
                            self.progress(id, "recognize", &progress.message).await;
                        }
                        result = &mut discovery => break result,
                    }
                }
            };
            while let Ok(progress) = rx.try_recv() {
                self.progress(id, "recognize", &progress.message).await;
            }
            let discovered = discovered?;
            diagnostics.extend(
                discovered
                    .diagnostics
                    .into_iter()
                    .map(|message| message.chars().take(2000).collect::<String>()),
            );
            diagnostics.truncate(12);
            reuse_failures.extend(discovered.reuse_failures);
            for observation in discovered.builtin_probes {
                builtin_probes.insert(observation.format.clone(), observation);
            }
            let mut retry = false;
            let mut extracted = None;
            let mut known_game = false;
            if let Some(adapter) = discovered.adapter {
                let probe = discovered
                    .probe
                    .ok_or_else(|| CoreError::InvalidData("适配器没有返回资源清单。".into()))?;
                let (adapter_id, version, source_directory) = match &adapter {
                    AdapterHandle::Python {
                        id,
                        version,
                        package_directory,
                        ..
                    } => (
                        id.clone(),
                        version.clone(),
                        package_directory.as_ref().map(|path| path.join("src")),
                    ),
                    _ => (
                        probe.adapter_id.clone(),
                        probe.adapter_version.clone(),
                        None,
                    ),
                };
                match self
                    .extract_adapter(id, adapter, probe, cancellation, false, None, None, None)
                    .await
                {
                    Ok(count) => {
                        extracted = Some(count);
                        known_game = known_versions.contains(&(adapter_id, version));
                    }
                    Err(CoreError::Engine(gamel10n_engines::EngineError::Incompatible {
                        stage,
                        message,
                    })) => {
                        if !excluded.insert((adapter_id.clone(), version.clone())) {
                            return Err(CoreError::InvalidData(
                                "候选适配器未被正确排除，识别已停止。".into(),
                            ));
                        }
                        self.progress(
                            id,
                            "recognize",
                            &format!("适配器 {adapter_id} {version} 不兼容：{message}；继续检查其他候选。"),
                        )
                        .await;
                        reuse_failures.push(GeneratedAdapterReuseFailure {
                            id: adapter_id,
                            version,
                            failed_stage: stage.into(),
                            evidence: vec![message],
                            source_directory,
                            original_roundtrip_passed: false,
                            input_fingerprint: None,
                        });
                        retry = true;
                    }
                    // Infrastructure failures and errors after committing an extraction must not
                    // spend analysis tokens or replace an otherwise usable adapter.
                    Err(error) => return Err(error),
                }
            }
            reuse_failures.truncate(6);
            let mut current = self.store.get(id).await?;
            current.extra.insert(
                "discovery".into(),
                json!({
                    "reuseFailures": reuse_failures,
                    "builtinProbes": builtin_probes.values().collect::<Vec<_>>(),
                    "diagnostics": diagnostics,
                }),
            );
            self.store.save(&current).await?;
            if !retry {
                if known_phase && !known_only && extracted.is_none() {
                    known_phase = false;
                    continue;
                }
                return Ok(ExistingRecognition {
                    extracted,
                    inventory,
                    known_game,
                });
            }
        }
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "Extraction binds an optional current verification to project inputs and catalog registration."
    )]
    async fn extract_adapter(
        &self,
        id: &str,
        adapter: AdapterHandle,
        probe: ProbeResult,
        cancellation: &CancellationToken,
        trial: bool,
        replaces: Option<&AdapterVersion>,
        verified_extraction: Option<Arc<VerifiedExtraction>>,
        inputs: Option<&AdaptationInputs>,
    ) -> Result<usize, CoreError> {
        let project = self.store.get(id).await?;
        let directory = self.project_directory(id)?;
        let files = probe.required_files.clone();
        if files.is_empty() {
            return Err(CoreError::InvalidData("适配器没有声明必需资源。".into()));
        }
        let operation = self
            .task_state(id)
            .await
            .map(|state| state.operation)
            .unwrap_or_else(|| "extract".into());
        self.progress(id, &operation, "正在保存原始资源快照。")
            .await;
        let target = directory.clone();
        let game = project.game_directory.clone();
        let resources = files.clone();
        let token = cancellation.clone();
        let inputs = inputs.cloned();
        let (source, hashes) = self
            .run_file_task(id, &operation, move |progress| {
                snapshot(
                    &target,
                    &game,
                    &resources,
                    inputs.as_ref(),
                    &token,
                    progress,
                )
            })
            .await?;
        if cancellation.is_cancelled() {
            return Err(gamel10n_engines::EngineError::Cancelled.into());
        }
        let verified = verified_extraction
            .map(|verified| verified.into_matching_extraction(&adapter, &hashes))
            .transpose()?
            .flatten();
        let mut extraction = if let Some(extraction) = verified {
            self.progress(id, &operation, "资源与本次验收一致，正在保存已验证的文本。")
                .await;
            extraction
        } else {
            let source_guard = SourceReadOnlyGuard::new(&directory, &source, &hashes)?;
            let mut request = AdapterRequest::new("extract", &source);
            request.resource_files = Some(files.clone());
            request.set_probe_context(&probe);
            self.progress(id, &operation, "正在提取并校验文本身份。")
                .await;
            let service = self.engine_service();
            let mut extraction = service
                .execute(&adapter, &request, cancellation)
                .await?
                .extraction
                .ok_or_else(|| gamel10n_engines::EngineError::Incompatible {
                    stage: "extraction",
                    message: "适配器未返回提取结果。".into(),
                })?;
            extraction
                .units
                .retain(|unit| !unit.source_text.trim().is_empty());
            validate_units(&extraction.units).map_err(|error| {
                gamel10n_engines::EngineError::Incompatible {
                    stage: "extraction",
                    message: error.to_string(),
                }
            })?;
            self.progress(id, &operation, "正在验证原文字节回填。")
                .await;
            let roundtrip = tempfile::tempdir()?;
            request.operation = "build".into();
            request.output_directory = Some(roundtrip.path().to_path_buf());
            let built = service
                .execute(&adapter, &request, cancellation)
                .await?
                .build
                .ok_or_else(|| CoreError::InvalidData("适配器未返回原文回填结果。".into()))?;
            if built.mode == BuildMode::Overlay && !built.files.is_empty() {
                let mut readback = built.extraction_request(&source, roundtrip.path(), &files);
                readback.set_probe_context(&probe);
                let actual = service
                    .execute(&adapter, &readback, cancellation)
                    .await?
                    .extraction
                    .ok_or_else(|| CoreError::InvalidData("覆盖包未返回回读文本。".into()))?;
                let by_id: BTreeMap<_, _> = actual
                    .units
                    .iter()
                    .filter(|unit| !unit.source_text.trim().is_empty())
                    .map(|unit| (unit.id.as_str(), unit))
                    .collect();
                if by_id.len() != extraction.units.len()
                    || extraction
                        .units
                        .iter()
                        .any(|unit| by_id.get(unit.id.as_str()).copied() != Some(unit))
                {
                    return Err(gamel10n_engines::EngineError::Incompatible {
                        stage: "original-roundtrip",
                        message: "原文覆盖包改变了文本内容或身份。".into(),
                    }
                    .into());
                }
                request.operation = "validate".into();
                built.set_output_context(&mut request);
                let validation = service
                    .execute(&adapter, &request, cancellation)
                    .await?
                    .validation
                    .ok_or_else(|| CoreError::InvalidData("覆盖包未返回结构验证结果。".into()))?;
                if !validation.passed || !validation.issues.is_empty() {
                    return Err(gamel10n_engines::EngineError::Incompatible {
                        stage: "original-roundtrip",
                        message: "原文覆盖包未通过结构验证。".into(),
                    }
                    .into());
                }
            }
            for (file, expected) in &hashes {
                let rebuilt = resolve_relative(roundtrip.path(), file)?;
                // Adapters may omit untouched files; every emitted source must round-trip exactly.
                if rebuilt.is_file() && hash_file(&rebuilt)? != *expected {
                    return Err(gamel10n_engines::EngineError::Incompatible {
                        stage: "original-roundtrip",
                        message: format!("原文回填改变资源：{file}"),
                    }
                    .into());
                }
                if hash_file(&resolve_relative(&source, file)?)? != *expected {
                    return Err(CoreError::InvalidData(format!(
                        "适配器修改了原文快照：{file}"
                    )));
                }
            }
            source_guard.restore()?;
            extraction
        };
        for warning in &probe.warnings {
            if !extraction.warnings.contains(warning) {
                extraction.warnings.push(warning.clone());
            }
        }
        let count = extraction.units.len();
        let stored = StoredAnalysis {
            schema_version: 1,
            format: adapter.id().into(),
            source_directory: source,
            resource_files: files,
            source_hashes: hashes,
            extraction,
            adapter: Some(adapter.clone()),
            probe: Some(probe.clone()),
        };
        self.commit_analysis(id, stored).await?;
        if let AdapterHandle::Python {
            package_directory: Some(package),
            ..
        } = &adapter
        {
            let catalog = EngineCatalog::open(self.data_directory())?;
            let compatibility = GameCompatibility {
                game_id: id.into(),
                title: project.name,
                variant: probe.engine_id.clone(),
                adapter_version: probe.adapter_version.clone(),
                structure_status: "passed".into(),
                runtime_status: "not-tested".into(),
                updated_at: Some(chrono::Utc::now().to_rfc3339()),
            };
            if trial {
                catalog.register_trial_package(package, compatibility, replaces)?;
            } else {
                catalog.register_package(package, Some(compatibility))?;
            }
        }
        for warning in &probe.warnings {
            self.log_task_message(id, warning).await;
        }
        Ok(count)
    }

    pub async fn adapt_project(
        &self,
        id: &str,
        instruction: String,
    ) -> Result<AdaptationResult, CoreError> {
        self.adapt_project_with_objective(id, instruction, None)
            .await
    }

    pub async fn adaptation_objective(
        &self,
        id: &str,
        instruction: &str,
    ) -> Result<AdaptationObjective, CoreError> {
        let project = self.store.get(id).await?;
        let saved = saved_adaptation_objective(&project, &self.project_directory(id)?)?;
        let has_error = self
            .task_state(id)
            .await
            .is_some_and(|task| task.succeeded == Some(false));
        Ok(resolve_adaptation_objective(
            None,
            saved,
            improving_existing(&project),
            instruction,
            has_error,
        ))
    }

    /// Keep the last explicit goal when continuing; new projects start with regular recognition.
    pub async fn recognition_objective(&self, id: &str) -> Result<AdaptationObjective, CoreError> {
        let project = self.store.get(id).await?;
        Ok(
            saved_adaptation_objective(&project, &self.project_directory(id)?)?
                .unwrap_or(AdaptationObjective::Develop),
        )
    }

    pub async fn adapt_project_with_objective(
        &self,
        id: &str,
        instruction: String,
        objective: Option<AdaptationObjective>,
    ) -> Result<AdaptationResult, CoreError> {
        let objective = match objective {
            Some(objective) => objective,
            None => self.adaptation_objective(id, &instruction).await?,
        };
        let cancellation = self
            .begin(id, "adapt", "正在智能适配：准备分析环境和已有进度。")
            .await?;
        let result = self
            .run_adaptation(
                id,
                instruction,
                AdaptationStart {
                    objective,
                    inventory: None,
                    require_model_analysis: false,
                },
                &cancellation,
            )
            .await;
        self.finish(
            id,
            "adapt",
            result
                .as_ref()
                .map_err(ToString::to_string)
                .and_then(adaptation_task_outcome),
        )
        .await?;
        result
    }

    async fn run_adaptation(
        &self,
        id: &str,
        instruction: String,
        start: AdaptationStart,
        cancellation: &CancellationToken,
    ) -> Result<AdaptationResult, CoreError> {
        if cancellation.is_cancelled() {
            return Err(gamel10n_engines::EngineError::Cancelled.into());
        }
        let mut project = self.store.get(id).await?;
        crate::game_directory::validate(&project.game_directory)?;
        // Validate configuration before creating an adaptation workspace or changing its state.
        let (provider, limits) = self.analysis_provider(id).await?;
        let objective = start.objective;
        self.metrics_provider(id, &provider).await;
        let task = match objective {
            AdaptationObjective::Repair => Some("repair"),
            AdaptationObjective::ExpandCoverage => Some("expand"),
            AdaptationObjective::Develop => None,
        };
        let continuing = self
            .project_directory(id)?
            .join("adaptation/current/session.json")
            .is_file();
        self.metrics_details(id, task, continuing.then_some("continued"), None)
            .await;
        let improve_existing = improving_existing(&project);
        let instruction =
            if objective == AdaptationObjective::Repair && instruction.trim().is_empty() {
                project
                    .extra
                    .get("adapterRepairNotes")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string()
            } else {
                instruction
            };
        let baseline = if objective == AdaptationObjective::Repair
            || (start.require_model_analysis && objective == AdaptationObjective::ExpandCoverage)
        {
            self.cached_analysis(id).await?
        } else {
            None
        };
        let directory = self.project_directory(id)?;
        let target = directory.clone();
        let game = project.game_directory.clone();
        let sources = baseline.clone();
        let token = cancellation.clone();
        let inputs = self
            .run_file_task(id, "adapt", move |progress| {
                let baseline = sources
                    .as_ref()
                    .map(|value| {
                        value.native().map_err(|_| {
                            CoreError::InvalidData(
                                "旧版提取结果没有可校验的资源快照，无法绑定本次适配输入。".into(),
                            )
                        })
                    })
                    .transpose()?;
                adaptation_inputs(&target, &game, baseline, &token, Some(progress))
            })
            .await?;
        let previous = if objective == AdaptationObjective::Repair {
            self.repair_predecessor(id).await?
        } else {
            None
        };
        if cancellation.is_cancelled() {
            return Err(gamel10n_engines::EngineError::Cancelled.into());
        }
        project
            .extra
            .insert("adaptationObjective".into(), json!(objective));
        self.store.save(&project).await?;
        let api = self.task_api_summary(id).await?;
        self.log_task_message(id, api.lines().next().unwrap_or("分析 API 已选择。"))
            .await;
        let discovery = project.extra.get("discovery");
        let reuse_failures = discovery
            .and_then(|value| value.get("reuseFailures"))
            .filter(|value| value.is_array())
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .map_err(|error| CoreError::InvalidData(format!("适配器复用记录无效：{error}")))?
            .unwrap_or_default();
        let builtin_probes = discovery
            .and_then(|value| value.get("builtinProbes"))
            .filter(|value| value.is_array())
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .map_err(|error| CoreError::InvalidData(format!("内置解析器探测记录无效：{error}")))?
            .unwrap_or_default();
        let current = catalog_adapter(&project);
        let catalog = EngineCatalog::open(self.data_directory())?;
        let packages = catalog
            .discovery_candidates(
                current
                    .as_ref()
                    .map(|value| (value.adapter_id.as_str(), value.version.as_str())),
            )?
            .into_iter()
            .map(|candidate| candidate.directory)
            .collect();
        let request = AdaptationRequest {
            game_id: id.into(),
            game_directory: project.game_directory.clone(),
            project_directory: directory,
            inputs,
            package_directory: self.data_directory().join("extensions"),
            package_directories: packages,
            user_instruction: instruction,
            objective: objective.as_str().into(),
            improve_existing,
            require_model_analysis: start.require_model_analysis,
            reuse_failures,
            builtin_probes,
            discovery_inventory: start.inventory,
            limits,
            baseline_units: baseline
                .as_ref()
                .map(|value| value.extraction().units.as_slice())
                .unwrap_or_default(),
        };
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<EngineProgress>();
        let report = move |progress| {
            let _ = tx.send(progress);
        };
        let service = self
            .engine_service()
            .with_disabled_families(catalog.disabled_family_ids()?);
        let future = service.adapt(&request, &provider, cancellation, Some(&report));
        tokio::pin!(future);
        let result = loop {
            tokio::select! {
                biased;
                Some(progress) = rx.recv() => self.engine_progress(id, "adapt", progress).await?,
                result = &mut future => break result,
            }
        };
        while let Ok(progress) = rx.try_recv() {
            self.engine_progress(id, "adapt", progress).await?;
        }
        let mut result = result?;
        let status = match result.status {
            gamel10n_engines::AdaptationStatus::Activated
            | gamel10n_engines::AdaptationStatus::Reused => "completed",
            gamel10n_engines::AdaptationStatus::Failed
            | gamel10n_engines::AdaptationStatus::Unavailable => "failed",
            gamel10n_engines::AdaptationStatus::Unsupported => "unsupported",
            _ => "paused",
        };
        self.metrics_details(
            id,
            None,
            (result.status == gamel10n_engines::AdaptationStatus::Reused).then_some("reused"),
            Some(status),
        )
        .await;
        write_json(
            &self.project_directory(id)?.join("adaptation-result.json"),
            &result,
        )?;
        if let Some(adapter) = &result.adapter {
            let probe = if let AdapterHandle::Python {
                package_directory: Some(package),
                ..
            } = adapter
            {
                let manifest = PackageManifest::load(package)?;
                let inventory = request
                    .inputs
                    .inventory(Inventory::scan(&project.game_directory)?)?;
                service
                    .probe_bounded_with_inputs(
                        adapter,
                        &project.game_directory,
                        &manifest.resources(&inventory),
                        &request.inputs,
                        cancellation,
                    )
                    .await?
            } else if request.inputs.files.is_empty() && request.inputs.excluded.is_empty() {
                service
                    .execute(
                        adapter,
                        &AdapterRequest::new("probe", &project.game_directory),
                        cancellation,
                    )
                    .await?
                    .probe
            } else {
                return Err(CoreError::InvalidData(
                    "该适配器无法对已绑定的制作输入执行探测。".into(),
                ));
            }
            .ok_or_else(|| CoreError::InvalidData("已发布适配器未匹配当前游戏。".into()))?;
            let trial = result.status == gamel10n_engines::AdaptationStatus::Activated;
            self.extract_adapter(
                id,
                adapter.clone(),
                probe,
                cancellation,
                trial,
                previous.as_ref(),
                result.verified_extraction.take(),
                Some(&request.inputs),
            )
            .await?;
            if trial && let Some(previous) = &previous {
                self.remember_adapter_replacement(id, previous).await?;
            }
        }
        Ok(result)
    }

    async fn engine_progress(
        &self,
        id: &str,
        operation: &str,
        progress: EngineProgress,
    ) -> Result<(), CoreError> {
        self.checkpoint_metrics(id).await;
        if progress.status_only {
            self.update_task_progress(id, operation, &progress.message)
                .await;
        } else {
            self.progress(id, operation, &progress.message).await;
        }
        let mut snapshot = json!(&progress);
        snapshot["operation"] = json!(operation);
        write_json(
            &self.project_directory(id)?.join("adaptation-progress.json"),
            &snapshot,
        )?;
        let _ = self.events.send(crate::AppEvent::EngineProgress {
            project_id: id.into(),
            progress,
        });
        Ok(())
    }

    pub async fn review_engine(
        &self,
        id: &str,
    ) -> Result<gamel10n_engines::review::EngineReviewResult, CoreError> {
        if !self
            .project_directory(id)?
            .join("adaptation/current/session.json")
            .is_file()
        {
            return Err(CoreError::InvalidData(
                "此项目没有可用于总结的适配记录，或相关工作文件已清理。已有经验总结仍可查看。"
                    .into(),
            ));
        }
        let (provider, limits) = self.analysis_provider(id).await?;
        let cancellation = self
            .begin(id, "review", "正在总结可供同类引擎复用的经验。")
            .await?;
        self.metrics_provider(id, &provider).await;
        let result=async {
            let (tx,mut rx)=tokio::sync::mpsc::unbounded_channel::<EngineProgress>();let report=move|p|{let _=tx.send(p);};let service=self.engine_service();let directory=self.project_directory(id)?;let knowledge=self.data_directory().join("extensions/engine-knowledge");
            let api = self.task_api_summary(id).await?;
            self.log_task_message(id, api.lines().next().unwrap_or("分析 API 已选择。")).await;
            let future=service.review(&directory,&knowledge,&provider,&limits,&cancellation,Some(&report));tokio::pin!(future);
            let result=loop {tokio::select! {result=&mut future=>break result?,Some(progress)=rx.recv()=>{self.engine_progress(id,"review",progress).await?;}}};
            while let Ok(progress)=rx.try_recv(){self.engine_progress(id,"review",progress).await?;}
            if result.reused { self.metrics_details(id, None, Some("reused"), None).await; }
            write_json(&directory.join("engine-review-result.json"),&result)?;
            if let Some(entry) = &result.entry_id { EngineCatalog::open(self.data_directory())?.register_knowledge(entry)?; }
            Ok::<_,CoreError>(result)
        }.await;
        self.finish(
            id,
            "review",
            result
                .as_ref()
                .map(|r| r.summary.clone())
                .map_err(ToString::to_string),
        )
        .await?;
        result
    }

    async fn build_source(
        &self,
        id: &str,
    ) -> Result<
        (
            AdapterHandle,
            PathBuf,
            Vec<String>,
            BTreeMap<String, String>,
            Option<ProbeResult>,
        ),
        CoreError,
    > {
        let cached = self
            .cached_analysis(id)
            .await?
            .ok_or_else(|| CoreError::InvalidData("请先提取游戏文本。".into()))?;
        if let Ok(stored) = cached.native() {
            crate::cleanup::require_source_materials(&stored.source_directory)?;
            return Ok((
                stored
                    .adapter
                    .clone()
                    .unwrap_or_else(|| AdapterHandle::Native {
                        format: stored.format.clone(),
                    }),
                stored.source_directory.clone(),
                stored.resource_files.clone(),
                stored.source_hashes.clone(),
                stored.probe.clone(),
            ));
        }
        let root = self.project_directory(id)?;
        let record: Value = read_json(&root.join("source.json"))?;
        let directory = resolve_relative(
            &root,
            record
                .get("directory")
                .and_then(Value::as_str)
                .ok_or_else(|| CoreError::InvalidData("原文快照记录缺少目录。".into()))?,
        )?;
        crate::cleanup::require_source_materials(&directory)?;
        let probe: ProbeResult = serde_json::from_value(
            record
                .get("probe")
                .cloned()
                .ok_or_else(|| CoreError::InvalidData("缺少保存的资源探测记录。".into()))?,
        )
        .map_err(|e| CoreError::InvalidData(e.to_string()))?;
        let package = resolve_relative(
            &self.data_directory().join("extensions/packages"),
            &format!("{}/{}", probe.adapter_id, probe.adapter_version),
        )?;
        let adapter = if package.is_dir() {
            let (manifest, runtime) = PackageManifest::load_runtime(&package)?;
            manifest.handle(&runtime)?
        } else if let Some(format) = probe
            .adapter_id
            .strip_prefix("vntextpatch.")
            .or_else(|| probe.adapter_id.strip_prefix("engine.vntextpatch."))
            .or_else(|| probe.adapter_id.strip_prefix("engine.vntextpatch-"))
        {
            AdapterHandle::Native {
                format: gamel10n_engines::vntextpatch::FORMATS
                    .iter()
                    .find(|candidate| candidate.eq_ignore_ascii_case(format))
                    .ok_or_else(|| CoreError::InvalidData("原文记录的内置脚本格式未知。".into()))?
                    .to_string(),
            }
        } else if probe.adapter_id == "engine.siglus" {
            AdapterHandle::BuiltinSiglus
        } else if probe.adapter_id == "engine.sfa16.aos-v2" {
            AdapterHandle::BuiltinSfa16
        } else if probe.adapter_id == "engine.kirikiri-smil-smile" {
            AdapterHandle::BuiltinKirikiriSmil
        } else {
            return Err(CoreError::InvalidData(format!(
                "适配器 {} {} 不存在或无法执行；已保存译文可继续校对和翻译。",
                probe.adapter_id, probe.adapter_version
            )));
        };
        let mut hashes = BTreeMap::new();
        for item in record
            .get("files")
            .and_then(Value::as_array)
            .ok_or_else(|| CoreError::InvalidData("原文快照记录缺少文件。".into()))?
        {
            let path = item
                .get("relativePath")
                .and_then(Value::as_str)
                .ok_or_else(|| CoreError::InvalidData("无效的资源记录。".into()))?;
            let hash = item
                .get("sha256")
                .and_then(Value::as_str)
                .ok_or_else(|| CoreError::InvalidData("资源缺少哈希。".into()))?;
            hashes.insert(path.to_owned(), hash.to_owned());
        }
        Ok((
            adapter,
            directory,
            probe.required_files.clone(),
            hashes,
            Some(probe),
        ))
    }

    pub async fn generate_patch(
        &self,
        id: &str,
        unit_ids: Option<Vec<String>>,
        reviewed_only: bool,
    ) -> Result<PathBuf, CoreError> {
        let cancellation = self.begin(id, "build", "正在检查原始资源和译文。").await?;
        let result = self
            .generate_patch_in_task(id, unit_ids.as_deref(), reviewed_only, &cancellation)
            .await;
        self.finish(
            id,
            "build",
            result
                .as_ref()
                .map(|p| format!("补丁已生成：{}。尚未验证游戏内显示。", p.display()))
                .map_err(ToString::to_string),
        )
        .await?;
        result
    }

    /// Builds under an already running project task, preserving its cancellation token.
    pub(crate) async fn generate_patch_in_task(
        &self,
        id: &str,
        unit_ids: Option<&[String]>,
        reviewed_only: bool,
        cancellation: &CancellationToken,
    ) -> Result<PathBuf, CoreError> {
        if cancellation.is_cancelled() {
            return Err(gamel10n_engines::EngineError::Cancelled.into());
        }
        let translations = self.saved_translations(id, unit_ids, reviewed_only).await?;
        if translations.is_empty() {
            return Err(CoreError::InvalidData(
                "所选范围没有可用于补丁的有效译文。".into(),
            ));
        }
        let (adapter, source, files, hashes, probe) = self.build_source(id).await?;
        let adapter_version = match &adapter {
            AdapterHandle::Python { id, version, .. } => Some(AdapterVersion {
                adapter_id: id.clone(),
                version: version.clone(),
            }),
            _ => probe.as_ref().map(|probe| AdapterVersion {
                adapter_id: probe.adapter_id.clone(),
                version: probe.adapter_version.clone(),
            }),
        };
        for (path, expected) in &hashes {
            if cancellation.is_cancelled() {
                return Err(gamel10n_engines::EngineError::Cancelled.into());
            }
            if hash_file(&resolve_relative(&source, path)?)? != *expected {
                return Err(CoreError::InvalidData(format!("原文快照已变化：{path}")));
            }
        }
        let root = self.project_directory(id)?;
        fs::create_dir_all(root.join("patches"))?;
        let fingerprint = {
            let mut inputs =
                json!({"adapter":adapter,"sources":hashes,"texts":translations,"version":2});
            if let Some(probe) = &probe
                && !probe.build_hints.is_empty()
            {
                inputs["buildHints"] = json!(probe.build_hints);
            }
            hash_text(
                &serde_json::to_string(&inputs)
                    .map_err(|e| CoreError::InvalidData(e.to_string()))?,
            )
        };
        let destination = root.join("patches").join(&fingerprint);
        let result_record = root
            .join("patches")
            .join(format!("{fingerprint}.result.json"));
        if destination.is_dir() && result_record.is_file() {
            let mut previous: PatchResult = read_json(&result_record)?;
            let manifest = crate::patching::read_manifest(&destination)?;
            let payload = resolve_relative(&destination, PAYLOAD_DIRECTORY)?;
            let valid =
                manifest
                    .files
                    .iter()
                    .try_fold(true, |valid, file| -> Result<bool, CoreError> {
                        Ok(valid
                            && hash_file(&resolve_relative(&payload, &file.relative_path)?)?
                                == file.sha256)
                    })?;
            if valid {
                if cancellation.is_cancelled() {
                    return Err(gamel10n_engines::EngineError::Cancelled.into());
                }
                previous
                    .extra
                    .insert("adapterVersionRef".into(), json!(adapter_version));
                write_json(&result_record, &previous)?;
                write_json(&root.join("latest-result.json"), &previous)?;
                return Ok(destination);
            }
            return Err(CoreError::InvalidData(
                "已有补丁内容发生变化，请移出这份补丁后重新生成。".into(),
            ));
        }
        let patch = tempfile::Builder::new()
            .prefix("patch-")
            .tempdir_in(root.join("patches"))?;
        let payload = patch.path().join(PAYLOAD_DIRECTORY);
        fs::create_dir(&payload)?;
        let source_guard = SourceReadOnlyGuard::new(&root, &source, &hashes)?;
        let mut request = AdapterRequest::new("build", &source);
        request.resource_files = Some(files);
        request.translations = Some(translations.clone());
        request.output_directory = Some(payload);
        if let Some(p) = &probe {
            request.set_probe_context(p);
        }
        self.progress(id, "build", "正在回填译文并验证补丁结构。")
            .await;
        let service = self.engine_service();
        let built = service
            .execute(&adapter, &request, cancellation)
            .await?
            .build
            .ok_or_else(|| CoreError::InvalidData("适配器未返回补丁文件清单。".into()))?;
        if built.mode == BuildMode::Overlay && built.files.is_empty() {
            return Err(CoreError::InvalidData(
                "所选译文没有产生需要写入的修改。".into(),
            ));
        }
        request.operation = "validate".into();
        built.set_output_context(&mut request);
        let validation = service
            .execute(&adapter, &request, cancellation)
            .await?
            .validation
            .ok_or_else(|| CoreError::InvalidData("适配器未返回结构验证结果。".into()))?;
        if !validation.passed || !validation.issues.is_empty() {
            return Err(CoreError::InvalidData(format!(
                "补丁结构验证失败：{}",
                validation
                    .issues
                    .iter()
                    .map(|i| i.message.as_str())
                    .collect::<Vec<_>>()
                    .join("；")
            )));
        }
        for (path, expected) in &hashes {
            if hash_file(&resolve_relative(&source, path)?)? != *expected {
                return Err(CoreError::InvalidData(format!(
                    "构建修改了原文快照：{path}"
                )));
            }
        }
        source_guard.restore()?;
        let mut manifest = build_manifest(patch.path(), &hashes)?;
        if built.mode == BuildMode::Overlay {
            manifest.mode = built.mode;
            manifest.required_sources.clone_from(&hashes);
        }
        let declared = built
            .files
            .iter()
            .map(|f| f.replace('\\', "/").to_lowercase())
            .collect::<std::collections::HashSet<_>>();
        manifest
            .files
            .retain(|f| declared.contains(&f.relative_path.replace('\\', "/").to_lowercase()));
        if manifest.files.is_empty() || manifest.files.len() != declared.len() {
            return Err(CoreError::InvalidData(
                "补丁文件与适配器声明不一致。".into(),
            ));
        }
        write_json(&patch.path().join("patch-manifest.json"), &manifest)?;
        let count = self
            .cached_analysis(id)
            .await?
            .map(|a| a.extraction().units.len())
            .unwrap_or(0);
        if destination.exists() {
            return Err(CoreError::InvalidData(
                "同一补丁目录已存在但缺少提交记录，已有文件已保留。".into(),
            ));
        }
        if cancellation.is_cancelled() {
            return Err(gamel10n_engines::EngineError::Cancelled.into());
        }
        let path = destination;
        let result = PatchResult {
            patch_directory: path
                .strip_prefix(&root)
                .map(Path::to_path_buf)
                .unwrap_or_else(|_| path.clone()),
            launch_executable: built.launch_executable,
            total_units: count,
            translated_units: translations.len(),
            is_trial: unit_ids.is_some(),
            validation: serde_json::to_value(validation)
                .map_err(|e| CoreError::InvalidData(e.to_string()))?,
            warnings: built.warnings,
            source_fingerprint: hash_text(
                &serde_json::to_string(&hashes)
                    .map_err(|e| CoreError::InvalidData(e.to_string()))?,
            ),
            extra: BTreeMap::from([("adapterVersionRef".into(), json!(adapter_version))]),
        };
        // The selection is tied to this immutable patch, so later edits do not change its preview.
        write_json(
            &root.join("patches").join(format!(
                "{}.texts.json",
                path.file_name().unwrap_or_default().to_string_lossy()
            )),
            &translations,
        )?;
        write_json(&result_record, &result)?;
        fs::rename(patch.path(), &path)?;
        write_json(&root.join("latest-result.json"), &result)?;
        Ok::<_, CoreError>(path)
    }

    pub async fn latest_patch(&self, id: &str) -> Result<Option<PatchResult>, CoreError> {
        let root = self.project_directory(id)?;
        let path = root.join("latest-result.json");
        if !path.is_file() {
            return Ok(None);
        }
        let mut result: PatchResult = read_json(&path)?;
        if result.patch_directory.is_relative() {
            result.patch_directory =
                resolve_relative(&root, &result.patch_directory.to_string_lossy())?;
        }
        Ok(Some(result))
    }

    pub async fn install_patch(&self, id: &str) -> Result<(), CoreError> {
        let project = self.store.get(id).await?;
        let patch = self
            .latest_patch(id)
            .await?
            .ok_or_else(|| CoreError::InvalidData("尚未生成补丁。".into()))?;
        let root = self.project_directory(id)?;
        let cancellation = self
            .begin(id, "install", "正在核对文件并安装汉化补丁。")
            .await?;
        let result = self
            .run_file_task(id, "install", move |progress| {
                PatchInstaller::new(root)
                    .with_cancellation(cancellation)
                    .with_progress(progress)
                    .install(&project.game_directory, &patch.patch_directory)
            })
            .await;
        self.finish(
            id,
            "install",
            result
                .as_ref()
                .map(|r| {
                    format!(
                        "补丁已安装：写入 {} 个文件，跳过 {} 个相同文件。",
                        r.installed_files, r.unchanged_files
                    )
                })
                .map_err(ToString::to_string),
        )
        .await?;
        result.map(|_| ())
    }
    pub async fn restore_game(&self, id: &str) -> Result<(), CoreError> {
        let project = self.store.get(id).await?;
        let root = self.project_directory(id)?;
        let cancellation = self
            .begin(id, "restore", "正在核对哈希并恢复原版。")
            .await?;
        let result = self
            .run_file_task(id, "restore", move |progress| {
                PatchInstaller::new(root)
                    .with_cancellation(cancellation)
                    .with_progress(progress)
                    .restore(&project.game_directory)
            })
            .await;
        self.finish(
            id,
            "restore",
            result
                .as_ref()
                .map(|r| {
                    format!(
                        "已恢复 {} 个原文件，移除 {} 个补丁文件。{}",
                        r.restored_files,
                        r.removed_files,
                        r.conflicts.join("；")
                    )
                })
                .map_err(ToString::to_string),
        )
        .await?;
        result.map(|_| ())
    }
    pub async fn verify_engine_version(
        &self,
        id: &str,
        package: &Path,
    ) -> Result<VerificationResult, CoreError> {
        let (_, source, files, hashes, probe) = self.build_source(id).await?;
        let manifest = PackageManifest::load(package)?;
        if probe
            .as_ref()
            .is_some_and(|probe| probe.adapter_id != manifest.id)
        {
            return Err(CoreError::InvalidData(
                "所选项目的原文快照属于另一个引擎，不能用于验证此发布包。".into(),
            ));
        }
        let adapter = manifest.handle(package)?;
        let baseline = self
            .cached_analysis(id)
            .await?
            .ok_or_else(|| CoreError::InvalidData("请先提取项目文本。".into()))?;
        let cancellation = self
            .begin(id, "verify", "正在验证所选适配器发布包。")
            .await?;
        let result = async {
            for (relative, expected) in &hashes {
                if hash_file(&crate::patching::safe_path(&source, relative)?)? != *expected {
                    return Err(CoreError::InvalidData(format!(
                        "原文快照已变化：{relative}"
                    )));
                }
            }
            let root = self.project_directory(id)?.join("validation");
            fs::create_dir_all(&root)?;
            let output = tempfile::Builder::new()
                .prefix("package-")
                .tempdir_in(root)?;
            let service = self.engine_service();
            if let Some(probe) = &probe {
                service
                    .verify_probed(
                        &adapter,
                        &source,
                        probe,
                        output.path(),
                        &baseline.extraction().units,
                        &cancellation,
                        None,
                    )
                    .await
            } else {
                service
                    .verify(
                        &adapter,
                        &source,
                        &files,
                        output.path(),
                        &baseline.extraction().units,
                        &cancellation,
                        None,
                    )
                    .await
            }
            .map_err(CoreError::from)
        }
        .await;
        self.finish(
            id,
            "verify",
            result
                .as_ref()
                .map(|r| r.diagnostics.join("\n"))
                .map_err(ToString::to_string),
        )
        .await?;
        result
    }
}

/// Binds baseline text identities to their recorded bytes without changing any source files.
fn adaptation_inputs(
    project: &Path,
    game: &Path,
    baseline: Option<&StoredAnalysis>,
    cancellation: &CancellationToken,
    progress: Option<FileProgress>,
) -> Result<AdaptationInputs, CoreError> {
    let mut installer = PatchInstaller::new(project);
    if let Some(progress) = &progress {
        installer = installer.with_progress(progress.clone());
    }
    let mut inputs = AdaptationInputs::default();
    let mut bound = BTreeSet::new();
    if let Some(baseline) = baseline {
        if baseline
            .source_directory
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
        {
            return Err(CoreError::InvalidData(
                "已有资源快照路径包含上级目录。".into(),
            ));
        }
        let sources = std::path::absolute(project.join("sources"))?;
        let source = std::path::absolute(&baseline.source_directory)?;
        crate::patching::check_path(&source)?;
        if source == sources || !source.starts_with(&sources) {
            return Err(CoreError::InvalidData(
                "已有提取结果的资源快照不属于当前项目。".into(),
            ));
        }
        let mut resources = BTreeMap::new();
        for relative in &baseline.resource_files {
            let relative = crate::patching::normalize_relative(relative)?;
            if resources
                .insert(relative.to_lowercase(), relative)
                .is_some()
            {
                return Err(CoreError::InvalidData("已有资源清单包含重复路径。".into()));
            }
        }
        let mut hashes = BTreeMap::new();
        for (relative, hash) in &baseline.source_hashes {
            let relative = crate::patching::normalize_relative(relative)?;
            if hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return Err(CoreError::InvalidData(format!(
                    "已有资源的 SHA-256 无效：{relative}"
                )));
            }
            if hashes.insert(relative.to_lowercase(), hash).is_some() {
                return Err(CoreError::InvalidData("已有资源哈希包含重复路径。".into()));
            }
        }
        if !resources.keys().eq(hashes.keys()) {
            return Err(CoreError::InvalidData(
                "已有资源清单与快照哈希不一致。".into(),
            ));
        }
        for (key, relative) in resources {
            if cancellation.is_cancelled() {
                return Err(gamel10n_engines::EngineError::Cancelled.into());
            }
            let path = crate::patching::safe_path(&source, &relative)?;
            if let Some(progress) = &progress {
                progress.file("正在校验已有提取资源", &path, &relative);
            }
            let path = match fs::metadata(&path) {
                Ok(metadata) if metadata.is_file() => path,
                Ok(_) => {
                    return Err(CoreError::InvalidData(format!(
                        "已有资源快照不是普通文件：{relative}"
                    )));
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    installer.resolve_original_file(game, &relative)?
                }
                Err(error) => return Err(error.into()),
            };
            if !hash_file(&path)?.eq_ignore_ascii_case(hashes[&key]) {
                return Err(CoreError::InvalidData(format!(
                    "适配输入与已有提取资源的哈希不一致：{relative}"
                )));
            }
            inputs
                .hashes
                .insert(relative.clone(), hashes[&key].to_ascii_lowercase());
            inputs.files.insert(relative, path);
            bound.insert(key);
        }
    }
    for entry in installer.read_journal(game)?.entries {
        if cancellation.is_cancelled() {
            return Err(gamel10n_engines::EngineError::Cancelled.into());
        }
        if let Some(backup) = entry.backup_relative_path {
            inputs
                .excluded
                .insert(crate::patching::normalize_relative(&backup)?);
        }
        let relative = crate::patching::normalize_relative(&entry.relative_path)?;
        if !entry.original_existed {
            inputs.excluded.insert(relative);
        } else if !bound.contains(&relative.to_lowercase()) {
            let source = installer.resolve_original_file(game, &relative)?;
            inputs.hashes.insert(relative.clone(), hash_file(&source)?);
            inputs.files.insert(relative, source);
        }
    }
    Ok(inputs)
}

/// The guard applies to committed project snapshots only, never installed game resources.
struct SourceReadOnlyGuard {
    originals: Vec<(PathBuf, fs::Permissions)>,
}

impl SourceReadOnlyGuard {
    fn new(
        project: &Path,
        source: &Path,
        hashes: &BTreeMap<String, String>,
    ) -> Result<Self, CoreError> {
        let source = std::path::absolute(source)?;
        let sources = std::path::absolute(project.join("sources"))?;
        crate::patching::check_path(&source)?;
        if !source.starts_with(&sources) {
            return Err(CoreError::InvalidData(
                "适配器输入必须是当前项目保存的原文快照。".into(),
            ));
        }
        let mut guard = Self {
            originals: Vec::new(),
        };
        for relative in hashes.keys() {
            let path = crate::patching::safe_path(&source, relative)?;
            let permissions = fs::metadata(&path)?.permissions();
            guard.originals.push((path.clone(), permissions.clone()));
            let mut read_only = permissions;
            read_only.set_readonly(true);
            fs::set_permissions(&path, read_only)?;
        }
        Ok(guard)
    }

    fn restore(mut self) -> Result<(), CoreError> {
        for (path, permissions) in &self.originals {
            fs::set_permissions(path, permissions.clone())?;
        }
        self.originals.clear();
        Ok(())
    }
}

impl Drop for SourceReadOnlyGuard {
    fn drop(&mut self) {
        for (path, permissions) in &self.originals {
            let _ = fs::set_permissions(path, permissions.clone());
        }
    }
}

fn snapshot(
    project: &Path,
    game: &Path,
    files: &[String],
    inputs: Option<&AdaptationInputs>,
    cancellation: &CancellationToken,
    progress: FileProgress,
) -> Result<(PathBuf, BTreeMap<String, String>), CoreError> {
    let installer = PatchInstaller::new(project).with_progress(progress.clone());
    let bound_hashes: BTreeMap<_, _> = inputs
        .into_iter()
        .flat_map(|inputs| &inputs.hashes)
        .map(|(relative, hash)| (relative.replace('\\', "/").to_lowercase(), hash))
        .collect();
    let mut hashes = BTreeMap::new();
    let mut originals = Vec::new();
    for file in files {
        if cancellation.is_cancelled() {
            return Err(gamel10n_engines::EngineError::Cancelled.into());
        }
        let source = match inputs {
            Some(inputs) => inputs.file(game, file)?,
            None => installer.resolve_original_file(game, file)?,
        };
        let hash = hash_file(&source)?;
        if let Some(expected) = bound_hashes.get(&file.replace('\\', "/").to_lowercase())
            && !hash.eq_ignore_ascii_case(expected)
        {
            return Err(CoreError::InvalidData(format!(
                "适配期间已绑定的资源发生变化：{file}"
            )));
        }
        hashes.insert(file.clone(), hash);
        originals.push((file, source));
    }
    let fingerprint = hash_text(
        &serde_json::to_string(&hashes).map_err(|e| CoreError::InvalidData(e.to_string()))?,
    );
    let target = project.join("sources").join(fingerprint);
    crate::patching::check_path(&target)?;
    if target.is_dir() {
        let mut missing = Vec::new();
        for (file, hash) in &hashes {
            let path = crate::patching::safe_path(&target, file)?;
            progress.file("正在校验已有原文快照", &path, file);
            match fs::metadata(&path) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound && inputs.is_some() => {
                    missing.push((file, path));
                    continue;
                }
                Err(error) => return Err(error.into()),
                Ok(_) => {}
            }
            if hash_file(&path)? != *hash {
                return Err(CoreError::InvalidData("已保存的原文快照发生变化。".into()));
            }
        }
        for (file, path) in missing {
            if cancellation.is_cancelled() {
                return Err(gamel10n_engines::EngineError::Cancelled.into());
            }
            let source = originals
                .iter()
                .find(|(relative, _)| *relative == file)
                .map(|(_, source)| source)
                .ok_or_else(|| CoreError::InvalidData("缺少快照资源的输入绑定。".into()))?;
            let parent = path
                .parent()
                .ok_or_else(|| CoreError::InvalidData("快照资源没有父目录。".into()))?;
            fs::create_dir_all(parent)?;
            crate::patching::check_path(&path)?;
            let temporary = tempfile::NamedTempFile::new_in(parent)?;
            progress.file("正在补齐已校验的缺失快照", source, file);
            fs::copy(source, temporary.path())?;
            if hash_file(temporary.path())? != hashes[file] {
                return Err(CoreError::InvalidData("读取期间资源发生变化。".into()));
            }
            temporary
                .persist_noclobber(path)
                .map_err(|error| error.error)?;
        }
        return Ok((target, hashes));
    }
    fs::create_dir_all(project.join("sources"))?;
    let pending = tempfile::tempdir_in(project.join("sources"))?;
    for (file, source) in originals {
        if cancellation.is_cancelled() {
            return Err(gamel10n_engines::EngineError::Cancelled.into());
        }
        let path = resolve_relative(pending.path(), file)?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        progress.file("正在复制原文快照", &source, file);
        fs::copy(source, &path)?;
        if hash_file(&path)? != hashes[file] {
            return Err(CoreError::InvalidData("读取期间资源发生变化。".into()));
        }
    }
    fs::rename(pending.path(), &target)?;
    Ok((target, hashes))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn baseline_analysis(project: &Path) -> StoredAnalysis {
        let source = project.join("sources/baseline");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("data.bin"), b"original").unwrap();
        StoredAnalysis {
            schema_version: 1,
            format: "fixture".into(),
            source_directory: source,
            resource_files: vec!["data.bin".into()],
            source_hashes: BTreeMap::from([("data.bin".into(), hash_text("original"))]),
            extraction: gamel10n_engines::protocol::ExtractionResult {
                units: vec![],
                warnings: vec![],
            },
            adapter: None,
            probe: None,
        }
    }

    #[test]
    fn adaptation_inputs_prefer_baseline_and_exclude_installed_additions_and_backups() {
        let root = tempfile::tempdir().unwrap();
        let project = root.path().join("project");
        let game = root.path().join("game");
        fs::create_dir(&game).unwrap();
        fs::write(game.join("data.bin"), b"original").unwrap();
        fs::write(game.join("other.bin"), b"other original").unwrap();
        let baseline = baseline_analysis(&project);
        let patch = project.join("patch");
        fs::create_dir_all(patch.join(PAYLOAD_DIRECTORY)).unwrap();
        for name in ["data.bin", "other.bin", "addon.bin"] {
            fs::write(patch.join(PAYLOAD_DIRECTORY).join(name), b"translated").unwrap();
        }
        build_manifest(&patch, &BTreeMap::new()).unwrap();
        let installer = PatchInstaller::new(&project);
        installer.install(&game, &patch).unwrap();
        fs::remove_file(game.join("data.bin.bak")).unwrap();
        let journal = fs::read(installer.journal_path()).unwrap();

        let inputs = adaptation_inputs(
            &project,
            &game,
            Some(&baseline),
            &CancellationToken::new(),
            None,
        )
        .unwrap();
        assert_eq!(
            inputs.file(&game, "data.bin").unwrap(),
            baseline.source_directory.join("data.bin")
        );
        assert_eq!(inputs.hashes["data.bin"], hash_text("original"));
        assert_eq!(
            inputs.file(&game, "other.bin").unwrap(),
            game.join("other.bin.bak")
        );
        assert!(inputs.excludes("addon.bin"));
        assert!(inputs.excludes("data.bin.bak"));
        assert!(inputs.excludes("other.bin.bak"));
        let inventory = inputs.inventory(Inventory::scan(&game).unwrap()).unwrap();
        assert_eq!(
            inventory
                .files
                .iter()
                .map(|file| file.path.as_str())
                .collect::<Vec<_>>(),
            ["data.bin", "other.bin"]
        );
        assert_eq!(fs::read(game.join("data.bin")).unwrap(), b"translated");
        assert_eq!(
            fs::read(game.join("other.bin.bak")).unwrap(),
            b"other original"
        );
        assert!(!game.join("data.bin.bak").exists());
        assert_eq!(fs::read(installer.journal_path()).unwrap(), journal);
    }

    #[test]
    fn missing_baseline_file_binds_only_matching_original_without_restoring_snapshot() {
        let root = tempfile::tempdir().unwrap();
        let project = root.path().join("project");
        let game = root.path().join("game");
        fs::create_dir(&game).unwrap();
        fs::write(game.join("data.bin"), b"original").unwrap();
        let mut baseline = baseline_analysis(&project);
        baseline.source_hashes = BTreeMap::from([(
            "DATA.BIN".into(),
            hash_text("original").to_ascii_uppercase(),
        )]);
        let snapshot = baseline.source_directory.join("data.bin");
        fs::remove_file(&snapshot).unwrap();
        let token = CancellationToken::new();

        let inputs = adaptation_inputs(&project, &game, Some(&baseline), &token, None).unwrap();
        assert_eq!(
            inputs.file(&game, "data.bin").unwrap(),
            game.join("data.bin")
        );
        assert!(!snapshot.exists());
        fs::write(game.join("data.bin"), b"different bytes").unwrap();
        assert!(adaptation_inputs(&project, &game, Some(&baseline), &token, None).is_err());
        assert!(!snapshot.exists());
        assert_eq!(fs::read(game.join("data.bin")).unwrap(), b"different bytes");
    }

    #[test]
    fn adaptation_inputs_reject_changed_snapshots_and_inconsistent_metadata() {
        for case in 0..8 {
            let root = tempfile::tempdir().unwrap();
            let project = root.path().join("project");
            let game = root.path().join("game");
            fs::create_dir(&game).unwrap();
            fs::write(game.join("data.bin"), b"original").unwrap();
            let mut baseline = baseline_analysis(&project);
            match case {
                0 => baseline.source_hashes.clear(),
                1 => baseline.resource_files.push("DATA.BIN".into()),
                2 => {
                    baseline
                        .source_hashes
                        .insert("DATA.BIN".into(), hash_text("original"));
                }
                3 => baseline.resource_files = vec!["../data.bin".into()],
                4 => {
                    baseline
                        .source_hashes
                        .insert("data.bin".into(), "invalid".into());
                }
                5 => baseline.source_directory = game.clone(),
                6 => baseline.source_directory = project.join("sources/../sources/baseline"),
                7 => fs::write(baseline.source_directory.join("data.bin"), b"changed").unwrap(),
                _ => unreachable!(),
            }
            assert!(
                adaptation_inputs(
                    &project,
                    &game,
                    Some(&baseline),
                    &CancellationToken::new(),
                    None
                )
                .is_err(),
                "case {case}"
            );
            assert_eq!(fs::read(game.join("data.bin")).unwrap(), b"original");
        }
    }

    #[tokio::test]
    async fn bound_snapshot_restores_only_missing_files_with_identical_hashes() {
        let root = tempfile::tempdir().unwrap();
        let game = root.path().join("game");
        fs::create_dir(&game).unwrap();
        fs::write(game.join("data.bin"), b"original").unwrap();
        let app = Application::open(root.path().join("data"), &crate::ToolPaths::new("missing"))
            .await
            .unwrap();
        let project = app.add_project(&game).await.unwrap();
        let directory = app.project_directory(&project.id).unwrap();
        let hashes = BTreeMap::from([("data.bin".to_string(), hash_text("original"))]);
        let source = directory
            .join("sources")
            .join(hash_text(&serde_json::to_string(&hashes).unwrap()));
        fs::create_dir_all(&source).unwrap();
        let inputs = AdaptationInputs {
            files: BTreeMap::from([("data.bin".into(), game.join("data.bin"))]),
            hashes: hashes.clone(),
            ..Default::default()
        };
        for changed in [false, true] {
            if changed {
                fs::write(source.join("data.bin"), b"changed snapshot").unwrap();
            }
            let target = directory.clone();
            let game = game.clone();
            let inputs = inputs.clone();
            let result = app
                .run_file_task(&project.id, "adapt", move |progress| {
                    snapshot(
                        &target,
                        &game,
                        &["data.bin".into()],
                        Some(&inputs),
                        &CancellationToken::new(),
                        progress,
                    )
                })
                .await;
            if changed {
                assert!(result.is_err());
                assert_eq!(
                    fs::read(source.join("data.bin")).unwrap(),
                    b"changed snapshot"
                );
            } else {
                let (actual, actual_hashes) = result.unwrap();
                assert_eq!(actual, source);
                assert_eq!(actual_hashes, hashes);
                assert_eq!(fs::read(source.join("data.bin")).unwrap(), b"original");
            }
        }
        assert_eq!(fs::read(game.join("data.bin")).unwrap(), b"original");
        fs::remove_file(source.join("data.bin")).unwrap();
        fs::write(game.join("data.bin"), b"changed input").unwrap();
        let result = app
            .run_file_task(&project.id, "adapt", move |progress| {
                snapshot(
                    &directory,
                    &game,
                    &["data.bin".into()],
                    Some(&inputs),
                    &CancellationToken::new(),
                    progress,
                )
            })
            .await;
        assert!(result.is_err());
        assert!(!source.join("data.bin").exists());
    }

    fn register_json_adapter(app: &Application, key: &str, script: &str) -> PathBuf {
        let package = app
            .data_directory()
            .join("extensions/packages")
            .join(format!("engine.{key}"))
            .join("1");
        fs::create_dir_all(package.join("src")).unwrap();
        let entry = package.join("src/adapter.py");
        fs::write(&entry, script).unwrap();
        write_json(&package.join("package.json"), &json!({
            "id":format!("engine.{key}"), "version":"1", "manifestVersion":4, "protocolVersion":1,
            "family":{"kind":"engine","key":key,"name":key},
            "resourcePatterns":["*.json"], "packageFiles":{"src/adapter.py":hash_file(&entry).unwrap()}
        })).unwrap();
        EngineCatalog::open(app.data_directory())
            .unwrap()
            .register_package(&package, None)
            .unwrap();
        package
    }

    #[tokio::test]
    async fn saved_overlay_names_survive_build_install_update_and_restore() {
        let tools = crate::ToolPaths::development();
        if !tools.python().is_file() {
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let game = root.path().join("game");
        fs::create_dir(&game).unwrap();
        let original = br#"["one","two","three","four"]"#;
        fs::write(game.join("text.json"), original).unwrap();
        fs::write(game.join("text.json.000"), b"unrelated media").unwrap();
        let app = Application::open(root.path().join("data"), &tools)
            .await
            .unwrap();
        let project = app.add_project(&game).await.unwrap();
        let script = include_str!("../../engines/tests/fixtures/overlay_adapter.py")
            .replace("CASE = 'valid'", "CASE = 'nonempty-original'");
        let package = register_json_adapter(&app, "fixture", &script);
        let adapter = PackageManifest::load(&package)
            .unwrap()
            .handle(&package)
            .unwrap();
        let replacement = root.path().join("replacement-patch");
        fs::create_dir_all(replacement.join(PAYLOAD_DIRECTORY)).unwrap();
        let previous_text = br#"["previous translation","two","three","four"]"#;
        fs::write(
            replacement.join(PAYLOAD_DIRECTORY).join("text.json"),
            previous_text,
        )
        .unwrap();
        build_manifest(
            &replacement,
            &BTreeMap::from([(
                "text.json".into(),
                hash_file(&game.join("text.json")).unwrap(),
            )]),
        )
        .unwrap();
        PatchInstaller::new(app.project_directory(&project.id).unwrap())
            .install(&game, &replacement)
            .unwrap();
        let token = CancellationToken::new();
        let mut request = AdapterRequest::new("probe", &game);
        request.resource_files = Some(vec!["text.json".into(), "text.json.000".into()]);
        let probe = app
            .engine_service()
            .execute(&adapter, &request, &token)
            .await
            .unwrap()
            .probe
            .unwrap();
        app.extract_adapter(&project.id, adapter, probe, &token, false, None, None, None)
            .await
            .unwrap();

        // Reopen to exercise persisted probe hints, not an in-memory request.
        drop(app);
        let app = Application::open(root.path().join("data"), &tools)
            .await
            .unwrap();
        for translated in ["第一条译文", "校对后的译文"] {
            app.save_translation_edit(&project.id, "0", translated.into(), true)
                .await
                .unwrap();
            let patch = app.generate_patch(&project.id, None, false).await.unwrap();
            assert!(!patch.join("text.json").exists());
            let payload = patch.join(PAYLOAD_DIRECTORY);
            assert!(!payload.join("text.json").exists());
            let contents: Value = read_json(&payload.join("text.json.001")).unwrap();
            assert_eq!(contents, json!({"0":translated}));
            assert_eq!(
                app.generate_patch(&project.id, None, false).await.unwrap(),
                patch
            );
            let wrong_game = root.path().join(format!("wrong-game-{translated}"));
            fs::create_dir(&wrong_game).unwrap();
            fs::write(wrong_game.join("text.json"), b"different version").unwrap();
            let error =
                PatchInstaller::new(root.path().join(format!("wrong-project-{translated}")))
                    .install(&wrong_game, &patch)
                    .unwrap_err();
            assert!(
                error.to_string().contains("原资源与当前游戏不一致"),
                "{error}"
            );
            assert!(!wrong_game.join("text.json.001").exists());
            app.install_patch(&project.id).await.unwrap();
            assert_eq!(fs::read(game.join("text.json")).unwrap(), original);
            assert!(!game.join("text.json.bak").exists());
            assert_eq!(
                fs::read(game.join("text.json.001")).unwrap(),
                fs::read(payload.join("text.json.001")).unwrap()
            );
            assert!(!game.join("text.json.002").exists());
        }
        // Switching back must remove an older overlay that would override the replacement.
        PatchInstaller::new(app.project_directory(&project.id).unwrap())
            .install(&game, &replacement)
            .unwrap();
        assert_eq!(fs::read(game.join("text.json")).unwrap(), previous_text);
        assert!(!game.join("text.json.001").exists());
        app.restore_game(&project.id).await.unwrap();
        assert_eq!(fs::read(game.join("text.json")).unwrap(), original);
        assert_eq!(
            fs::read(game.join("text.json.000")).unwrap(),
            b"unrelated media"
        );
        assert!(!game.join("text.json.001").exists());
        assert!(!game.join("text.json.bak").exists());
    }

    #[tokio::test]
    async fn readding_known_game_defers_local_recovery_until_recognition() {
        let tools = crate::ToolPaths::development();
        if !tools.python().is_file() {
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let game = root.path().join("game");
        fs::create_dir(&game).unwrap();
        let original = serde_json::to_vec(&vec!["line"; SMALL_EXTRACTION_THRESHOLD]).unwrap();
        fs::write(game.join("text.json"), &original).unwrap();
        let app = Application::open(root.path().join("data"), &tools)
            .await
            .unwrap();
        let project = app.add_project(&game).await.unwrap();
        let script = include_str!("../../engines/tests/fixtures/json_adapter.py").replace(
            "r = json.load(sys.stdin)",
            "r = json.load(sys.stdin)\nwith Path(__file__).with_name('calls.log').open('a', encoding='utf8') as log:\n    log.write(r['operation'] + '\\n')",
        );
        let package = register_json_adapter(&app, "fixture", &script);
        assert!(matches!(
            app.recognize(&project.id).await.unwrap(),
            RecognitionResult::Extracted(SMALL_EXTRACTION_THRESHOLD)
        ));
        app.remove_projects(std::slice::from_ref(&project.id), true)
            .await
            .unwrap();
        let reimported = app.add_project(&game).await.unwrap();
        assert!(!reimported.extra.contains_key("analysis"));
        assert!(app.task_state(&reimported.id).await.is_none());
        assert_eq!(app.consumption_history().await.unwrap().records.len(), 1);
        assert_eq!(
            fs::read_to_string(package.join("src/calls.log"))
                .unwrap()
                .lines()
                .collect::<Vec<_>>(),
            ["probe", "extract", "build"]
        );
        assert!(matches!(
            app.recognize(&reimported.id).await.unwrap(),
            RecognitionResult::Extracted(SMALL_EXTRACTION_THRESHOLD)
        ));
        let reimported = app.open_project(&reimported.id).await.unwrap();
        assert_eq!(
            reimported.extra["analysis"]["unitCount"],
            SMALL_EXTRACTION_THRESHOLD
        );
        assert_eq!(
            app.task_state(&reimported.id).await.unwrap().succeeded,
            Some(true)
        );
        assert_eq!(
            fs::read_to_string(package.join("src/calls.log"))
                .unwrap()
                .lines()
                .collect::<Vec<_>>(),
            ["probe", "extract", "build", "probe", "extract", "build"]
        );
        assert!(
            !app.project_directory(&reimported.id)
                .unwrap()
                .join("adaptation")
                .exists()
        );
        assert_eq!(fs::read(game.join("text.json")).unwrap(), original);
    }

    #[tokio::test]
    async fn regular_recognition_tries_next_candidate_after_original_roundtrip_mismatch() {
        let tools = crate::ToolPaths::development();
        if !tools.python().is_file() {
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let game = root.path().join("game");
        fs::create_dir(&game).unwrap();
        let original = serde_json::to_vec(&vec!["line"; SMALL_EXTRACTION_THRESHOLD]).unwrap();
        fs::write(game.join("text.json"), &original).unwrap();
        let app = Application::open(root.path().join("data"), &tools)
            .await
            .unwrap();
        let project = app.add_project(&game).await.unwrap();
        let fixture = include_str!("../../engines/tests/fixtures/json_adapter.py");
        let broken = fixture.replace(
            "(root / 'text.json').read_bytes() if changed == data else json.dumps(changed, ensure_ascii=False).encode('utf8')",
            "b'[]'",
        );
        register_json_adapter(&app, "fixture-a-invalid", &broken);
        register_json_adapter(&app, "fixture-z-valid", fixture);
        assert!(matches!(
            app.recognize(&project.id).await.unwrap(),
            RecognitionResult::Extracted(SMALL_EXTRACTION_THRESHOLD)
        ));
        let saved = app.store.get(&project.id).await.unwrap();
        assert_eq!(
            saved.extra["analysis"]["adapterId"],
            "engine.fixture-z-valid"
        );
        assert_eq!(
            saved.extra["discovery"]["reuseFailures"][0]["id"],
            "engine.fixture-a-invalid"
        );
        assert_eq!(
            saved.extra["discovery"]["reuseFailures"][0]["failedStage"],
            "original-roundtrip"
        );
        assert!(
            !app.project_directory(&project.id)
                .unwrap()
                .join("adaptation")
                .exists()
        );
        assert_eq!(fs::read(game.join("text.json")).unwrap(), original);
    }

    #[tokio::test]
    async fn regular_recognition_tries_next_candidate_after_extraction_is_rejected() {
        let tools = crate::ToolPaths::development();
        if !tools.python().is_file() {
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let game = root.path().join("game");
        fs::create_dir(&game).unwrap();
        let original = serde_json::to_vec(&vec!["line"; SMALL_EXTRACTION_THRESHOLD]).unwrap();
        fs::write(game.join("text.json"), &original).unwrap();
        let app = Application::open(root.path().join("data"), &tools)
            .await
            .unwrap();
        let project = app.add_project(&game).await.unwrap();
        let fixture = include_str!("../../engines/tests/fixtures/json_adapter.py");
        let rejected = fixture.replace(
            "elif r['operation'] == 'extract':",
            "elif r['operation'] == 'extract':\n    out.update(success=False, errorKind='incompatible', error='unsupported XP3 info/name layout')",
        );
        register_json_adapter(&app, "fixture-a-invalid", &rejected);
        register_json_adapter(&app, "fixture-z-valid", fixture);
        assert!(matches!(
            app.recognize(&project.id).await.unwrap(),
            RecognitionResult::Extracted(SMALL_EXTRACTION_THRESHOLD)
        ));
        let saved = app.store.get(&project.id).await.unwrap();
        assert_eq!(
            saved.extra["analysis"]["adapterId"],
            "engine.fixture-z-valid"
        );
        let failure = &saved.extra["discovery"]["reuseFailures"][0];
        assert_eq!(failure["id"], "engine.fixture-a-invalid");
        assert_eq!(failure["failedStage"], "extraction");
        assert_eq!(failure["evidence"][0], "unsupported XP3 info/name layout");
        assert_eq!(fs::read(game.join("text.json")).unwrap(), original);
    }

    #[tokio::test]
    async fn rejected_extraction_without_other_candidates_enters_smart_adaptation() {
        let tools = crate::ToolPaths::development();
        if !tools.python().is_file() {
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let game = root.path().join("game");
        fs::create_dir(&game).unwrap();
        let original = br#"["one","two"]"#;
        fs::write(game.join("text.json"), original).unwrap();
        let app = Application::open(root.path().join("data"), &tools)
            .await
            .unwrap();
        let project = app.add_project(&game).await.unwrap();
        let rejected = include_str!("../../engines/tests/fixtures/json_adapter.py").replace(
            "elif r['operation'] == 'extract':",
            "elif r['operation'] == 'extract':\n    out.update(success=False, errorKind='incompatible', error='unsupported XP3 info/name layout')",
        );
        register_json_adapter(&app, "fixture", &rejected);
        let error = app.recognize(&project.id).await.unwrap_err();
        assert!(error.to_string().contains("API"), "{error}");
        assert_eq!(
            app.task_state(&project.id).await.unwrap().operation,
            "adapt"
        );
        let saved = app.store.get(&project.id).await.unwrap();
        assert_eq!(
            saved.extra["discovery"]["reuseFailures"][0]["failedStage"],
            "extraction"
        );
        assert_eq!(fs::read(game.join("text.json")).unwrap(), original);
    }

    #[tokio::test]
    async fn unclassified_extraction_failure_stops_without_model_fallback() {
        let tools = crate::ToolPaths::development();
        if !tools.python().is_file() {
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let game = root.path().join("game");
        fs::create_dir(&game).unwrap();
        fs::write(game.join("text.json"), br#"["one","two"]"#).unwrap();
        let app = Application::open(root.path().join("data"), &tools)
            .await
            .unwrap();
        let project = app.add_project(&game).await.unwrap();
        let broken = include_str!("../../engines/tests/fixtures/json_adapter.py").replace(
            "elif r['operation'] == 'extract':",
            "elif r['operation'] == 'extract':\n    out.update(success=False, error='dependency unavailable')",
        );
        register_json_adapter(&app, "fixture", &broken);
        let error = app.recognize(&project.id).await.unwrap_err();
        assert!(
            error.to_string().contains("dependency unavailable"),
            "{error}"
        );
        assert_eq!(
            app.task_state(&project.id).await.unwrap().operation,
            "recognize"
        );
        assert!(
            !app.project_directory(&project.id)
                .unwrap()
                .join("adaptation")
                .exists()
        );
    }

    #[tokio::test]
    async fn recognition_recovers_deleted_texts_with_trial_adapters_without_promoting_other_games()
    {
        let tools = crate::ToolPaths::development();
        if !tools.python().is_file() {
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let game = root.path().join("game");
        fs::create_dir(&game).unwrap();
        let original = br#"["one","two","three","four"]"#;
        fs::write(game.join("text.json"), original).unwrap();
        let app = Application::open(root.path().join("data"), &tools)
            .await
            .unwrap();
        let project = app.add_project(&game).await.unwrap();
        let package = register_json_adapter(
            &app,
            "fixture",
            include_str!("../../engines/tests/fixtures/json_adapter.py"),
        );
        let adapter = PackageManifest::load(&package)
            .unwrap()
            .handle(&package)
            .unwrap();
        let token = CancellationToken::new();
        let probe = app
            .engine_service()
            .execute(&adapter, &AdapterRequest::new("probe", &game), &token)
            .await
            .unwrap()
            .probe
            .unwrap();
        app.extract_adapter(&project.id, adapter, probe, &token, true, None, None, None)
            .await
            .unwrap();
        let original_ids = app
            .text_page(&project.id, "", 0, 100)
            .await
            .unwrap()
            .units
            .into_iter()
            .map(|unit| unit.id)
            .collect::<Vec<_>>();
        app.save_translation_edit(&project.id, "0", "已校对译文".into(), true)
            .await
            .unwrap();
        app.remove_projects(std::slice::from_ref(&project.id), false)
            .await
            .unwrap();
        app.add_project(&game).await.unwrap();
        let saved = app.workbench_row(&project.id, "0").await.unwrap();
        assert!(saved.reviewed);
        assert_eq!(saved.text.as_deref(), Some("已校对译文"));

        app.remove_projects(std::slice::from_ref(&project.id), true)
            .await
            .unwrap();
        let restored = app.add_project(&game).await.unwrap();
        assert_eq!(restored.id, project.id);
        assert!(!restored.extra.contains_key("analysis"));
        assert!(app.task_state(&project.id).await.is_none());
        assert!(app.consumption_history().await.unwrap().records.is_empty());
        let session = app
            .project_directory(&project.id)
            .unwrap()
            .join("adaptation/current/session.json");
        write_json(&session, &json!({"lastStatus":"Incomplete"})).unwrap();
        assert!(matches!(
            app.recognize(&project.id).await.unwrap(),
            RecognitionResult::Extracted(4)
        ));
        let restored = app.open_project(&project.id).await.unwrap();
        assert_eq!(restored.extra["analysis"]["unitCount"], 4);
        assert_eq!(restored.extra["analysis"]["adapterVersion"], "1");
        let restored_ids = app
            .text_page(&project.id, "", 0, 100)
            .await
            .unwrap()
            .units
            .into_iter()
            .map(|unit| unit.id)
            .collect::<Vec<_>>();
        assert_eq!(restored_ids, original_ids);
        let catalog = EngineCatalog::open(app.data_directory()).unwrap();
        let detail = catalog.version_detail("engine.fixture", "1").unwrap();
        assert!(detail.lifecycle.requires_confirmation && detail.lifecycle.feedback.is_empty());
        assert!(!detail.is_candidate);
        assert!(catalog.discovery_candidates(None).unwrap().is_empty());
        assert!(
            catalog
                .discovery_candidates_for_game("another-game", None)
                .unwrap()
                .is_empty()
        );
        assert!(
            catalog
                .discovery_candidates_for_game(&project.id, None)
                .unwrap()[0]
                .is_known_game
        );

        assert!(matches!(
            app.recognize(&project.id).await.unwrap(),
            RecognitionResult::Extracted(4)
        ));
        assert_eq!(
            read_json::<Value>(&session).unwrap()["lastStatus"],
            "Incomplete"
        );
        fs::remove_file(
            app.project_directory(&project.id)
                .unwrap()
                .join("extraction/native.json"),
        )
        .unwrap();
        app.invalidate_project_cache(&project.id).await;
        assert!(matches!(
            app.recognize(&project.id).await.unwrap(),
            RecognitionResult::Extracted(4)
        ));
        let history = app.consumption_history().await.unwrap();
        assert_eq!(history.records.len(), 3);
        assert!(history.records.iter().all(|record| record.api.requests == 0
            && record.mode == "reused"
            && record.model.is_none()));
        let other = root.path().join("other-game");
        fs::create_dir(&other).unwrap();
        fs::write(other.join("text.json"), original).unwrap();
        let other_project = app.add_project(&other).await.unwrap();
        assert!(!other_project.extra.contains_key("analysis"));
        assert!(app.task_state(&other_project.id).await.is_none());
        let error = app.recognize(&other_project.id).await.unwrap_err();
        assert!(error.to_string().contains("API"), "{error}");
        assert!(
            app.cached_analysis(&other_project.id)
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(fs::read(game.join("text.json")).unwrap(), original);
    }

    #[tokio::test]
    async fn known_game_recovery_checks_latest_history_before_generic_recognition_scores() {
        let tools = crate::ToolPaths::development();
        if !tools.python().is_file() {
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let game = root.path().join("game");
        fs::create_dir(&game).unwrap();
        fs::write(game.join("text.json"), br#"["one","two","three","four"]"#).unwrap();
        let app = Application::open(root.path().join("data"), &tools)
            .await
            .unwrap();
        let project = app.add_project(&game).await.unwrap();
        let catalog = EngineCatalog::open(app.data_directory()).unwrap();
        for key in ["older", "newer"] {
            let script = include_str!("../../engines/tests/fixtures/json_adapter.py")
                .replace("engine.fixture", &format!("engine.{key}"));
            let script = if key == "older" {
                script.replace(
                    "enumerate(data)], warnings",
                    "enumerate(data[:2])], warnings",
                )
            } else {
                script
            };
            let package = register_json_adapter(&app, key, &script);
            if key == "older" {
                let mut manifest: Value = read_json(&package.join("package.json")).unwrap();
                manifest["recognition"] = json!({"schemaVersion":1,"required":[{"patterns":["text.json"],"minimumSize":1}],"hints":[]});
                write_json(&package.join("package.json"), &manifest).unwrap();
            }
            catalog
                .register_trial_package(
                    &package,
                    GameCompatibility {
                        game_id: project.id.clone(),
                        title: project.name.clone(),
                        variant: "fixture".into(),
                        adapter_version: "1".into(),
                        structure_status: "passed".into(),
                        runtime_status: "not-tested".into(),
                        updated_at: None,
                    },
                    None,
                )
                .unwrap();
        }
        let restored = app.add_project(&game).await.unwrap();
        assert!(!restored.extra.contains_key("analysis"));
        assert!(app.task_state(&project.id).await.is_none());
        assert!(matches!(
            app.recognize(&project.id).await.unwrap(),
            RecognitionResult::Extracted(4)
        ));
        let restored = app.open_project(&project.id).await.unwrap();
        assert_eq!(restored.extra["analysis"]["adapterId"], "engine.newer");
        assert_eq!(restored.extra["analysis"]["unitCount"], 4);
        assert!(
            app.consumption_history()
                .await
                .unwrap()
                .records
                .iter()
                .all(|record| record.api.requests == 0)
        );
    }

    #[tokio::test]
    async fn adding_known_game_defers_damaged_package_diagnostics_until_recognition() {
        let tools = crate::ToolPaths::development();
        if !tools.python().is_file() {
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let game = root.path().join("game");
        fs::create_dir(&game).unwrap();
        fs::write(game.join("text.json"), br#"["one","two"]"#).unwrap();
        let app = Application::open(root.path().join("data"), &tools)
            .await
            .unwrap();
        let project = app.add_project(&game).await.unwrap();
        let package = register_json_adapter(
            &app,
            "fixture",
            include_str!("../../engines/tests/fixtures/json_adapter.py"),
        );
        EngineCatalog::open(app.data_directory())
            .unwrap()
            .register_trial_package(
                &package,
                GameCompatibility {
                    game_id: project.id.clone(),
                    title: project.name,
                    variant: "fixture".into(),
                    adapter_version: "1".into(),
                    structure_status: "passed".into(),
                    runtime_status: "not-tested".into(),
                    updated_at: None,
                },
                None,
            )
            .unwrap();
        fs::write(package.join("src/adapter.py"), b"damaged payload").unwrap();
        app.remove_projects(std::slice::from_ref(&project.id), true)
            .await
            .unwrap();
        let restored = app.add_project(&game).await.unwrap();
        assert_eq!(restored.id, project.id);
        assert!(!restored.extra.contains_key("analysis"));
        assert!(!restored.extra.contains_key("discovery"));
        assert!(app.task_state(&project.id).await.is_none());
        assert!(app.consumption_history().await.unwrap().records.is_empty());
        let error = app.recognize(&project.id).await.unwrap_err();
        assert!(error.to_string().contains("API"), "{error}");
        let task = app.task_state(&project.id).await.unwrap();
        assert_eq!(task.succeeded, Some(false));
        let restored = app.open_project(&project.id).await.unwrap();
        assert!(
            restored.extra["discovery"]["diagnostics"]
                .as_array()
                .unwrap()
                .iter()
                .any(|diagnostic| diagnostic.as_str().unwrap().contains("适配器文件已变化"))
        );
        assert!(
            !app.project_directory(&project.id)
                .unwrap()
                .join("adaptation")
                .exists()
        );
        assert!(
            app.consumption_history()
                .await
                .unwrap()
                .records
                .iter()
                .all(|record| record.api.requests == 0)
        );
    }

    #[tokio::test]
    async fn small_extraction_enters_smart_check_and_retains_text_when_api_is_missing() {
        let tools = crate::ToolPaths::development();
        if !tools.python().is_file() {
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let game = root.path().join("game");
        fs::create_dir(&game).unwrap();
        fs::write(game.join("text.json"), br#"["one","two"]"#).unwrap();
        let app = Application::open(root.path().join("data"), &tools)
            .await
            .unwrap();
        let project = app.add_project(&game).await.unwrap();
        register_json_adapter(
            &app,
            "fixture",
            include_str!("../../engines/tests/fixtures/json_adapter.py"),
        );
        let error = app.recognize(&project.id).await.unwrap_err();
        assert!(error.to_string().contains("API"), "{error}");
        assert_eq!(
            app.text_page(&project.id, "", 0, 100).await.unwrap().total,
            2
        );
        let state = app.task_state(&project.id).await.unwrap();
        assert_eq!(state.operation, "adapt");
        assert_eq!(state.succeeded, Some(false));
    }

    #[tokio::test]
    async fn continued_recognition_skips_regular_extraction_after_reopening() {
        let root = tempfile::tempdir().unwrap();
        let game = root.path().join("game");
        fs::create_dir(&game).unwrap();
        fs::write(game.join("text.json"), br#"["one","two"]"#).unwrap();
        let data = root.path().join("data");
        let tools = crate::ToolPaths::new("missing");
        let app = Application::open(data.clone(), &tools).await.unwrap();
        let project = app.add_project(&game).await.unwrap();
        register_json_adapter(
            &app,
            "fixture",
            include_str!("../../engines/tests/fixtures/json_adapter.py"),
        );
        let directory = app.project_directory(&project.id).unwrap();
        let session = directory.join("adaptation/current/session.json");
        write_json(
            &session,
            &json!({"objective":"Develop","lastStatus":"BudgetExhausted","notes":"retained findings"}),
        )
        .unwrap();
        let mut saved = app.open_project(&project.id).await.unwrap();
        saved.extra.insert(
            "discovery".into(),
            json!({"diagnostics":["Retained format evidence"]}),
        );
        app.store.save(&saved).await.unwrap();
        drop(app);

        let app = Application::open(data, &tools).await.unwrap();
        let error = app.recognize(&project.id).await.unwrap_err();
        assert!(error.to_string().contains("API"), "{error}");
        assert_eq!(
            app.task_state(&project.id).await.unwrap().operation,
            "adapt"
        );
        assert!(app.cached_analysis(&project.id).await.unwrap().is_none());
        assert!(!directory.join("sources").exists());
        assert_eq!(
            app.open_project(&project.id).await.unwrap().extra["discovery"]["diagnostics"],
            json!(["Retained format evidence"])
        );
        assert_eq!(
            read_json::<Value>(&session).unwrap()["notes"],
            "retained findings"
        );
    }

    #[test]
    fn unfinished_adaptation_keeps_successful_sessions_on_the_regular_path() {
        let root = tempfile::tempdir().unwrap();
        let session = root.path().join("adaptation/current/session.json");
        assert!(!has_unfinished_adaptation(root.path(), None).unwrap());
        for status in [
            "Cancelled",
            "BudgetExhausted",
            "Failed",
            "Incomplete",
            "WaitingForUser",
            "Unsupported",
        ] {
            write_json(&session, &json!({"lastStatus":status})).unwrap();
            assert!(
                has_unfinished_adaptation(root.path(), None).unwrap(),
                "{status}"
            );
        }
        for status in ["Activated", "Reused"] {
            write_json(&session, &json!({"lastStatus":status})).unwrap();
            assert!(
                !has_unfinished_adaptation(root.path(), None).unwrap(),
                "{status}"
            );
            // A new run can be interrupted before replacing the previous successful session status.
            let interrupted = crate::TaskState {
                project_id: "fixture".into(),
                operation: "adapt".into(),
                running: false,
                message: String::new(),
                succeeded: None,
            };
            assert!(has_unfinished_adaptation(root.path(), Some(&interrupted)).unwrap());
        }
        fs::write(&session, "interrupted JSON").unwrap();
        assert!(has_unfinished_adaptation(root.path(), None).unwrap());
    }

    #[tokio::test]
    async fn forced_smart_recognition_does_not_execute_the_regular_parser() {
        let root = tempfile::tempdir().unwrap();
        let game = root.path().join("game");
        fs::create_dir(&game).unwrap();
        fs::write(game.join("text.json"), br#"["one","two"]"#).unwrap();
        let app = Application::open(root.path().join("data"), &crate::ToolPaths::new("missing"))
            .await
            .unwrap();
        let project = app.add_project(&game).await.unwrap();
        register_json_adapter(
            &app,
            "fixture",
            include_str!("../../engines/tests/fixtures/json_adapter.py"),
        );
        let error = app
            .recognize_with_mode(&project.id, String::new(), RecognitionMode::Intelligent)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("API"), "{error}");
        assert!(app.cached_analysis(&project.id).await.unwrap().is_none());
        assert!(
            !app.project_directory(&project.id)
                .unwrap()
                .join("sources")
                .exists()
        );
        assert_eq!(
            app.task_state(&project.id).await.unwrap().operation,
            "adapt"
        );
    }

    #[test]
    fn built_in_parser_identity_is_not_used_as_a_catalog_family_pin() {
        let root = tempfile::tempdir().unwrap();
        let mut project: crate::GameProject = serde_json::from_value(json!({
            "schemaVersion":2,"id":"000000000000000000000001","name":"fixture",
            "gameDirectory":root.path(),"settings":{},
            "analysis":{"adapterId":"vntextpatch-whale","adapterVersion":"1.0.1"}
        }))
        .unwrap();
        assert!(catalog_adapter(&project).is_none());
        let catalog = EngineCatalog::open(root.path()).unwrap();
        assert!(
            catalog
                .discovery_candidates(
                    catalog_adapter(&project)
                        .as_ref()
                        .map(|adapter| (adapter.adapter_id.as_str(), adapter.version.as_str()))
                )
                .is_ok()
        );
        project.extra.get_mut("analysis").unwrap()["adapterId"] = json!("engine.fixture");
        assert_eq!(
            catalog_adapter(&project).unwrap().adapter_id,
            "engine.fixture"
        );
    }

    #[tokio::test]
    async fn verified_extraction_skips_reparsing_but_changed_sources_use_normal_checks() {
        let tools = crate::ToolPaths::development();
        if !tools.python().is_file() {
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let game = root.path().join("game");
        fs::create_dir(&game).unwrap();
        fs::write(game.join("text.json"), br#"["one","two","three","four"]"#).unwrap();
        let app = Application::open(root.path().join("data"), &tools)
            .await
            .unwrap();
        let project = app.add_project(&game).await.unwrap();
        let package = app
            .data_directory()
            .join("extensions/packages/engine.fixture/1");
        fs::create_dir_all(package.join("src")).unwrap();
        let entry = package.join("src/adapter.py");
        let fixture = include_str!("../../engines/tests/fixtures/json_adapter.py").replace(
            "r = json.load(sys.stdin)",
            "r = json.load(sys.stdin)\nwith Path(__file__).with_name('calls.log').open('a', encoding='utf8') as log:\n    log.write(r['operation'] + '\\n')",
        );
        fs::write(&entry, fixture).unwrap();
        write_json(&package.join("package.json"), &json!({
            "id":"engine.fixture", "version":"1", "manifestVersion":4, "protocolVersion":1,
                "family":{"kind":"engine","key":"fixture","name":"Fixture"},
            "resourcePatterns":["*.json"], "packageFiles":{"src/adapter.py":hash_file(&entry).unwrap()}
        })).unwrap();
        let manifest = PackageManifest::load(&package).unwrap();
        let adapter = manifest.handle(&package).unwrap();
        let token = CancellationToken::new();
        let verified = app
            .engine_service()
            .verify(
                &adapter,
                &game,
                &["text.json".into()],
                &root.path().join("verification"),
                &[],
                &token,
                None,
            )
            .await
            .unwrap();
        assert!(verified.passed, "{:?}", verified.diagnostics);
        // A saved/reloaded report cannot supply an in-memory verification proof.
        let saved: VerificationResult = serde_json::from_value(json!(&verified)).unwrap();
        assert!(saved.verified_extraction.is_none());
        let proof = verified.verified_extraction.unwrap();
        let calls = entry.with_file_name("calls.log");
        let before = fs::read_to_string(&calls).unwrap();
        let probe = ProbeResult {
            engine_id: "fixture".into(),
            adapter_id: "engine.fixture".into(),
            adapter_version: "1".into(),
            description: "fixture".into(),
            required_files: vec!["text.json".into()],
            warnings: vec![],
            original_executable: None,
            build_hints: BTreeMap::new(),
        };
        app.extract_adapter(
            &project.id,
            adapter.clone(),
            probe.clone(),
            &token,
            false,
            None,
            Some(proof.clone()),
            None,
        )
        .await
        .unwrap();
        assert_eq!(fs::read_to_string(&calls).unwrap(), before);
        assert_eq!(
            app.cached_analysis(&project.id)
                .await
                .unwrap()
                .unwrap()
                .extraction()
                .units[0]
                .source_text,
            "one"
        );

        fs::write(game.join("text.json"), br#"["ONE","two","three","four"]"#).unwrap();
        let baseline = app.cached_analysis(&project.id).await.unwrap().unwrap();
        let inputs = adaptation_inputs(
            &app.project_directory(&project.id).unwrap(),
            &game,
            Some(baseline.native().unwrap()),
            &token,
            None,
        )
        .unwrap();
        app.extract_adapter(
            &project.id,
            adapter.clone(),
            probe.clone(),
            &token,
            false,
            None,
            Some(proof.clone()),
            Some(&inputs),
        )
        .await
        .unwrap();
        assert_eq!(fs::read_to_string(&calls).unwrap(), before);
        assert_eq!(
            app.cached_analysis(&project.id)
                .await
                .unwrap()
                .unwrap()
                .extraction()
                .units[0]
                .source_text,
            "one"
        );
        app.extract_adapter(
            &project.id,
            adapter,
            probe,
            &token,
            false,
            None,
            Some(proof),
            None,
        )
        .await
        .unwrap();
        assert_eq!(
            fs::read_to_string(&calls)
                .unwrap()
                .strip_prefix(&before)
                .unwrap()
                .lines()
                .collect::<Vec<_>>(),
            ["extract", "build"]
        );
        assert_eq!(
            app.cached_analysis(&project.id)
                .await
                .unwrap()
                .unwrap()
                .extraction()
                .units[0]
                .source_text,
            "ONE"
        );
    }

    #[tokio::test]
    async fn failed_adaptation_is_saved_as_a_failed_task() {
        let root = tempfile::tempdir().unwrap();
        let game = root.path().join("game");
        fs::create_dir(&game).unwrap();
        let app = Application::open(root.path().join("data"), &crate::ToolPaths::new("missing"))
            .await
            .unwrap();
        let project = app.add_project(&game).await.unwrap();
        let result: AdaptationResult = serde_json::from_value(json!({
            "status":"Failed", "adapter":null,
            "usage":{"inputTokens":0,"outputTokens":0,"estimated":false},
            "summary":"适配未完成", "nextAction":"服务恢复后继续适配",
            "diagnostics":[], "pendingCoverage":[], "pendingRuntimeChecks":[],
            "failure":{"category":"api","code":"http-error","httpStatus":502,
                "message":"服务商异常（HTTP 502）", "nextAction":"服务恢复后继续适配"}
        }))
        .unwrap();
        app.begin(&project.id, "adapt", "正在分析").await.unwrap();
        app.finish(&project.id, "adapt", adaptation_task_outcome(&result))
            .await
            .unwrap();
        let state: crate::TaskState = read_json(
            &app.project_directory(&project.id)
                .unwrap()
                .join("rust-task.json"),
        )
        .unwrap();
        assert_eq!(state.succeeded, Some(false));
        assert!(state.message.contains("HTTP 502"));
        assert!(state.message.contains("继续适配"));
        assert_eq!(
            app.store.get(&project.id).await.unwrap().extra["taskSummary"]["succeeded"],
            false
        );
    }

    #[test]
    fn explicit_and_saved_adaptation_objectives_take_precedence_over_defaults() {
        use AdaptationObjective::{Develop, ExpandCoverage, Repair};
        assert_eq!(
            resolve_adaptation_objective(
                Some(ExpandCoverage),
                Some(Repair),
                true,
                "字体错误",
                true
            ),
            ExpandCoverage
        );
        assert_eq!(
            resolve_adaptation_objective(None, Some(Develop), true, "字体错误", true),
            Develop
        );
        assert_eq!(
            resolve_adaptation_objective(None, None, true, "字体错误", false),
            Repair
        );
        assert_eq!(
            resolve_adaptation_objective(None, None, true, "", true),
            Repair
        );
        assert_eq!(
            resolve_adaptation_objective(None, None, true, "", false),
            ExpandCoverage
        );
        assert_eq!(
            resolve_adaptation_objective(None, None, false, "", false),
            Develop
        );
    }

    #[tokio::test]
    async fn adaptation_defaults_restore_session_objectives_and_recent_explicit_choices() {
        let root = tempfile::tempdir().unwrap();
        let game = root.path().join("game");
        fs::create_dir(&game).unwrap();
        let data = root.path().join("data");
        let app = Application::open(data.clone(), &crate::ToolPaths::new("missing"))
            .await
            .unwrap();
        let mut project = app.add_project(&game).await.unwrap();
        project
            .extra
            .insert("adapterId".into(), json!("engine.fixture"));
        app.store.save(&project).await.unwrap();
        assert_eq!(
            app.recognition_objective(&project.id).await.unwrap(),
            AdaptationObjective::Develop
        );
        assert_eq!(
            app.adaptation_objective(&project.id, "修复字体")
                .await
                .unwrap(),
            AdaptationObjective::Repair
        );
        assert_eq!(
            app.adaptation_objective(&project.id, "").await.unwrap(),
            AdaptationObjective::ExpandCoverage
        );
        write_json(
            &app.project_directory(&project.id)
                .unwrap()
                .join("adaptation/current/session.json"),
            &json!({"objective":1}),
        )
        .unwrap();
        assert_eq!(
            app.recognition_objective(&project.id).await.unwrap(),
            AdaptationObjective::Repair
        );
        assert_eq!(
            app.adaptation_objective(&project.id, "补全菜单")
                .await
                .unwrap(),
            AdaptationObjective::Repair
        );
        project
            .extra
            .insert("adaptationObjective".into(), json!("ExpandCoverage"));
        app.store.save(&project).await.unwrap();
        let id = project.id;
        drop(app);
        let reopened = Application::open(data, &crate::ToolPaths::new("missing"))
            .await
            .unwrap();
        assert_eq!(
            reopened.recognition_objective(&id).await.unwrap(),
            AdaptationObjective::ExpandCoverage
        );
        assert_eq!(
            reopened
                .adaptation_objective(&id, "补全菜单")
                .await
                .unwrap(),
            AdaptationObjective::ExpandCoverage
        );
    }

    #[test]
    fn snapshot_guard_rejects_writes_and_restores_permissions_when_dropped() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("sources/fixture");
        fs::create_dir_all(&source).unwrap();
        let path = source.join("data.bin");
        fs::write(&path, b"original").unwrap();
        let hashes = BTreeMap::from([("data.bin".into(), hash_text("original"))]);
        {
            let _guard = SourceReadOnlyGuard::new(root.path(), &source, &hashes).unwrap();
            assert!(fs::metadata(&path).unwrap().permissions().readonly());
            #[cfg(windows)]
            assert!(fs::write(&path, b"modified").is_err());
        }
        assert!(!fs::metadata(&path).unwrap().permissions().readonly());
        assert_eq!(fs::read(path).unwrap(), b"original");
    }

    #[test]
    fn snapshot_guard_cleans_up_partial_setup_and_refuses_actual_game_directory() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("sources/fixture");
        fs::create_dir_all(&source).unwrap();
        let first = source.join("a.bin");
        fs::write(&first, b"original").unwrap();
        let hashes = BTreeMap::from([
            ("a.bin".into(), hash_text("original")),
            ("missing.bin".into(), hash_text("missing")),
        ]);
        assert!(SourceReadOnlyGuard::new(root.path(), &source, &hashes).is_err());
        assert!(!fs::metadata(&first).unwrap().permissions().readonly());
        assert!(
            SourceReadOnlyGuard::new(root.path(), &root.path().join("game"), &BTreeMap::new())
                .is_err()
        );
    }
}
