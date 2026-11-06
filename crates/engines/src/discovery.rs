//! Bounded directory evidence and reusable adapter discovery.

use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{Read, Seek, SeekFrom},
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex},
};

use gamel10n_translation::files::{hash_file, read_json};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::EngineError;

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
}

impl AdapterHandle {
    pub fn id(&self) -> &str {
        match self {
            Self::Native { format } => format,
            Self::Python { id, .. } => id,
            Self::BuiltinSiglus => "engine.siglus",
            Self::BuiltinSfa16 => "engine.sfa16.aos-v2",
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
                    self.resource_patterns
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

#[derive(Clone, Debug, Deserialize, Serialize)]
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

#[derive(Clone, Debug, Deserialize, Serialize)]
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
        let mut unknown = !self.complete;
        for (index, file) in self
            .files
            .iter()
            .filter(|file| {
                rule.patterns
                    .iter()
                    .any(|pattern| glob_match(pattern, &file.path))
            })
            .enumerate()
        {
            if index >= 128 {
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

pub fn glob_match(pattern: &str, path: &str) -> bool {
    if pattern.len() > 1024 || path.len() > 4096 {
        return false;
    }
    let pattern: Vec<_> = pattern.replace('\\', "/").to_lowercase().chars().collect();
    let path: Vec<_> = path.replace('\\', "/").to_lowercase().chars().collect();
    let mut memo = vec![None; (pattern.len() + 1) * (path.len() + 1)];
    fn matched(p: &[char], s: &[char], i: usize, j: usize, memo: &mut [Option<bool>]) -> bool {
        let key = i * (s.len() + 1) + j;
        if let Some(value) = memo[key] {
            return value;
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
        memo[key] = Some(value);
        value
    }
    matched(&pattern, &path, 0, 0, &mut memo)
}

fn decode_hex(value: &str) -> Option<Vec<u8>> {
    if value.is_empty() || value.len() > 128 || !value.len().is_multiple_of(2) {
        return None;
    }
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            Some((char::from(pair[0]).to_digit(16)? * 16 + char::from(pair[1]).to_digit(16)?) as u8)
        })
        .collect()
}

/// Built-in format/resource selection follows the original parser catalog.
pub fn native_formats_for(inventory: &Inventory) -> Vec<&'static str> {
    const FORMATS: &[(&str, &[&str])] = &[
        ("AdvHd", &["*.ws2"]),
        ("ArtemisAsb", &["*.asb"]),
        ("ArtemisAst", &["*.ast"]),
        ("ArtemisTxt", &["*.art", "*.txt"]),
        ("Age", &["*.bin"]),
        ("CatSystem", &["*.cst"]),
        ("CSystem", &["*.a0"]),
        ("Ethornell", &["*.bgi", "<extensionless>"]),
        ("Kaguya", &["*.dat"]),
        ("KirikiriKs", &["*.ks", "*.txt"]),
        ("KirikiriSoc", &["*.soc"]),
        ("KirikiriTjs", &["*.tjs"]),
        ("Majiro", &["*.mjo"]),
        ("Musica", &["*.sc"]),
        ("Mware", &["*.nut"]),
        ("Propeller", &["*.msc"]),
        ("Qlie", &["*.s"]),
        ("RealLive", &["*.rl", "*.txt"]),
        ("Renpy", &["*.rpy"]),
        ("ShSystem", &["*.hst"]),
        ("SilkysMap", &["*.map"]),
        ("SilkysMes", &["*.mes"]),
        ("Softpal", &["*.src"]),
        ("SystemNnnDev", &["*.nnn"]),
        ("SystemNnnRelease", &["*.spt"]),
        ("TmrHiroAdvSystemCode", &["*.srp"]),
        (
            "TmrHiroAdvSystemText",
            &["<extensionless>", "*.dat", "*.txt"],
        ),
        ("Whale", &["*.txt", "<extensionless>"]),
        ("Yuris", &["*.ybn"]),
    ];
    FORMATS
        .iter()
        .filter(|(_, patterns)| {
            inventory.files.iter().any(|file| {
                patterns.iter().any(|pattern| {
                    if *pattern == "<extensionless>" {
                        Path::new(&file.path).extension().is_none()
                    } else {
                        glob_match(pattern, file.path.rsplit('/').next().unwrap_or(&file.path))
                    }
                })
            })
        })
        .map(|(format, _)| *format)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

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
