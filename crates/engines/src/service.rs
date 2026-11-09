//! UI-neutral engine execution and independent resource verification.

use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    fs::{self, File},
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use gamel10n_translation::{
    TokenUsage, TranslationUnit,
    files::{hash_file, hash_text, read_json, write_json},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    AdapterHandle, CancellationToken, EngineError, PackageManifest, ToolPaths, ToolProcess,
    discovery::{Inventory, RecognitionStatus, native_formats_for, resolve_relative},
    protocol::{AdapterRequest, AdapterResponse, BuildMode, ExtractionResult, ProbeResult},
    python::PythonAdapter,
    vntextpatch::VnTextPatch,
};

pub type ProgressCallback<'a> = Option<&'a (dyn Fn(EngineProgress) + Send + Sync)>;

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineProgress {
    pub stage: String,
    pub message: String,
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
    pub units: usize,
    pub runtime_tested: bool,
    pub resource_hashes: BTreeMap<String, String>,
    #[serde(default = "cacheable_verification")]
    pub cacheable: bool,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub stage_timings_ms: BTreeMap<String, u64>,
    /// Only the current invocation can hand off extracted text to the project.
    #[serde(skip)]
    pub verified_extraction: Option<Arc<VerifiedExtraction>>,
}

/// A successful verification's original text, bound to its package and resource bytes.
/// It is intentionally absent from saved sessions and published verification reports.
#[derive(Clone, Debug)]
pub struct VerifiedExtraction {
    adapter_fingerprint: String,
    resource_hashes: BTreeMap<String, String>,
    extraction: ExtractionResult,
}

impl VerifiedExtraction {
    /// The caller supplies freshly checked snapshot hashes. Changed inputs use normal extraction.
    pub fn into_matching_extraction(
        self: Arc<Self>,
        adapter: &AdapterHandle,
        resource_hashes: &BTreeMap<String, String>,
    ) -> Result<Option<ExtractionResult>, EngineError> {
        if &self.resource_hashes != resource_hashes
            || verified_adapter_fingerprint(adapter)?.as_deref()
                != Some(self.adapter_fingerprint.as_str())
        {
            return Ok(None);
        }
        Ok(Some(Arc::unwrap_or_clone(self).extraction))
    }
}

fn verified_adapter_fingerprint(adapter: &AdapterHandle) -> Result<Option<String>, EngineError> {
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
    Ok(Some(hash_text(&serde_json::to_string(&manifest)?)))
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

#[derive(Clone, Debug)]
pub struct EngineService {
    pub asset_root: PathBuf,
    pub tools: ToolPaths,
    pub shared_directory: PathBuf,
    pub(crate) disabled_families: BTreeSet<String>,
    excluded_adapters: BTreeSet<(String, String)>,
    preferred_adapter: Option<String>,
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
            disabled_families: BTreeSet::new(),
            excluded_adapters: BTreeSet::new(),
            preferred_adapter: None,
        }
    }

    pub fn with_shared_directory(mut self, directory: impl Into<PathBuf>) -> Self {
        self.shared_directory = directory.into();
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
        manifest.handle(directory)?;
        self.tool_paths(Some(directory), cancellation).await?;
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
        fs::create_dir_all(&self.shared_directory)?;
        let helper = self.shared_directory.join("rust-host-v1.py");
        if fs::read_to_string(&helper).ok().as_deref() != Some(SOURCE) {
            fs::write(&helper, SOURCE)?;
        }
        let result: Value = ToolProcess::new(self.tools.python())
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
            ))
            .execute(request, cancellation)
            .await?;
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
        let pin = self.tools.root.join("engine-tools/siglus.json");
        if pin.is_file() {
            let reference: Value =
                read_json(&pin).map_err(|error| EngineError::Protocol(error.to_string()))?;
            let library = self.reference_library(&reference)?;
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
                include_bytes!("../../../adapters/scripts/common/kirikiri_text.py"),
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
                if hash_file(&path).map_err(|error| EngineError::Protocol(error.to_string()))?
                    != expected
                {
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
        let mut paths = json!({"python":self.tools.python(),"vnTextPatch":self.tools.vntextpatch(),"vnTextPatchCommand":[self.tools.vntextpatch()],"hostCommand":[self.tools.vntextpatch()]});
        if let Some(package) = package {
            let native_pin = package.join("tools/vntextpatch-reference.json");
            if native_pin.is_file() {
                let pin: Value = read_json(&native_pin)
                    .map_err(|error| EngineError::Protocol(error.to_string()))?;
                let executable = self.resolve_native_tools(&pin)?;
                paths["vnTextPatch"] = json!(executable);
                paths["vnTextPatchCommand"] = json!([executable]);
                paths["hostCommand"] = json!([executable]);
            }
            let dependency = package.join("tools/engine-reference.json");
            if dependency.is_file() {
                let pin: Value = read_json(&dependency)
                    .map_err(|error| EngineError::Protocol(error.to_string()))?;
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
                    for (relative, expected) in files {
                        let path = resolve_relative(&self.tools.root.join("python"), relative)?;
                        if hash_file(&path)
                            .map_err(|error| EngineError::Protocol(error.to_string()))?
                            != expected.as_str().unwrap_or("")
                        {
                            return Err(EngineError::Rejected(format!(
                                "共享 Python 文件已变化：{relative}"
                            )));
                        }
                    }
                }
                let reference = pin
                    .get("reference")
                    .ok_or_else(|| EngineError::Protocol("依赖缺少 reference。".into()))?;
                let library = if reference.get("kind").and_then(Value::as_str) == Some("siglus") {
                    let prepared = self.siglus(cancellation).await?;
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
                    self.reference_library(reference)?
                };
                paths["engineLibrary"] = json!(library);
            }
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

    // Verification supplies the snapshot hashes and, when reusing extraction,
    // the output hashes observed before that extraction. Both are rechecked afterward.
    async fn execute_with_hashes(
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
            let output = if output.exists() {
                output.canonicalize()?
            } else {
                output.to_path_buf()
            };
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
            request
                .resource_files
                .as_ref()
                .map(|files| match expected_source_hashes {
                    Some(expected) => selected_hashes(expected, files),
                    None => hashes(&request.source_directory, files),
                })
                .transpose()?
        } else {
            None
        };
        let patch_hashes = if matches!(request.operation.as_str(), "validate" | "extract") {
            request
                .output_directory
                .as_ref()
                .map(|output| match expected_patch_hashes {
                    Some(expected) => Ok(expected.clone()),
                    None => directory_hashes(output),
                })
                .transpose()?
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
                    .execute(&request, cancellation)
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
                    .execute(&request, cancellation)
                    .await
            }
            AdapterHandle::BuiltinSfa16 => {
                PythonAdapter::new(&self.tools, &self.sfa16_entry()?)?
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
        if let Some(expected) = source_hashes
            && hashes(
                &request.source_directory,
                &expected.keys().cloned().collect::<Vec<_>>(),
            )? != expected
        {
            return Err(EngineError::Rejected(
                "适配器修改了只读源资源；操作未被接受。".into(),
            ));
        }
        if let (Some(expected), Some(output)) = (patch_hashes, &request.output_directory)
            && directory_hashes(output)? != expected
        {
            return Err(EngineError::Rejected(
                "适配器在读取或校验期间修改了补丁输出。".into(),
            ));
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
            if let Some(executable) = &build.launch_executable
                && (!declared.contains(&executable.to_lowercase())
                    || !executable.to_lowercase().ends_with(".exe"))
            {
                return Err(EngineError::Protocol(
                    "补丁启动程序必须是声明输出中的 EXE。".into(),
                ));
            }
            for file in Inventory::scan(output)?.files {
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
                    && build.launch_executable.is_none()
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
            cancellation,
            progress,
        )
        .await
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "Discovery separates probe-only recognition from complete intelligent reuse checks."
    )]
    async fn discover_with_inventory_and_failures(
        &self,
        source: &Path,
        packages: &[PathBuf],
        inventory: &Inventory,
        previous_failures: &[GeneratedAdapterReuseFailure],
        verification: DiscoveryVerification,
        cancellation: &CancellationToken,
        progress: ProgressCallback<'_>,
    ) -> Result<DiscoveryResult, EngineError> {
        let mut candidates = Vec::new();
        let mut diagnostics = Vec::new();
        let mut reuse_failures = Vec::new();
        let mut builtin_probes = Vec::new();
        let has_xp3 = inventory
            .files
            .iter()
            .any(|file| file.path.to_lowercase().ends_with(".xp3"));
        let probe_smil = has_xp3
            && self
                .preferred_adapter
                .as_deref()
                .is_none_or(|id| id == "engine.kirikiri-smil-smile");
        let mut probe_elapsed = Duration::ZERO;
        if probe_smil
            && !self
                .disabled_families
                .contains("engine.kirikiri-smil-smile")
            && !self
                .excluded_adapters
                .contains(&("engine.kirikiri-smil-smile".into(), "1".into()))
        {
            let adapter = AdapterHandle::BuiltinKirikiriSmil;
            let resources = inventory
                .files
                .iter()
                .filter(|file| file.path.to_lowercase().ends_with(".xp3"))
                .map(|file| file.path.clone())
                .collect::<Vec<_>>();
            Self::report(
                progress,
                "检查内置解析器",
                "正在检查 KiriKiri SMIL 映射、过滤器及脚本样本。",
            );
            let started = Instant::now();
            let probe = self
                .probe_bounded(&adapter, source, &resources, cancellation)
                .await?;
            probe_elapsed += started.elapsed();
            if let Some(probe) = probe {
                let usable = if verification == DiscoveryVerification::ProbeOnly {
                    true
                } else {
                    let mut request = AdapterRequest::new("extract", source);
                    request.resource_files = Some(probe.required_files.clone());
                    match self.execute(&adapter, &request, cancellation).await {
                        Ok(response) => response
                            .extraction
                            .is_some_and(|result| !result.units.is_empty()),
                        Err(EngineError::Incompatible { message, .. }) => {
                            diagnostics
                                .push(format!("内置解析器 {} 提取不兼容：{message}", adapter.id()));
                            false
                        }
                        Err(error) => return Err(error),
                    }
                };
                if usable {
                    return Ok(DiscoveryResult {
                        adapter: Some(adapter),
                        probe: Some(probe),
                        diagnostics,
                        file_count: inventory.files.len(),
                        complete: inventory.complete,
                        reuse_failures,
                        builtin_probes,
                    });
                }
            }
        }
        for directory in packages {
            match PackageManifest::load_runtime(directory) {
                Ok((manifest, directory)) => {
                    if self.disabled_families.contains(&manifest.id) {
                        continue;
                    }
                    if self
                        .excluded_adapters
                        .contains(&(manifest.id.clone(), manifest.version.clone()))
                    {
                        continue;
                    }
                    let (status, evidence) =
                        inventory.recognize(source, manifest.recognition.as_ref());
                    if status != RecognitionStatus::NoMatch {
                        let resources = manifest.resources(inventory);
                        if !resources.is_empty()
                            && !candidates.iter().any(
                                |candidate: &(usize, PathBuf, PackageManifest, Vec<String>)| {
                                    candidate.1 == directory
                                },
                            )
                        {
                            candidates.push((evidence, directory, manifest, resources));
                        }
                    }
                }
                Err(error) => {
                    let message = format!("适配器 {} 加载失败：{error}", directory.display());
                    Self::report(progress, "适配器检查未通过", &message);
                    diagnostics.push(message);
                }
            }
        }
        candidates.sort_by_key(|candidate| std::cmp::Reverse(candidate.0));
        for (recognition_evidence, directory, manifest, resources) in candidates {
            if cancellation.is_cancelled() {
                return Err(EngineError::Cancelled);
            }
            if probe_elapsed >= Duration::from_secs(45) {
                if verification == DiscoveryVerification::ProbeOnly {
                    return Err(EngineError::Timeout);
                }
                let message = "已有扩展适配器检查达到本次时间预算，继续检查内置解析器。";
                Self::report(progress, "适配器检查超时", message);
                diagnostics.push(message.into());
                break;
            }
            Self::report(
                progress,
                "匹配已有适配器",
                format!("正在检查 {} {}。", manifest.id, manifest.version),
            );
            let mut phase = "load";
            let mut recognized = false;
            let mut failure = None;
            let source_directory = directory
                .join("src")
                .is_dir()
                .then(|| directory.join("src"));
            let attempt = async {
                let handle = manifest.handle(&directory)?;
                phase = "probe";
                let started = Instant::now();
                let probe = self
                    .probe_bounded_with_limit(
                        &handle,
                        source,
                        &resources,
                        &crate::adaptation::AdaptationInputs::default(),
                        cancellation,
                        Duration::from_secs(45).saturating_sub(probe_elapsed),
                    )
                    .await;
                probe_elapsed += started.elapsed();
                let (probe, _) = probe?;
                if let Some(probe) = probe {
                    recognized = true;
                    if verification == DiscoveryVerification::ProbeOnly {
                        return Ok(Some((handle, probe)));
                    }
                    phase = "fingerprint";
                    Self::report(
                        progress,
                        "核对资源指纹",
                        format!("{} 已识别资源，正在核对资源内容。", manifest.id),
                    );
                    tokio::task::yield_now().await;
                    let fingerprint = reuse_input_fingerprint(&directory, source, &probe)?;
                    if let Some(previous) = previous_failures.iter().find(|previous| {
                        previous.id == manifest.id
                            && previous.version == manifest.version
                            && previous.input_fingerprint.as_deref() == Some(&fingerprint)
                    }) {
                        let mut previous = previous.clone();
                        previous.source_directory = source_directory.clone();
                        failure = Some(previous);
                        return Ok(None);
                    }
                    phase = "verify";
                    Self::report(
                        progress,
                        "验证资源兼容性",
                        format!("正在验证 {} 的文本提取、原文回填和中文修改。", manifest.id),
                    );
                    tokio::task::yield_now().await;
                    let work = tempfile::tempdir()?;
                    let verified = self
                        .verify_probed(
                            &handle,
                            source,
                            &probe,
                            work.path(),
                            &[],
                            cancellation,
                            progress,
                        )
                        .await?;
                    if verified.passed {
                        return Ok::<_, EngineError>(Some((handle, probe)));
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
                        && reuse_input_fingerprint(&directory, source, &probe)
                            .ok()
                            .as_deref()
                            == Some(&fingerprint);
                    failure = Some(GeneratedAdapterReuseFailure {
                        id: manifest.id.clone(),
                        version: manifest.version.clone(),
                        failed_stage: stage,
                        evidence: verified
                            .diagnostics
                            .iter()
                            .rev()
                            .take(3)
                            .collect::<Vec<_>>()
                            .into_iter()
                            .rev()
                            .map(|item| item.chars().take(2000).collect())
                            .collect(),
                        source_directory: source_directory.clone(),
                        original_roundtrip_passed: verified.original_roundtrip_passed,
                        input_fingerprint: stable.then_some(fingerprint),
                    });
                    return Ok(None);
                }
                Ok(None)
            };
            match attempt.await {
                Ok(Some((handle, probe))) => {
                    return Ok(DiscoveryResult {
                        adapter: Some(handle),
                        probe: Some(probe),
                        diagnostics,
                        file_count: inventory.files.len(),
                        complete: inventory.complete,
                        reuse_failures,
                        builtin_probes,
                    });
                }
                Ok(None) => {
                    if let Some(failure) = failure {
                        for evidence in &failure.evidence {
                            let message = format!(
                                "适配器 {} {} 未通过检查：{evidence}",
                                manifest.id, manifest.version
                            );
                            Self::report(progress, "适配器检查未通过", &message);
                            diagnostics.push(message);
                        }
                        if reuse_failures.len() < 6 {
                            reuse_failures.push(failure);
                        }
                    }
                }
                Err(EngineError::Cancelled) => return Err(EngineError::Cancelled),
                Err(
                    error @ (EngineError::Io(_)
                    | EngineError::ToolMissing(_)
                    | EngineError::Timeout),
                ) if verification == DiscoveryVerification::ProbeOnly => {
                    return Err(error);
                }
                Err(EngineError::Timeout) if phase == "probe" => {
                    let message = format!(
                        "适配器 {} {} 在资源探测阶段超时，可使用智能适配继续分析。",
                        manifest.id, manifest.version
                    );
                    if recognition_evidence > 0 && reuse_failures.len() < 6 {
                        reuse_failures.push(GeneratedAdapterReuseFailure {
                            id: manifest.id.clone(),
                            version: manifest.version.clone(),
                            failed_stage: phase.into(),
                            evidence: vec![message.clone()],
                            source_directory: source_directory.clone(),
                            original_roundtrip_passed: false,
                            input_fingerprint: None,
                        });
                    }
                    Self::report(progress, "适配器检查超时", &message);
                    diagnostics.push(message);
                    break;
                }
                Err(error) => {
                    if (recognized || recognition_evidence > 0) && reuse_failures.len() < 6 {
                        reuse_failures.push(GeneratedAdapterReuseFailure {
                            id: manifest.id.clone(),
                            version: manifest.version.clone(),
                            failed_stage: phase.into(),
                            evidence: vec![error.to_string().chars().take(2000).collect()],
                            source_directory: source_directory.clone(),
                            original_roundtrip_passed: false,
                            input_fingerprint: None,
                        });
                    }
                    let message = format!(
                        "适配器 {} {} 检查失败：{error}",
                        manifest.id, manifest.version
                    );
                    Self::report(progress, "适配器检查未通过", &message);
                    diagnostics.push(message);
                }
            }
        }
        let mut builtins = Vec::new();
        if inventory
            .files
            .iter()
            .any(|file| file.path.eq_ignore_ascii_case("scr.aos"))
            && inventory
                .files
                .iter()
                .any(|file| file.path.eq_ignore_ascii_case("sfa16.exe"))
        {
            builtins.push(AdapterHandle::BuiltinSfa16);
        }
        if inventory
            .files
            .iter()
            .any(|file| file.path.eq_ignore_ascii_case("scene.pck"))
            && inventory
                .files
                .iter()
                .any(|file| file.path.eq_ignore_ascii_case("gameexe.dat"))
        {
            builtins.push(AdapterHandle::BuiltinSiglus);
        }
        builtins.extend(native_formats_for(inventory).into_iter().map(|format| {
            AdapterHandle::Native {
                format: format.into(),
            }
        }));
        for adapter in builtins {
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
            if self.disabled_families.contains(&family) {
                continue;
            }
            Self::report(
                progress,
                "检查内置解析器",
                format!("正在检查 {}。", adapter.id()),
            );
            let request = AdapterRequest::new("probe", source);
            match self.execute(&adapter, &request, cancellation).await {
                Ok(response) => {
                    if let Some(probe) = response.probe {
                        if self
                            .excluded_adapters
                            .contains(&(probe.adapter_id.clone(), probe.adapter_version.clone()))
                        {
                            continue;
                        }
                        if matches!(adapter, AdapterHandle::Native { .. })
                            && let Some(format) = format
                        {
                            record_builtin_probe(
                                &mut builtin_probes,
                                format,
                                "script-probe-passed",
                                probe.required_files.iter().take(8).cloned().collect(),
                                None,
                            );
                        }
                        if verification == DiscoveryVerification::ProbeOnly {
                            return Ok(DiscoveryResult {
                                adapter: Some(adapter),
                                probe: Some(probe),
                                diagnostics,
                                file_count: inventory.files.len(),
                                complete: inventory.complete,
                                reuse_failures,
                                builtin_probes,
                            });
                        }
                        let mut extract = AdapterRequest::new("extract", source);
                        extract.resource_files = Some(probe.required_files.clone());
                        Self::report(
                            progress,
                            "检查文本提取",
                            format!("{} 已识别资源，正在检查文本提取。", adapter.id()),
                        );
                        match self.execute(&adapter, &extract, cancellation).await {
                            Ok(response)
                                if response.extraction.as_ref().is_some_and(|result| {
                                    result
                                        .units
                                        .iter()
                                        .any(|unit| !unit.source_text.trim().is_empty())
                                }) =>
                            {
                                return Ok(DiscoveryResult {
                                    adapter: Some(adapter),
                                    probe: Some(probe),
                                    diagnostics,
                                    file_count: inventory.files.len(),
                                    complete: inventory.complete,
                                    reuse_failures,
                                    builtin_probes,
                                });
                            }
                            Ok(_) => {}
                            Err(EngineError::Cancelled) => return Err(EngineError::Cancelled),
                            Err(error) => {
                                if let Some(format) = format {
                                    record_builtin_probe(
                                        &mut builtin_probes,
                                        format,
                                        "failed",
                                        vec![],
                                        Some(error.to_string().chars().take(2000).collect()),
                                    );
                                }
                                let message =
                                    format!("内置解析器 {} 提取失败：{error}", adapter.id());
                                Self::report(progress, "适配器检查未通过", &message);
                                diagnostics.push(message);
                            }
                        }
                    }
                }
                Err(EngineError::Cancelled) => return Err(EngineError::Cancelled),
                Err(
                    error @ (EngineError::Io(_)
                    | EngineError::ToolMissing(_)
                    | EngineError::Timeout),
                ) if verification == DiscoveryVerification::ProbeOnly => {
                    return Err(error);
                }
                Err(error) => {
                    if let Some(format) = format {
                        record_builtin_probe(
                            &mut builtin_probes,
                            format,
                            "failed",
                            vec![],
                            Some(error.to_string().chars().take(2000).collect()),
                        );
                    }
                    let message = format!("内置解析器 {} 检查失败：{error}", adapter.id());
                    Self::report(progress, "适配器检查未通过", &message);
                    diagnostics.push(message);
                }
            }
        }
        Ok(DiscoveryResult {
            adapter: None,
            probe: None,
            diagnostics,
            file_count: inventory.files.len(),
            complete: inventory.complete,
            reuse_failures,
            builtin_probes,
        })
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
        let deadline = cancellation.child_token();
        let operation = self.probe_bounded_inner(adapter, source, resources, inputs, &deadline);
        tokio::pin!(operation);
        tokio::select! {
            result = &mut operation => result,
            () = tokio::time::sleep(limit) => {
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
            cancellation,
            progress,
        )
        .await
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "Coverage anchors are independent of resource and baseline verification inputs."
    )]
    pub(crate) async fn verify_with_coverage(
        &self,
        adapter: &AdapterHandle,
        source: &Path,
        resources: &[String],
        directory: &Path,
        baseline: &[TranslationUnit],
        coverage: &Value,
        expected_source_hashes: Option<&BTreeMap<String, String>>,
        probe: Option<&ProbeResult>,
        cancellation: &CancellationToken,
        progress: ProgressCallback<'_>,
    ) -> Result<VerificationResult, EngineError> {
        let preparation_started = Instant::now();
        let mut stage_timings_ms = BTreeMap::new();
        Self::report(progress, "准备验收资源", "正在准备并校验资源副本。");
        fs::create_dir_all(directory)?;
        for resource in resources
            .iter()
            .filter(|path| path.to_lowercase().ends_with(".exe"))
        {
            let profile = format!("{resource}.le.config");
            if resolve_relative(source, &profile)?.is_file()
                && !resources
                    .iter()
                    .any(|path| path.eq_ignore_ascii_case(&profile))
            {
                return Err(EngineError::Rejected(format!(
                    "原程序的转区配置未纳入必需资源：{profile}"
                )));
            }
        }
        // Only reuse hashes calculated by this invocation's input inspection, not
        // hashes from a previous verification or a metadata-only file check.
        let original_hashes = match expected_source_hashes {
            Some(expected) => selected_hashes(expected, resources)?,
            None => hashes(source, resources)?,
        };
        let source_directory = directory.join("sources");
        fs::create_dir_all(&source_directory)?;
        for relative in resources {
            if cancellation.is_cancelled() {
                return Err(EngineError::Cancelled);
            }
            let input = resolve_relative(source, relative)?;
            let output = resolve_relative(&source_directory, relative)?;
            if let Some(parent) = output.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(input, output)?;
        }
        let before = hashes(&source_directory, resources)?;
        if before != original_hashes {
            return Err(EngineError::Rejected(
                "资源在创建验收副本时发生变化，请重新暂存。".into(),
            ));
        }
        let mut result = VerificationResult {
            resource_hashes: original_hashes,
            cacheable: true,
            ..VerificationResult::default()
        };
        stage_timings_ms.insert(
            "resource-preparation".into(),
            preparation_started.elapsed().as_millis() as u64,
        );
        let mut phase = "initial-extraction";
        let mut phase_started = Instant::now();
        let adapter_fingerprint = verified_adapter_fingerprint(adapter)?;
        let mut verified_extraction = None;
        let verify = async {
            Self::report(
                progress,
                "验收文本提取",
                "正在检查文本身份与已有译文可复用性。",
            );
            let mut request = AdapterRequest::new("extract", &source_directory);
            request.resource_files = Some(resources.to_vec());
            if let Some(probe) = probe {
                request.set_probe_context(probe);
            }
            let response = self
                .execute_with_hashes(adapter, &request, Some(&before), None, cancellation)
                .await?;
            let accepts_validation_context = matches!(
                adapter,
                AdapterHandle::Python { .. } | AdapterHandle::BuiltinKirikiriSmil
            ) && response.validation_context_version == Some(1);
            let extracted = response
                .extraction
                .ok_or_else(|| EngineError::Protocol("缺少提取结果。".into()))?;
            let units: Vec<_> = extracted
                .units
                .into_iter()
                .filter(|unit| !unit.source_text.trim().is_empty())
                .collect();
            gamel10n_translation::validate_units(&units)
                .map_err(|error| EngineError::Rejected(error.to_string()))?;
            for sample in coverage["samples"].as_array().into_iter().flatten() {
                if !units.iter().any(|unit| {
                    Some(unit.resource_path.as_str()) == sample["resourcePath"].as_str()
                        && Some(unit.locator.as_str()) == sample["locator"].as_str()
                        && Some(unit.source_text.as_str()) == sample["sourceText"].as_str()
                }) {
                    return Err(EngineError::Rejected(format!(
                        "独立覆盖锚点不匹配：{} {}",
                        sample["resourcePath"], sample["locator"]
                    )));
                }
            }
            let by_id: BTreeMap<_, _> = units.iter().map(|unit| (unit.id.as_str(), unit)).collect();
            record_verification_stage(&mut stage_timings_ms, phase, &mut phase_started);
            phase = "existing-text-compatibility";
            for previous in baseline {
                if by_id.get(previous.id.as_str()).is_none_or(|unit| {
                    unit.resource_path != previous.resource_path
                        || unit.locator != previous.locator
                        || unit.source_text != previous.source_text
                        || unit.context != previous.context
                        || unit.speaker != previous.speaker
                        || unit.kind != previous.kind
                        || token_set(unit) != token_set(previous)
                }) {
                    return Err(EngineError::Rejected(format!(
                        "已有文本身份或上下文发生变化：{}",
                        previous.id
                    )));
                }
            }
            result.units = units.len();
            let original: BTreeMap<_, _> = units
                .iter()
                .map(|unit| (unit.id.clone(), unit.source_text.clone()))
                .collect();
            record_verification_stage(&mut stage_timings_ms, phase, &mut phase_started);
            phase = "repeated-extraction";
            let repeated = self
                .execute_with_hashes(adapter, &request, Some(&before), None, cancellation)
                .await?
                .extraction
                .ok_or_else(|| EngineError::Protocol("重复提取缺少结果。".into()))?;
            let repeated: BTreeMap<_, _> = repeated
                .units
                .iter()
                .filter(|unit| !unit.source_text.trim().is_empty())
                .map(|unit| (unit.id.as_str(), unit))
                .collect();
            if repeated.len() != units.len()
                || units.iter().any(|unit| {
                    repeated.get(unit.id.as_str()).is_none_or(|again| {
                        again.source_text != unit.source_text
                            || again.resource_path != unit.resource_path
                            || again.locator != unit.locator
                            || again.kind != unit.kind
                            || again.speaker != unit.speaker
                            || token_set(again) != token_set(unit)
                    })
                })
            {
                return Err(EngineError::Rejected(
                    "重复提取改变了文本内容或身份。".into(),
                ));
            }
            let changed: BTreeMap<_, _> = units
                .iter()
                .enumerate()
                .map(|(index, unit)| {
                    (
                        unit.id.clone(),
                        gamel10n_translation::text::verification_text(unit, index, false),
                    )
                })
                .collect();
            let mutable: Vec<_> = units
                .iter()
                .enumerate()
                .filter(|(_, unit)| changed[&unit.id] != unit.source_text)
                .collect();
            let mut partial = BTreeMap::new();
            if let Some(&(index, unit)) = mutable.first() {
                partial.insert(
                    unit.id.clone(),
                    gamel10n_translation::text::verification_text(unit, index, true),
                );
            }
            if mutable.len() > 3 {
                let (index, unit) = mutable[mutable.len() / 2];
                partial.insert(
                    unit.id.clone(),
                    gamel10n_translation::text::verification_text(unit, index, true),
                );
            }
            for (stage, translations) in [
                ("原文回填", original),
                ("部分试译", partial),
                ("中文修改", changed),
            ] {
                record_verification_stage(&mut stage_timings_ms, phase, &mut phase_started);
                phase = match stage {
                    "原文回填" => "original-byte-roundtrip",
                    "部分试译" => "partial-translation-build",
                    _ => "full-translation-build",
                };
                if stage != "原文回填" && mutable.is_empty() {
                    return Err(EngineError::Rejected(
                        "所有提取文本均为保护标记或空白，无法生成有效测试译文；请提取可见文字。"
                            .into(),
                    ));
                }
                Self::report(
                    progress,
                    "验收资源回填",
                    format!("正在检查{stage}和未改动文本。"),
                );
                let output = directory.join(match stage {
                    "原文回填" => "original",
                    "中文修改" => "changed",
                    _ => "partial",
                });
                if output.exists() {
                    return Err(EngineError::Rejected(
                        "验收输出目录已存在，请使用新的验收目录。".into(),
                    ));
                }
                fs::create_dir_all(&output)?;
                let mut build = AdapterRequest::new("build", &source_directory);
                build.resource_files = Some(resources.to_vec());
                build.output_directory = Some(output.clone());
                build.translations = Some(translations.clone());
                if let Some(probe) = probe {
                    build.set_probe_context(probe);
                }
                let built = self
                    .execute_with_hashes(adapter, &build, Some(&before), None, cancellation)
                    .await?
                    .build
                    .ok_or_else(|| EngineError::Protocol("缺少回填结果。".into()))?;
                let overlay = built.mode == BuildMode::Overlay;
                for relative in resources.iter().filter(|_| !overlay) {
                    let target = resolve_relative(&output, relative)?;
                    if !target.is_file() {
                        return Err(EngineError::Rejected(format!("回填遗漏原资源：{relative}")));
                    }
                    if stage == "原文回填"
                        && hash_file(&target)
                            .map_err(|error| EngineError::Protocol(error.to_string()))?
                            != before[relative]
                    {
                        return Err(EngineError::Rejected(format!(
                            "原文回填改变了资源字节：{relative}"
                        )));
                    }
                    if relative.to_lowercase().ends_with(".le.config")
                        && hash_file(&target)
                            .map_err(|error| EngineError::Protocol(error.to_string()))?
                            != before[relative]
                    {
                        return Err(EngineError::Rejected(format!(
                            "回填改变了原启动转区配置：{relative}"
                        )));
                    }
                }
                if stage == "原文回填" && (!overlay || built.files.is_empty()) {
                    result.original_roundtrip_passed = true;
                    continue;
                }
                let mut extract = built.extraction_request(&source_directory, &output, resources);
                if let Some(probe) = probe {
                    extract.set_probe_context(probe);
                }
                record_verification_stage(&mut stage_timings_ms, phase, &mut phase_started);
                phase = match stage {
                    "原文回填" => "original-re-extraction",
                    "部分试译" => "partial-translation-re-extraction",
                    _ => "full-translation-re-extraction",
                };
                // Bind reused text to actual output bytes, including companion files.
                let output_hashes = (accepts_validation_context || overlay)
                    .then(|| directory_hashes(&output))
                    .transpose()?;
                let reextracted = self
                    .execute_with_hashes(
                        adapter,
                        &extract,
                        if overlay {
                            Some(&before)
                        } else {
                            output_hashes.as_ref()
                        },
                        if overlay {
                            output_hashes.as_ref()
                        } else {
                            None
                        },
                        cancellation,
                    )
                    .await?
                    .extraction
                    .ok_or_else(|| EngineError::Protocol("缺少回填后的提取结果。".into()))?;
                let after: BTreeMap<_, _> = reextracted
                    .units
                    .iter()
                    .filter(|unit| !unit.source_text.trim().is_empty())
                    .map(|unit| (verification_key(adapter, unit), unit))
                    .collect();
                if after.len() != units.len() {
                    return Err(EngineError::Rejected(format!("{stage}改变了文本数量。")));
                }
                for unit in &units {
                    let actual = after.get(&verification_key(adapter, unit)).ok_or_else(|| {
                        EngineError::Rejected(format!("回填丢失文本 ID：{}", unit.id))
                    })?;
                    let expected = translations.get(&unit.id).unwrap_or(&unit.source_text);
                    let builtin_sfa = matches!(adapter, AdapterHandle::BuiltinSfa16);
                    let expected = if builtin_sfa && stage != "原文回填" {
                        expected.replace('・', "·").replace('♪', "～")
                    } else {
                        expected.clone()
                    };
                    if actual.source_text != expected
                        || actual.resource_path != unit.resource_path
                        || actual.locator != unit.locator
                        || (overlay && actual.context != unit.context)
                        || (!builtin_sfa && actual.speaker != unit.speaker)
                        || actual.kind != unit.kind
                        || token_set(actual) != token_set(unit)
                    {
                        return Err(EngineError::Rejected(format!(
                            "{stage}未保留文本、身份或上下文：{}",
                            unit.id
                        )));
                    }
                }
                let mut validate = build;
                record_verification_stage(&mut stage_timings_ms, phase, &mut phase_started);
                phase = match stage {
                    "原文回填" => "original-validation",
                    "部分试译" => "partial-translation-validation",
                    _ => "full-translation-validation",
                };
                validate.operation = "validate".into();
                built.set_output_context(&mut validate);
                let validation_context = if accepts_validation_context {
                    let temporary = tempfile::Builder::new()
                        .prefix("validation-context-")
                        .tempdir_in(directory)?;
                    let path = temporary.path().join("context.json");
                    write_json(
                        &path,
                        &ValidationContext {
                            schema_version: 1,
                            source_directory: &source_directory,
                            output_directory: &output,
                            resource_files: resources,
                            output_files: &built.files,
                            original_units: &units,
                            output_units: &reextracted.units,
                        },
                    )
                    .map_err(|error| EngineError::Protocol(error.to_string()))?;
                    validate
                        .extra
                        .insert("validationContextFile".into(), json!(path.canonicalize()?));
                    Some(temporary)
                } else {
                    None
                };
                let validation = self
                    .execute_with_hashes(
                        adapter,
                        &validate,
                        Some(&before),
                        output_hashes.as_ref(),
                        cancellation,
                    )
                    .await?
                    .validation
                    .ok_or_else(|| EngineError::Protocol("缺少适配器校验结果。".into()))?;
                drop(validation_context);
                if !validation.passed || !validation.issues.is_empty() {
                    return Err(EngineError::Rejected(
                        validation
                            .issues
                            .iter()
                            .map(|issue| issue.message.as_str())
                            .collect::<Vec<_>>()
                            .join("；"),
                    ));
                }
                if !matches!(
                    adapter,
                    AdapterHandle::Python { .. } | AdapterHandle::BuiltinKirikiriSmil
                ) && hashes(&source_directory, resources)? != before
                {
                    return Err(EngineError::Rejected("适配器修改了只读资源副本。".into()));
                }
                if stage == "原文回填" {
                    result.original_roundtrip_passed = true;
                }
            }
            verified_extraction = Some(ExtractionResult {
                units,
                warnings: extracted.warnings,
            });
            Ok::<_, EngineError>(())
        }
        .await;
        record_verification_stage(&mut stage_timings_ms, phase, &mut phase_started);
        match verify {
            Ok(()) => {
                result.passed = true;
                result.diagnostics.push("文本提取、原文逐字节回填、中文修改及部分翻译验收通过；游戏启动与显示尚未测试。".into());
            }
            Err(EngineError::Cancelled) => return Err(EngineError::Cancelled),
            Err(error) => {
                result.cacheable = !matches!(
                    &error,
                    EngineError::Io(_) | EngineError::Timeout | EngineError::ToolMissing(_)
                );
                result.diagnostics.push(format!("[{phase}] {error}"));
            }
        }
        Self::report(
            progress,
            "复核验收资源",
            "正在复核原始资源与验收副本的完整性。",
        );
        for (root, expected, description) in [
            (source_directory.as_path(), &before, "验收副本"),
            (source, &result.resource_hashes, "原始资源"),
        ] {
            match hashes(root, resources) {
                Ok(actual) if &actual == expected => {}
                Ok(_) => {
                    result.passed = false;
                    result.cacheable = false;
                    result
                        .diagnostics
                        .push(format!("验收期间{description}发生变化。"));
                }
                Err(error) => {
                    result.passed = false;
                    result.cacheable = false;
                    result
                        .diagnostics
                        .push(format!("{description}完整性复核失败：{error}"));
                }
            }
        }
        record_verification_stage(
            &mut stage_timings_ms,
            "original-resource-check",
            &mut phase_started,
        );
        result.stage_timings_ms = stage_timings_ms;
        if result.passed
            && result.original_roundtrip_passed
            && let (Some(adapter_fingerprint), Some(extraction)) =
                (adapter_fingerprint, verified_extraction)
        {
            result.verified_extraction = Some(Arc::new(VerifiedExtraction {
                adapter_fingerprint,
                resource_hashes: result.resource_hashes.clone(),
                extraction,
            }));
        }
        write_json(&directory.join("verification.json"), &result)
            .map_err(|error| EngineError::Protocol(error.to_string()))?;
        Ok(result)
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
    package: &Path,
    source: &Path,
    probe: &ProbeResult,
) -> Result<String, EngineError> {
    Ok(hash_text(&json!({"verifierRevision":6,"host":"rust-python-v2","manifest":hash_file(&package.join("package.json")).map_err(|error|EngineError::Protocol(error.to_string()))?,"resources":hashes(source,&probe.required_files)?,"buildHints":probe.build_hints}).to_string()))
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
    let mut seen = BTreeSet::new();
    for unit in units {
        if unit.id.trim().is_empty() || !seen.insert(&unit.id) {
            return Err(EngineError::Incompatible {
                stage: "extraction",
                message: "提取包含空 ID 或重复 ID。".into(),
            });
        }
        resolve_relative(source, &unit.resource_path)?;
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

    #[tokio::test]
    async fn verification_checks_copied_bytes_and_source_changes_with_known_hashes() {
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
                &cancellation,
                None,
            )
            .await
            .expect_err("the copied bytes must match the inspected hashes");
        assert!(
            error.to_string().contains("创建验收副本时发生变化"),
            "{error}"
        );

        fs::write(source.join("text.json"), original).expect("restore fixture");
        let mutate_after_copy = |event: EngineProgress| {
            if event.stage == "验收文本提取" {
                fs::write(
                    source.join("text.json"),
                    b"[\"ONE\",\"two\",\"three\",\"four\"]",
                )
                .expect("mutation after copying");
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
                &cancellation,
                Some(&mutate_after_copy),
            )
            .await
            .expect("verification diagnostics");
        assert!(!result.passed);
        assert!(!result.cacheable);
        assert!(
            result.original_roundtrip_passed,
            "the independent copy stays unchanged"
        );
        assert_eq!(
            fs::read(verification.join("sources/text.json")).unwrap(),
            original
        );
        assert_eq!(result.resource_hashes.len(), 1);
        assert!(
            result
                .diagnostics
                .iter()
                .any(|item| item.contains("原始资源发生变化"))
        );
    }

    #[tokio::test]
    async fn verification_preserves_adapter_errors_and_checks_both_resource_roots() {
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
        let mutate_source = |event: EngineProgress| {
            if event.stage == "验收文本提取" {
                fs::write(source.join("text.json"), b"[\"ONE\"]").expect("mutation");
            }
        };
        let result = service
            .verify(
                &adapter,
                &source,
                &["text.json".into()],
                &root.path().join("verify"),
                &[],
                &CancellationToken::new(),
                Some(&mutate_source),
            )
            .await
            .expect("preserved failure diagnostics");
        assert!(!result.passed);
        assert!(!result.cacheable);
        for expected in [
            "fixture extraction failure",
            "验收副本完整性复核失败",
            "原始资源发生变化",
        ] {
            assert!(
                result
                    .diagnostics
                    .iter()
                    .any(|item| item.contains(expected)),
                "{:?}",
                result.diagnostics
            );
        }
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
            "r = json.load(sys.stdin)\nassert r['operation'] == 'probe', 'recognition must only probe'\nwith Path(__file__).with_name('calls.log').open('a', encoding='utf8') as log:\n    log.write(r['operation'] + '\\n')",
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
        assert_eq!(
            fs::read_to_string(entry.with_file_name("calls.log"))
                .expect("calls")
                .lines()
                .collect::<Vec<_>>(),
            ["probe", "probe"]
        );

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
    Path(p['dataPath']).write_bytes(b'x'*p['length'])
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
        let result = service
            .probe_bounded(
                &adapter,
                root.path(),
                &["scripts.dat".into()],
                &CancellationToken::new(),
            )
            .await;
        assert!(
            matches!(result,Err(EngineError::Rejected(message)) if message.contains("只读探测片段"))
        );
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
Path(r['responseFile']).write_text(json.dumps(dict(protocolVersion=1,success=False,errorKind='incompatible',error='unsupported layout')),encoding='utf8')
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
        assert!(matches!(error, EngineError::Rejected(message) if message.contains("只读源资源")));
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
}
