//! Existing game-local translation memory and proofreading file contracts.

use crate::{
    TranslationUnit,
    compatibility::legacy_json,
    files::{self, hash_text, read_json, write_json},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum StorageError {
    #[error(transparent)]
    File(#[from] files::FileError),
    #[error("文件操作失败：{0}")]
    Io(#[from] std::io::Error),
    #[error("翻译记录无效：{0}")]
    Invalid(String),
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TranslationPolicy {
    pub target_language: String,
    pub style: String,
    #[serde(default)]
    pub terminology: BTreeMap<String, String>,
    #[serde(default = "version")]
    pub version: String,
}
fn version() -> String {
    "1".into()
}
impl Default for TranslationPolicy {
    fn default() -> Self {
        Self {
            target_language: "简体中文".into(),
            style: String::new(),
            terminology: BTreeMap::new(),
            version: version(),
        }
    }
}

fn protection_signature(unit: &TranslationUnit) -> String {
    let mut tokens: Vec<_> = unit
        .protected_tokens
        .iter()
        .flatten()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    tokens.sort_by(|left, right| left.encode_utf16().cmp(right.encode_utf16()));
    if tokens.is_empty() {
        String::new()
    } else {
        format!("\nprotected:{}", legacy_json(&json!(tokens)))
    }
}

pub fn source_signature(unit: &TranslationUnit) -> String {
    hash_text(
        &(legacy_json(
            &json!({ "SourceText":unit.source_text, "Context":unit.context,
        "Speaker":unit.speaker, "Kind":unit.kind }),
        ) + &protection_signature(unit)),
    )
}

fn emphasis_groups(source: &str) -> Vec<&str> {
    let mut groups = Vec::new();
    let mut offset = 0;
    while let Some(start) = source[offset..].find("[[") {
        let start = offset + start;
        let Some(end) = source[start + 2..].find("]]") else {
            break;
        };
        let end = start + 2 + end;
        if end > start + 2
            && source[start + 2..end]
                .bytes()
                .all(|byte| byte.is_ascii_alphabetic())
        {
            groups.push(&source[start..end + 2]);
        }
        offset = end + 2;
    }
    groups
}

pub fn memory_key(unit: &TranslationUnit, policy: &TranslationPolicy) -> String {
    key_with_emphasis(unit, policy, !emphasis_groups(&unit.source_text).is_empty())
}

fn key_with_emphasis(unit: &TranslationUnit, policy: &TranslationPolicy, emphasis: bool) -> String {
    let mut terms: Vec<_> = policy
        .terminology
        .iter()
        .filter(|(term, _)| {
            unit.source_text.contains(term.as_str())
                || unit.context.contains(term.as_str())
                || unit
                    .speaker
                    .as_deref()
                    .is_some_and(|speaker| speaker.contains(term.as_str()))
        })
        .map(|(source, target)| [source, target])
        .collect();
    terms.sort_by(|left, right| left[0].encode_utf16().cmp(right[0].encode_utf16()));
    let version = format!(
        "{}{}",
        policy.version,
        if emphasis { "\nbracket-emphasis-1" } else { "" }
    );
    hash_text(
        &(legacy_json(
            &json!({ "SourceText":unit.source_text,"Context":unit.context,"Speaker":unit.speaker,
        "Kind":unit.kind,"TargetLanguage":policy.target_language,"Style":policy.style,"Version":version,"Terms":terms }),
        ) + &protection_signature(unit)),
    )
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryEntry {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    pub source_text: String,
    pub context: String,
    pub translation: String,
    pub provider: String,
    pub created_at: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct MemoryShard {
    schema_version: u32,
    entries: BTreeMap<String, MemoryEntry>,
}

pub struct TranslationMemory {
    directory: PathBuf,
    entries: BTreeMap<String, MemoryEntry>,
    legacy_imported: bool,
}
impl TranslationMemory {
    /// Read-only until a translation is committed. Legacy individual records are imported in memory.
    pub fn load(directory: PathBuf, legacy_directory: Option<&Path>) -> Result<Self, StorageError> {
        let mut result = Self {
            directory,
            entries: BTreeMap::new(),
            legacy_imported: false,
        };
        let marker: Option<Value> =
            read_recoverable(&result.directory.join(".legacy-import.json"))?;
        if marker.as_ref().is_some_and(|value| {
            value.get("schemaVersion") != Some(&json!(1))
                || value.get("entryCount").and_then(Value::as_u64).is_none()
        }) {
            return Err(StorageError::Invalid("旧版缓存导入记录无效".into()));
        }
        if let Some(legacy) = legacy_directory.filter(|path| path.is_dir() && marker.is_none()) {
            result.legacy_imported = true;
            for entry in fs::read_dir(legacy)? {
                let path = entry?.path();
                let Some(key) = path
                    .file_stem()
                    .and_then(|name| name.to_str())
                    .filter(|key| valid_key(key))
                else {
                    continue;
                };
                if path.extension().is_none_or(|extension| extension != "json") {
                    continue;
                }
                let value: MemoryEntry = read_json(&path)?;
                if value.key.as_deref() != Some(key) {
                    return Err(StorageError::Invalid("旧版缓存键不一致".into()));
                }
                result.entries.insert(key.into(), value);
            }
        }
        if result.directory.is_dir() {
            let mut prefixes = BTreeSet::new();
            for entry in fs::read_dir(&result.directory)? {
                let name = entry?.file_name().to_string_lossy().into_owned();
                if (name.len() == 7 && name.ends_with(".json")
                    || name.len() == 11 && name.ends_with(".json.bak"))
                    && name[..2]
                        .bytes()
                        .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
                {
                    prefixes.insert(name[..2].to_owned());
                }
            }
            for prefix in prefixes {
                let shard = read_recoverable::<MemoryShard>(
                    &result.directory.join(format!("{prefix}.json")),
                )?
                .ok_or_else(|| StorageError::Invalid("缓存分片丢失".into()))?;
                if !matches!(shard.schema_version, 1 | 2) {
                    return Err(StorageError::Invalid("不支持的缓存分片版本".into()));
                }
                for (key, mut entry) in shard.entries {
                    if !valid_key(&key)
                        || !key.starts_with(&prefix)
                        || entry.key.as_ref().is_some_and(|stored| stored != &key)
                        || (shard.schema_version == 1 && entry.key.is_none())
                    {
                        return Err(StorageError::Invalid("缓存分片键不一致".into()));
                    }
                    entry.key = None;
                    result.entries.insert(key, entry);
                }
            }
        }
        Ok(result)
    }
    pub fn find(
        &self,
        unit: &TranslationUnit,
        policy: &TranslationPolicy,
    ) -> Result<Option<&str>, StorageError> {
        let key = memory_key(unit, policy);
        let emphasis = emphasis_groups(&unit.source_text);
        let entry = self.entries.get(&key).or_else(|| {
            (!emphasis.is_empty())
                .then(|| self.entries.get(&key_with_emphasis(unit, policy, false)))
                .flatten()
        });
        let Some(entry) = entry else {
            return Ok(None);
        };
        if entry.source_text != unit.source_text || entry.context != unit.context {
            return Err(StorageError::Invalid("缓存原文与上下文不一致".into()));
        }
        if !self.entries.contains_key(&key)
            && emphasis
                .iter()
                .any(|group| entry.translation.contains(group))
        {
            return Ok(None);
        }
        Ok(Some(&entry.translation))
    }
    pub fn store_batch(&mut self, entries: Vec<(String, MemoryEntry)>) -> Result<(), StorageError> {
        let mut prefixes = BTreeSet::new();
        if self.legacy_imported {
            prefixes.extend(self.entries.keys().map(|key| key[..2].to_owned()));
        }
        for (key, _) in &entries {
            if !valid_key(key) {
                return Err(StorageError::Invalid("缓存键无效".into()));
            }
            prefixes.insert(key[..2].to_owned());
        }
        let pending: BTreeMap<_, _> = entries
            .into_iter()
            .map(|(key, mut entry)| {
                entry.key = None;
                (key, entry)
            })
            .collect();
        for prefix in prefixes {
            let mut values: BTreeMap<_, _> = self
                .entries
                .range(prefix.clone()..)
                .take_while(|(key, _)| key.starts_with(&prefix))
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect();
            values.extend(
                pending
                    .range(prefix.clone()..)
                    .take_while(|(key, _)| key.starts_with(&prefix))
                    .map(|(key, value)| (key.clone(), value.clone())),
            );
            let shard = MemoryShard {
                schema_version: 2,
                entries: values,
            };
            write_json(&self.directory.join(format!("{prefix}.json")), &shard)?;
            self.entries.extend(shard.entries);
        }
        if self.legacy_imported {
            write_json(
                &self.directory.join(".legacy-import.json"),
                &json!({"schemaVersion":1,"entryCount":self.entries.len()}),
            )?;
            self.legacy_imported = false;
        }
        Ok(())
    }
}

pub fn valid_key(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TranslationEdit {
    pub source_signature: String,
    pub language: String,
    pub text: String,
    pub reviewed: bool,
    pub updated_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retranslation_id: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_generated: bool,
}
impl TranslationEdit {
    pub fn new(unit: &TranslationUnit, language: &str, text: String, reviewed: bool) -> Self {
        Self {
            source_signature: source_signature(unit),
            language: language.into(),
            text,
            reviewed,
            updated_at: now(),
            retranslation_id: None,
            is_generated: false,
        }
    }
    pub fn matches(&self, unit: &TranslationUnit, language: &str) -> bool {
        self.language == language && self.source_signature == source_signature(unit)
    }
}

pub(crate) fn valid_edit(id: &str, edit: &TranslationEdit) -> bool {
    !id.trim().is_empty() && valid_key(&edit.source_signature) && !edit.language.trim().is_empty()
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TranslationEdits {
    #[serde(default, skip_serializing_if = "is_zero")]
    pub revision: u64,
    pub schema_version: u32,
    pub entries: BTreeMap<String, TranslationEdit>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub candidates: BTreeMap<String, TranslationEdit>,
    #[serde(default, flatten)]
    pub extra: BTreeMap<String, Value>,
}
impl Default for TranslationEdits {
    fn default() -> Self {
        Self {
            revision: 0,
            schema_version: 1,
            entries: BTreeMap::new(),
            candidates: BTreeMap::new(),
            extra: BTreeMap::new(),
        }
    }
}
impl TranslationEdits {
    pub fn load(directory: &Path) -> Result<Self, StorageError> {
        crate::edits::load_edits(directory).map(|(edits, _)| edits)
    }
    pub(crate) fn load_snapshot(directory: &Path) -> Result<Self, StorageError> {
        let path = directory.join("translation-edits.json");
        let value = if path.exists() {
            read_json(&path)?
        } else {
            Self::default()
        };
        if value.schema_version != 1
            || value
                .entries
                .iter()
                .chain(&value.candidates)
                .any(|(key, entry)| !valid_edit(key, entry))
        {
            return Err(StorageError::Invalid(
                "校对记录格式无效，请保留 translation-edits.json 检查".into(),
            ));
        }
        Ok(value)
    }
    pub fn find(&self, unit: &TranslationUnit, language: &str) -> Option<&TranslationEdit> {
        self.entries
            .get(&unit.id)
            .filter(|edit| edit.matches(unit, language))
    }
    pub fn candidate(&self, unit: &TranslationUnit, language: &str) -> Option<&TranslationEdit> {
        self.candidates
            .get(&unit.id)
            .filter(|edit| edit.matches(unit, language))
    }
    pub fn save(&self, directory: &Path) -> Result<(), StorageError> {
        Ok(write_json(&directory.join("translation-edits.json"), self)?)
    }
}

fn is_zero(value: &u64) -> bool {
    *value == 0
}

pub fn read_recoverable<T: serde::de::DeserializeOwned>(
    path: &Path,
) -> Result<Option<T>, StorageError> {
    let backup = PathBuf::from(format!("{}.bak", path.display()));
    if path.is_file() {
        match read_json(path) {
            Ok(value) => return Ok(Some(value)),
            Err(error) => {
                if backup.is_file() {
                    return Ok(Some(read_json(&backup)?));
                }
                return Err(error.into());
            }
        }
    }
    if backup.is_file() {
        return Ok(Some(read_json(&backup)?));
    }
    Ok(None)
}

/// RFC3339 UTC timestamps remain readable by the existing DateTimeOffset contracts.
pub fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn unit(source: &str, context: &str) -> TranslationUnit {
        TranslationUnit {
            id: "1".into(),
            resource_path: "story".into(),
            locator: "1".into(),
            source_text: source.into(),
            context: context.into(),
            speaker: None,
            kind: "dialogue".into(),
            protected_tokens: None,
        }
    }
    #[test]
    fn source_signature_matches_dotnet_fixture() {
        assert_eq!(
            source_signature(&unit("He said \"你好\" + <b>x</b> 😀", "场景")),
            "e7222a0c4862a9f7e1c277d8f976d5ac759da7275898e5814b63bb5d975bce2a"
        );
    }
    #[test]
    fn memory_key_matches_dotnet_fixture() {
        let policy = TranslationPolicy {
            style: "natural".into(),
            ..Default::default()
        };
        assert_eq!(
            memory_key(&unit("hello", "scene"), &policy),
            "e8fcbdd30f06267f147df4305bb2e15db671a429d9b977ff5fa8b1677887ffe8"
        );
    }
    #[test]
    fn reads_v1_v2_shards_and_legacy_edits_without_changing_identity() {
        let directory = tempfile::tempdir().unwrap();
        let unit = unit("hello", "scene");
        let policy = TranslationPolicy {
            style: "natural".into(),
            ..Default::default()
        };
        let key = memory_key(&unit, &policy);
        let entry = MemoryEntry {
            key: Some(key.clone()),
            source_text: unit.source_text.clone(),
            context: unit.context.clone(),
            translation: "你好".into(),
            provider: "old".into(),
            created_at: "2026-01-01T00:00:00Z".into(),
        };
        write_json(
            &directory
                .path()
                .join("memory")
                .join(format!("{}.json", &key[..2])),
            &MemoryShard {
                schema_version: 1,
                entries: BTreeMap::from([(key, entry)]),
            },
        )
        .unwrap();
        let memory = TranslationMemory::load(directory.path().join("memory"), None).unwrap();
        assert_eq!(memory.find(&unit, &policy).unwrap(), Some("你好"));
        let mut edits = TranslationEdits::default();
        edits.entries.insert(
            unit.id.clone(),
            TranslationEdit::new(&unit, &policy.target_language, "您好".into(), true),
        );
        edits.save(directory.path()).unwrap();
        assert_eq!(
            TranslationEdits::load(directory.path())
                .unwrap()
                .find(&unit, &policy.target_language)
                .unwrap()
                .text,
            "您好"
        );
    }
}
