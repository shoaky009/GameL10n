//! Read-only resource inputs with temporary layout copies only when paths require them.
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};

use crate::{CancellationToken, EngineError, discovery::resolve_relative};

/// Holds input files open for an operation without retaining another resource tree.
pub struct ReadOnlySource {
    directory: PathBuf,
    _temporary: Option<tempfile::TempDir>,
    _files: Vec<File>,
}

impl ReadOnlySource {
    pub fn open(
        directory: &Path,
        files: &BTreeMap<String, PathBuf>,
        cancellation: &CancellationToken,
    ) -> Result<Self, EngineError> {
        Self::open_excluding(directory, files, &BTreeSet::new(), cancellation)
    }

    /// A logical original-resource view must omit installed patch additions and backups.
    pub fn open_excluding(
        directory: &Path,
        files: &BTreeMap<String, PathBuf>,
        excluded: &BTreeSet<String>,
        cancellation: &CancellationToken,
    ) -> Result<Self, EngineError> {
        let mut directory = std::path::absolute(directory)?;
        let mut handles = Vec::with_capacity(files.len());
        let mut direct = true;
        for (relative, path) in files {
            if cancellation.is_cancelled() {
                return Err(EngineError::Cancelled);
            }
            handles.push(open_read_only(path)?);
            direct &= resolve_relative(&directory, relative)?
                .canonicalize()
                .is_ok_and(|expected| path.canonicalize().is_ok_and(|actual| expected == actual));
        }
        if !direct && let Some((relative, path)) = files.first_key_value() {
            let mut root = std::path::absolute(path)?;
            for _ in Path::new(relative).components() {
                root.pop();
            }
            if files.iter().all(|(relative, actual)| {
                resolve_relative(&root, relative)
                    .and_then(|path| Ok(path.canonicalize()?))
                    .is_ok_and(|path| actual.canonicalize().is_ok_and(|actual| path == actual))
            }) {
                directory = root;
                direct = true;
            }
        }
        if direct {
            for relative in excluded {
                if resolve_relative(&directory, relative)?.exists() {
                    direct = false;
                    break;
                }
            }
        }
        if direct {
            return Ok(Self {
                directory,
                _temporary: None,
                _files: handles,
            });
        }

        // Renamed backups and inputs from different roots need the adapter's logical layout.
        // The temporary directory is removed on success, failure, and cancellation.
        let temporary = tempfile::tempdir()?;
        let mut buffer = [0_u8; 128 * 1024];
        for ((relative, _), input) in files.iter().zip(&mut handles) {
            let output = resolve_relative(temporary.path(), relative)?;
            if let Some(parent) = output.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut output = File::create(output)?;
            loop {
                if cancellation.is_cancelled() {
                    return Err(EngineError::Cancelled);
                }
                let count = input.read(&mut buffer)?;
                if count == 0 {
                    break;
                }
                output.write_all(&buffer[..count])?;
            }
        }
        Ok(Self {
            directory: temporary.path().to_path_buf(),
            _temporary: Some(temporary),
            _files: handles,
        })
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }
}

fn open_read_only(path: &Path) -> Result<File, EngineError> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(1); // FILE_SHARE_READ: writes and replacement stay blocked while in use.
    }
    Ok(options.open(path)?)
}

pub(crate) fn lock_inputs(
    files: &BTreeMap<String, PathBuf>,
    cancellation: &CancellationToken,
) -> Result<Vec<File>, EngineError> {
    files
        .values()
        .map(|path| {
            if cancellation.is_cancelled() {
                return Err(EngineError::Cancelled);
            }
            open_read_only(path)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direct_and_legacy_layouts_borrow_inputs_without_changing_permissions() {
        let root = tempfile::tempdir().unwrap();
        let game = root.path().join("game");
        let legacy = root.path().join("sources/legacy");
        for directory in [&game, &legacy] {
            fs::create_dir_all(directory.join("scripts")).unwrap();
            let path = directory.join("scripts/scene.bin");
            fs::write(&path, b"original").unwrap();
            let files = BTreeMap::from([("scripts/scene.bin".into(), path.clone())]);
            {
                let source =
                    ReadOnlySource::open(&game, &files, &CancellationToken::new()).unwrap();
                assert_eq!(source.directory(), directory);
                assert!(source._temporary.is_none());
                assert!(!path.metadata().unwrap().permissions().readonly());
                #[cfg(windows)]
                {
                    assert!(fs::write(&path, b"modified").is_err());
                    assert!(fs::remove_file(&path).is_err());
                }
            }
            assert_eq!(fs::read(&path).unwrap(), b"original");
            fs::write(&path, b"writable after operation").unwrap();
        }
    }

    #[test]
    fn renamed_backup_layout_is_temporary_and_originals_remain_untouched() {
        let game = tempfile::tempdir().unwrap();
        let backup = game.path().join("scene.bin.bak");
        fs::write(&backup, b"original").unwrap();
        fs::write(game.path().join("scene.bin"), b"installed patch").unwrap();
        let files = BTreeMap::from([("scene.bin".into(), backup.clone())]);
        let temporary;
        {
            let input =
                ReadOnlySource::open(game.path(), &files, &CancellationToken::new()).unwrap();
            temporary = input.directory().to_path_buf();
            assert_ne!(temporary, game.path());
            assert_eq!(fs::read(temporary.join("scene.bin")).unwrap(), b"original");
        }
        assert!(!temporary.exists());
        assert_eq!(fs::read(&backup).unwrap(), b"original");
        assert_eq!(
            fs::read(game.path().join("scene.bin")).unwrap(),
            b"installed patch"
        );
    }

    #[test]
    fn cancellation_and_failed_setup_release_input_handles() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("a.bin");
        fs::write(&path, b"original").unwrap();
        let mut files = BTreeMap::from([("a.bin".into(), path.clone())]);
        files.insert("missing.bin".into(), root.path().join("missing.bin"));
        assert!(ReadOnlySource::open(root.path(), &files, &CancellationToken::new()).is_err());
        let token = CancellationToken::new();
        token.cancel();
        assert!(matches!(
            ReadOnlySource::open(root.path(), &files, &token),
            Err(EngineError::Cancelled)
        ));
        fs::write(&path, b"writable").unwrap();
    }
}
