//! Ordered, per-game upgrades. A version is committed only after its files are usable.
mod workbench_v1;
use crate::{CoreError, GameProject, workbench::page_file::Pages};
use gamel10n_translation::{
    files::{FileError, read_json, write_json},
    storage::TranslationPolicy,
};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fs, io, path::Path};

#[derive(Deserialize, Serialize)]
struct Versions {
    version: u32,
}

pub(crate) use workbench_v1::Stamp;
use workbench_v1::stamp;
const CURRENT_VERSION: u32 = 1;
const RECORD: &str = "project-migrations.json";
const FOLDER: &str = "workbench-v1";

fn recoverable(result: Result<Pages, CoreError>) -> Result<Option<Pages>, CoreError> {
    match result {
        Ok(pages) => Ok(Some(pages)),
        Err(CoreError::DamagedWorkbench(_)) => Ok(None),
        Err(CoreError::Io(error)) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn version(record: &Path) -> Result<u32, CoreError> {
    match read_json::<Versions>(record) {
        Ok(record) if record.version > CURRENT_VERSION => Err(CoreError::InvalidData(
            "项目使用了更新版本的文本存储。".into(),
        )),
        Ok(record) => Ok(record.version),
        Err(FileError::Io { source, .. }) if source.kind() == io::ErrorKind::NotFound => Ok(0),
        Err(FileError::Json { source, .. }) if !source.is_io() => Ok(0),
        Err(error) => Err(error.into()),
    }
}

#[derive(PartialEq, Eq)]
pub(crate) struct Stamps {
    files: Vec<Stamp>,
    // A fast atomic replacement may leave the directory timestamp unchanged.
    shards: BTreeMap<String, Stamp>,
}
pub(crate) fn stamps(dir: &Path, legacy: &Path) -> io::Result<Stamps> {
    let root = dir.join(FOLDER);
    let files = [
        dir.join(RECORD),
        root.join(crate::workbench::page_file::DATA),
        root.join(crate::workbench::page_file::INDEX),
        dir.join("extraction/native.json"),
        dir.join("extraction"),
        dir.join("translation-errors.json"),
        legacy.to_owned(),
    ]
    .iter()
    .map(|p| stamp(p))
    .collect::<io::Result<_>>()?;
    let mut shards = BTreeMap::new();
    workbench_v1::list(&dir.join("memory"), &mut shards, "memory", false)?;
    Ok(Stamps { files, shards })
}
pub(crate) fn open(
    directory: &Path,
    legacy: &Path,
    project: &GameProject,
    policy: &TranslationPolicy,
    repair: bool,
) -> Result<Option<Pages>, CoreError> {
    let root = directory.join(FOLDER);
    let inputs = workbench_v1::inputs(directory, legacy, project, policy)?;
    let record = directory.join(RECORD);
    if version(&record)? == CURRENT_VERSION
        && !repair
        && let Some(pages) = recoverable(Pages::open(&root, &inputs, &project.game_directory))?
    {
        return Ok(Some(pages));
    }
    if !directory.join("extraction").is_dir() {
        return Ok(None);
    }
    fs::create_dir_all(&root)?;
    let lock = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(root.join("migration.lock"))?;
    lock.lock()?;
    let previous = version(&record)?;
    // A process exit can leave an unpublished temporary file. Only the lock holder creates these.
    for entry in fs::read_dir(&root)? {
        let entry = entry?;
        if entry
            .file_name()
            .to_string_lossy()
            .starts_with(crate::workbench::page_file::TEMP_PREFIX)
            && entry.file_type()?.is_file()
        {
            fs::remove_file(entry.path())?;
        }
    }
    let pages = workbench_v1::ensure(directory, legacy, project, policy, &root, repair)?;
    if pages.is_some() && previous < CURRENT_VERSION {
        write_json(
            &record,
            &Versions {
                version: CURRENT_VERSION,
            },
        )?;
    }
    Ok(pages)
}
