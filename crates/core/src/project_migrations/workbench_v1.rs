use crate::{
    CoreError, GameProject,
    application::load_analysis,
    workbench::{Workbench, page_file::Pages},
};
use gamel10n_translation::{files::hash_bytes, storage::TranslationPolicy};
use std::{collections::BTreeMap, fs, io, path::Path, sync::Arc, time::SystemTime};
pub(crate) type Stamp = Option<(u64, SystemTime)>;
pub(super) fn stamp(path: &Path) -> io::Result<Stamp> {
    match fs::metadata(path) {
        Ok(m) => Ok(Some((m.len(), m.modified()?))),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}
pub(super) fn list(
    path: &Path,
    files: &mut BTreeMap<String, Stamp>,
    prefix: &str,
    recursive: bool,
) -> io::Result<()> {
    let entries = match fs::read_dir(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        other => other?,
    };
    for entry in entries {
        let entry = entry?;
        let p = entry.path();
        let name = format!("{prefix}/{}", entry.file_name().to_string_lossy());
        let kind = entry.file_type()?;
        if kind.is_file()
            && (name.ends_with(".json") || name.ends_with(".bak") || name.ends_with(".sha256"))
        {
            match entry.metadata() {
                Ok(info) => {
                    files.insert(name, Some((info.len(), info.modified()?)));
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error),
            }
        } else if recursive && kind.is_dir() {
            list(&p, files, &name, false)?;
        }
    }
    Ok(())
}
pub(super) fn inputs(
    dir: &Path,
    legacy: &Path,
    project: &GameProject,
    policy: &TranslationPolicy,
) -> Result<String, CoreError> {
    let mut files = BTreeMap::new();
    let native = stamp(&dir.join("extraction/native.json"))?;
    if native.is_some() {
        files.insert("native".into(), native);
    } else {
        list(&dir.join("extraction"), &mut files, "extraction", true)?;
    }
    files.insert(
        "failures".into(),
        stamp(&dir.join("translation-errors.json"))?,
    );
    list(&dir.join("memory"), &mut files, "memory", false)?;
    if !dir.join("memory/.legacy-import.json").is_file() {
        list(legacy, &mut files, "legacy", false)?;
    }
    let bytes = serde_json::to_vec(&(files, &project.id, project.extra.get("analysis"), policy))
        .map_err(|e| CoreError::InvalidData(e.to_string()))?;
    Ok(hash_bytes(&bytes))
}
pub(super) fn ensure(
    dir: &Path,
    legacy: &Path,
    project: &GameProject,
    policy: &TranslationPolicy,
    root: &Path,
    repair: bool,
) -> Result<Option<Pages>, CoreError> {
    let mut key = inputs(dir, legacy, project, policy)?;
    if !repair
        && let Some(pages) = super::recoverable(Pages::open(root, &key, &project.game_directory))?
    {
        return Ok(Some(pages));
    }
    if let Some(pages) = super::recoverable(Pages::reindex(root, &key, &project.game_directory))? {
        return Ok(Some(pages));
    }
    // Source files can be replaced by a translation or extraction in another process.
    // Publish only pages built from a stable set of committed inputs.
    loop {
        let mut markers = BTreeMap::new();
        let Some(analysis) = load_analysis(dir, project, &mut markers)?.map(Arc::new) else {
            return Ok(None);
        };
        let state = Workbench::load(dir.into(), legacy.into(), &analysis, policy.clone())?;
        let latest = inputs(dir, legacy, project, policy)?;
        if latest == key {
            return Pages::build(
                root,
                &key,
                &project.game_directory,
                &markers,
                &analysis.extraction().units,
                &state,
            )
            .map(Some);
        }
        key = latest;
    }
}
