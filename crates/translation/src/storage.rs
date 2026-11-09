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
    legacy_directory: Option<PathBuf>,
    legacy_keys: BTreeMap<String, Vec<String>>,
    cached_prefix: String,
    entries: BTreeMap<String, MemoryEntry>,
}
impl TranslationMemory {
    /// Opens read-only history, loading at most one shard at a time until a batch is committed.
    pub fn load(directory: PathBuf, legacy_directory: Option<&Path>) -> Result<Self, StorageError> {
        let marker: Option<Value> = read_recoverable(&directory.join(".legacy-import.json"))?;
        if marker.as_ref().is_some_and(|value| {
            value.get("schemaVersion") != Some(&json!(1))
                || value.get("entryCount").and_then(Value::as_u64).is_none()
        }) {
            return Err(StorageError::Invalid("旧版缓存导入记录无效".into()));
        }
        let legacy_directory = legacy_directory
            .filter(|path| path.is_dir() && marker.is_none())
            .map(Path::to_path_buf);
        let mut legacy_keys = BTreeMap::<String, Vec<String>>::new();
        if let Some(legacy) = &legacy_directory {
            for entry in fs::read_dir(legacy)? {
                let path = entry?.path();
                let Some(key) = path
                    .file_stem()
                    .and_then(|name| name.to_str())
                    .filter(|key| valid_key(key))
                else {
                    continue;
                };
                if path
                    .extension()
                    .is_some_and(|extension| extension == "json")
                {
                    legacy_keys
                        .entry(key[..2].into())
                        .or_default()
                        .push(key.into());
                }
            }
        }
        Ok(Self {
            directory,
            legacy_directory,
            legacy_keys,
            cached_prefix: String::new(),
            entries: BTreeMap::new(),
        })
    }

    fn read_shard(&self, prefix: &str) -> Result<BTreeMap<String, MemoryEntry>, StorageError> {
        let mut entries = BTreeMap::new();
        if let Some(legacy) = &self.legacy_directory
            && let Some(keys) = self.legacy_keys.get(prefix)
        {
            for key in keys {
                let mut entry: MemoryEntry = read_json(&legacy.join(format!("{key}.json")))?;
                if entry.key.as_ref() != Some(key) {
                    return Err(StorageError::Invalid("旧版缓存键不一致".into()));
                }
                entry.key = None;
                entries.insert(key.clone(), entry);
            }
        }
        if let Some(shard) =
            read_recoverable::<MemoryShard>(&self.directory.join(format!("{prefix}.json")))?
        {
            if !matches!(shard.schema_version, 1 | 2) {
                return Err(StorageError::Invalid("不支持的缓存分片版本".into()));
            }
            for (key, mut entry) in shard.entries {
                if !valid_key(&key)
                    || !key.starts_with(prefix)
                    || entry.key.as_ref().is_some_and(|stored| stored != &key)
                    || (shard.schema_version == 1 && entry.key.is_none())
                {
                    return Err(StorageError::Invalid("缓存分片键不一致".into()));
                }
                entry.key = None;
                entries.insert(key, entry);
            }
        }
        Ok(entries)
    }

    fn shard(&mut self, prefix: &str) -> Result<&BTreeMap<String, MemoryEntry>, StorageError> {
        if self.cached_prefix != prefix {
            self.entries = self.read_shard(prefix)?;
            self.cached_prefix = prefix.into();
        }
        Ok(&self.entries)
    }

    /// Drops loaded text before a long API wait; subsequent lookups reopen the required shard.
    pub fn clear_cache(&mut self) {
        self.entries.clear();
        self.cached_prefix.clear();
    }

    pub fn find(
        &mut self,
        unit: &TranslationUnit,
        policy: &TranslationPolicy,
    ) -> Result<Option<&str>, StorageError> {
        let mut key = memory_key(unit, policy);
        let emphasis = emphasis_groups(&unit.source_text);
        let primary = self.shard(&key[..2])?.contains_key(&key);
        if !primary {
            if emphasis.is_empty() {
                return Ok(None);
            }
            key = key_with_emphasis(unit, policy, false);
        }
        let Some(entry) = self.shard(&key[..2])?.get(&key) else {
            return Ok(None);
        };
        if entry.source_text != unit.source_text || entry.context != unit.context {
            return Err(StorageError::Invalid("缓存原文与上下文不一致".into()));
        }
        if !primary
            && emphasis
                .iter()
                .any(|group| entry.translation.contains(group))
        {
            return Ok(None);
        }
        Ok(Some(&entry.translation))
    }

    pub fn store_batch(&mut self, entries: Vec<(String, MemoryEntry)>) -> Result<(), StorageError> {
        let mut pending = BTreeMap::<String, BTreeMap<String, MemoryEntry>>::new();
        for (key, mut entry) in entries {
            if !valid_key(&key) {
                return Err(StorageError::Invalid("缓存键无效".into()));
            }
            entry.key = None;
            pending
                .entry(key[..2].into())
                .or_default()
                .insert(key, entry);
        }
        let importing = self.legacy_directory.is_some();
        let mut prefixes: BTreeSet<_> = pending.keys().cloned().collect();
        if importing {
            prefixes.extend(self.legacy_keys.keys().cloned());
            if self.directory.is_dir() {
                for entry in fs::read_dir(&self.directory)? {
                    let name = entry?.file_name().to_string_lossy().into_owned();
                    if (name.len() == 7 && name.ends_with(".json")
                        || name.len() == 11 && name.ends_with(".json.bak"))
                        && name.as_bytes()[..2]
                            .iter()
                            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
                    {
                        prefixes.insert(name[..2].to_owned());
                    }
                }
            }
        }
        self.clear_cache();
        let mut entry_count = 0;
        for prefix in prefixes {
            let mut values = self.read_shard(&prefix)?;
            values.extend(pending.remove(&prefix).unwrap_or_default());
            let shard = MemoryShard {
                schema_version: 2,
                entries: values,
            };
            write_json(&self.directory.join(format!("{prefix}.json")), &shard)?;
            entry_count += shard.entries.len();
        }
        if importing {
            write_json(
                &self.directory.join(".legacy-import.json"),
                &json!({"schemaVersion":1,"entryCount":entry_count}),
            )?;
            self.legacy_directory = None;
            self.legacy_keys.clear();
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

    fn memory_entry(unit: &TranslationUnit, translation: &str) -> MemoryEntry {
        MemoryEntry {
            key: None,
            source_text: unit.source_text.clone(),
            context: unit.context.clone(),
            translation: translation.into(),
            provider: "fixture".into(),
            created_at: now(),
        }
    }

    #[test]
    fn batch_commit_preserves_unloaded_records_and_recovers_backup_shards() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("memory");
        let policy = TranslationPolicy::default();
        let first = unit("hello", "scene");
        let key = memory_key(&first, &policy);
        let other_key = format!("{}{}", &key[..2], "0".repeat(62));
        let path = directory.join(format!("{}.json", &key[..2]));
        let backup = path.with_extension("json.bak");
        write_json(
            &backup,
            &MemoryShard {
                schema_version: 2,
                entries: BTreeMap::from([
                    (key.clone(), memory_entry(&first, "旧译文")),
                    (
                        other_key.clone(),
                        memory_entry(&unit("other", "scene"), "其他译文"),
                    ),
                ]),
            },
        )
        .unwrap();
        let mut memory = TranslationMemory::load(directory, None).unwrap();
        memory
            .store_batch(vec![(key.clone(), memory_entry(&first, "新译文"))])
            .unwrap();
        let saved: MemoryShard = read_json(&path).unwrap();
        assert_eq!(saved.entries[&other_key].translation, "其他译文");
        assert_eq!(memory.find(&first, &policy).unwrap(), Some("新译文"));

        // A writer must merge the latest file even if this reader has already cached the prefix.
        let mut changed = saved;
        changed.entries.get_mut(&other_key).unwrap().translation = "外部更新".into();
        write_json(&path, &changed).unwrap();
        memory
            .store_batch(vec![(key, memory_entry(&first, "再次更新"))])
            .unwrap();
        let saved: MemoryShard = read_json(&path).unwrap();
        assert_eq!(saved.entries[&other_key].translation, "外部更新");
    }

    #[test]
    fn lazy_legacy_import_keeps_modern_values_and_migrates_unread_records() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("memory");
        let legacy = root.path().join("legacy");
        let policy = TranslationPolicy::default();
        let units = [unit("hello", "scene"), unit("bye", "scene")];
        let keys: Vec<_> = units.iter().map(|unit| memory_key(unit, &policy)).collect();
        for (unit, key) in units.iter().zip(&keys) {
            let mut entry = memory_entry(unit, "旧版译文");
            entry.key = Some(key.clone());
            write_json(&legacy.join(format!("{key}.json")), &entry).unwrap();
        }
        let mut modern = TranslationMemory::load(directory.clone(), None).unwrap();
        modern
            .store_batch(vec![(keys[0].clone(), memory_entry(&units[0], "新版译文"))])
            .unwrap();
        let mut memory = TranslationMemory::load(directory.clone(), Some(&legacy)).unwrap();
        assert_eq!(memory.find(&units[0], &policy).unwrap(), Some("新版译文"));
        assert!(!directory.join(".legacy-import.json").exists());
        memory.store_batch(vec![]).unwrap();
        fs::remove_dir_all(&legacy).unwrap();
        let mut reopened = TranslationMemory::load(directory.clone(), Some(&legacy)).unwrap();
        assert_eq!(reopened.find(&units[1], &policy).unwrap(), Some("旧版译文"));
        let marker: Value = read_json(&directory.join(".legacy-import.json")).unwrap();
        assert_eq!(marker["entryCount"], 2);
    }

    #[test]
    fn lazy_lookup_preserves_emphasis_fallback_and_reports_requested_corruption() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("memory");
        let policy = TranslationPolicy::default();
        let unit = unit("[[LOUD]] hello", "scene");
        let old_key = key_with_emphasis(&unit, &policy, false);
        let new_key = memory_key(&unit, &policy);
        let mut memory = TranslationMemory::load(directory.clone(), None).unwrap();
        memory
            .store_batch(vec![(old_key.clone(), memory_entry(&unit, "大声：你好"))])
            .unwrap();
        assert_eq!(memory.find(&unit, &policy).unwrap(), Some("大声：你好"));
        memory
            .store_batch(vec![(old_key, memory_entry(&unit, "[[LOUD]]你好"))])
            .unwrap();
        assert_eq!(memory.find(&unit, &policy).unwrap(), None);
        memory
            .store_batch(vec![(new_key.clone(), memory_entry(&unit, "新规则译文"))])
            .unwrap();
        assert_eq!(memory.find(&unit, &policy).unwrap(), Some("新规则译文"));
        fs::write(
            directory.join(format!("{}.json", &new_key[..2])),
            b"invalid JSON",
        )
        .unwrap();
        let mut reopened = TranslationMemory::load(directory, None).unwrap();
        assert!(reopened.find(&unit, &policy).is_err());
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
        let mut memory = TranslationMemory::load(directory.path().join("memory"), None).unwrap();
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
