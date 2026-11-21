use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use gamel10n_translation::files::{hash_file, hash_text};
use serde_json::json;

use super::{
    BuiltinAdapterProbeObservation, DiscoveryResult, DiscoveryVerification, EngineService,
    GeneratedAdapterReuseFailure, ProgressCallback,
};
use crate::{
    AdapterHandle, CancellationToken, EngineError, PackageManifest,
    discovery::{Inventory, RecognitionStatus, native_candidates},
    protocol::{AdapterRequest, ProbeResult},
};

const PACKAGE_PROBE_BUDGET: Duration = Duration::from_secs(45);

struct Discovery<'a> {
    service: &'a EngineService,
    source: &'a Path,
    inventory: &'a Inventory,
    verification: DiscoveryVerification,
    cancellation: &'a CancellationToken,
    progress: ProgressCallback<'a>,
    probe_elapsed: Duration,
    diagnostics: Vec<String>,
    reuse_failures: Vec<GeneratedAdapterReuseFailure>,
    builtin_probes: Vec<BuiltinAdapterProbeObservation>,
    selected: Option<(&'a str, &'a str)>,
}

struct PackageCandidate {
    evidence: usize,
    directory: PathBuf,
    manifest: PackageManifest,
    resources: Vec<String>,
}

impl EngineService {
    #[expect(
        clippy::too_many_arguments,
        reason = "Discovery separates probe-only recognition from complete intelligent reuse checks."
    )]
    pub(super) async fn discover_with_inventory_and_failures(
        &self,
        source: &Path,
        packages: &[PathBuf],
        inventory: &Inventory,
        previous_failures: &[GeneratedAdapterReuseFailure],
        verification: DiscoveryVerification,
        selected: Option<(&str, &str)>,
        cancellation: &CancellationToken,
        progress: ProgressCallback<'_>,
    ) -> Result<DiscoveryResult, EngineError> {
        let mut search = Discovery {
            service: self,
            source,
            inventory,
            verification,
            cancellation,
            progress,
            probe_elapsed: Duration::ZERO,
            diagnostics: Vec::new(),
            reuse_failures: Vec::new(),
            builtin_probes: Vec::new(),
            selected,
        };
        let package_only = selected.is_some() && !packages.is_empty();
        let mut matched = if package_only {
            None
        } else {
            search.probe_smil().await?
        };
        if matched.is_none() {
            matched = search.probe_packages(packages, previous_failures).await?;
        }
        if matched.is_none() && !self.ordered_packages_only && !package_only {
            matched = search.probe_builtins().await?;
        }
        let (adapter, probe) = matched.unzip();
        Ok(DiscoveryResult {
            adapter,
            probe,
            diagnostics: search.diagnostics,
            file_count: inventory.files.len(),
            complete: inventory.complete,
            reuse_failures: search.reuse_failures,
            builtin_probes: search.builtin_probes,
        })
    }

    /// Runs only the requested catalog version, including versions outside automatic matching.
    pub async fn probe_selected(
        &self,
        source: &Path,
        inventory: &Inventory,
        version: (&str, &str),
        package: Option<PathBuf>,
        cancellation: &CancellationToken,
    ) -> Result<(AdapterHandle, ProbeResult), EngineError> {
        let result = self
            .discover_with_inventory_and_failures(
                source,
                package.as_slice(),
                inventory,
                &[],
                DiscoveryVerification::ProbeOnly,
                Some(version),
                cancellation,
                None,
            )
            .await?;
        result
            .adapter
            .zip(result.probe)
            .filter(|(_, probe)| probe.adapter_version == version.1)
            .ok_or_else(|| EngineError::Incompatible {
                stage: "probe",
                message: result.diagnostics.into_iter().next().unwrap_or_else(|| {
                    format!(
                        "{} {} 未匹配此游戏的资源。请核对适配器版本和游戏目录。",
                        version.0, version.1
                    )
                }),
            })
    }
}

impl Discovery<'_> {
    async fn probe_smil(&mut self) -> Result<Option<(AdapterHandle, ProbeResult)>, EngineError> {
        const ID: &str = "engine.kirikiri-smil-smile";
        if self.service.ordered_packages_only
            || self.selected.is_some_and(|version| version != (ID, "1"))
            || self
                .service
                .preferred_adapter
                .as_deref()
                .is_some_and(|id| id != ID)
            || self.service.disabled_families.contains(ID)
        {
            return Ok(None);
        }
        let is_archive = |path: &str| path.to_lowercase().ends_with(".xp3");
        let files = &self.inventory.files;
        if !files.iter().any(|file| is_archive(&file.path))
            || self
                .service
                .excluded_adapters
                .contains(&(ID.into(), "1".into()))
        {
            return Ok(None);
        }
        let resources: Vec<_> = files
            .iter()
            .filter(|file| is_archive(&file.path))
            .map(|file| file.path.clone())
            .collect();
        let adapter = AdapterHandle::BuiltinKirikiriSmil;
        EngineService::report(
            self.progress,
            "检查内置解析器",
            "正在检查 KiriKiri SMIL 映射、过滤器及脚本样本。",
        );
        let started = Instant::now();
        let probe = self
            .service
            .probe_bounded_with_inputs(
                &adapter,
                self.source,
                &resources,
                &self.service.discovery_inputs,
                self.cancellation,
            )
            .await?;
        self.probe_elapsed += started.elapsed();
        let Some(probe) = probe else { return Ok(None) };
        if self.verification == DiscoveryVerification::ProbeOnly {
            return Ok(Some((adapter, probe)));
        }
        let mut request = AdapterRequest::new("extract", self.source);
        request.resource_files = Some(probe.required_files.clone());
        match self
            .service
            .execute(&adapter, &request, self.cancellation)
            .await
        {
            Ok(response)
                if response
                    .extraction
                    .as_ref()
                    .is_some_and(|result| !result.units.is_empty()) =>
            {
                Ok(Some((adapter, probe)))
            }
            Ok(_) => Ok(None),
            Err(EngineError::Incompatible { message, .. }) => {
                self.diagnostics
                    .push(format!("内置解析器 {} 提取不兼容：{message}", adapter.id()));
                Ok(None)
            }
            Err(error) => Err(error),
        }
    }

    fn package_candidates(&mut self, packages: &[PathBuf]) -> Vec<PackageCandidate> {
        let mut candidates: Vec<PackageCandidate> = Vec::new();
        for directory in packages {
            let (manifest, directory) = match PackageManifest::load_runtime(directory) {
                Ok(package) => package,
                Err(error) => {
                    let message = format!("适配器 {} 加载失败：{error}", directory.display());
                    EngineService::report(self.progress, "适配器检查未通过", &message);
                    self.diagnostics.push(message);
                    continue;
                }
            };
            if self.service.disabled_families.contains(&manifest.id)
                || self
                    .service
                    .excluded_adapters
                    .contains(&(manifest.id.clone(), manifest.version.clone()))
            {
                continue;
            }
            if self
                .selected
                .is_some_and(|version| version != (manifest.id.as_str(), manifest.version.as_str()))
            {
                self.diagnostics
                    .push("发布包的运行版本与所选版本不同，请选择对应的运行版本。".into());
                continue;
            }
            let (status, evidence) = self
                .inventory
                .recognize_with(manifest.recognition.as_ref(), &|path| {
                    self.service.discovery_inputs.file(self.source, path).ok()
                });
            if status == RecognitionStatus::NoMatch && self.selected.is_none() {
                continue;
            }
            let resources = manifest.resources(self.inventory);
            if resources.is_empty()
                || candidates
                    .iter()
                    .any(|candidate| candidate.directory == directory)
            {
                continue;
            }
            candidates.push(PackageCandidate {
                evidence,
                directory,
                manifest,
                resources,
            });
        }
        if !self.service.ordered_packages_only {
            candidates.sort_by_key(|candidate| std::cmp::Reverse(candidate.evidence));
        }
        candidates
    }

    async fn probe_packages(
        &mut self,
        packages: &[PathBuf],
        previous_failures: &[GeneratedAdapterReuseFailure],
    ) -> Result<Option<(AdapterHandle, ProbeResult)>, EngineError> {
        for candidate in self.package_candidates(packages) {
            let manifest = &candidate.manifest;
            if self.cancellation.is_cancelled() {
                return Err(EngineError::Cancelled);
            }
            if self.probe_elapsed >= PACKAGE_PROBE_BUDGET {
                if self.verification == DiscoveryVerification::ProbeOnly {
                    return Err(EngineError::Timeout);
                }
                let message = "已有扩展适配器检查达到本次时间预算，继续检查内置解析器。";
                EngineService::report(self.progress, "适配器检查超时", message);
                self.diagnostics.push(message.into());
                break;
            }
            EngineService::report(
                self.progress,
                "匹配已有适配器",
                format!("正在检查 {} {}。", manifest.id, manifest.version),
            );
            let mut phase = "load";
            let mut recognized = false;
            let source_directory = candidate
                .directory
                .join("src")
                .is_dir()
                .then(|| candidate.directory.join("src"));
            let attempt = async {
                let handle = manifest.handle(&candidate.directory)?;
                phase = "probe";
                let started = Instant::now();
                let probe = self
                    .service
                    .probe_bounded_with_limit(
                        &handle,
                        self.source,
                        &candidate.resources,
                        &self.service.discovery_inputs,
                        self.cancellation,
                        PACKAGE_PROBE_BUDGET.saturating_sub(self.probe_elapsed),
                    )
                    .await;
                self.probe_elapsed += started.elapsed();
                let (probe, _) = probe?;
                let Some(probe) = probe else { return Ok(None) };
                recognized = true;
                if !self.service.ordered_packages_only {
                    crate::adaptation_assets::check_reused_archive_scope(
                        &manifest.id,
                        &probe.required_files,
                    )?;
                }
                if self.verification == DiscoveryVerification::ProbeOnly
                    && super::display::QlieDisplay::detect(
                        &handle,
                        self.source,
                        &probe.required_files,
                        &self.service.discovery_inputs,
                    )?
                    .is_none()
                {
                    return Ok(Some((handle, probe)));
                }
                let failure = self
                    .verify_package(
                        &candidate,
                        &handle,
                        &probe,
                        previous_failures,
                        source_directory.as_deref(),
                        &mut phase,
                    )
                    .await?;
                if let Some(failure) = failure {
                    self.record_reuse_failure(failure);
                    return Ok(None);
                }
                Ok(Some((handle, probe)))
            };
            match attempt.await {
                Ok(Some(matched)) => return Ok(Some(matched)),
                Ok(None) => {}
                Err(error @ (EngineError::Cancelled | EngineError::Isolation(_))) => {
                    return Err(error);
                }
                Err(
                    error @ (EngineError::Io(_)
                    | EngineError::ToolMissing(_)
                    | EngineError::Timeout),
                ) if self.verification == DiscoveryVerification::ProbeOnly => {
                    return Err(error);
                }
                Err(EngineError::Timeout) if phase == "probe" => {
                    let message = format!(
                        "适配器 {} {} 在资源探测阶段超时，可使用智能适配继续分析。",
                        manifest.id, manifest.version
                    );
                    if candidate.evidence > 0 && self.reuse_failures.len() < 6 {
                        self.reuse_failures.push(GeneratedAdapterReuseFailure {
                            id: manifest.id.clone(),
                            version: manifest.version.clone(),
                            failed_stage: phase.into(),
                            evidence: vec![message.clone()],
                            source_directory,
                            original_roundtrip_passed: false,
                            input_fingerprint: None,
                        });
                    }
                    EngineService::report(self.progress, "适配器检查超时", &message);
                    self.diagnostics.push(message);
                    break;
                }
                Err(error) => {
                    if (recognized || candidate.evidence > 0) && self.reuse_failures.len() < 6 {
                        self.reuse_failures.push(GeneratedAdapterReuseFailure {
                            id: manifest.id.clone(),
                            version: manifest.version.clone(),
                            failed_stage: phase.into(),
                            evidence: vec![error.to_string().chars().take(2000).collect()],
                            source_directory,
                            original_roundtrip_passed: false,
                            input_fingerprint: None,
                        });
                    }
                    let message = format!(
                        "适配器 {} {} 检查失败：{error}",
                        manifest.id, manifest.version
                    );
                    EngineService::report(self.progress, "适配器检查未通过", &message);
                    self.diagnostics.push(message);
                }
            }
        }
        Ok(None)
    }

    async fn verify_package(
        &self,
        candidate: &PackageCandidate,
        handle: &AdapterHandle,
        probe: &ProbeResult,
        previous_failures: &[GeneratedAdapterReuseFailure],
        source_directory: Option<&Path>,
        phase: &mut &'static str,
    ) -> Result<Option<GeneratedAdapterReuseFailure>, EngineError> {
        let manifest = &candidate.manifest;
        *phase = "fingerprint";
        EngineService::report(
            self.progress,
            "核对资源指纹",
            format!("{} 已识别资源，正在核对资源内容。", manifest.id),
        );
        tokio::task::yield_now().await;
        let inputs = &self.service.discovery_inputs;
        let original_files = probe
            .required_files
            .iter()
            .map(|path| Ok((path.clone(), inputs.file(self.source, path)?)))
            .collect::<Result<_, EngineError>>()?;
        let fingerprint = reuse_input_fingerprint(manifest, &original_files, probe)?;
        if self.verification == DiscoveryVerification::ProbeOnly
            && gamel10n_translation::files::read_json::<String>(
                &self.service.display_cache(&fingerprint),
            )
            .is_ok_and(|key| key == fingerprint)
        {
            return Ok(None);
        }
        if let Some(previous) = previous_failures.iter().find(|previous| {
            previous.id == manifest.id
                && previous.version == manifest.version
                && previous.input_fingerprint.as_deref() == Some(&fingerprint)
        }) {
            let mut previous = previous.clone();
            previous.source_directory = source_directory.map(Path::to_owned);
            return Ok(Some(previous));
        }
        *phase = "verify";
        EngineService::report(
            self.progress,
            "验证资源兼容性",
            format!("正在验证 {} 的文本提取、原文回填和中文修改。", manifest.id),
        );
        tokio::task::yield_now().await;
        let work = tempfile::tempdir()?;
        let original = crate::ReadOnlySource::open_excluding(
            self.source,
            &original_files,
            &inputs.excluded,
            self.cancellation,
        )?;
        let verified = self
            .service
            .verify_probed(
                handle,
                original.directory(),
                probe,
                work.path(),
                &[],
                self.cancellation,
                self.progress,
            )
            .await?;
        if verified.passed {
            return Ok(None);
        }
        let stage = verified
            .diagnostics
            .last()
            .and_then(|diagnostic| diagnostic.strip_prefix('['))
            .and_then(|text| text.split_once(']'))
            .map(|(stage, _)| stage)
            .filter(|stage| stage.len() < 80)
            .unwrap_or(phase)
            .to_owned();
        let stable = verified.cacheable
            && reuse_input_fingerprint(manifest, &original_files, probe)
                .ok()
                .as_deref()
                == Some(&fingerprint);
        Ok(Some(GeneratedAdapterReuseFailure {
            id: manifest.id.clone(),
            version: manifest.version.clone(),
            failed_stage: stage,
            evidence: verified
                .diagnostics
                .iter()
                .skip(verified.diagnostics.len().saturating_sub(3))
                .map(|item| item.chars().take(2000).collect())
                .collect(),
            source_directory: source_directory.map(Path::to_owned),
            original_roundtrip_passed: verified.original_roundtrip_passed,
            input_fingerprint: stable.then_some(fingerprint),
        }))
    }

    fn record_reuse_failure(&mut self, failure: GeneratedAdapterReuseFailure) {
        for evidence in &failure.evidence {
            let message = format!(
                "适配器 {} {} 未通过检查：{evidence}",
                failure.id, failure.version
            );
            EngineService::report(self.progress, "适配器检查未通过", &message);
            self.diagnostics.push(message);
        }
        if self.reuse_failures.len() < 6 {
            self.reuse_failures.push(failure);
        }
    }

    async fn probe_builtins(
        &mut self,
    ) -> Result<Option<(AdapterHandle, ProbeResult)>, EngineError> {
        let inventory = self.inventory;
        for (adapter, native_resources) in builtin_candidates(inventory) {
            let format = match &adapter {
                AdapterHandle::Native { format } => Some(format.as_str()),
                AdapterHandle::BuiltinSiglus => Some("Siglus"),
                _ => None,
            };
            let family = match &adapter {
                AdapterHandle::Native { format } => {
                    format!("engine.vntextpatch-{}", format.to_lowercase())
                }
                _ => adapter.id().to_string(),
            };
            if self.service.disabled_families.contains(&family)
                || self.selected.is_some_and(|(id, _)| id != family)
            {
                continue;
            }
            EngineService::report(
                self.progress,
                "检查内置解析器",
                format!("正在检查 {}。", adapter.id()),
            );
            let mut request = AdapterRequest::new("probe", self.source);
            if let Some(resources) = native_resources {
                inventory.require_complete_native_inventory()?;
                request.resource_files = Some(resources.into_iter().map(str::to_owned).collect());
            }
            let original = self.original_builtin_probe(&adapter, &mut request)?;
            if let Some(original) = &original {
                request.source_directory = original.directory().to_owned();
            }
            let probe = match self
                .service
                .execute(&adapter, &request, self.cancellation)
                .await
            {
                Ok(response) => response.probe,
                Err(error @ (EngineError::Cancelled | EngineError::Isolation(_))) => {
                    return Err(error);
                }
                Err(
                    error @ (EngineError::Io(_)
                    | EngineError::ToolMissing(_)
                    | EngineError::Timeout),
                ) if self.verification == DiscoveryVerification::ProbeOnly => return Err(error),
                Err(error) => {
                    self.record_builtin_failure(&adapter, format, "检查", &error);
                    continue;
                }
            };
            let Some(probe) = probe else { continue };
            if self
                .service
                .excluded_adapters
                .contains(&(probe.adapter_id.clone(), probe.adapter_version.clone()))
            {
                continue;
            }
            if let AdapterHandle::Native { format } = &adapter {
                record_builtin_probe(
                    &mut self.builtin_probes,
                    format,
                    "script-probe-passed",
                    probe.required_files.iter().take(8).cloned().collect(),
                    None,
                );
            }
            if self.verification == DiscoveryVerification::ProbeOnly {
                return Ok(Some((adapter, probe)));
            }

            let mut extract = AdapterRequest::new("extract", self.source);
            extract.resource_files = Some(probe.required_files.clone());
            EngineService::report(
                self.progress,
                "检查文本提取",
                format!("{} 已识别资源，正在检查文本提取。", adapter.id()),
            );
            match self
                .service
                .execute(&adapter, &extract, self.cancellation)
                .await
            {
                Ok(response) => {
                    if response.extraction.is_some_and(|result| {
                        result
                            .units
                            .iter()
                            .any(|unit| !unit.source_text.trim().is_empty())
                    }) {
                        return Ok(Some((adapter, probe)));
                    }
                }
                Err(error @ (EngineError::Cancelled | EngineError::Isolation(_))) => {
                    return Err(error);
                }
                Err(error) => self.record_builtin_failure(&adapter, format, "提取", &error),
            }
        }
        Ok(None)
    }

    fn original_builtin_probe(
        &self,
        adapter: &AdapterHandle,
        request: &mut AdapterRequest,
    ) -> Result<Option<crate::ReadOnlySource>, EngineError> {
        let inputs = &self.service.discovery_inputs;
        if self.selected.is_none() || (inputs.files.is_empty() && inputs.excluded.is_empty()) {
            return Ok(None);
        }
        // With a patch installed, probe and extract must see the same original resources.
        // Stage only the selected parser's files when the backups have different names.
        let paths = request.resource_files.get_or_insert_with(|| {
            self.inventory
                .files
                .iter()
                .filter(|file| {
                    let path = file.path.to_ascii_lowercase();
                    match adapter {
                        AdapterHandle::BuiltinSfa16 => matches!(
                            path.as_str(),
                            "scr.aos" | "sfa16.exe" | "sfa16.exe.le.config"
                        ),
                        AdapterHandle::BuiltinSiglus => {
                            matches!(path.as_str(), "scene.pck" | "gameexe.dat")
                                || (!path.contains('/')
                                    && (path.ends_with(".exe") || path.ends_with(".exe.le.config")))
                                || (path.starts_with("g00/_moji_") && path.ends_with(".g00"))
                        }
                        _ => false,
                    }
                })
                .map(|file| file.path.clone())
                .collect()
        });
        let files = paths
            .iter()
            .map(|path| Ok((path.clone(), inputs.file(self.source, path)?)))
            .collect::<Result<_, EngineError>>()?;
        crate::ReadOnlySource::open_excluding(
            self.source,
            &files,
            &inputs.excluded,
            self.cancellation,
        )
        .map(Some)
    }

    fn record_builtin_failure(
        &mut self,
        adapter: &AdapterHandle,
        format: Option<&str>,
        stage: &str,
        error: &EngineError,
    ) {
        if let Some(format) = format {
            record_builtin_probe(
                &mut self.builtin_probes,
                format,
                "failed",
                vec![],
                Some(error.to_string().chars().take(2000).collect()),
            );
        }
        let message = format!("内置解析器 {} {stage}失败：{error}", adapter.id());
        EngineService::report(self.progress, "适配器检查未通过", &message);
        self.diagnostics.push(message);
    }
}

fn builtin_candidates(
    inventory: &Inventory,
) -> impl Iterator<Item = (AdapterHandle, Option<Vec<&str>>)> {
    let has_file = |name: &str| {
        inventory
            .files
            .iter()
            .any(|file| file.path.eq_ignore_ascii_case(name))
    };
    let mut builtins = Vec::new();
    if has_file("scr.aos") && has_file("sfa16.exe") {
        builtins.push(AdapterHandle::BuiltinSfa16);
    }
    if has_file("scene.pck") && has_file("gameexe.dat") {
        builtins.push(AdapterHandle::BuiltinSiglus);
    }
    let native = native_candidates(inventory).map(|(format, resources)| {
        (
            AdapterHandle::Native {
                format: format.into(),
            },
            Some(resources),
        )
    });
    builtins
        .into_iter()
        .map(|adapter| (adapter, None))
        .chain(native)
}

fn record_builtin_probe(
    observations: &mut Vec<BuiltinAdapterProbeObservation>,
    format: &str,
    status: &str,
    resources: Vec<String>,
    diagnostic: Option<String>,
) {
    observations.retain(|observation| !observation.format.eq_ignore_ascii_case(format));
    observations.push(BuiltinAdapterProbeObservation {
        format: format.into(),
        status: status.into(),
        resources,
        diagnostic,
        observed_at: chrono::Utc::now(),
    });
}

fn reuse_input_fingerprint(
    manifest: &PackageManifest,
    files: &std::collections::BTreeMap<String, PathBuf>,
    probe: &ProbeResult,
) -> Result<String, EngineError> {
    let manifest_hash = hash_text(&serde_json::to_string(manifest)?);
    let resources = files
        .iter()
        .map(|(relative, path)| {
            hash_file(path)
                .map(|hash| (relative.clone(), hash))
                .map_err(|error| EngineError::Protocol(error.to_string()))
        })
        .collect::<Result<_, _>>()?;
    Ok(reuse_key(&manifest_hash, &resources, probe))
}

pub(super) fn reuse_key(
    manifest_hash: &str,
    resources: &std::collections::BTreeMap<String, String>,
    probe: &ProbeResult,
) -> String {
    let inputs = json!({
        "verifierRevision": super::VERIFIER_REVISION,
        "manifest": manifest_hash,
        "resources": resources,
        "resourceOrder":probe.required_files,
        "originalExecutable":probe.original_executable,
        "buildHints": probe.build_hints,
    });
    hash_text(&inputs.to_string())
}
