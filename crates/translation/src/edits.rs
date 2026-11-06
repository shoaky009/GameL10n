//! Shared proofreading state with a bounded incremental file, independent of project/UI types.
use crate::{
    files::{JsonLines, append_json_line, write_json},
    storage::{StorageError, TranslationEdit, TranslationEdits, valid_edit},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, Weak},
};

static STORES: Mutex<BTreeMap<PathBuf, Weak<EditStore>>> = Mutex::new(BTreeMap::new());

#[derive(Default, Deserialize, Serialize)]
pub struct EditChanges {
    pub entries: BTreeMap<String, Option<TranslationEdit>>,
    pub candidates: BTreeMap<String, Option<TranslationEdit>>,
}
impl EditChanges {
    fn is_empty(&self) -> bool {
        self.entries.is_empty() && self.candidates.is_empty()
    }
    fn validate(&self) -> Result<(), StorageError> {
        if self
            .entries
            .iter()
            .chain(&self.candidates)
            .any(|(id, edit)| {
                id.trim().is_empty() || edit.as_ref().is_some_and(|edit| !valid_edit(id, edit))
            })
        {
            return Err(StorageError::Invalid(
                "校对增量记录格式无效，请保留 translation-edits.jsonl 检查".into(),
            ));
        }
        Ok(())
    }
    fn apply(self, edits: &mut TranslationEdits) {
        for (target, changes) in [
            (&mut edits.entries, self.entries),
            (&mut edits.candidates, self.candidates),
        ] {
            for (id, value) in changes {
                if let Some(edit) = value {
                    target.insert(id, edit);
                } else {
                    target.remove(&id);
                }
            }
        }
    }
}
#[derive(Deserialize, Serialize)]
struct Commit {
    revision: u64,
    changes: EditChanges,
}
struct State {
    edits: TranslationEdits,
    bytes: u64,
}
pub struct EditStore {
    directory: PathBuf,
    state: Mutex<State>,
}

pub(crate) fn load_edits(directory: &Path) -> Result<(TranslationEdits, u64), StorageError> {
    let mut edits = TranslationEdits::load_snapshot(directory)?;
    let journal = directory.join("translation-edits.jsonl");
    let mut bytes = 0;
    if journal.try_exists()? {
        let mut lines = JsonLines::<Commit>::open(&journal)?;
        for line in lines.by_ref() {
            let commit = line?;
            if commit.revision <= edits.revision {
                continue;
            }
            if commit.revision != edits.revision + 1 {
                return Err(StorageError::Invalid(
                    "校对增量记录不连续，请保留文件检查".into(),
                ));
            }
            commit.changes.validate()?;
            commit.changes.apply(&mut edits);
            edits.revision = commit.revision;
        }
        bytes = lines.valid_bytes;
    }
    Ok((edits, bytes))
}

impl EditStore {
    pub fn open(directory: &Path) -> Result<Arc<Self>, StorageError> {
        let directory = fs::canonicalize(directory)?;
        let mut stores = STORES
            .lock()
            .map_err(|_| StorageError::Invalid("校对存储不可用".into()))?;
        stores.retain(|_, value| value.strong_count() > 0);
        if let Some(store) = stores.get(&directory).and_then(Weak::upgrade) {
            return Ok(store);
        }
        drop(stores);
        let loaded = load_edits(&directory);
        let mut stores = STORES
            .lock()
            .map_err(|_| StorageError::Invalid("校对存储不可用".into()))?;
        if let Some(store) = stores.get(&directory).and_then(Weak::upgrade) {
            return Ok(store);
        }
        let (edits, bytes) = loaded?;
        let store = Arc::new(Self {
            directory: directory.clone(),
            state: Mutex::new(State { edits, bytes }),
        });
        stores.insert(directory, Arc::downgrade(&store));
        Ok(store)
    }
    pub fn read<T>(&self, read: impl FnOnce(&TranslationEdits) -> T) -> Result<T, StorageError> {
        let state = self
            .state
            .lock()
            .map_err(|_| StorageError::Invalid("校对存储不可用".into()))?;
        Ok(read(&state.edits))
    }
    /// Builds a delta against the latest state; memory changes only after durable commit.
    pub fn update<T, E: From<StorageError>>(
        &self,
        update: impl FnOnce(&TranslationEdits) -> Result<(T, EditChanges), E>,
    ) -> Result<T, E> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| E::from(StorageError::Invalid("校对存储不可用".into())))?;
        let (result, changes) = update(&state.edits)?;
        if changes.is_empty() {
            return Ok(result);
        }
        changes.validate()?;
        let commit =
            Commit {
                revision: state.edits.revision.checked_add(1).ok_or_else(|| {
                    StorageError::Invalid("校对记录版本无效，请保留文件检查".into())
                })?,
                changes,
            };
        let path = self.directory.join("translation-edits.jsonl");
        state.bytes = append_json_line(&path, state.bytes, &commit).map_err(StorageError::from)?;
        commit.changes.apply(&mut state.edits);
        state.edits.revision = commit.revision;
        // Compact geometrically rather than rewriting a growing snapshot every fixed batch count.
        let snapshot_bytes = fs::metadata(self.directory.join("translation-edits.json"))
            .map(|m| m.len())
            .unwrap_or(0);
        if state.bytes >= (4 * 1024 * 1024).max(snapshot_bytes.saturating_mul(2)) {
            // The delta is already durable; a failed compaction is retried on the next commit.
            let _ = self.compact_locked(&mut state);
        }
        Ok(result)
    }
    fn compact_locked(&self, state: &mut State) -> Result<(), StorageError> {
        write_json(&self.directory.join("translation-edits.json"), &state.edits)?;
        let journal = self.directory.join("translation-edits.jsonl");
        if journal.try_exists()? {
            fs::OpenOptions::new()
                .write(true)
                .open(journal)?
                .set_len(0)?;
        }
        state.bytes = 0;
        Ok(())
    }
    pub fn compact(&self) -> Result<(), StorageError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| StorageError::Invalid("校对存储不可用".into()))?;
        if state.bytes > 0 {
            self.compact_locked(&mut state)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn change(id: &str, text: &str) -> EditChanges {
        EditChanges {
            entries: BTreeMap::from([(
                id.into(),
                Some(TranslationEdit {
                    text: text.into(),
                    source_signature: "a".repeat(64),
                    language: "zh".into(),
                    reviewed: false,
                    updated_at: "now".into(),
                    is_generated: true,
                    retranslation_id: Some("b".repeat(32)),
                }),
            )]),
            ..Default::default()
        }
    }
    fn commit(store: &EditStore, changes: EditChanges) -> Result<(), StorageError> {
        store.update(|_| Ok(((), changes)))
    }

    #[test]
    fn replay_recovers_completed_deltas_and_compaction_without_replacing_manual_changes() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("child")).unwrap();
        let store = EditStore::open(root.path()).unwrap();
        assert!(Arc::ptr_eq(
            &store,
            &EditStore::open(&root.path().join("child/..")).unwrap()
        ));
        commit(&store, change("1", "generated")).unwrap();
        let mut manual = change("1", "corrected");
        manual
            .entries
            .get_mut("1")
            .unwrap()
            .as_mut()
            .unwrap()
            .is_generated = false;
        commit(&store, manual).unwrap();
        let journal = root.path().join("translation-edits.jsonl");
        fs::OpenOptions::new()
            .append(true)
            .open(&journal)
            .unwrap()
            .write_all(b"{unfinished")
            .unwrap();
        drop(store);
        let store = EditStore::open(root.path()).unwrap();
        commit(&store, change("2", "next")).unwrap();
        // Crash after the new snapshot is committed but before the old journal is truncated.
        store
            .read(|edits| write_json(&root.path().join("translation-edits.json"), edits))
            .unwrap()
            .unwrap();
        drop(store);
        let store = EditStore::open(root.path()).unwrap();
        store
            .read(|edits| {
                assert_eq!(edits.entries["1"].text, "corrected");
                assert!(!edits.entries["1"].is_generated);
                assert_eq!(edits.entries["2"].text, "next");
            })
            .unwrap();
        store.compact().unwrap();
        assert_eq!(fs::metadata(journal).unwrap().len(), 0);
    }

    #[test]
    fn failed_append_does_not_publish_memory_and_complete_corruption_is_rejected() {
        let root = tempfile::tempdir().unwrap();
        let store = EditStore::open(root.path()).unwrap();
        let journal = root.path().join("translation-edits.jsonl");
        fs::create_dir(&journal).unwrap();
        assert!(commit(&store, change("1", "not committed")).is_err());
        assert!(store.read(|edits| edits.entries.is_empty()).unwrap());
        fs::remove_dir(&journal).unwrap();
        commit(&store, change("1", "committed")).unwrap();
        let committed_bytes = fs::metadata(&journal).unwrap().len();
        drop(store);
        fs::OpenOptions::new()
            .append(true)
            .open(&journal)
            .unwrap()
            .write_all(b"bad complete record\n")
            .unwrap();
        assert!(EditStore::open(root.path()).is_err());
        let mut invalid = change("2", "invalid signature");
        invalid
            .entries
            .get_mut("2")
            .unwrap()
            .as_mut()
            .unwrap()
            .source_signature = "invalid".into();
        append_json_line(
            &journal,
            committed_bytes,
            &Commit {
                revision: 2,
                changes: invalid,
            },
        )
        .unwrap();
        assert!(EditStore::open(root.path()).is_err());
    }

    #[test]
    fn concurrent_writers_merge_current_edits_without_blocking_other_projects() {
        use std::{sync::mpsc, thread, time::Duration};

        let root = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let store = EditStore::open(root.path()).unwrap();
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        thread::scope(|scope| {
            let writer = Arc::clone(&store);
            let first = scope.spawn(move || {
                writer.update(|_| -> Result<_, StorageError> {
                    entered_tx.send(()).unwrap();
                    release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                    Ok(((), change("1", "first")))
                })
            });
            entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            let writer = Arc::clone(&store);
            let second = scope.spawn(move || {
                writer.update(|edits| -> Result<_, StorageError> {
                    assert_eq!(edits.entries["1"].text, "first");
                    Ok(((), change("2", "second")))
                })
            });
            commit(
                &EditStore::open(other.path()).unwrap(),
                change("other", "independent"),
            )
            .unwrap();
            release_tx.send(()).unwrap();
            first.join().unwrap().unwrap();
            second.join().unwrap().unwrap();
        });
        drop(store);
        let restored = TranslationEdits::load(root.path()).unwrap();
        assert_eq!(restored.entries["1"].text, "first");
        assert_eq!(restored.entries["2"].text, "second");
    }
    use std::io::Write;
}
