//! Bounded directory evidence and reusable adapter discovery.

use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet, HashMap},
    fs::{self, File},
    io::{Read, Seek, SeekFrom},
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex},
};

use gamel10n_translation::files::{hash_file, read_json};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    EngineError,
    vntextpatch::{BUILTIN_FORMATS, FORMATS},
};

mod filter;
pub use filter::DiscoveryFilter;

const RULE_FILE_LIMIT: usize = 128;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "runtime", rename_all = "camelCase")]
pub enum AdapterHandle {
    Native {
        format: String,
    },
    Python {
        entry: PathBuf,
        package_directory: Option<PathBuf>,
        id: String,
        version: String,
    },
    BuiltinSiglus,
    BuiltinSfa16,
    BuiltinKirikiriSmil,
}

impl AdapterHandle {
    pub fn id(&self) -> &str {
        match self {
            Self::Native { format } => format,
            Self::Python { id, .. } => id,
            Self::BuiltinSiglus => "engine.siglus",
            Self::BuiltinSfa16 => "engine.sfa16.aos-v2",
            Self::BuiltinKirikiriSmil => "engine.kirikiri-smil-smile",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PackageManifest {
    #[serde(alias = "Id")]
    pub id: String,
    #[serde(alias = "Version")]
    pub version: String,
    #[serde(default, alias = "ManifestVersion")]
    pub manifest_version: u32,
    #[serde(default = "protocol_one", alias = "ProtocolVersion")]
    pub protocol_version: u32,
    #[serde(default, alias = "ResourceFiles")]
    pub resource_files: Vec<String>,
    #[serde(default, alias = "ResourcePatterns")]
    pub resource_patterns: Vec<String>,
    #[serde(default, alias = "PackageFiles")]
    pub package_files: BTreeMap<String, String>,
    #[serde(default, alias = "Recognition")]
    pub recognition: Option<Recognition>,
    #[serde(default, alias = "Family")]
    pub family: Value,
    #[serde(default, flatten)]
    pub metadata: BTreeMap<String, Value>,
}

fn protocol_one() -> u32 {
    1
}

impl PackageManifest {
    pub fn load(directory: &Path) -> Result<Self, EngineError> {
        read_json(&directory.join("package.json"))
            .map_err(|error| EngineError::Protocol(error.to_string()))
    }

    /// Resolves an explicitly verified Python replacement without changing a project's source identity.
    pub fn load_runtime(directory: &Path) -> Result<(Self, PathBuf), EngineError> {
        let manifest = Self::load(directory)?;
        if !matches!(manifest.manifest_version, 2 | 3) {
            return Ok((manifest, directory.into()));
        }
        let Some(packages) = directory.parent().and_then(Path::parent) else {
            return Ok((manifest, directory.into()));
        };
        let Some(extensions) = packages.parent() else {
            return Ok((manifest, directory.into()));
        };
        let mapping_path = extensions.join("runtime-migrations.json");
        if !mapping_path.is_file() {
            return Ok((manifest, directory.into()));
        }
        let mappings: RuntimeMappings =
            read_json(&mapping_path).map_err(|error| EngineError::Protocol(error.to_string()))?;
        if mappings.schema_version != 1 {
            return Err(EngineError::Protocol(
                "适配器运行包映射版本不受支持。".into(),
            ));
        }
        let key = format!("{}/{}", manifest.id, manifest.version);
        let Some(mapping) = mappings.entries.get(&key) else {
            return Ok((manifest, directory.into()));
        };
        let replacement = resolve_relative(packages, &mapping.target)?;
        let hash =
            |path: &Path| hash_file(path).map_err(|error| EngineError::Protocol(error.to_string()));
        if hash(&directory.join("package.json"))? != mapping.source_manifest_sha256
            || hash(&replacement.join("package.json"))? != mapping.target_manifest_sha256
        {
            return Err(EngineError::Rejected(
                "适配器运行包映射的来源或目标已变化，请重新验证。".into(),
            ));
        }
        let runtime_manifest = Self::load(&replacement)?;
        if runtime_manifest.manifest_version != 4 || runtime_manifest.id != manifest.id {
            return Err(EngineError::Rejected(
                "映射目标必须是同一引擎的 Python 发布包。".into(),
            ));
        }
        let source = runtime_manifest.metadata.get("migrationSource");
        if source
            .and_then(|value| value.get("version"))
            .and_then(Value::as_str)
            != Some(&manifest.version)
            || source
                .and_then(|value| value.get("manifestSha256"))
                .and_then(Value::as_str)
                != Some(&mapping.source_manifest_sha256)
        {
            return Err(EngineError::Rejected(
                "Python 发布包与运行包映射的来源不一致。".into(),
            ));
        }
        Ok((runtime_manifest, replacement))
    }

    /// Verifies the Python package and resolves its entry point.
    pub fn handle(&self, directory: &Path) -> Result<AdapterHandle, EngineError> {
        if self.manifest_version != 4 || self.protocol_version != 1 {
            return Err(EngineError::Rejected(format!(
                "适配器 {} {} 使用不支持的运行格式或清单版本；请使用 Python 适配器并重新验收发布。",
                self.id, self.version
            )));
        }
        if self.package_files.is_empty() || !self.package_files.contains_key("src/adapter.py") {
            return Err(EngineError::Protocol(
                "Python 发布包缺少完整文件哈希或 src/adapter.py。".into(),
            ));
        }
        for (relative, expected) in &self.package_files {
            let path = resolve_relative(directory, relative)?;
            if expected.len() != 64
                || hash_file(&path).map_err(|error| EngineError::Protocol(error.to_string()))?
                    != *expected
            {
                return Err(EngineError::Rejected(format!(
                    "适配器文件已变化，请恢复原发布包：{relative}"
                )));
            }
        }
        Ok(AdapterHandle::Python {
            entry: resolve_relative(directory, "src/adapter.py")?,
            package_directory: Some(directory.into()),
            id: self.id.clone(),
            version: self.version.clone(),
        })
    }

    pub fn resources(&self, inventory: &Inventory) -> Vec<String> {
        if self.resource_patterns.is_empty() {
            self.resource_files
                .iter()
                .filter(|path| {
                    inventory
                        .files
                        .iter()
                        .any(|file| file.path.eq_ignore_ascii_case(path))
                })
                .cloned()
                .collect()
        } else {
            inventory
                .files
                .iter()
                .filter(|file| {
                    (!is_documentation_file(&file.path)
                        || self
                            .resource_files
                            .iter()
                            .any(|path| path.eq_ignore_ascii_case(&file.path)))
                        && self
                            .resource_patterns
                            .iter()
                            .any(|pattern| glob_match(pattern, &file.path))
                })
                .map(|file| file.path.clone())
                .collect()
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeMappings {
    schema_version: u32,
    entries: BTreeMap<String, RuntimeMapping>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeMapping {
    target: String,
    source_manifest_sha256: String,
    target_manifest_sha256: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Recognition {
    pub schema_version: u32,
    #[serde(default, deserialize_with = "null_default")]
    pub required: Vec<FileRule>,
    #[serde(default, deserialize_with = "null_default")]
    pub hints: Vec<FileRule>,
}

impl Recognition {
    pub fn validate(&self) -> Result<(), EngineError> {
        if self.schema_version != 1 || self.required.len() + self.hints.len() > 32 {
            return Err(EngineError::Protocol("识别声明版本或规则数量无效。".into()));
        }
        for rule in self.required.iter().chain(&self.hints) {
            if rule.patterns.is_empty()
                || rule.patterns.len() > 16
                || rule.signatures.len() > 8
                || rule
                    .minimum_size
                    .zip(rule.maximum_size)
                    .is_some_and(|(min, max)| min > max)
            {
                return Err(EngineError::Protocol("识别文件规则无效。".into()));
            }
            for pattern in &rule.patterns {
                validate_pattern(pattern)?;
            }
            for signature in &rule.signatures {
                if decode_hex(&signature.hex).is_none()
                    || !matches!(signature.origin.as_str(), "start" | "end")
                {
                    return Err(EngineError::Protocol(
                        "识别签名必须是 1–64 字节的十六进制，origin 为 start/end。".into(),
                    ));
                }
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase")]
pub struct FileRule {
    #[serde(default, deserialize_with = "null_default")]
    pub patterns: Vec<String>,
    pub minimum_size: Option<u64>,
    pub maximum_size: Option<u64>,
    #[serde(default, deserialize_with = "null_default")]
    pub signatures: Vec<Signature>,
}

fn null_default<'de, T: Deserialize<'de> + Default, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<T, D::Error> {
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, Hash)]
pub struct Signature {
    pub offset: u64,
    pub hex: String,
    #[serde(default = "start_origin")]
    pub origin: String,
}
fn start_origin() -> String {
    "start".into()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum RecognitionStatus {
    Match,
    NoMatch,
    Unknown,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InventoryFile {
    pub path: String,
    pub length: u64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Inventory {
    pub files: Vec<InventoryFile>,
    pub complete: bool,
    #[serde(skip)]
    signatures: Arc<Mutex<SignatureCache>>,
}

#[derive(Debug, Default)]
struct SignatureCache {
    reads: usize,
    ranges: BTreeMap<(PathBuf, u64, usize), Option<Vec<u8>>>,
}

impl Inventory {
    /// Complete implicit resource selection; explicit adapter resources use their own contract.
    pub(crate) fn candidate_resources(&self) -> Result<Vec<String>, EngineError> {
        self.require_complete_native_inventory()?;
        Ok(self
            .files
            .iter()
            .filter(|file| !is_documentation_file(&file.path))
            .map(|file| file.path.clone())
            .collect())
    }

    pub(crate) fn require_complete_native_inventory(&self) -> Result<(), EngineError> {
        if !self.complete {
            return Err(EngineError::Rejected(
                "资源目录扫描不完整，无法自动选择原生解析器资源；请明确指定资源范围。".into(),
            ));
        }
        Ok(())
    }

    pub fn scan(root: &Path) -> Result<Self, EngineError> {
        if !root.is_dir() {
            return Err(EngineError::Protocol("游戏目录不存在。".into()));
        }
        let mut value = Self {
            files: Vec::new(),
            complete: true,
            signatures: Arc::default(),
        };
        let mut stack = vec![root.to_path_buf()];
        while let Some(directory) = stack.pop() {
            let entries = match fs::read_dir(&directory) {
                Ok(entries) => entries,
                Err(_) => {
                    value.complete = false;
                    continue;
                }
            };
            for entry in entries {
                let Ok(entry) = entry else {
                    value.complete = false;
                    continue;
                };
                let Ok(metadata) = entry.path().symlink_metadata() else {
                    value.complete = false;
                    continue;
                };
                if is_link(&metadata) {
                    value.complete = false;
                    continue;
                }
                if metadata.is_dir() {
                    stack.push(entry.path());
                } else if metadata.is_file() {
                    if value.files.len() >= 100_000 {
                        value.complete = false;
                        stack.clear();
                        break;
                    }
                    let relative = entry
                        .path()
                        .strip_prefix(root)
                        .map_err(|error| EngineError::Protocol(error.to_string()))?
                        .to_string_lossy()
                        .replace('\\', "/");
                    value.files.push(InventoryFile {
                        path: relative,
                        length: metadata.len(),
                    });
                }
            }
        }
        value.files.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(value)
    }

    pub fn recognize(
        &self,
        root: &Path,
        recognition: Option<&Recognition>,
    ) -> (RecognitionStatus, usize) {
        self.recognize_with(recognition, &|relative| Some(root.join(relative)))
    }

    pub(crate) fn recognize_with(
        &self,
        recognition: Option<&Recognition>,
        resolve: &impl Fn(&str) -> Option<PathBuf>,
    ) -> (RecognitionStatus, usize) {
        let Some(recognition) = recognition else {
            return (RecognitionStatus::Unknown, 0);
        };
        if recognition.schema_version != 1
            || recognition.required.len() + recognition.hints.len() > 32
        {
            return (RecognitionStatus::Unknown, 0);
        }
        let mut reads = 0;
        let mut evidence = 0;
        let mut unknown = false;
        for rule in &recognition.required {
            match self.rule_matches(rule, &mut reads, resolve) {
                RecognitionStatus::Match => evidence += 1,
                RecognitionStatus::NoMatch => return (RecognitionStatus::NoMatch, evidence),
                RecognitionStatus::Unknown => unknown = true,
            }
        }
        for rule in &recognition.hints {
            if self.rule_matches(rule, &mut reads, resolve) == RecognitionStatus::Match {
                evidence += 1;
            }
        }
        (
            if unknown {
                RecognitionStatus::Unknown
            } else {
                RecognitionStatus::Match
            },
            evidence,
        )
    }

    fn rule_matches(
        &self,
        rule: &FileRule,
        reads: &mut usize,
        resolve: &impl Fn(&str) -> Option<PathBuf>,
    ) -> RecognitionStatus {
        if rule.patterns.is_empty()
            || rule.patterns.len() > 16
            || rule.signatures.len() > 8
            || rule
                .patterns
                .iter()
                .any(|pattern| validate_pattern(pattern).is_err())
        {
            return RecognitionStatus::Unknown;
        }
        self.matching_rule_files(
            rule,
            reads,
            resolve,
            self.files.iter().filter(|file| {
                rule.patterns
                    .iter()
                    .any(|pattern| glob_match(pattern, &file.path))
            }),
        )
    }

    fn matching_rule_files<'a>(
        &self,
        rule: &FileRule,
        reads: &mut usize,
        resolve: &impl Fn(&str) -> Option<PathBuf>,
        files: impl Iterator<Item = &'a InventoryFile>,
    ) -> RecognitionStatus {
        let mut unknown = !self.complete;
        for (index, file) in files.enumerate() {
            if index >= RULE_FILE_LIMIT {
                unknown = true;
                break;
            }
            if rule.minimum_size.is_some_and(|size| file.length < size)
                || rule.maximum_size.is_some_and(|size| file.length > size)
            {
                continue;
            }
            let mut matched = true;
            for signature in &rule.signatures {
                let Some(expected) = decode_hex(&signature.hex) else {
                    unknown = true;
                    matched = false;
                    break;
                };
                let offset = match signature.origin.as_str() {
                    "start" => signature.offset,
                    "end" => match file
                        .length
                        .checked_sub(signature.offset)
                        .and_then(|end| end.checked_sub(expected.len() as u64))
                    {
                        Some(offset) => offset,
                        None => {
                            matched = false;
                            break;
                        }
                    },
                    _ => {
                        unknown = true;
                        matched = false;
                        break;
                    }
                };
                if *reads >= 512 {
                    unknown = true;
                    matched = false;
                    break;
                }
                *reads += 1;
                let read = resolve(&file.path)
                    .and_then(|path| self.signature(path, offset, expected.len()));
                match read {
                    Some(bytes) if bytes == expected => {}
                    Some(_) => {
                        matched = false;
                        break;
                    }
                    None => {
                        unknown = true;
                        matched = false;
                        break;
                    }
                }
            }
            if matched {
                return RecognitionStatus::Match;
            }
        }
        if unknown {
            RecognitionStatus::Unknown
        } else {
            RecognitionStatus::NoMatch
        }
    }

    fn signature(&self, path: PathBuf, offset: u64, length: usize) -> Option<Vec<u8>> {
        let mut cache = self.signatures.lock().ok()?;
        let key = (path, offset, length);
        if let Some(value) = cache.ranges.get(&key) {
            return value.clone();
        }
        if cache.reads >= 512 {
            return None;
        }
        cache.reads += 1;
        let value = (|| -> std::io::Result<Vec<u8>> {
            let mut stream = File::open(&key.0)?;
            stream.seek(SeekFrom::Start(offset))?;
            let mut bytes = vec![0; length];
            stream.read_exact(&mut bytes)?;
            Ok(bytes)
        })()
        .ok();
        cache.ranges.insert(key, value.clone());
        value
    }
}

pub(crate) fn is_link(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}

/// Resolve an exact path without permitting traversal, drive aliases or reparse points.
pub fn resolve_relative(root: &Path, relative: &str) -> Result<PathBuf, EngineError> {
    if relative.is_empty()
        || relative.contains(':')
        || relative.contains('\0')
        || relative.starts_with(['/', '\\'])
    {
        return Err(EngineError::Protocol("路径必须位于指定目录内。".into()));
    }
    let normalized = relative.replace('\\', "/");
    validate_segments(&normalized, false)?;
    let path = Path::new(&normalized);
    if path
        .components()
        .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(EngineError::Protocol("路径包含目录穿越。".into()));
    }
    let mut result = root.to_path_buf();
    for component in path.components() {
        result.push(component);
        if let Ok(metadata) = result.symlink_metadata()
            && is_link(&metadata)
        {
            return Err(EngineError::Protocol(
                "资源路径不能使用符号链接或重解析点。".into(),
            ));
        }
    }
    Ok(result)
}

pub fn validate_pattern(pattern: &str) -> Result<(), EngineError> {
    validate_segments(&pattern.replace('\\', "/"), true)
}

fn validate_segments(value: &str, pattern: bool) -> Result<(), EngineError> {
    if value.is_empty()
        || value.len() > 1024
        || value.starts_with('/')
        || value.contains(':')
        || value.chars().any(|value| {
            value.is_control() || "<>\"|".contains(value) || !pattern && "*?".contains(value)
        })
    {
        return Err(EngineError::Protocol("相对路径或资源模式无效。".into()));
    }
    for part in value.split('/') {
        let device = part.split('.').next().unwrap_or("").to_ascii_uppercase();
        if part.is_empty()
            || matches!(part, "." | "..")
            || part.ends_with(['.', ' '])
            || ["CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$", "CLOCK$"]
                .contains(&device.as_str())
            || device.len() == 4
                && (device.starts_with("COM") || device.starts_with("LPT"))
                && device.as_bytes()[3].is_ascii_digit()
        {
            return Err(EngineError::Protocol(
                "路径含有目录穿越或 Windows 保留名称。".into(),
            ));
        }
    }
    Ok(())
}

#[inline]
fn norm_ascii(byte: u8) -> u8 {
    if byte == b'\\' {
        b'/'
    } else {
        byte.to_ascii_lowercase()
    }
}

/// Zero-allocation O(P + S) matcher for ASCII patterns containing no `**`.
fn glob_match_ascii_single(pattern: &[u8], path: &[u8]) -> bool {
    let (mut pi, mut si) = (0usize, 0usize);
    let mut star_pi: Option<usize> = None;
    let mut star_si = 0usize;
    while si < path.len() {
        let sc = norm_ascii(path[si]);
        if pi < pattern.len() && pattern[pi] == b'*' {
            star_pi = Some(pi);
            pi += 1;
            star_si = si;
        } else if pi < pattern.len()
            && (norm_ascii(pattern[pi]) == sc || (pattern[pi] == b'?' && sc != b'/'))
        {
            pi += 1;
            si += 1;
        } else if let Some(sp) = star_pi {
            if norm_ascii(path[star_si]) == b'/' {
                return false;
            }
            star_si += 1;
            si = star_si;
            pi = sp + 1;
        } else {
            return false;
        }
    }
    while pi < pattern.len() && pattern[pi] == b'*' {
        pi += 1;
    }
    pi == pattern.len()
}

pub fn glob_match(pattern: &str, path: &str) -> bool {
    if pattern.len() > 1024 || path.len() > 4096 {
        return false;
    }
    if pattern.is_ascii() && path.is_ascii() {
        let pb = pattern.as_bytes();
        let sb = path.as_bytes();
        if !pb.windows(2).any(|w| w == b"**") {
            return glob_match_ascii_single(pb, sb);
        }
        // Fast path for the most common manifest pattern: `**/<single-level-pattern>` (e.g. `**/*.ks`).
        if (pb.starts_with(b"**/") || pb.starts_with(b"**\\"))
            && !pb[3..].windows(2).any(|w| w == b"**")
        {
            let rest = &pb[3..];
            if glob_match_ascii_single(rest, sb) {
                return true;
            }
            for (idx, &b) in sb.iter().enumerate() {
                if (b == b'/' || b == b'\\') && glob_match_ascii_single(rest, &sb[idx + 1..]) {
                    return true;
                }
            }
            return false;
        }
    }
    let pattern: Vec<_> = pattern.replace('\\', "/").to_lowercase().chars().collect();
    let path: Vec<_> = path.replace('\\', "/").to_lowercase().chars().collect();
    let mut memo = vec![0u8; (pattern.len() + 1) * (path.len() + 1)];
    fn matched(p: &[char], s: &[char], i: usize, j: usize, memo: &mut [u8]) -> bool {
        let key = i * (s.len() + 1) + j;
        if memo[key] != 0 {
            return memo[key] == 2;
        }
        let value = if i == p.len() {
            j == s.len()
        } else if p[i..].starts_with(&['*', '*', '/']) {
            ((j == 0 || s[j - 1] == '/') && matched(p, s, i + 3, j, memo))
                || (j < s.len() && matched(p, s, i, j + 1, memo))
        } else if p[i..].starts_with(&['*', '*']) {
            matched(p, s, i + 2, j, memo) || (j < s.len() && matched(p, s, i, j + 1, memo))
        } else if p[i] == '*' {
            matched(p, s, i + 1, j, memo)
                || (j < s.len() && s[j] != '/' && matched(p, s, i, j + 1, memo))
        } else {
            j < s.len()
                && (p[i] == s[j] || p[i] == '?' && s[j] != '/')
                && matched(p, s, i + 1, j + 1, memo)
        };
        memo[key] = if value { 2 } else { 1 };
        value
    }
    matched(&pattern, &path, 0, 0, &mut memo)
}

fn decode_hex(value: &str) -> Option<Vec<u8>> {
    if value.is_empty() || value.len() > 128 || !value.len().is_multiple_of(2) {
        return None;
    }
    let (pairs, _) = value.as_bytes().as_chunks::<2>();
    pairs
        .iter()
        .map(|&[high, low]| {
            Some((char::from(high).to_digit(16)? * 16 + char::from(low).to_digit(16)?) as u8)
        })
        .collect()
}

/// Built-in format/resource selection follows the original parser catalog.
pub fn native_formats_for(inventory: &Inventory) -> Vec<&'static str> {
    native_candidates(inventory)
        .map(|(format, _)| format)
        .collect()
}

fn split_file_path(path: &str) -> (&str, &str) {
    path.rsplit_once('/').unwrap_or(("", path))
}

fn native_extension(path: &str) -> Cow<'_, str> {
    let (_, name) = split_file_path(path);
    // Match the worker's Path.GetExtension, including names such as ".txt".
    let extension = name.rsplit_once('.').map_or("", |(_, extension)| extension);
    if extension.bytes().any(|byte| byte.is_ascii_uppercase()) {
        Cow::Owned(extension.to_ascii_lowercase())
    } else {
        Cow::Borrowed(extension)
    }
}

pub(crate) fn native_candidates(
    inventory: &Inventory,
) -> impl Iterator<Item = (&'static str, Vec<&str>)> {
    let mut files_by_extension: HashMap<_, Vec<_>> = BUILTIN_FORMATS
        .iter()
        .flat_map(|format| {
            format
                .extensions
                .iter()
                .map(|&extension| Cow::Borrowed(extension))
                .chain(
                    format
                        .companions()
                        .iter()
                        .map(|name| native_extension(name)),
                )
        })
        .map(|extension| (extension, Vec::new()))
        .collect();
    for file in &inventory.files {
        if let Some(files) = files_by_extension.get_mut(native_extension(&file.path).as_ref())
            && !is_documentation_file(&file.path)
        {
            files.push(file.path.as_str());
        }
    }

    let tyrano = crate::tyrano::TyranoData::from_inventory(inventory, &files_by_extension["ks"]);
    FORMATS.iter().filter_map(move |&key| {
        let format = BUILTIN_FORMATS.iter().find(|format| format.key == key)?;
        let mut resources: Vec<_> = format
            .extensions
            .iter()
            .flat_map(|&extension| files_by_extension[extension].iter().copied())
            .collect();
        if key == "KirikiriKs" {
            resources.retain(|path| !tyrano.contains(path));
        }
        if resources.is_empty() {
            return None;
        }
        if !format.companions().is_empty() {
            let script_directories: BTreeSet<_> = resources
                .iter()
                .copied()
                .map(|file| split_file_path(file).0)
                .collect();
            for companion in format.companions() {
                resources.extend(
                    files_by_extension[native_extension(companion).as_ref()]
                        .iter()
                        .copied()
                        .filter(|path| {
                            let (directory, name) = split_file_path(path);
                            name.eq_ignore_ascii_case(companion)
                                && script_directories.contains(directory)
                        }),
                );
            }
        }
        resources.sort_unstable();
        resources.dedup();
        Some((key, resources))
    })
}

/// Exact documentation names are excluded from automatic resource candidates only.
pub(crate) fn is_documentation_file(path: &str) -> bool {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    let (stem, extension) = name.rsplit_once('.').unwrap_or((name, ""));
    ["", "txt", "md", "markdown", "rst"]
        .iter()
        .any(|value| extension.eq_ignore_ascii_case(value))
        && [
            "readme",
            "read me",
            "read_me",
            "read-me",
            "license",
            "licence",
            "changelog",
            "changes",
            "authors",
            "contributors",
            "copying",
        ]
        .iter()
        .any(|value| stem.eq_ignore_ascii_case(value))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn documentation_names_match_exact_basenames_and_document_extensions() {
        for stem in [
            "README",
            "Read Me",
            "Read_Me",
            "Read-Me",
            "LICENSE",
            "LICENCE",
            "CHANGELOG",
            "CHANGES",
            "AUTHORS",
            "CONTRIBUTORS",
            "COPYING",
        ] {
            for suffix in ["", ".txt", ".MD", ".Markdown", ".RST"] {
                assert!(is_documentation_file(&format!("nested/{stem}{suffix}")));
                assert!(is_documentation_file(&format!("nested\\{stem}{suffix}")));
            }
        }
        for path in [
            "story.txt",
            "Readme.ks",
            "license.dat",
            "README.en.txt",
            "myreadme.txt",
            "readme_extra.txt",
            "README/story.txt",
            "readme.txt.ks",
        ] {
            assert!(!is_documentation_file(path), "{path}");
        }
    }

    #[test]
    fn documentation_stays_in_inventory_without_triggering_native_formats() {
        let root = tempfile::tempdir().unwrap();
        for name in ["ReAdMe.TxT", "LICENSE", "ChAnGeS.MD"] {
            fs::write(root.path().join(name), b"documentation").unwrap();
        }
        let inventory = Inventory::scan(root.path()).unwrap();
        assert_eq!(inventory.files.len(), 3);
        assert!(native_formats_for(&inventory).is_empty());
        assert!(inventory.candidate_resources().unwrap().is_empty());
        fs::write(root.path().join("story.txt"), b"scenario").unwrap();
        let mut inventory = Inventory::scan(root.path()).unwrap();
        assert!(native_formats_for(&inventory).contains(&"KirikiriKs"));
        assert_eq!(inventory.candidate_resources().unwrap(), ["story.txt"]);
        inventory.complete = false;
        assert!(matches!(
            inventory.candidate_resources(),
            Err(EngineError::Rejected(_))
        ));
    }

    #[test]
    fn native_candidates_send_one_script_to_each_text_parser_in_catalog_order() {
        let mut files: Vec<_> = (0..10_000)
            .map(|index| InventoryFile {
                path: format!("audio/{index:05}.ogg"),
                length: 0,
            })
            .collect();
        files.push(InventoryFile {
            path: "story.txt".into(),
            length: 0,
        });
        let inventory = Inventory {
            files,
            complete: true,
            signatures: Arc::default(),
        };
        assert_eq!(
            native_candidates(&inventory).collect::<Vec<_>>(),
            [
                ("ArtemisTxt", vec!["story.txt"]),
                ("KirikiriKs", vec!["story.txt"]),
                ("RealLive", vec!["story.txt"]),
                ("TmrHiroAdvSystemText", vec!["story.txt"]),
                ("Whale", vec!["story.txt"]),
            ]
        );
    }

    #[test]
    fn native_candidates_keep_extensionless_scripts_and_same_directory_companions() {
        let root = tempfile::tempdir().unwrap();
        for name in [
            "assets/POINT.DAT",
            "script/point.dat",
            "script/READMe",
            "script/scene",
            "script/story.SrC",
            "script/TeXt.DaT",
            "script/voice.ogg",
        ] {
            let path = root.path().join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, b"resource").unwrap();
        }
        let inventory = Inventory::scan(root.path()).unwrap();
        let candidates: BTreeMap<_, _> = native_candidates(&inventory).collect();
        assert_eq!(candidates["Ethornell"], ["script/scene"]);
        assert_eq!(candidates["Whale"], ["script/scene"]);
        assert_eq!(
            candidates["Softpal"],
            ["script/TeXt.DaT", "script/point.dat", "script/story.SrC"]
        );
    }

    #[test]
    fn resource_patterns_exclude_documents_but_keep_explicit_resource_contracts() {
        let root = tempfile::tempdir().unwrap();
        for name in ["ReAdMe.TxT", "LICENSE.md", "story.txt"] {
            fs::write(root.path().join(name), b"resource").unwrap();
        }
        let inventory = Inventory::scan(root.path()).unwrap();
        let mut manifest: PackageManifest = serde_json::from_value(serde_json::json!({
            "id":"fixture","version":"1","resourcePatterns":["*.txt"]
        }))
        .unwrap();
        assert_eq!(manifest.resources(&inventory), ["story.txt"]);
        manifest.resource_files = vec!["README.TXT".into(), "LICENSE.md".into()];
        assert_eq!(manifest.resources(&inventory), ["ReAdMe.TxT", "story.txt"]);
        manifest.resource_patterns.clear();
        assert_eq!(manifest.resources(&inventory), ["README.TXT", "LICENSE.md"]);
    }

    #[test]
    fn verified_runtime_mapping_preserves_manifest_and_checks_both_packages() {
        use gamel10n_translation::files::write_json;
        use serde_json::json;

        let temp = tempfile::tempdir().expect("fixture");
        let legacy = temp.path().join("packages/engine.example/legacy");
        let python = temp.path().join("packages/engine.example/python");
        fs::create_dir_all(&legacy).expect("fixture");
        fs::create_dir_all(python.join("src")).expect("fixture");
        let legacy_path = legacy.join("package.json");
        write_json(
            &legacy_path,
            &json!({"id":"engine.example","version":"legacy","manifestVersion":3}),
        )
        .expect("fixture");
        let source_hash = hash_file(&legacy_path).expect("hash");
        fs::write(python.join("src/adapter.py"), "print('{}')").expect("fixture");
        write_json(&python.join("package.json"), &json!({
            "id":"engine.example","version":"python","manifestVersion":4,
            "packageFiles":{"src/adapter.py":hash_file(&python.join("src/adapter.py")).expect("hash")},
            "migrationSource":{"version":"legacy","manifestSha256":source_hash}
        })).expect("fixture");
        write_json(
            &temp.path().join("runtime-migrations.json"),
            &json!({
                "schemaVersion":1,"entries":{"engine.example/legacy":{
                    "target":"engine.example/python","sourceManifestSha256":source_hash,
                    "targetManifestSha256":hash_file(&python.join("package.json")).expect("hash")
                }}
            }),
        )
        .expect("fixture");

        let (manifest, runtime) =
            PackageManifest::load_runtime(&legacy).expect("verified replacement");
        assert_eq!(runtime, python);
        assert_eq!(manifest.version, "python");
        assert!(manifest.handle(&runtime).is_ok());
        assert_eq!(hash_file(&legacy_path).expect("hash"), source_hash);
        fs::write(python.join("src/adapter.py"), "changed").expect("tamper");
        assert!(manifest.handle(&runtime).is_err());
        fs::write(python.join("package.json"), "{}").expect("tamper");
        assert!(PackageManifest::load_runtime(&legacy).is_err());
    }

    #[test]
    fn patterns_cover_root_and_nested_resources_without_crossing_components() {
        assert!(glob_match("**/*.DAT", "script.dat"));
        assert!(glob_match("**/*.dat", "a/b/script.dat"));
        assert!(!glob_match("*.dat", "a/script.dat"));
    }
    #[test]
    fn required_signature_is_checked_on_the_same_file_as_size() {
        let temp = tempfile::tempdir().expect("fixture");
        fs::write(temp.path().join("x.pak"), b"PACKabcdEND").expect("fixture");
        let inventory = Inventory::scan(temp.path()).expect("scan");
        let recognition = Recognition {
            schema_version: 1,
            required: vec![FileRule {
                patterns: vec!["*.pak".into()],
                minimum_size: Some(8),
                maximum_size: None,
                signatures: vec![Signature {
                    offset: 0,
                    hex: "454e44".into(),
                    origin: "end".into(),
                }],
            }],
            hints: vec![],
        };
        assert_eq!(
            inventory.recognize(temp.path(), Some(&recognition)).0,
            RecognitionStatus::Match
        );
    }
    #[test]
    fn traversal_and_unknown_rule_versions_cannot_grant_compatibility() {
        assert!(resolve_relative(Path::new("."), "../game.exe").is_err());
        let inventory = Inventory {
            files: vec![],
            complete: true,
            signatures: Arc::default(),
        };
        assert_eq!(
            inventory
                .recognize(
                    Path::new("."),
                    Some(&Recognition {
                        schema_version: 2,
                        ..Recognition::default()
                    })
                )
                .0,
            RecognitionStatus::Unknown
        );
    }
}
