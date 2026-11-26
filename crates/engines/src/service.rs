//! UI-neutral engine execution and independent resource verification.

mod discover;
mod display;
mod verification;

pub(crate) const VERIFIER_REVISION: u32 = 24;

use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    ffi::OsString,
    fs::{self, File},
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant, SystemTime},
};

use gamel10n_translation::{
    TokenUsage, TranslationUnit,
    files::{hash_file, read_json, write_json},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    AdapterHandle, CancellationToken, EngineError, PackageManifest, ToolPaths, ToolProcess,
    discovery::{Inventory, resolve_relative},
    protocol::{AdapterRequest, AdapterResponse, BuildMode, ExtractionResult, ProbeResult},
    python::PythonAdapter,
    vntextpatch::VnTextPatch,
};

fn canonical_output(path: &Path) -> Result<PathBuf, EngineError> {
    let path = std::path::absolute(path)?;
    for ancestor in path.ancestors() {
        match ancestor.canonicalize() {
            Ok(root) => {
                let remaining = path.strip_prefix(ancestor).expect("ancestor of output");
                if remaining
                    .components()
                    .any(|c| !matches!(c, std::path::Component::Normal(_)))
                {
                    return Err(EngineError::Protocol("输出路径包含目录穿越。".into()));
                }
                return Ok(root.join(remaining));
            }
            Err(e)
                if e.kind() == std::io::ErrorKind::NotFound
                    && ancestor.symlink_metadata().is_err() =>
            {
                continue;
            }
            Err(e) => return Err(e.into()),
        }
    }
    Err(EngineError::Protocol("无法解析输出路径。".into()))
}

pub type ProgressCallback<'a> = Option<&'a (dyn Fn(EngineProgress) + Send + Sync)>;

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineProgress {
    #[serde(skip)]
    pub translation_preview: Option<Arc<ExtractionResult>>,
    pub stage: String,
    pub message: String,
    /// The message is model-authored and retains its punctuation in project logs.
    #[serde(skip)]
    pub message_from_model: bool,
    pub usage: TokenUsage,
    pub tool_name: Option<String>,
    pub turn: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_requests: Option<u64>,
    #[serde(default)]
    pub api_requests_estimated: bool,
    /// Refreshes the current status without adding a user-facing log entry.
    #[serde(default)]
    pub status_only: bool,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub progress_note: Value,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub last_breakthrough: Value,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveryResult {
    pub adapter: Option<AdapterHandle>,
    pub probe: Option<ProbeResult>,
    pub diagnostics: Vec<String>,
    pub file_count: usize,
    pub complete: bool,
    #[serde(default)]
    pub reuse_failures: Vec<GeneratedAdapterReuseFailure>,
    #[serde(default)]
    pub builtin_probes: Vec<BuiltinAdapterProbeObservation>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GeneratedAdapterReuseFailure {
    pub id: String,
    pub version: String,
    pub failed_stage: String,
    pub evidence: Vec<String>,
    #[serde(default)]
    pub source_directory: Option<PathBuf>,
    #[serde(default)]
    pub original_roundtrip_passed: bool,
    #[serde(default)]
    pub input_fingerprint: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BuiltinAdapterProbeObservation {
    pub format: String,
    pub status: String,
    pub resources: Vec<String>,
    pub diagnostic: Option<String>,
    pub observed_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VerificationResult {
    pub passed: bool,
    pub original_roundtrip_passed: bool,
    pub diagnostics: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub issues: Vec<crate::VerificationIssue>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub adapter_issues: Vec<gamel10n_translation::ValidationIssue>,
    pub units: usize,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extracted_sources: BTreeMap<String, usize>,
    pub runtime_tested: bool,
    pub resource_hashes: BTreeMap<String, String>,
    #[serde(default = "cacheable_verification")]
    pub cacheable: bool,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub stage_timings_ms: BTreeMap<String, u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archive_member_retry: Option<ArchiveMemberRetry>,
    /// Only the current invocation can hand off extracted text to the project.
    #[serde(skip)]
    pub verified_extraction: Option<Arc<VerifiedExtraction>>,
}

/// Inputs for reproducing a partial translation failure in a small PFS fixture.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArchiveMemberRetry {
    pub resource: String,
    pub members: BTreeSet<String>,
    pub translations: BTreeMap<String, String>,
}

impl ArchiveMemberRetry {
    fn for_speaker_difference(
        issue: &crate::VerificationIssue,
        unit: &TranslationUnit,
        units: &[TranslationUnit],
        translations: &BTreeMap<String, String>,
    ) -> Option<Self> {
        if !unit.resource_path.to_ascii_lowercase().ends_with(".pfs")
            || issue.code != crate::VerificationIssueCode::IdentityMismatch
            || issue.field_differences.len() != 1
            || issue.field_differences[0].field != "speaker"
            || translations.is_empty()
            || translations.len() > 2
            || translations.values().any(|value| value.len() > 4096)
        {
            return None;
        }
        let member = |unit: &TranslationUnit| {
            let (member, _) = unit.locator.rsplit_once(":block:")?;
            (unit.resource_path == issue.resource_path.as_deref()?
                && member.to_ascii_lowercase().ends_with(".ast"))
            .then(|| member.to_owned())
        };
        let mut members = BTreeSet::from([member(unit)?]);
        for id in translations.keys() {
            let translated = units.iter().find(|unit| unit.id == *id)?;
            members.insert(member(translated)?);
        }
        Some(Self {
            resource: unit.resource_path.clone(),
            members,
            translations: translations.clone(),
        })
    }
}

/// A successful verification's original text, bound to its package and resource bytes.
/// It is intentionally absent from saved sessions and published verification reports.
#[derive(Clone, Debug)]
pub struct VerifiedExtraction {
    adapter_fingerprint: String,
    resource_hashes: BTreeMap<String, String>,
    extraction: Arc<ExtractionResult>,
}

impl VerifiedExtraction {
    /// The caller supplies freshly checked snapshot hashes. Changed inputs use normal extraction.
    pub fn into_matching_extraction(
        self: Arc<Self>,
        service: &EngineService,
        adapter: &AdapterHandle,
        resource_hashes: &BTreeMap<String, String>,
    ) -> Result<Option<ExtractionResult>, EngineError> {
        if &self.resource_hashes != resource_hashes
            || verified_adapter_fingerprint(service, adapter)?.as_deref()
                != Some(self.adapter_fingerprint.as_str())
        {
            return Ok(None);
        }
        Ok(Some(Arc::unwrap_or_clone(
            Arc::unwrap_or_clone(self).extraction,
        )))
    }
}

fn verified_adapter_fingerprint(
    service: &EngineService,
    adapter: &AdapterHandle,
) -> Result<Option<String>, EngineError> {
    let AdapterHandle::Python {
        package_directory: Some(directory),
        id,
        version,
        entry,
    } = adapter
    else {
        return Ok(None);
    };
    if !directory.join("package.json").is_file() {
        return Ok(None);
    }
    let manifest = PackageManifest::load(directory)?;
    if &manifest.id != id || &manifest.version != version {
        return Ok(None);
    }
    let declared = manifest.handle(directory)?;
    if let AdapterHandle::Python {
        entry: declared_entry,
        ..
    } = declared
        && entry.canonicalize()? != declared_entry.canonicalize()?
    {
        return Ok(None);
    }
    service
        .verification_package_hash(&manifest, directory)
        .map(Some)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ValidationContext<'a> {
    schema_version: u32,
    source_directory: &'a Path,
    output_directory: &'a Path,
    resource_files: &'a [String],
    output_files: &'a [String],
    original_units: &'a [TranslationUnit],
    output_units: &'a [TranslationUnit],
}

fn cacheable_verification() -> bool {
    true
}

/// Reusable non-isolated helper script directory owned by an application or service instance,
/// automatically cleaned up when the last owner drops on normal exit.
#[derive(Clone, Debug, Default)]
pub struct SharedHelperDir(Arc<Mutex<Option<tempfile::TempDir>>>);

impl SharedHelperDir {
    pub fn new() -> Self {
        Self::default()
    }

    fn ensure_scripts(
        &self,
        source: &str,
        readonly_inputs: &str,
    ) -> Result<(PathBuf, PathBuf), EngineError> {
        let mut guard = self
            .0
            .lock()
            .map_err(|_| EngineError::Protocol("辅助脚本缓存不可用。".into()))?;
        let needs_init = guard.as_ref().is_none_or(|dir| {
            !dir.path().join("rust-host-v1.py").is_file()
                || !dir.path().join("readonly_inputs.py").is_file()
        });
        if needs_init {
            let directory = tempfile::tempdir()?;
            fs::write(directory.path().join("rust-host-v1.py"), source)?;
            fs::write(directory.path().join("readonly_inputs.py"), readonly_inputs)?;
            *guard = Some(directory);
        }
        let dir_path = guard
            .as_ref()
            .expect("shared helper initialized")
            .path()
            .to_path_buf();
        let helper = dir_path.join("rust-host-v1.py");
        Ok((dir_path, helper))
    }
}

#[derive(Clone, Debug)]
pub struct EngineService {
    pub asset_root: PathBuf,
    pub tools: ToolPaths,
    pub shared_directory: PathBuf,
    shared_helper_dir: SharedHelperDir,
    pub(crate) disabled_families: BTreeSet<String>,
    excluded_adapters: BTreeSet<(String, String)>,
    preferred_adapter: Option<String>,
    ordered_packages_only: bool,
    discovery_inputs: crate::adaptation::AdaptationInputs,
    pub(crate) build_identity: Option<(String, String)>,
    pub(crate) isolate_execution: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DiscoveryVerification {
    ProbeOnly,
    Complete,
}

impl EngineService {
    pub fn new(asset_root: impl Into<PathBuf>, tools: ToolPaths) -> Self {
        let shared_directory = tools.root.join("engine-tools");
        Self {
            asset_root: asset_root.into(),
            tools,
            shared_directory,
            shared_helper_dir: SharedHelperDir::new(),
            disabled_families: BTreeSet::new(),
            excluded_adapters: BTreeSet::new(),
            preferred_adapter: None,
            ordered_packages_only: false,
            discovery_inputs: Default::default(),
            build_identity: None,
            isolate_execution: cfg!(windows),
        }
    }

    pub fn with_shared_directory(mut self, directory: impl Into<PathBuf>) -> Self {
        self.shared_directory = directory.into();
        self
    }

    pub fn with_shared_helper_dir(mut self, directory: SharedHelperDir) -> Self {
        self.shared_helper_dir = directory;
        self
    }

    pub fn with_execution_isolation(mut self, enabled: bool) -> Self {
        self.isolate_execution = cfg!(windows) && enabled;
        self
    }

    /// Identifies the host binary in diagnostic logs without changing model context.
    pub fn with_build_identity(mut self, version: &str, git_hash: &str) -> Self {
        self.build_identity = Some((version.into(), git_hash.into()));
        self
    }

    /// Checks supplied packages in their given order without built-in fallback.
    pub fn with_ordered_packages_only(mut self) -> Self {
        self.ordered_packages_only = true;
        self
    }

    /// Probe and verify published packages against the original resources behind an installed patch.
    pub fn with_discovery_inputs(mut self, inputs: crate::adaptation::AdaptationInputs) -> Self {
        self.discovery_inputs = inputs;
        self
    }

    pub fn with_disabled_families(mut self, families: impl IntoIterator<Item = String>) -> Self {
        self.disabled_families = families.into_iter().collect();
        self
    }

    /// Keeps an existing project's adapter ahead of newly available built-in profiles.
    pub fn with_preferred_adapter(mut self, adapter_id: Option<String>) -> Self {
        self.preferred_adapter = adapter_id;
        self
    }

    /// Skips candidates already found incompatible during this recognition attempt.
    pub fn with_excluded_adapters(
        mut self,
        adapters: impl IntoIterator<Item = (String, String)>,
    ) -> Self {
        self.excluded_adapters = adapters.into_iter().collect();
        self
    }

    /// Checks a published package and its pinned shared dependencies without reading a game.
    pub async fn verify_package(
        &self,
        directory: &Path,
        cancellation: &CancellationToken,
    ) -> Result<VerificationResult, EngineError> {
        if cancellation.is_cancelled() {
            return Err(EngineError::Cancelled);
        }
        let manifest = PackageManifest::load(directory)?;
        self.verify_package_handle(directory, &manifest, cancellation)
            .await?;
        let files: Vec<_> = manifest
            .package_files
            .keys()
            .filter(|file| file.ends_with(".py"))
            .map(|file| resolve_relative(directory, file))
            .collect::<Result<_, _>>()?;
        self.helper(&json!({"operation":"syntax","files":files}), cancellation)
            .await?;
        Ok(VerificationResult {
            passed: true,
            diagnostics: vec![
                "发布文件哈希、Python 语法及固定依赖校验通过；本次未执行游戏资源验收或运行游戏。"
                    .into(),
            ],
            ..VerificationResult::default()
        })
    }

    /// Checks the published files and freshly hashes pinned dependencies before manual verification.
    pub async fn verify_package_handle(
        &self,
        directory: &Path,
        manifest: &PackageManifest,
        cancellation: &CancellationToken,
    ) -> Result<AdapterHandle, EngineError> {
        let adapter = manifest.handle(directory)?;
        self.tool_paths_checked(Some(directory), true, cancellation)
            .await?;
        Ok(adapter)
    }

    pub(crate) fn report(progress: ProgressCallback<'_>, stage: &str, message: impl Into<String>) {
        if let Some(progress) = progress {
            progress(EngineProgress {
                stage: stage.into(),
                message: message.into(),
                ..EngineProgress::default()
            });
        }
    }

    pub(crate) async fn helper(
        &self,
        request: &Value,
        cancellation: &CancellationToken,
    ) -> Result<Value, EngineError> {
        const SOURCE: &str = include_str!("../python/host.py");
        const READONLY_INPUTS: &str = include_str!("../python/readonly_inputs.py");
        let isolated = self.isolate_execution
            && matches!(request["operation"].as_str(), Some("run" | "run_tool"));
        let (_fresh_dir, helper_dir, helper) = if isolated {
            // Fresh files inherit grantable ACLs instead of retaining permissions from an old install.
            let directory = tempfile::tempdir()?;
            let helper = directory.path().join("rust-host-v1.py");
            fs::write(&helper, SOURCE)?;
            fs::write(directory.path().join("readonly_inputs.py"), READONLY_INPUTS)?;
            let dir_path = directory.path().to_path_buf();
            (Some(directory), dir_path, helper)
        } else {
            let (dir_path, helper) = self
                .shared_helper_dir
                .ensure_scripts(SOURCE, READONLY_INPUTS)?;
            (None, dir_path, helper)
        };
        let mut process = ToolProcess::new(self.tools.python())
            .with_arguments([
                OsString::from("-I"),
                OsString::from("-X"),
                OsString::from("utf8"),
                helper.into_os_string(),
            ])
            .with_timeout(Duration::from_secs(
                request
                    .get("timeoutSeconds")
                    .and_then(Value::as_u64)
                    .unwrap_or(600)
                    .clamp(1, 600)
                    + 10,
            ));
        if isolated {
            fs::write(
                helper_dir.join("isolation.py"),
                include_str!("../python/isolation.py"),
            )?;
            process = process.with_arguments([OsString::from("--isolated")]);
            let workspace = PathBuf::from(
                request["workspace"]
                    .as_str()
                    .ok_or_else(|| EngineError::Protocol("缺少执行工作目录。".into()))?,
            );
            let mut read_paths = vec![self.tools.root.clone(), helper_dir];
            let mut input_paths = Vec::new();
            if let Some(paths) = request["resourcePaths"].as_object() {
                input_paths.extend(paths.values().filter_map(Value::as_str).map(PathBuf::from));
            }
            // Only run_engine_python supplies these host-resolved paths. run_tool
            // arguments must not become filesystem grants through extra JSON keys.
            if request["operation"] == "run" {
                if let Some(path) = request["resourceDirectory"]
                    .as_str()
                    .filter(|p| !p.is_empty())
                {
                    input_paths.push(path.into());
                }
                if let Some(path) = request["library"].as_str().filter(|path| !path.is_empty()) {
                    read_paths.push(path.into());
                }
                for key in ["hostCommand", "vnTextPatchCommand"] {
                    if let Some(executable) = request["toolPaths"][key][0].as_str() {
                        let path = Path::new(executable);
                        if path.is_absolute()
                            && path.is_file()
                            && let Some(parent) = path.parent()
                        {
                            read_paths.push(parent.into());
                        }
                    }
                }
            }
            let work = resolve_relative(&workspace, "work")?;
            fs::create_dir_all(&work)?;
            read_paths.push(workspace.clone());
            process = process
                .with_current_directory(&workspace)
                .with_isolation(self.tools.isolated_runner(), read_paths, vec![work])
                .with_isolated_inputs(input_paths);
        }
        let result: Value = process.execute(request, cancellation).await?;
        if !result
            .get("success")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            return Err(EngineError::Rejected(
                result
                    .get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("Python 辅助操作失败。")
                    .into(),
            ));
        }
        Ok(result.get("result").cloned().unwrap_or(Value::Null))
    }

    pub(crate) async fn inspect_native(
        &self,
        request: &Value,
        cancellation: &CancellationToken,
    ) -> Result<Value, EngineError> {
        let mut request = request.clone();
        request["protocolVersion"] = json!(1);
        let response: Value = ToolProcess::new(self.tools.vntextpatch())
            .with_arguments([OsString::from("inspect")])
            .execute(&request, cancellation)
            .await?;
        if response["success"].as_bool() != Some(true) {
            return Err(EngineError::Rejected(
                response["error"]
                    .as_str()
                    .unwrap_or("静态分析失败。")
                    .into(),
            ));
        }
        Ok(response["result"].clone())
    }

    pub(crate) async fn siglus(
        &self,
        cancellation: &CancellationToken,
    ) -> Result<Value, EngineError> {
        self.siglus_checked(false, cancellation).await
    }

    async fn siglus_checked(
        &self,
        force_rehash: bool,
        cancellation: &CancellationToken,
    ) -> Result<Value, EngineError> {
        let pin = self.tools.root.join("engine-tools/siglus.json");
        if pin.is_file() {
            let reference: Value =
                read_json(&pin).map_err(|error| EngineError::Protocol(error.to_string()))?;
            let library = self
                .reference_library_checked(&reference, force_rehash)
                .await?;
            return self.helper(&json!({"operation":"prepare_siglus_zip","library":library,"shared":self.shared_directory,"reference":reference}),cancellation).await;
        }
        self.helper(&json!({"operation":"prepare_siglus","assets":self.asset_root,"shared":self.shared_directory}), cancellation).await
    }

    pub(crate) fn sfa16_entry(&self) -> Result<PathBuf, EngineError> {
        let root = self.shared_directory.join("builtins/sfa16-v2");
        fs::create_dir_all(&root)?;
        let entry = root.join("adapter.py");
        let source = include_str!("../python/sfa16.py");
        if fs::read_to_string(&entry).ok().as_deref() != Some(source) {
            fs::write(&entry, source)?;
        }
        Ok(entry)
    }

    pub(crate) fn kirikiri_smil_entry(&self) -> Result<PathBuf, EngineError> {
        let root = self.shared_directory.join("builtins/kirikiri-smil-v1");
        fs::create_dir_all(&root)?;
        let files: &[(&str, &[u8])] = &[
            (
                "adapter.py",
                include_bytes!("../../../adapters/kirikiri/smil_adapter.py"),
            ),
            (
                "xp3_adler.py",
                include_bytes!("../../../adapters/components/xp3_adler.py"),
            ),
            (
                "xp3_smil.py",
                include_bytes!("../../../adapters/components/xp3_smil.py"),
            ),
            (
                "kirikiri_scn_text.py",
                include_bytes!("../../../adapters/components/kirikiri_scn_text.py"),
            ),
            (
                "kirikiri_text.py",
                include_bytes!("../../../adapters/kirikiri/kirikiri_text.py"),
            ),
            (
                "gamel10n_host.py",
                include_bytes!("../../../adapters/components/gamel10n_host.py"),
            ),
        ];
        for (name, source) in files {
            let path = root.join(name);
            if fs::read(&path).ok().as_deref() != Some(*source) {
                fs::write(path, source)?;
            }
        }
        Ok(root.join("adapter.py"))
    }

    pub(crate) fn current_reference(&self) -> Result<Value, EngineError> {
        for root in [
            &self.shared_directory,
            &self.tools.root.join("engine-tools"),
        ] {
            let current = root.join("current.json");
            if current.is_file() {
                return read_json(&current)
                    .map_err(|error| EngineError::Protocol(error.to_string()));
            }
        }
        Err(EngineError::Rejected(
            "缺少固定版本的共享引擎工具库，请通过设置恢复工具文件。".into(),
        ))
    }

    pub(crate) fn reference_library(&self, reference: &Value) -> Result<PathBuf, EngineError> {
        self.resolve_reference_library(reference, false)
    }

    async fn reference_library_checked(
        &self,
        reference: &Value,
        force_rehash: bool,
    ) -> Result<PathBuf, EngineError> {
        if !force_rehash {
            return self.reference_library(reference);
        }
        let service = self.clone();
        let reference = reference.clone();
        tokio::task::spawn_blocking(move || service.resolve_reference_library(&reference, true))
            .await
            .map_err(|error| EngineError::Protocol(error.to_string()))?
    }

    fn resolve_reference_library(
        &self,
        reference: &Value,
        force_rehash: bool,
    ) -> Result<PathBuf, EngineError> {
        let revision = reference
            .get("revision")
            .and_then(Value::as_str)
            .ok_or_else(|| EngineError::Protocol("引擎依赖缺少 revision。".into()))?;
        if revision.len() != 40 || !revision.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(EngineError::Protocol("引擎依赖版本无效。".into()));
        }
        let relative = format!("references/{revision}/library.zip");
        let expected = reference
            .get("librarySha256")
            .and_then(Value::as_str)
            .ok_or_else(|| EngineError::Protocol("引擎依赖缺少文件哈希。".into()))?;
        for root in [
            &self.shared_directory,
            &self.tools.root.join("engine-tools"),
        ] {
            let path = resolve_relative(root, &relative)?;
            if path.is_file() {
                if verified_file_hash(&path, force_rehash)? != expected {
                    return Err(EngineError::Rejected(
                        "共享引擎工具库内容已变化，请恢复固定版本。".into(),
                    ));
                }
                return Ok(path);
            }
        }
        Err(EngineError::Rejected(format!(
            "缺少固定版本引擎库 {revision}，请通过设置恢复工具文件。"
        )))
    }

    pub(crate) async fn tool_paths(
        &self,
        package: Option<&Path>,
        cancellation: &CancellationToken,
    ) -> Result<Value, EngineError> {
        self.tool_paths_checked(package, false, cancellation).await
    }

    async fn tool_paths_checked(
        &self,
        package: Option<&Path>,
        force_rehash: bool,
        cancellation: &CancellationToken,
    ) -> Result<Value, EngineError> {
        let mut paths = json!({"python":self.tools.python()});
        let mut native = Some(self.tools.vntextpatch());
        if let Some(package) = package {
            let native_pin = package.join(crate::native_tools::PIN_PATH);
            let has_native_pin = native_pin.is_file();
            if has_native_pin {
                let pin: Value = read_json(&native_pin)
                    .map_err(|error| EngineError::Protocol(error.to_string()))?;
                native = Some(self.resolve_native_tools(&pin)?);
            }
            let dependency = package.join("tools/engine-reference.json");
            if dependency.is_file() {
                let pin: Value = read_json(&dependency)
                    .map_err(|error| EngineError::Protocol(error.to_string()))?;
                if pin["nativePinRequired"] == true && !has_native_pin {
                    native = None;
                }
                let runtime: Value = read_json(&self.tools.root.join("python/runtime.json"))
                    .map_err(|error| EngineError::Protocol(error.to_string()))?;
                if pin.get("pythonSha256").and_then(Value::as_str)
                    != runtime.get("archiveSha256").and_then(Value::as_str)
                    || pin.get("pythonVersion").and_then(Value::as_str) != Some("3.14.10")
                {
                    return Err(EngineError::Rejected(
                        "适配器固定的 Python 版本与当前工具不一致，请恢复原版本。".into(),
                    ));
                }
                if let Some(files) = runtime.get("files").and_then(Value::as_object) {
                    let python_root = self.tools.root.join("python");
                    let checks: Vec<(String, String)> = files
                        .iter()
                        .map(|(relative, expected)| {
                            (relative.clone(), expected.as_str().unwrap_or("").to_owned())
                        })
                        .collect();
                    tokio::task::spawn_blocking(move || {
                        for (relative, expected) in checks {
                            let path = resolve_relative(&python_root, &relative)?;
                            if verified_file_hash(&path, force_rehash)? != expected {
                                return Err(EngineError::Rejected(format!(
                                    "共享 Python 文件已变化：{relative}"
                                )));
                            }
                        }
                        Ok::<_, EngineError>(())
                    })
                    .await
                    .map_err(|error| EngineError::Protocol(error.to_string()))??;
                }
                let reference = pin
                    .get("reference")
                    .ok_or_else(|| EngineError::Protocol("依赖缺少 reference。".into()))?;
                let library = if reference.get("kind").and_then(Value::as_str) == Some("siglus") {
                    let prepared = self.siglus_checked(force_rehash, cancellation).await?;
                    if reference.get("revision") != prepared.get("revision") {
                        return Err(EngineError::Rejected(
                            "缺少此版本的 Siglus 工具，请恢复原工具文件。".into(),
                        ));
                    }
                    PathBuf::from(
                        prepared["library"]
                            .as_str()
                            .ok_or_else(|| EngineError::Protocol("Siglus 工具路径无效。".into()))?,
                    )
                } else {
                    self.reference_library_checked(reference, force_rehash)
                        .await?
                };
                paths["engineLibrary"] = json!(library);
            }
        }
        if let Some(executable) = native {
            paths["vnTextPatch"] = json!(executable);
            paths["vnTextPatchCommand"] = json!([executable]);
            paths["hostCommand"] = json!([executable]);
        }
        Ok(paths)
    }

    pub async fn execute(
        &self,
        adapter: &AdapterHandle,
        request: &AdapterRequest,
        cancellation: &CancellationToken,
    ) -> Result<AdapterResponse, EngineError> {
        self.execute_with_hashes(adapter, request, None, None, cancellation)
            .await
    }

    /// Supplies previously checked source and patch hashes to the adapter's integrity checks.
    /// A passing validation rechecks the entire supplied source baseline.
    /// Callers must keep the checked inputs stable until execution starts.
    pub async fn execute_with_hashes(
        &self,
        adapter: &AdapterHandle,
        request: &AdapterRequest,
        expected_source_hashes: Option<&BTreeMap<String, String>>,
        expected_patch_hashes: Option<&BTreeMap<String, String>>,
        cancellation: &CancellationToken,
    ) -> Result<AdapterResponse, EngineError> {
        let mut request = request.clone();
        if let Some(output) = &request.output_directory {
            let source = request.source_directory.canonicalize()?;
            let output = canonical_output(output)?;
            if output == source || output.starts_with(&source) || source.starts_with(&output) {
                return Err(EngineError::Protocol(
                    "适配器输出目录必须与源目录分离。".into(),
                ));
            }
        }
        if let Some(files) = &request.resource_files {
            for relative in files {
                resolve_relative(&request.source_directory, relative)?;
            }
        }
        if let Some(output) = request
            .output_directory
            .as_ref()
            .filter(|_| request.operation == "extract")
        {
            if request.extra.get("buildMode").and_then(Value::as_str) != Some("overlay")
                || !matches!(adapter, AdapterHandle::Python { .. })
            {
                return Err(EngineError::Protocol(
                    "结合补丁提取文本需要 Python 适配器显式支持 overlay 模式。".into(),
                ));
            }
            let files: Vec<String> = serde_json::from_value(
                request
                    .extra
                    .get("outputFiles")
                    .cloned()
                    .unwrap_or(Value::Null),
            )?;
            for relative in files {
                if !resolve_relative(output, &relative)?.is_file() {
                    return Err(EngineError::Protocol(format!(
                        "覆盖包声明的文件不存在：{relative}"
                    )));
                }
            }
        }
        let source_hashes = if matches!(
            adapter,
            AdapterHandle::Python { .. } | AdapterHandle::BuiltinKirikiriSmil
        ) && request.operation != "probe"
        {
            match request.resource_files.as_ref() {
                Some(files) => Some(match expected_source_hashes {
                    Some(expected) => selected_hashes(expected, files)?,
                    None => {
                        let source_dir = request.source_directory.clone();
                        let files = files.clone();
                        tokio::task::spawn_blocking(move || hashes(&source_dir, &files))
                            .await
                            .map_err(|error| EngineError::Protocol(error.to_string()))??
                    }
                }),
                None => None,
            }
        } else {
            None
        };
        let patch_hashes = if matches!(request.operation.as_str(), "validate" | "extract") {
            match request.output_directory.as_ref() {
                Some(output) => Some(match expected_patch_hashes {
                    Some(expected) => expected.clone(),
                    None => {
                        let output = output.clone();
                        tokio::task::spawn_blocking(move || directory_hashes(&output))
                            .await
                            .map_err(|error| EngineError::Protocol(error.to_string()))??
                    }
                }),
                None => None,
            }
        } else {
            None
        };
        let response = match adapter {
            AdapterHandle::Native { format } => {
                request.format = Some(format.clone());
                VnTextPatch::new(&self.tools)
                    .execute(&request, cancellation)
                    .await
            }
            AdapterHandle::Python {
                entry,
                package_directory,
                ..
            } => {
                if let Some(package) = package_directory {
                    if package.join("package.json").is_file() {
                        PackageManifest::load(package)?.handle(package)?;
                    }
                    request
                        .extra
                        .insert("adapterDirectory".into(), json!(package));
                }
                request.extra.insert(
                    "toolPaths".into(),
                    self.tool_paths(package_directory.as_deref(), cancellation)
                        .await?,
                );
                PythonAdapter::new(&self.tools, entry)?
                    .with_execution_isolation(self.isolate_execution)
                    .execute_with_assets(self, &request, cancellation)
                    .await
            }
            AdapterHandle::BuiltinSiglus => {
                let prepared = self.siglus(cancellation).await?;
                let entry = PathBuf::from(
                    prepared["entry"]
                        .as_str()
                        .ok_or_else(|| EngineError::Protocol("Siglus 入口无效。".into()))?,
                );
                request.extra.insert(
                    "toolPaths".into(),
                    json!({"engineLibrary":prepared["library"],"python":self.tools.python()}),
                );
                PythonAdapter::new(&self.tools, &entry)?
                    .with_execution_isolation(false)
                    .execute(&request, cancellation)
                    .await
            }
            AdapterHandle::BuiltinSfa16 => {
                PythonAdapter::new(&self.tools, &self.sfa16_entry()?)?
                    .with_execution_isolation(false)
                    .execute(&request, cancellation)
                    .await
            }
            AdapterHandle::BuiltinKirikiriSmil => {
                let entry = self.kirikiri_smil_entry()?;
                let reference: Value = serde_json::from_str(include_str!(
                    "../../../third_party/engine-tools/current.json"
                ))?;
                let library = self.reference_library(&reference)?;
                request.extra.insert(
                    "toolPaths".into(),
                    json!({"engineLibrary":library,"python":self.tools.python()}),
                );
                PythonAdapter::new(&self.tools, &entry)?
                    .with_execution_isolation(false)
                    .execute(&request, cancellation)
                    .await
            }
        };
        if response
            .as_ref()
            .is_err_and(|error| !matches!(error, EngineError::Incompatible { .. }))
        {
            return response;
        }
        let validated_sources = expected_source_hashes.filter(|_| {
            request.operation == "validate"
                && response.as_ref().is_ok_and(|response| {
                    response
                        .validation
                        .as_ref()
                        .is_some_and(|validation| validation.passed && validation.issues.is_empty())
                })
        });
        if let Some(expected) = validated_sources.or(source_hashes.as_ref()) {
            let source_dir = request.source_directory.clone();
            let files: Vec<_> = expected.keys().cloned().collect();
            let actual = tokio::task::spawn_blocking(move || hashes(&source_dir, &files))
                .await
                .map_err(|error| EngineError::Protocol(error.to_string()))??;
            if actual != *expected {
                return Err(EngineError::Rejected(
                    "适配器修改了只读源资源；操作未被接受。".into(),
                ));
            }
        }
        if let (Some(expected), Some(output)) = (patch_hashes, &request.output_directory) {
            let output = output.clone();
            let actual = tokio::task::spawn_blocking(move || directory_hashes(&output))
                .await
                .map_err(|error| EngineError::Protocol(error.to_string()))??;
            if actual != expected {
                return Err(EngineError::Rejected(
                    "适配器在读取或校验期间修改了补丁输出。".into(),
                ));
            }
        }
        let mut response = response?;
        // A script may report its implementation version; project pins use the immutable package version.
        if let (
            AdapterHandle::Python {
                package_directory: Some(package),
                id,
                version,
                ..
            },
            Some(probe),
        ) = (adapter, response.probe.as_mut())
            && package.join("package.json").is_file()
        {
            probe.adapter_id.clone_from(id);
            probe.adapter_version.clone_from(version);
        }
        if let Some(probe) = &response.probe {
            if let Some(launch) = &probe.launch {
                launch.validate_relative()?;
            }
            if probe.required_files.is_empty() {
                return Err(EngineError::Protocol("适配器没有选择必需资源。".into()));
            }
            for relative in &probe.required_files {
                resolve_relative(&request.source_directory, relative)?;
                if request.resource_files.as_ref().is_some_and(|files| {
                    !files.iter().any(|file| file.eq_ignore_ascii_case(relative))
                }) {
                    return Err(EngineError::Protocol(format!(
                        "适配器选择了候选以外的资源：{relative}"
                    )));
                }
            }
            let normalized: BTreeSet<_> = probe
                .required_files
                .iter()
                .map(|path| path.replace('\\', "/").to_lowercase())
                .collect();
            if normalized.len() != probe.required_files.len() {
                return Err(EngineError::Protocol("适配器返回重复的必需资源。".into()));
            }
            if let Some(executable) = &probe.original_executable {
                resolve_relative(&request.source_directory, executable)?;
                if !normalized.contains(&executable.replace('\\', "/").to_lowercase()) {
                    return Err(EngineError::Protocol(
                        "原启动程序必须包含在 requiredFiles 中。".into(),
                    ));
                }
            }
        }
        if let Some(extraction) = &mut response.extraction {
            validate_raw_units(&extraction.units, &request.source_directory)?;
            if matches!(
                adapter,
                AdapterHandle::Python { .. } | AdapterHandle::BuiltinKirikiriSmil
            ) {
                let allowed: BTreeSet<_> = if let Some(resources) = &request.resource_files {
                    resources
                        .iter()
                        .map(|path| path.replace('\\', "/").to_lowercase())
                        .collect()
                } else {
                    Inventory::scan(&request.source_directory)?
                        .files
                        .into_iter()
                        .map(|file| file.path.to_lowercase())
                        .collect()
                };
                for unit in &extraction.units {
                    if unit.id.encode_utf16().count() > 512 {
                        return Err(EngineError::Incompatible {
                            stage: "extraction",
                            message: "提取的文本 ID 超过 512 字符。".into(),
                        });
                    }
                    if !allowed.contains(&unit.resource_path.replace('\\', "/").to_lowercase()) {
                        return Err(EngineError::Incompatible {
                            stage: "extraction",
                            message: format!("提取的文字不属于所选资源：{}", unit.resource_path),
                        });
                    }
                }
                if !extraction
                    .units
                    .iter()
                    .any(|unit| !unit.source_text.trim().is_empty())
                {
                    return Err(EngineError::Incompatible {
                        stage: "extraction",
                        message: "适配器返回的提取记录为空或全部为纯空白，没有可翻译内容。".into(),
                    });
                }
                extraction
                    .units
                    .retain(|unit| !unit.source_text.trim().is_empty());
            }
        }
        if let Some(validation) = &response.validation
            && validation.passed
            && !validation.issues.is_empty()
        {
            return Err(EngineError::Protocol(
                "适配器同时报告校验通过和阻塞问题。".into(),
            ));
        }
        if let Some(build) = &response.build {
            if build.mode == BuildMode::Overlay && !matches!(adapter, AdapterHandle::Python { .. })
            {
                return Err(EngineError::Protocol(
                    "覆盖包模式需要 Python 适配器。".into(),
                ));
            }
            let output = request
                .output_directory
                .as_ref()
                .ok_or_else(|| EngineError::Protocol("构建缺少输出目录。".into()))?;
            let declared: BTreeSet<_> =
                build.files.iter().map(|path| path.to_lowercase()).collect();
            if declared.len() != build.files.len() {
                return Err(EngineError::Protocol("补丁包含重复文件路径。".into()));
            }
            for relative in &build.files {
                if build.mode == BuildMode::Overlay
                    && resolve_relative(&request.source_directory, relative)?.exists()
                {
                    return Err(EngineError::Protocol(format!(
                        "覆盖包不能替换原资源，请使用新增路径或 replace 模式：{relative}"
                    )));
                }
                if !resolve_relative(output, relative)?.is_file() {
                    return Err(EngineError::Protocol(format!(
                        "适配器声明的输出不存在：{relative}"
                    )));
                }
            }
            let launch = build.launch_target()?;
            if let Some(launch) = &launch
                && launch.location == crate::protocol::LaunchLocation::Patch
                && !declared.contains(&launch.configuration.executable.to_lowercase())
            {
                return Err(EngineError::Protocol(
                    "补丁启动程序必须包含在声明输出中。".into(),
                ));
            }
            if let Some(executable) = &build.launch_executable
                && (!declared.contains(&executable.to_lowercase())
                    || !executable.to_lowercase().ends_with(".exe"))
            {
                return Err(EngineError::Protocol(
                    "补丁启动程序必须是声明输出中的 EXE。".into(),
                ));
            }
            let inventory = Inventory::scan(output)?;
            if !inventory.complete || crate::discovery::is_link(&output.symlink_metadata()?) {
                return Err(EngineError::Protocol("无法完整核对补丁输出目录。".into()));
            }
            for file in inventory.files {
                if !declared.contains(&file.path.to_lowercase()) {
                    return Err(EngineError::Protocol(format!(
                        "补丁包含未声明输出：{}",
                        file.path
                    )));
                }
                let extension = Path::new(&file.path)
                    .extension()
                    .and_then(|value| value.to_str())
                    .unwrap_or("")
                    .to_lowercase();
                if ["exe", "bat", "cmd", "ps1"].contains(&extension.as_str())
                    && launch.is_none()
                    && !resolve_relative(&request.source_directory, &file.path)?.is_file()
                {
                    return Err(EngineError::Protocol(
                        "可执行补丁输出必须声明明确的 EXE 启动入口。".into(),
                    ));
                }
                if extension == "exe" {
                    let path = resolve_relative(output, &file.path)?;
                    let original = resolve_relative(&request.source_directory, &file.path)?
                        .is_file()
                        .then(|| request.source_directory.join(&file.path));
                    let original = if matches!(adapter, AdapterHandle::BuiltinSfa16)
                        && file.path.eq_ignore_ascii_case("sfa16.zh.exe")
                    {
                        Some(request.source_directory.join("sfa16.exe"))
                    } else {
                        original
                    };
                    let checked=self.inspect_native(&json!({"operation":"executable_check","path":path,"original":original}),cancellation).await?;
                    if checked["passed"].as_bool() != Some(true) {
                        return Err(EngineError::Rejected(format!(
                            "补丁 EXE 静态加载检查未通过：{}",
                            checked["issues"]
                        )));
                    }
                }
            }
        }
        Ok(response)
    }

    pub async fn discover(
        &self,
        source: &Path,
        packages: &[PathBuf],
        cancellation: &CancellationToken,
        progress: ProgressCallback<'_>,
    ) -> Result<DiscoveryResult, EngineError> {
        Self::report(progress, "扫描游戏资源", "正在读取目录与格式线索。");
        let inventory = Inventory::scan(source)?;
        self.discover_with_inventory(source, packages, &inventory, cancellation, progress)
            .await
    }

    pub async fn discover_with_inventory(
        &self,
        source: &Path,
        packages: &[PathBuf],
        inventory: &Inventory,
        cancellation: &CancellationToken,
        progress: ProgressCallback<'_>,
    ) -> Result<DiscoveryResult, EngineError> {
        self.discover_with_inventory_and_failures(
            source,
            packages,
            inventory,
            &[],
            DiscoveryVerification::ProbeOnly,
            None,
            cancellation,
            progress,
        )
        .await
    }

    pub async fn discover_with_failures(
        &self,
        source: &Path,
        packages: &[PathBuf],
        previous_failures: &[GeneratedAdapterReuseFailure],
        cancellation: &CancellationToken,
        progress: ProgressCallback<'_>,
    ) -> Result<DiscoveryResult, EngineError> {
        let inventory = Inventory::scan(source)?;
        self.discover_with_inventory_and_failures(
            source,
            packages,
            &inventory,
            previous_failures,
            DiscoveryVerification::Complete,
            None,
            cancellation,
            progress,
        )
        .await
    }

    pub async fn probe_bounded(
        &self,
        adapter: &AdapterHandle,
        source: &Path,
        resources: &[String],
        cancellation: &CancellationToken,
    ) -> Result<Option<ProbeResult>, EngineError> {
        self.probe_bounded_with_evidence(adapter, source, resources, cancellation)
            .await
            .map(|(probe, _)| probe)
    }

    pub(crate) async fn probe_bounded_with_evidence(
        &self,
        adapter: &AdapterHandle,
        source: &Path,
        resources: &[String],
        cancellation: &CancellationToken,
    ) -> Result<(Option<ProbeResult>, Value), EngineError> {
        self.probe_bounded_with_limit(
            adapter,
            source,
            resources,
            &crate::adaptation::AdaptationInputs::default(),
            cancellation,
            Duration::from_secs(45),
        )
        .await
    }

    pub async fn probe_bounded_with_inputs(
        &self,
        adapter: &AdapterHandle,
        source: &Path,
        resources: &[String],
        inputs: &crate::adaptation::AdaptationInputs,
        cancellation: &CancellationToken,
    ) -> Result<Option<ProbeResult>, EngineError> {
        self.probe_inputs_with_evidence(adapter, source, resources, inputs, cancellation)
            .await
            .map(|(probe, _)| probe)
    }

    pub(crate) async fn probe_inputs_with_evidence(
        &self,
        adapter: &AdapterHandle,
        source: &Path,
        resources: &[String],
        inputs: &crate::adaptation::AdaptationInputs,
        cancellation: &CancellationToken,
    ) -> Result<(Option<ProbeResult>, Value), EngineError> {
        self.probe_bounded_with_limit(
            adapter,
            source,
            resources,
            inputs,
            cancellation,
            Duration::from_secs(45),
        )
        .await
    }

    async fn probe_bounded_with_limit(
        &self,
        adapter: &AdapterHandle,
        source: &Path,
        resources: &[String],
        inputs: &crate::adaptation::AdaptationInputs,
        cancellation: &CancellationToken,
        limit: Duration,
    ) -> Result<(Option<ProbeResult>, Value), EngineError> {
        let budget = crate::process::ProcessBudget::new(limit);
        self.probe_bounded_with_budget(adapter, source, resources, inputs, cancellation, &budget)
            .await
    }

    async fn probe_bounded_with_budget(
        &self,
        adapter: &AdapterHandle,
        source: &Path,
        resources: &[String],
        inputs: &crate::adaptation::AdaptationInputs,
        cancellation: &CancellationToken,
        budget: &Arc<crate::process::ProcessBudget>,
    ) -> Result<(Option<ProbeResult>, Value), EngineError> {
        let deadline = cancellation.child_token();
        let operation = crate::process::ProcessBudget::scope(
            Arc::clone(budget),
            self.probe_bounded_inner(adapter, source, resources, inputs, &deadline),
        );
        tokio::pin!(operation);
        tokio::select! {
            result = &mut operation => result,
            () = budget.expired() => {
                deadline.cancel();
                let _ = operation.await;
                Err(EngineError::Timeout)
            }
        }
    }

    async fn probe_bounded_inner(
        &self,
        adapter: &AdapterHandle,
        source: &Path,
        resources: &[String],
        inputs: &crate::adaptation::AdaptationInputs,
        cancellation: &CancellationToken,
    ) -> Result<(Option<ProbeResult>, Value), EngineError> {
        let staged = tempfile::tempdir()?;
        let mut files = Vec::new();
        for relative in resources {
            if cancellation.is_cancelled() {
                return Err(EngineError::Cancelled);
            }
            let path = inputs.file(source, relative)?;
            let length = path.metadata()?.len();
            let target = resolve_relative(staged.path(), relative)?;
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            let allowance = 65_536;
            let mut bytes = Vec::new();
            File::open(path)?.take(allowance).read_to_end(&mut bytes)?;
            fs::write(target, &bytes)?;
            files.push(json!({"path":relative,"length":length,"headerBytes":bytes.len()}));
        }
        let staged_hashes = hashes(
            staged.path(),
            &files
                .iter()
                .filter_map(|value| value["path"].as_str().map(str::to_owned))
                .collect::<Vec<_>>(),
        )?;
        let header_bytes: u64 = files
            .iter()
            .filter_map(|file| file["headerBytes"].as_u64())
            .sum();
        let mut evidence_ranges: BTreeMap<(String, u64, u64), String> = files
            .iter()
            .filter_map(|file| {
                let path = file["path"].as_str()?;
                Some((
                    (path.to_owned(), 0, file["headerBytes"].as_u64()?),
                    staged_hashes.get(path)?.clone(),
                ))
            })
            .collect();
        let mut request = AdapterRequest::new("probe", staged.path());
        request.resource_files = Some(resources.to_vec());
        request.extra.insert("probeFiles".into(), json!(files));
        request.extra.insert("probeReadLimits".into(),json!({"maximumRounds":4,"maximumRanges":64,"maximumRangeBytes":1_048_576,"maximumTotalBytes":8_388_608}));
        let ranges_directory = tempfile::tempdir()?;
        let mut ranges = Vec::new();
        let mut seen = BTreeSet::new();
        let mut total = 0_u64;
        let mut range_hashes: BTreeMap<PathBuf, String> = BTreeMap::new();
        for round in 0..=4 {
            let response = self.execute(adapter, &request, cancellation).await?;
            if hashes(
                staged.path(),
                &staged_hashes.keys().cloned().collect::<Vec<_>>(),
            )? != staged_hashes
            {
                return Err(EngineError::Rejected("适配器修改了探测输入。".into()));
            }
            for (path, expected) in &range_hashes {
                if hash_file(path).map_err(|error| EngineError::Protocol(error.to_string()))?
                    != *expected
                {
                    return Err(EngineError::Rejected("适配器修改了只读探测片段。".into()));
                }
            }
            if response.probe_reads.is_empty() {
                let ranges = evidence_ranges
                    .iter()
                    .map(|((path, offset, length), hash)| {
                        json!({"path": path, "offset": offset, "length": length, "sha256": hash})
                    })
                    .collect::<Vec<_>>();
                return Ok((
                    response.probe,
                    json!({"ranges": ranges, "readFiles": files.len(), "readBytes": header_bytes + total}),
                ));
            }
            if round == 4 || response.probe.is_some() {
                return Err(EngineError::Protocol(
                    "适配器探测读取轮数或结果冲突。".into(),
                ));
            }
            for range in response.probe_reads {
                if !resources.contains(&range.path)
                    || range.length == 0
                    || range.length > 1_048_576
                    || ranges.len() >= 64
                    || total + range.length > 8_388_608
                    || !seen.insert((range.path.clone(), range.offset, range.length))
                {
                    return Err(EngineError::Protocol(
                        "适配器请求了超出范围、重复或过量的探测片段。".into(),
                    ));
                }
                let path = inputs.file(source, &range.path)?;
                if range
                    .offset
                    .checked_add(range.length)
                    .is_none_or(|end| end > path.metadata().map(|value| value.len()).unwrap_or(0))
                {
                    return Err(EngineError::Protocol("探测片段超出文件长度。".into()));
                }
                let mut stream = File::open(path)?;
                stream.seek(SeekFrom::Start(range.offset))?;
                let mut bytes = vec![0; range.length as usize];
                stream.read_exact(&mut bytes)?;
                let path = ranges_directory
                    .path()
                    .join(format!("range-{}.bin", ranges.len()));
                fs::write(&path, bytes)?;
                let hash =
                    hash_file(&path).map_err(|error| EngineError::Protocol(error.to_string()))?;
                if evidence_ranges
                    .insert(
                        (range.path.clone(), range.offset, range.length),
                        hash.clone(),
                    )
                    .is_some_and(|previous| previous != hash)
                {
                    return Err(EngineError::Rejected("探测期间原资源片段发生变化。".into()));
                }
                range_hashes.insert(path.clone(), hash);
                total += range.length;
                ranges.push(json!({"path":range.path,"offset":range.offset,"length":range.length,"dataPath":path}));
            }
            request.extra.insert("probeRanges".into(), json!(ranges));
        }
        Err(EngineError::Protocol("适配器未完成探测。".into()))
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "Verification exposes independent resource, baseline and execution controls without coupling to project storage."
    )]
    pub async fn verify(
        &self,
        adapter: &AdapterHandle,
        source: &Path,
        resources: &[String],
        directory: &Path,
        baseline: &[TranslationUnit],
        cancellation: &CancellationToken,
        progress: ProgressCallback<'_>,
    ) -> Result<VerificationResult, EngineError> {
        self.verify_with_coverage(
            adapter,
            source,
            resources,
            directory,
            baseline,
            &Value::Null,
            None,
            None,
            None,
            cancellation,
            progress,
        )
        .await
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "Verification retains per-game probe choices without staging unrelated resources."
    )]
    pub async fn verify_probed(
        &self,
        adapter: &AdapterHandle,
        source: &Path,
        probe: &ProbeResult,
        directory: &Path,
        baseline: &[TranslationUnit],
        cancellation: &CancellationToken,
        progress: ProgressCallback<'_>,
    ) -> Result<VerificationResult, EngineError> {
        self.verify_with_coverage(
            adapter,
            source,
            &probe.required_files,
            directory,
            baseline,
            &Value::Null,
            None,
            Some(probe),
            None,
            cancellation,
            progress,
        )
        .await
    }
}

fn record_verification_stage(
    timings: &mut BTreeMap<String, u64>,
    phase: &str,
    started: &mut Instant,
) {
    *timings.entry(phase.into()).or_default() += started.elapsed().as_millis() as u64;
    *started = Instant::now();
}

fn directory_hashes(root: &Path) -> Result<BTreeMap<String, String>, EngineError> {
    let inventory = Inventory::scan(root)?;
    if !inventory.complete {
        return Err(EngineError::Rejected(
            "补丁目录扫描未完成，无法确认只读验证。".into(),
        ));
    }
    hashes(
        root,
        &inventory
            .files
            .into_iter()
            .map(|file| file.path)
            .collect::<Vec<_>>(),
    )
}

#[derive(Clone, PartialEq, Eq)]
struct FileStamp {
    len: u64,
    modified: Option<SystemTime>,
    #[cfg(windows)]
    creation_time: u64,
    #[cfg(unix)]
    ino: u64,
    #[cfg(unix)]
    ctime: i64,
    #[cfg(unix)]
    ctime_nsec: i64,
}

fn file_stamp(path: &Path) -> Result<FileStamp, EngineError> {
    let meta = fs::metadata(path)?;
    #[cfg(unix)]
    use std::os::unix::fs::MetadataExt;
    #[cfg(windows)]
    use std::os::windows::fs::MetadataExt;
    Ok(FileStamp {
        len: meta.len(),
        modified: meta.modified().ok(),
        #[cfg(windows)]
        creation_time: meta.creation_time(),
        #[cfg(unix)]
        ino: meta.ino(),
        #[cfg(unix)]
        ctime: meta.ctime(),
        #[cfg(unix)]
        ctime_nsec: meta.ctime_nsec(),
    })
}

fn file_hash_cache() -> &'static Mutex<HashMap<PathBuf, (FileStamp, String)>> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, (FileStamp, String)>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Caches SHA-256 digests of runtime/library files keyed by metadata for normal execution,
/// while explicit integrity checks pass `force_rehash = true` to always recompute from disk.
fn verified_file_hash(path: &Path, force_rehash: bool) -> Result<String, EngineError> {
    let stamp = file_stamp(path)?;
    let cache = file_hash_cache();
    if !force_rehash
        && let Ok(guard) = cache.lock()
        && let Some((cached_stamp, digest)) = guard.get(path)
        && *cached_stamp == stamp
    {
        return Ok(digest.clone());
    }
    let digest = hash_file(path).map_err(|error| EngineError::Protocol(error.to_string()))?;
    if let Ok(mut guard) = cache.lock() {
        if guard.len() >= 512 {
            guard.clear();
        }
        guard.insert(path.to_path_buf(), (stamp, digest.clone()));
    }
    Ok(digest)
}

pub(crate) fn hashes(
    root: &Path,
    resources: &[String],
) -> Result<BTreeMap<String, String>, EngineError> {
    resources
        .iter()
        .map(|relative| {
            Ok((
                relative.clone(),
                hash_file(&resolve_relative(root, relative)?)
                    .map_err(|error| EngineError::Protocol(error.to_string()))?,
            ))
        })
        .collect()
}

fn selected_hashes(
    expected: &BTreeMap<String, String>,
    resources: &[String],
) -> Result<BTreeMap<String, String>, EngineError> {
    resources
        .iter()
        .map(|path| {
            expected
                .get(path)
                .cloned()
                .map(|hash| (path.clone(), hash))
                .ok_or_else(|| EngineError::Protocol(format!("验收资源缺少内容哈希：{path}")))
        })
        .collect()
}

fn verification_key(adapter: &AdapterHandle, unit: &TranslationUnit) -> String {
    if matches!(adapter, AdapterHandle::BuiltinSfa16) {
        format!("{}\n{}\n{}", unit.resource_path, unit.locator, unit.kind)
    } else {
        unit.id.clone()
    }
}

fn token_set(unit: &TranslationUnit) -> BTreeSet<&str> {
    unit.protected_tokens
        .iter()
        .flatten()
        .map(String::as_str)
        .collect()
}

fn validate_raw_units(units: &[TranslationUnit], source: &Path) -> Result<(), EngineError> {
    let mut seen = std::collections::HashSet::with_capacity(units.len());
    let mut checked_paths = std::collections::HashSet::new();
    for unit in units {
        if unit.id.trim().is_empty() || !seen.insert(unit.id.as_str()) {
            return Err(EngineError::Incompatible {
                stage: "extraction",
                message: "提取包含空 ID 或重复 ID。".into(),
            });
        }
        if checked_paths.insert(unit.resource_path.as_str()) {
            resolve_relative(source, &unit.resource_path)?;
        }
        if let Some(tokens) = &unit.protected_tokens
            && (tokens.len() > 64
                || tokens.iter().any(|token| {
                    token.is_empty()
                        || token.encode_utf16().count() > 256
                        || !unit.source_text.contains(token)
                }))
        {
            return Err(EngineError::Incompatible {
                stage: "extraction",
                message: "提取的运行时占位符声明无效。".into(),
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[tokio::test]
    async fn python_execution_does_not_depend_on_cached_host_scripts() {
        use std::os::windows::fs::OpenOptionsExt;

        let root = tempfile::tempdir().unwrap();
        let shared = root.path().join("shared");
        fs::create_dir(&shared).unwrap();
        let locks: Vec<_> = ["rust-host-v1.py", "readonly_inputs.py"]
            .iter()
            .map(|name| {
                let path = shared.join(name);
                fs::write(&path, "cached helper").unwrap();
                fs::OpenOptions::new()
                    .read(true)
                    .share_mode(0)
                    .open(path)
                    .unwrap()
            })
            .collect();
        let workspace = root.path().join("adaptation/current");
        fs::create_dir_all(&workspace).unwrap();
        let service =
            EngineService::new(root.path(), ToolPaths::development()).with_shared_directory(shared);
        for isolated in [false, true] {
            let result = service
                .clone()
                .with_execution_isolation(isolated)
                .helper(
                    &json!({
                        "operation":"run", "workspace":workspace,
                        "script":"work/probe.py",
                        "content":"import tempfile\nwith tempfile.TemporaryDirectory() as temporary:\n    save_sample('work/sample.bin', b'sample')\nprint('helper-ok')",
                        "resourcePaths":{},
                    }),
                    &CancellationToken::new(),
                )
                .await
                .unwrap();
            assert_eq!(result["exitCode"], 0, "{result}");
            assert_eq!(result["stdout"].as_str().unwrap().trim(), "helper-ok");
            assert_eq!(result["registeredSamples"], json!(["work/sample.bin"]));
            let retained: BTreeSet<_> = fs::read_dir(workspace.join("work"))
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .collect();
            assert_eq!(
                retained,
                BTreeSet::from([OsString::from("probe.py"), OsString::from("sample.bin")])
            );
        }
        drop(locks);
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn run_tool_extra_arguments_cannot_expand_file_access() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("adaptation/current");
        let tools = ToolPaths::development();
        crate::python::copy_python_runtime(&tools, &workspace.join("python")).unwrap();
        let allowed = root.path().join("allowed.txt");
        let private = root.path().join("private.txt");
        fs::write(&allowed, "allowed").unwrap();
        fs::write(&private, "must not leak").unwrap();
        let script = format!(
            "{}\n{}",
            include_str!("../python/isolation.py"),
            r#"from pathlib import Path
configure_tempfiles(True)
assert Path(sys.argv[1]).read_text(encoding='utf-8') == 'allowed'
try:
    Path(sys.argv[2]).read_text(encoding='utf-8')
except PermissionError:
    print('denied')
else:
    raise AssertionError('Private input was readable')
"#,
        );
        let service = EngineService::new(root.path(), tools)
            .with_shared_directory(root.path().join("shared"));
        let result = service.helper(&json!({
            "operation":"run_tool", "workspace":workspace,
            "executable":"python/python.exe",
            "arguments":["-I", "-X", "utf8", "-c", script, "resources/allowed.txt", private],
            "resourcePaths":{"allowed.txt":allowed},
            "library":root.path(), "resourceDirectory":root.path(),
        }), &CancellationToken::new()).await.unwrap();
        assert_eq!(result["exitCode"], 0, "{result}");
        assert_eq!(
            result["stdout"].as_str().unwrap().trim(),
            "denied",
            "{result}"
        );
    }

    #[tokio::test]
    async fn verification_checks_original_hashes_without_retaining_source_copies() {
        let Some(service) = service() else {
            return;
        };
        let root = tempfile::tempdir().expect("fixture");
        let source = root.path().join("source");
        fs::create_dir_all(&source).expect("source");
        let original = b"[\"one\",\"two\",\"three\",\"four\"]";
        fs::write(source.join("text.json"), original).expect("resource");
        let resources = ["text.json".into()];
        let mut expected = hashes(&source, &resources).expect("hash");
        // Input inspection may also hash approved resources outside this trial.
        expected.insert("outside-trial.dat".into(), "unused".into());
        let entry = root.path().join("adapter.py");
        fs::write(&entry, include_str!("../tests/fixtures/json_adapter.py")).expect("adapter");
        let adapter = AdapterHandle::Python {
            entry,
            package_directory: None,
            id: "engine.fixture".into(),
            version: "1".into(),
        };
        let cancellation = CancellationToken::new();
        fs::write(
            source.join("text.json"),
            b"[\"ONE\",\"two\",\"three\",\"four\"]",
        )
        .expect("same-length mutation before copying");
        let error = service
            .verify_with_coverage(
                &adapter,
                &source,
                &resources,
                &root.path().join("stale"),
                &[],
                &Value::Null,
                Some(&expected),
                None,
                None,
                &cancellation,
                None,
            )
            .await
            .expect_err("the copied bytes must match the inspected hashes");
        assert!(error.to_string().contains("验收输入发生变化"), "{error}");

        fs::write(source.join("text.json"), original).expect("restore fixture");
        let inspect_input = |event: EngineProgress| {
            if event.stage == "验收文本提取" {
                #[cfg(windows)]
                assert!(fs::write(source.join("text.json"), b"mutation").is_err());
            }
        };
        let verification = root.path().join("changed");
        let result = service
            .verify_with_coverage(
                &adapter,
                &source,
                &resources,
                &verification,
                &[],
                &Value::Null,
                Some(&expected),
                None,
                None,
                &cancellation,
                Some(&inspect_input),
            )
            .await
            .expect("verification diagnostics");
        assert!(result.passed, "{:?}", result.diagnostics);
        assert!(result.original_roundtrip_passed);
        assert!(!verification.join("sources").exists());
        assert_eq!(fs::read(source.join("text.json")).unwrap(), original);
        assert_eq!(result.resource_hashes.len(), 1);
    }

    #[tokio::test]
    async fn verification_rejects_adapter_writes_without_changing_originals() {
        let Some(service) = service() else {
            return;
        };
        let root = tempfile::tempdir().expect("fixture");
        let source = root.path().join("source");
        fs::create_dir_all(&source).expect("source");
        fs::write(source.join("text.json"), b"[\"one\"]").expect("resource");
        let entry = root.path().join("adapter.py");
        fs::write(&entry, include_str!("../tests/fixtures/json_adapter.py").replace(
            "elif r['operation'] == 'extract':",
            "elif r['operation'] == 'extract':\n    (root / 'text.json').unlink()\n    raise ValueError('fixture extraction failure')",
        )).expect("adapter");
        let adapter = AdapterHandle::Python {
            entry,
            package_directory: None,
            id: "engine.fixture".into(),
            version: "1".into(),
        };
        let result = service
            .verify(
                &adapter,
                &source,
                &["text.json".into()],
                &root.path().join("verify"),
                &[],
                &CancellationToken::new(),
                None,
            )
            .await
            .expect("preserved failure diagnostics");
        assert!(!result.passed);
        assert!(
            result
                .diagnostics
                .iter()
                .any(|message| message.contains("read-only")),
            "{:?}",
            result.diagnostics
        );
        assert_eq!(fs::read(source.join("text.json")).unwrap(), b"[\"one\"]");
        assert!(!root.path().join("verification/sources").exists());
    }

    #[tokio::test]
    async fn verification_reextracts_declared_generated_companions() {
        let Some(service) = service() else {
            return;
        };
        let root = tempfile::tempdir().expect("fixture");
        let source = root.path().join("source");
        fs::create_dir_all(&source).expect("source");
        fs::write(
            source.join("text.json"),
            "[\"one\",\"two\",\"three\",\"four\"]",
        )
        .expect("resource");
        let entry = root.path().join("adapter.py");
        let script = include_str!("../tests/fixtures/json_adapter.py")
            .replace(
                "elif r['operation'] == 'extract':",
                r#"elif r['operation'] == 'extract':
    if data and isinstance(data[0], int):
        mapping_path = next((root / path for path in r['resourceFiles'] if path == 'text.map'), None)
        if mapping_path is None:
            raise ValueError('missing generated text mapping in resourceFiles')
        mapping = json.loads(mapping_path.read_text(encoding='utf8'))
        data = [mapping[index] for index in data]"#,
            )
            .replace(
                "    out['build'] = dict(files=['text.json'], warnings=[])",
                r#"    files = ['text.json']
    if changed != data:
        (output / 'text.json').write_text(json.dumps(list(range(len(changed)))), encoding='utf8')
        (output / 'text.map').write_text(json.dumps(changed, ensure_ascii=False), encoding='utf8')
        files.append('text.map')
    out['build'] = dict(files=files, warnings=[])"#,
            );
        fs::write(&entry, script).expect("adapter");
        let adapter = AdapterHandle::Python {
            entry,
            package_directory: None,
            id: "engine.fixture".into(),
            version: "1".into(),
        };
        let verification = root.path().join("verification");
        let result = service
            .verify(
                &adapter,
                &source,
                &["text.json".into()],
                &verification,
                &[],
                &CancellationToken::new(),
                None,
            )
            .await
            .expect("verification");
        assert!(result.passed, "{:?}", result.diagnostics);
        assert!(!source.join("text.map").exists());
        assert!(!verification.join("original/text.map").exists());
        assert!(verification.join("partial/text.map").is_file());
        assert!(verification.join("changed/text.map").is_file());
    }

    #[tokio::test]
    async fn verification_rejects_invalid_or_incomplete_build_outputs() {
        let Some(service) = service() else {
            return;
        };
        let root = tempfile::tempdir().expect("fixture");
        let source = root.path().join("source");
        fs::create_dir_all(&source).expect("source");
        fs::write(source.join("text.json"), "[\"one\"]").expect("resource");
        let entry = root.path().join("adapter.py");
        let adapter = AdapterHandle::Python {
            entry: entry.clone(),
            package_directory: None,
            id: "engine.fixture".into(),
            version: "1".into(),
        };
        for (name, build, expected) in [
            (
                "undeclared-companion",
                "(output / 'text.map').write_text('{}')\n    out['build'] = dict(files=['text.json'], warnings=[])",
                "未声明输出",
            ),
            (
                "missing-companion",
                "out['build'] = dict(files=['text.json', 'text.map'], warnings=[])",
                "声明的输出不存在",
            ),
            (
                "outside-output",
                "out['build'] = dict(files=['text.json', '/text.map'], warnings=[])",
                "路径必须位于指定目录内",
            ),
            (
                "duplicate-output",
                "out['build'] = dict(files=['text.json', 'text.json'], warnings=[])",
                "重复文件路径",
            ),
            (
                "omitted-source",
                "(output / 'text.json').unlink()\n    out['build'] = dict(files=[], warnings=[])",
                "回填遗漏原资源",
            ),
        ] {
            let script = include_str!("../tests/fixtures/json_adapter.py").replace(
                "out['build'] = dict(files=['text.json'], warnings=[])",
                build,
            );
            fs::write(&entry, script).expect("adapter");
            let result = service
                .verify(
                    &adapter,
                    &source,
                    &["text.json".into()],
                    &root.path().join(name),
                    &[],
                    &CancellationToken::new(),
                    None,
                )
                .await
                .expect("verification");
            assert!(!result.passed, "{name}");
            assert!(
                result
                    .diagnostics
                    .iter()
                    .any(|message| message.contains(expected)),
                "{name}: {:?}",
                result.diagnostics
            );
        }
    }

    #[tokio::test]
    async fn regular_discovery_only_probes_and_still_checks_published_file_hashes() {
        let Some(service) = service() else {
            return;
        };
        let root = tempfile::tempdir().expect("fixture");
        let game = root.path().join("game");
        let package = root.path().join("package");
        fs::create_dir_all(&game).expect("game");
        fs::create_dir_all(package.join("src")).expect("package");
        fs::write(game.join("text.json"), "[\"one\",\"two\"]").expect("resource");
        let entry = package.join("src/adapter.py");
        let script = include_str!("../tests/fixtures/json_adapter.py").replace(
            "r = json.load(sys.stdin)",
            "r = json.load(sys.stdin)\nassert r['operation'] == 'probe', 'recognition must only probe'",
        );
        fs::write(&entry, &script).expect("adapter");
        write_json(
            &package.join("package.json"),
            &json!({
                "id":"engine.fixture", "version":"published", "manifestVersion":4,
                "protocolVersion":1, "resourcePatterns":["*.json"],
                "packageFiles":{"src/adapter.py":hash_file(&entry).expect("hash")}
            }),
        )
        .expect("manifest");
        let cancellation = CancellationToken::new();
        let inventory = Inventory::scan(&game).expect("inventory");
        let first = service
            .discover(&game, std::slice::from_ref(&package), &cancellation, None)
            .await
            .expect("discovery");
        assert!(first.adapter.is_some());
        assert_eq!(first.probe.unwrap().adapter_version, "published");
        let second = service
            .discover_with_inventory(
                &game,
                std::slice::from_ref(&package),
                &inventory,
                &cancellation,
                None,
            )
            .await
            .expect("discovery with inventory");
        assert!(second.adapter.is_some());
        fs::write(&entry, format!("{script}\n# changed after publication\n")).expect("change");
        let changed = service
            .discover(&game, &[package], &cancellation, None)
            .await
            .expect("invalid package is skipped");
        assert!(changed.adapter.is_none());
        assert!(
            changed
                .diagnostics
                .iter()
                .any(|text| text.contains("适配器文件已变化"))
        );
    }

    #[tokio::test]
    async fn documentation_alone_does_not_start_a_native_parser() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("rEaDmE.tXt"), b"documentation").unwrap();
        fs::write(root.path().join("LICENSE"), b"documentation").unwrap();
        let service = EngineService::new(
            root.path(),
            ToolPaths::new(root.path().join("missing-tools")),
        );
        let result = service
            .discover(root.path(), &[], &CancellationToken::new(), None)
            .await
            .unwrap();
        assert!(result.adapter.is_none());
        assert_eq!(result.file_count, 2);
    }

    #[tokio::test]
    async fn regular_native_discovery_passes_only_automatic_resource_candidates() {
        let root = tempfile::tempdir().unwrap();
        let tools = ToolPaths::development();
        if !tools.vntextpatch().is_file() {
            return;
        }
        let service =
            EngineService::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."), tools);
        let script = "\u{feff}*start\nこれは物語の台詞です。[p]\n";
        fs::write(root.path().join("story.txt"), script).unwrap();
        fs::write(root.path().join("ReAdMe.TxT"), script).unwrap();
        let audio = root.path().join("voice.ogg");
        fs::write(&audio, b"audio").unwrap();
        let token = CancellationToken::new();
        let inventory = Inventory::scan(root.path()).unwrap();
        let adapter = AdapterHandle::Native {
            format: "KirikiriKs".into(),
        };
        let original = service
            .execute(
                &adapter,
                &AdapterRequest::new("extract", root.path()),
                &token,
            )
            .await
            .unwrap()
            .extraction
            .unwrap();
        fs::remove_file(audio).unwrap();
        let result = service
            .discover_with_inventory(root.path(), &[], &inventory, &token, None)
            .await
            .unwrap();
        assert_eq!(result.probe.as_ref().unwrap().required_files, ["story.txt"]);
        let mut extract = AdapterRequest::new("extract", root.path());
        extract.resource_files = Some(result.probe.unwrap().required_files);
        let units = service
            .execute(&result.adapter.unwrap(), &extract, &token)
            .await
            .unwrap()
            .extraction
            .unwrap()
            .units;
        assert!(!units.is_empty());
        assert!(units.iter().all(|unit| unit.resource_path == "story.txt"));
        assert!(
            units
                .iter()
                .map(|unit| (&unit.id, &unit.source_text))
                .eq(original
                    .units
                    .iter()
                    .map(|unit| (&unit.id, &unit.source_text)))
        );
    }

    #[tokio::test]
    async fn regular_discovery_excludes_only_the_failed_package_version() {
        let Some(service) = service() else {
            return;
        };
        let root = tempfile::tempdir().expect("fixture");
        let game = root.path().join("game");
        fs::create_dir_all(&game).expect("game");
        fs::write(game.join("text.json"), "[\"one\"]").expect("resource");
        let mut packages = Vec::new();
        for version in ["2", "1"] {
            let package = root.path().join(version);
            fs::create_dir_all(package.join("src")).expect("package");
            let entry = package.join("src/adapter.py");
            fs::write(&entry, include_str!("../tests/fixtures/json_adapter.py")).expect("adapter");
            write_json(
                &package.join("package.json"),
                &json!({
                    "id":"engine.fixture", "version":version, "manifestVersion":4,
                    "protocolVersion":1, "resourcePatterns":["*.json"],
                    "packageFiles":{"src/adapter.py":hash_file(&entry).expect("hash")}
                }),
            )
            .expect("manifest");
            packages.push(package);
        }
        let discovered = service
            .with_excluded_adapters([("engine.fixture".into(), "2".into())])
            .discover(&game, &packages, &CancellationToken::new(), None)
            .await
            .expect("next version");
        assert_eq!(discovered.probe.unwrap().adapter_version, "1");
    }

    #[tokio::test]
    async fn cross_game_discovery_rejects_ambiguous_script_volumes_from_an_old_package() {
        let Some(service) = service() else {
            return;
        };
        let root = tempfile::tempdir().unwrap();
        let game = root.path().join("game");
        fs::create_dir_all(game.join("update")).unwrap();
        fs::write(game.join("scr.pak"), "[\"Original scripts\"]").unwrap();
        fs::write(game.join("update/scr.pak"), "[\"Other installation\"]").unwrap();
        let package = root.path().join("package");
        fs::create_dir_all(package.join("src")).unwrap();
        let entry = package.join("src/adapter.py");
        fs::write(
            &entry,
            include_str!("../tests/fixtures/json_adapter.py")
                .replace(
                    "requiredFiles=['text.json']",
                    "requiredFiles=r['resourceFiles']",
                )
                .replace("text.json", "scr.pak"),
        )
        .unwrap();
        write_json(
            &package.join("package.json"),
            &json!({
                "id":"template.nekopack4a-advscript2","version":"1","manifestVersion":4,
                "protocolVersion":1,"resourcePatterns":["**/*.pak"],
                "packageFiles":{"src/adapter.py":hash_file(&entry).unwrap()}
            }),
        )
        .unwrap();
        let cancellation = CancellationToken::new();
        let packages = [package];
        let discovered = service
            .discover(&game, &packages, &cancellation, None)
            .await
            .unwrap();
        assert!(discovered.adapter.is_none());
        let failure = &discovered.reuse_failures[0];
        assert_eq!(failure.failed_stage, "probe");
        assert!(failure.evidence[0].contains("update/scr.pak"));

        let known = service
            .with_ordered_packages_only()
            .discover(&game, &packages, &cancellation, None)
            .await
            .unwrap();
        assert_eq!(known.probe.unwrap().required_files.len(), 2);
    }

    #[tokio::test]
    async fn regular_discovery_propagates_missing_tools_instead_of_requesting_adaptation() {
        let root = tempfile::tempdir().expect("fixture");
        let game = root.path().join("game");
        let package = root.path().join("package");
        fs::create_dir_all(&game).expect("game");
        fs::create_dir_all(package.join("src")).expect("package");
        fs::write(game.join("text.json"), "[\"one\"]").expect("resource");
        let entry = package.join("src/adapter.py");
        fs::write(&entry, include_str!("../tests/fixtures/json_adapter.py")).expect("adapter");
        write_json(
            &package.join("package.json"),
            &json!({
                "id":"engine.fixture", "version":"1", "manifestVersion":4,
                "protocolVersion":1, "resourcePatterns":["*.json"],
                "packageFiles":{"src/adapter.py":hash_file(&entry).expect("hash")}
            }),
        )
        .expect("manifest");
        let service = EngineService::new(root.path(), ToolPaths::new(root.path().join("missing")));
        let error = service
            .discover(&game, &[package], &CancellationToken::new(), None)
            .await
            .expect_err("missing Python is an operational failure");
        assert!(matches!(error, EngineError::ToolMissing(_)));
    }

    #[tokio::test]
    async fn discovery_keeps_partial_rebuild_evidence_and_reuses_only_unchanged_failures() {
        let Some(service) = service() else {
            return;
        };
        let root = tempfile::tempdir().expect("fixture");
        let game = root.path().join("game");
        let package = root.path().join("package");
        fs::create_dir_all(&game).expect("game");
        fs::create_dir_all(package.join("src")).expect("package");
        fs::write(
            game.join("text.json"),
            "[\"one\",\"two\",\"three\",\"four\"]",
        )
        .expect("resource");
        let entry = package.join("src/adapter.py");
        fs::write(
            &entry,
            include_str!("../tests/fixtures/json_adapter.py").replace(
                "translations.get(str(i), value)",
                "translations.get(str(i), 'CORRUPTED')",
            ),
        )
        .expect("bad partial adapter");
        write_json(&package.join("package.json"),&json!({"id":"engine.fixture","version":"1","manifestVersion":4,"protocolVersion":1,"resourcePatterns":["*.json"],"packageFiles":{"src/adapter.py":hash_file(&entry).expect("hash")}})).expect("manifest");
        let cancellation = CancellationToken::new();
        let first = service
            .discover_with_failures(
                &game,
                std::slice::from_ref(&package),
                &[],
                &cancellation,
                None,
            )
            .await
            .expect("first discovery");
        assert!(first.adapter.is_none());
        let failure = &first.reuse_failures[0];
        assert_eq!(failure.failed_stage, "partial-translation-re-extraction");
        assert!(failure.original_roundtrip_passed);
        assert!(failure.input_fingerprint.is_some());
        assert_eq!(
            failure.source_directory.as_deref(),
            Some(package.join("src").as_path())
        );
        let checked = std::sync::atomic::AtomicBool::new(false);
        let report = |event: EngineProgress| {
            if event.stage == "验收文本提取" {
                checked.store(true, std::sync::atomic::Ordering::Relaxed);
            }
        };
        let reused = service
            .discover_with_failures(
                &game,
                std::slice::from_ref(&package),
                &first.reuse_failures,
                &cancellation,
                Some(&report),
            )
            .await
            .expect("reused evidence");
        assert!(!checked.load(std::sync::atomic::Ordering::Relaxed));
        assert_eq!(
            reused.reuse_failures[0].input_fingerprint,
            failure.input_fingerprint
        );
        fs::write(
            game.join("text.json"),
            "[\"changed\",\"two\",\"three\",\"four\"]",
        )
        .expect("new input");
        service
            .discover_with_failures(
                &game,
                &[package],
                &first.reuse_failures,
                &cancellation,
                Some(&report),
            )
            .await
            .expect("new check");
        assert!(checked.load(std::sync::atomic::Ordering::Relaxed));
    }

    #[tokio::test]
    async fn generated_extractions_reject_outside_resources_and_oversized_ids() {
        let Some(service) = service() else {
            return;
        };
        let root = tempfile::tempdir().expect("fixture");
        fs::write(root.path().join("selected.dat"), b"selected").expect("resource");
        fs::write(root.path().join("other.dat"), b"other").expect("resource");
        let entry = root.path().join("adapter.py");
        let adapter = AdapterHandle::Python {
            entry: entry.clone(),
            package_directory: None,
            id: "engine.fixture".into(),
            version: "1".into(),
        };
        for (resource, id, expected) in [
            ("other.dat", "stable".into(), "不属于所选资源"),
            ("selected.dat", "😀".repeat(257), "超过 512"),
        ] {
            fs::write(&entry,format!("import json,sys\nr=json.load(sys.stdin)\nwith open(r['responseFile'],'w',encoding='utf-8') as output: json.dump(dict(protocolVersion=1,success=True,extraction=dict(units=[dict(id={},resourcePath={},locator='1',sourceText='text',context='',kind='dialogue')],warnings=[])),output)\n",json!(id),json!(resource))).expect("fixture adapter");
            let mut request = AdapterRequest::new("extract", root.path());
            request.resource_files = Some(vec!["selected.dat".into()]);
            let error = service
                .execute(&adapter, &request, &CancellationToken::new())
                .await
                .expect_err("invalid extraction");
            assert!(error.to_string().contains(expected), "{error}");
        }
    }

    fn service() -> Option<EngineService> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let tools = ToolPaths::development();
        tools
            .python()
            .is_file()
            .then(|| EngineService::new(&root, tools))
    }

    #[tokio::test]
    async fn ignored_chinese_writes_produce_a_bounded_independent_counterexample() {
        let root = tempfile::tempdir().unwrap();
        let tools = ToolPaths::development();
        #[cfg(unix)]
        let tools = if !tools.python().is_file() {
            let tools = ToolPaths::new(root.path().join("tools"));
            fs::create_dir_all(tools.python().parent().unwrap()).unwrap();
            std::os::unix::fs::symlink("/usr/bin/python3", tools.python()).unwrap();
            tools
        } else {
            tools
        };
        let service =
            EngineService::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."), tools)
                .with_shared_directory(root.path().join("shared"));
        let source = root.path().join("source");
        fs::create_dir_all(&source).unwrap();
        let original = "[\"原文\",\"second\"]";
        fs::write(source.join("text.json"), original).unwrap();
        let entry = root.path().join("adapter.py");
        fs::write(
            &entry,
            include_str!("../tests/fixtures/json_adapter.py").replace(
                "changed = [translations.get(str(i), value) for i,value in enumerate(data)]",
                "changed = data",
            ),
        )
        .unwrap();
        let adapter = AdapterHandle::Python {
            entry,
            package_directory: None,
            id: "engine.fixture".into(),
            version: "1".into(),
        };
        let result = service
            .verify(
                &adapter,
                &source,
                &["text.json".into()],
                &root.path().join("verify"),
                &[],
                &CancellationToken::new(),
                None,
            )
            .await
            .unwrap();
        assert!(!result.passed);
        assert!(result.original_roundtrip_passed);
        let issue = &result.issues[0];
        assert_eq!(issue.code, crate::VerificationIssueCode::TextMismatch);
        assert_eq!(issue.stage, "partial-translation-re-extraction");
        assert_eq!(issue.resource_path.as_deref(), Some("text.json"));
        assert_eq!(issue.unit_id.as_deref(), Some("0"));
        assert_ne!(issue.expected, issue.actual);
        assert_eq!(
            fs::read_to_string(source.join("text.json")).unwrap(),
            original
        );
        let legacy: VerificationResult = serde_json::from_value(json!({"passed":false,"originalRoundtripPassed":false,"diagnostics":[],"units":0,"runtimeTested":false,"resourceHashes":{}})).unwrap();
        assert!(legacy.issues.is_empty());
    }

    #[tokio::test]
    async fn bounded_probe_evidence_tracks_dynamic_reads_without_reading_entire_resource() {
        let Some(service) = service() else {
            return;
        };
        let root = tempfile::tempdir().expect("fixture");
        let resource = root.path().join("scripts.dat");
        let mut bytes = vec![0; 131072];
        fs::write(&resource, &bytes).expect("resource");
        let entry = root.path().join("adapter.py");
        fs::write(&entry, r#"import json,sys
r=json.load(sys.stdin)
out=dict(protocolVersion=1,success=True)
if r.get('probeRanges'):
    out['probe']=dict(engineId='format',adapterId='template.format',adapterVersion='1',description='format',requiredFiles=['scripts.dat'])
else:
    out['probeReads']=[dict(path='scripts.dat',offset=131056,length=16)]
with open(r['responseFile'],'w',encoding='utf-8') as output: json.dump(out,output)
"#).expect("adapter");
        let adapter = AdapterHandle::Python {
            entry,
            package_directory: None,
            id: "template.format".into(),
            version: "1".into(),
        };
        let resources = ["scripts.dat".into()];
        let cancellation = CancellationToken::new();
        let (first_probe, first) = service
            .probe_bounded_with_evidence(&adapter, root.path(), &resources, &cancellation)
            .await
            .expect("first probe");
        let (_, repeated) = service
            .probe_bounded_with_evidence(&adapter, root.path(), &resources, &cancellation)
            .await
            .expect("repeated probe");
        assert_eq!(first, repeated, "temporary paths must not affect evidence");
        assert_eq!(first["readBytes"], 65_552);
        assert_eq!(first["ranges"].as_array().unwrap().len(), 2);
        bytes[131071] = 1;
        fs::write(&resource, &bytes).expect("same-length change");
        let (next_probe, next) = service
            .probe_bounded_with_evidence(&adapter, root.path(), &resources, &cancellation)
            .await
            .expect("changed probe");
        assert_eq!(
            first_probe.unwrap().required_files,
            next_probe.unwrap().required_files,
        );
        assert_ne!(first, next, "changed probe inputs must invalidate evidence");
    }

    #[tokio::test]
    async fn probe_ranges_remain_read_only_across_local_rounds() {
        let Some(service) = service() else {
            return;
        };
        let root = tempfile::tempdir().expect("fixture");
        fs::write(root.path().join("scripts.dat"), vec![0; 131072]).expect("resource");
        let entry = root.path().join("adapter.py");
        fs::write(&entry,r#"import json,sys
from pathlib import Path
r=json.load(sys.stdin)
out=dict(protocolVersion=1,success=True)
if r.get('probeRanges'):
    p=r['probeRanges'][0]
    try:
        Path(p['dataPath']).write_bytes(b'x'*p['length'])
    except PermissionError:
        out.update(success=False,error='只读探测片段禁止写入。')
    else:
        out['probe']=dict(engineId='format',adapterId='template.format',adapterVersion='1',description='format',requiredFiles=['scripts.dat'])
else:
    out['probeReads']=[dict(path='scripts.dat',offset=131056,length=16)]
with open(r['responseFile'],'w',encoding='utf-8') as output: json.dump(out,output)
"#).expect("adapter");
        let adapter = AdapterHandle::Python {
            entry,
            package_directory: None,
            id: "template.format".into(),
            version: "1".into(),
        };
        for isolated in [false, true] {
            let result = service
                .clone()
                .with_execution_isolation(isolated)
                .probe_bounded(
                    &adapter,
                    root.path(),
                    &["scripts.dat".into()],
                    &CancellationToken::new(),
                )
                .await;
            assert!(
                matches!(&result,Err(EngineError::Rejected(message)) if message.contains("只读探测片段")),
                "{result:?}"
            );
        }
        assert_eq!(
            fs::read(root.path().join("scripts.dat")).expect("original"),
            vec![0; 131072]
        );
    }

    #[tokio::test]
    async fn rejected_extraction_cannot_hide_changed_source_bytes() {
        let Some(service) = service() else {
            return;
        };
        let root = tempfile::tempdir().expect("fixture");
        let source = root.path().join("source");
        fs::create_dir(&source).expect("source");
        fs::write(source.join("scripts.dat"), b"original").expect("resource");
        let entry = root.path().join("adapter.py");
        fs::write(
            &entry,
            r#"import json,sys
from pathlib import Path
r=json.load(sys.stdin)
(Path(r['sourceDirectory'])/'scripts.dat').write_bytes(b'changed')
Path(r['responseFile']).write_text(json.dumps(dict(protocolVersion=1,success=False,error='unsupported layout')),encoding='utf8')
"#,
        )
        .expect("adapter");
        let adapter = AdapterHandle::Python {
            entry,
            package_directory: None,
            id: "engine.fixture".into(),
            version: "1".into(),
        };
        let mut request = AdapterRequest::new("extract", &source);
        request.resource_files = Some(vec!["scripts.dat".into()]);
        let error = service
            .execute(&adapter, &request, &CancellationToken::new())
            .await
            .expect_err("source mutation is fatal even when extraction is rejected");
        assert!(error.to_string().contains("read-only"), "{error}");
        assert_eq!(fs::read(source.join("scripts.dat")).unwrap(), b"original");
    }

    #[tokio::test]
    async fn successful_validation_cannot_accept_a_changed_patch() {
        let Some(service) = service() else {
            return;
        };
        let root = tempfile::tempdir().expect("fixture");
        let source = root.path().join("source");
        let output = root.path().join("output");
        fs::create_dir_all(&source).expect("source");
        fs::create_dir_all(&output).expect("output");
        fs::write(source.join("scripts.dat"), b"original").expect("resource");
        fs::write(output.join("scripts.dat"), b"translated").expect("patch");
        let extracted_hashes = directory_hashes(&output).expect("extraction baseline");
        let entry = root.path().join("adapter.py");
        fs::write(&entry,r#"import json,sys
from pathlib import Path
r=json.load(sys.stdin)
(Path(r['outputDirectory'])/'scripts.dat').write_bytes(b'changed during validation')
with open(r['responseFile'],'w',encoding='utf-8') as output:
    json.dump(dict(protocolVersion=1,success=True,validation=dict(passed=True,issues=[],runtimeStatus='not-tested')),output)
"#).expect("adapter");
        let adapter = AdapterHandle::Python {
            entry: entry.clone(),
            package_directory: None,
            id: "template.format".into(),
            version: "1".into(),
        };
        let mut request = AdapterRequest::new("validate", source);
        request.resource_files = Some(vec!["scripts.dat".into()]);
        request.output_directory = Some(output);
        let result = service
            .execute(&adapter, &request, &CancellationToken::new())
            .await;
        assert!(
            matches!(result,Err(EngineError::Rejected(message)) if message.contains("校验期间修改了补丁"))
        );
        // A read-only validator must also reject bytes changed since the reused extraction.
        let readonly = fs::read_to_string(&entry).unwrap().replace(
            "(Path(r['outputDirectory'])/'scripts.dat').write_bytes(b'changed during validation')",
            "",
        );
        fs::write(&entry, readonly).unwrap();
        let result = service
            .execute_with_hashes(
                &adapter,
                &request,
                None,
                Some(&extracted_hashes),
                &CancellationToken::new(),
            )
            .await;
        assert!(
            matches!(result,Err(EngineError::Rejected(message)) if message.contains("校验期间修改了补丁"))
        );
    }

    #[tokio::test]
    async fn explicit_package_verification_always_rehashes_pinned_runtime_files() {
        let root = tempfile::tempdir().expect("fixture");
        let tools_root = root.path().join("tools");
        let python_root = tools_root.join("python");
        let package = root.path().join("pkg");
        fs::create_dir_all(&python_root).expect("python");
        fs::create_dir_all(package.join("tools")).expect("pkg tools");

        let runtime_file = python_root.join("python314.dll");
        fs::write(&runtime_file, b"tampered-runtime-bytes").expect("runtime file");
        let expected_digest = gamel10n_translation::files::hash_bytes(b"original-runtime-bytes");
        let stamp = file_stamp(&runtime_file).expect("stamp");
        file_hash_cache()
            .lock()
            .expect("cache")
            .insert(runtime_file.clone(), (stamp, expected_digest.clone()));

        // Normal cached lookup still trusts matching metadata, simulating preserved mtime/len.
        assert_eq!(
            verified_file_hash(&runtime_file, false).expect("cached"),
            expected_digest
        );

        let revision = "0123456789abcdef0123456789abcdef01234567";
        let library = tools_root
            .join("engine-tools/references")
            .join(revision)
            .join("library.zip");
        fs::create_dir_all(library.parent().expect("lib dir")).expect("create lib dir");
        fs::write(&library, b"zip-content").expect("library");
        let library_sha = hash_file(&library).expect("library sha");

        write_json(
            &python_root.join("runtime.json"),
            &json!({
                "archiveSha256": "archive-sha",
                "files": { "python314.dll": expected_digest },
            }),
        )
        .expect("runtime.json");
        write_json(
            &package.join("tools/engine-reference.json"),
            &json!({
                "pythonSha256": "archive-sha",
                "pythonVersion": "3.14.10",
                "reference": {
                    "revision": revision,
                    "librarySha256": library_sha,
                },
            }),
        )
        .expect("engine-reference.json");
        fs::create_dir_all(package.join("src")).expect("pkg src");
        let entry = package.join("src/adapter.py");
        fs::write(&entry, "print('ok')\n").expect("adapter");
        let entry_sha = hash_file(&entry).expect("entry sha");
        write_json(
            &package.join("package.json"),
            &json!({
                "manifestVersion": 4,
                "protocolVersion": 1,
                "id": "test.pkg",
                "version": "1.0.0",
                "packageFiles": { "src/adapter.py": entry_sha },
            }),
        )
        .expect("package.json");

        let service = EngineService::new(root.path(), ToolPaths::new(tools_root));
        let error = service
            .verify_package(&package, &CancellationToken::new())
            .await
            .expect_err("explicit verify_package must rehash instead of trusting stale cache");
        assert!(
            matches!(error, EngineError::Rejected(ref message) if message.contains("共享 Python 文件已变化")),
            "unexpected error: {error}"
        );
        // Rehash also refreshed the cache with the actual on-disk digest.
        let actual_runtime_sha = gamel10n_translation::files::hash_bytes(b"tampered-runtime-bytes");
        assert_eq!(
            verified_file_hash(&runtime_file, false).expect("refreshed"),
            actual_runtime_sha
        );

        // Also verify library.zip bypasses a stale cache entry during verify_package.
        write_json(
            &python_root.join("runtime.json"),
            &json!({
                "archiveSha256": "archive-sha",
                "files": { "python314.dll": actual_runtime_sha },
            }),
        )
        .expect("updated runtime.json");
        fs::write(&library, b"tampered-zip").expect("tampered library");
        let lib_stamp = file_stamp(&library).expect("lib stamp");
        file_hash_cache()
            .lock()
            .expect("cache")
            .insert(library.clone(), (lib_stamp, library_sha));
        let lib_error = service
            .verify_package(&package, &CancellationToken::new())
            .await
            .expect_err("explicit verify_package must rehash library.zip");
        assert!(
            matches!(lib_error, EngineError::Rejected(ref message) if message.contains("共享引擎工具库内容已变化")),
            "unexpected library error: {lib_error}"
        );
    }

    #[test]
    fn shared_helper_dir_reuses_directory_and_cleans_up_on_drop() {
        let dir_path = {
            let shared = SharedHelperDir::new();
            let cloned = shared.clone();
            let (first_dir, first_helper) = shared.ensure_scripts("a", "b").expect("init");
            let (second_dir, second_helper) = cloned.ensure_scripts("a", "b").expect("reuse");
            assert_eq!(first_dir, second_dir);
            assert_eq!(first_helper, second_helper);
            assert!(first_helper.is_file());
            assert!(first_dir.join("readonly_inputs.py").is_file());
            first_dir
        };
        assert!(
            !dir_path.exists(),
            "shared helper temp directory should be deleted when its last owner drops"
        );
    }
}
