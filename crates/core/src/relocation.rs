//! Data-directory transfer that retains the source and refuses differing destination files.
use crate::{Application, CoreError, ToolPaths};
use gamel10n_translation::files::{hash_file, read_json, write_json};
use serde_json::{Value, json};
use std::{
    fs::{self, FileTimes},
    io::{self, BufWriter, Write},
    path::{Path, PathBuf},
};

impl Application {
    pub async fn relocate(
        &self,
        destination: &Path,
        migrate: bool,
        tools: &ToolPaths,
    ) -> Result<Application, CoreError> {
        for project in self.list_projects("").await {
            if self
                .task_state(&project.id)
                .await
                .is_some_and(|s| s.running)
            {
                return Err(CoreError::InvalidData(
                    "请先暂停运行中的任务再切换保存位置。".into(),
                ));
            }
        }
        let source = std::path::absolute(self.data_directory())?;
        let destination = std::path::absolute(destination)?;
        crate::patching::check_path(&source)?;
        crate::patching::check_path(&destination)?;
        let normalized = |p: &Path| {
            p.to_string_lossy()
                .replace('/', "\\")
                .trim_end_matches('\\')
                .to_uppercase()
        };
        let left = normalized(&source);
        let right = normalized(&destination);
        if left == right
            || left.starts_with(&(right.clone() + "\\"))
            || right.starts_with(&(left + "\\"))
        {
            return Err(CoreError::InvalidData(
                "保存位置不能相同或互相包含。".into(),
            ));
        }
        for project in self.list_projects("").await {
            let game = normalized(&project.game_directory);
            if right == game
                || right.starts_with(&(game.clone() + "\\"))
                || game.starts_with(&(right.clone() + "\\"))
            {
                return Err(CoreError::InvalidData(
                    "保存位置不能与游戏目录重叠。".into(),
                ));
            }
        }
        if destination.exists() {
            let metadata = fs::symlink_metadata(&destination)?;
            if metadata.file_type().is_symlink() {
                return Err(CoreError::InvalidData("目标目录不能是链接。".into()));
            }
        }
        if migrate {
            self.store.ensure_writable().await?;
        }
        // Own the target's writer lease before copying; the returned host retains it.
        let next = Application::open(destination.clone(), tools).await?;
        next.store.ensure_writable().await?;
        if migrate {
            let old = source.clone();
            let new = destination.clone();
            tokio::task::spawn_blocking(move || transfer(&old, &new)).await??;
            *next.session_keys.lock().await = self.session_keys.lock().await.clone();
        }
        // Validate copied project metadata before committing the application's location pointer.
        next.store.reload().await?;
        let local = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        if std::env::var_os("GAMEL10N_DATA")
            .or_else(|| std::env::var_os("AVG_TRANSLATOR_DATA"))
            .is_none()
        {
            write_json(
                &local.join("GameL10n/location.json"),
                &json!({"dataDirectory":destination}),
            )?;
        }
        Ok(next)
    }
}

fn transfer(source: &Path, target: &Path) -> Result<(), CoreError> {
    crate::patching::check_path(source)?;
    crate::patching::check_path(target)?;
    let mut files = Vec::new();
    collect(source, source, &mut files)?;
    for relative in &files {
        let destination = crate::patching::safe_path(target, relative)?;
        let input = source.join(relative);
        let identical = if !destination.exists() {
            true
        } else if let Some(expected) = relocated_metadata(&input, relative, source, target)? {
            read_json::<Value>(&destination)? == expected
        } else {
            hash_file(&input)? == hash_file(&destination)?
        };
        if !identical {
            return Err(CoreError::InvalidData(format!(
                "目标目录已有不同内容：{relative}。请选择空目录或已有相同副本。"
            )));
        }
    }
    for relative in files {
        let input = source.join(&relative);
        let output = crate::patching::safe_path(target, &relative)?;
        if output.exists() {
            continue;
        }
        if let Some(parent) = output.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut temporary = tempfile::NamedTempFile::new_in(
            output
                .parent()
                .ok_or_else(|| CoreError::InvalidData("迁移目标缺少父目录。".into()))?,
        )?;
        if let Some(value) = relocated_metadata(&input, &relative, source, target)? {
            let mut writer = BufWriter::new(temporary.as_file_mut());
            serde_json::to_writer_pretty(&mut writer, &value)
                .map_err(|error| CoreError::InvalidData(error.to_string()))?;
            writer.flush()?;
        } else {
            io::copy(&mut fs::File::open(&input)?, temporary.as_file_mut())?;
            if hash_file(&input)? != hash_file(temporary.path())? {
                return Err(CoreError::InvalidData(format!(
                    "迁移文件校验失败：{relative}"
                )));
            }
        }
        temporary.as_file().sync_all()?;
        let metadata = fs::metadata(&input)?;
        let mut times = FileTimes::new();
        if let Ok(time) = metadata.modified() {
            times = times.set_modified(time);
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::FileTimesExt;
            if let Ok(time) = metadata.created() {
                times = times.set_created(time);
            }
        }
        temporary.as_file().set_times(times)?;
        fs::set_permissions(temporary.path(), metadata.permissions())?;
        temporary
            .persist_noclobber(output)
            .map_err(|error| CoreError::Io(error.error))?;
    }
    Ok(())
}
fn collect(root: &Path, path: &Path, files: &mut Vec<String>) -> Result<(), CoreError> {
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let p = entry.path();
        let metadata = fs::symlink_metadata(&p)?;
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if metadata.file_attributes() & 0x400 != 0 {
                return Err(CoreError::InvalidData(
                    "数据目录包含重解析点，不能自动迁移。".into(),
                ));
            }
        }
        if metadata.file_type().is_symlink() {
            return Err(CoreError::InvalidData(
                "数据目录包含链接，不能自动迁移。".into(),
            ));
        }
        if metadata.is_dir() {
            collect(root, &p, files)?;
        } else if metadata.is_file()
            && entry.file_name() != ".app.lock"
            && entry.file_name() != ".task.lock"
        {
            files.push(
                p.strip_prefix(root)
                    .map_err(|e| CoreError::InvalidData(e.to_string()))?
                    .to_string_lossy()
                    .into_owned(),
            );
        }
    }
    Ok(())
}
fn relocated_metadata(
    input: &Path,
    relative: &str,
    source: &Path,
    target: &Path,
) -> Result<Option<Value>, CoreError> {
    let relative = relative.replace('\\', "/");
    let native = relative.ends_with("/extraction/native.json");
    let patch = relative.ends_with("/latest-result.json")
        || relative.ends_with(".result.json")
        || relative.ends_with("/runtime-test.json");
    if !native && !patch {
        return Ok(None);
    }
    let mut value: Value = read_json(input)?;
    if native {
        remap_field(&mut value, "sourceDirectory", source, target);
        if let Some(adapter) = value.get_mut("adapter") {
            for field in ["entry", "packageDirectory", "package_directory"] {
                remap_field(adapter, field, source, target);
            }
        }
    }
    if patch {
        remap_field(&mut value, "patchDirectory", source, target);
    }
    Ok(Some(value))
}

fn remap_field(value: &mut Value, key: &str, source: &Path, target: &Path) {
    if let Some(Value::String(text)) = value.get_mut(key)
        && let Ok(relative) = Path::new(text).strip_prefix(source)
    {
        *text = target.join(relative).to_string_lossy().into_owned();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transfer_can_resume_remapped_metadata_and_preserves_text_and_original_files() {
        let fixture = tempfile::tempdir().unwrap();
        let source = fixture.path().join("source");
        let target = fixture.path().join("target");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&target).unwrap();
        let native = source.join("games/id/extraction/native.json");
        let literal = source
            .join("literal dialogue")
            .to_string_lossy()
            .into_owned();
        write_json(&native, &json!({"sourceDirectory":source.join("games/id/sources/hash"),"adapter":{"runtime":"python","entry":source.join("extensions/packages/engine.fixture/v1/src/adapter.py"),"package_directory":source.join("extensions/packages/engine.fixture/v1")},"extraction":{"units":[{"sourceText":literal,"context":literal}]}})).unwrap();
        let before = fs::read(&native).unwrap();
        let resource = source.join("games/id/sources/hash/data.bin");
        fs::create_dir_all(resource.parent().unwrap()).unwrap();
        fs::write(&resource, b"original").unwrap();
        transfer(&source, &target).unwrap();
        transfer(&source, &target).unwrap();
        let migrated: Value = read_json(&target.join("games/id/extraction/native.json")).unwrap();
        assert_eq!(
            migrated["sourceDirectory"],
            json!(target.join("games/id/sources/hash"))
        );
        assert_eq!(
            migrated["adapter"]["package_directory"],
            json!(target.join("extensions/packages/engine.fixture/v1"))
        );
        assert_eq!(migrated["extraction"]["units"][0]["sourceText"], literal);
        assert_eq!(fs::read(&native).unwrap(), before);
        assert_eq!(
            fs::read(target.join("games/id/sources/hash/data.bin")).unwrap(),
            b"original"
        );
    }

    #[test]
    fn transfer_rejects_differing_destination_before_copying_any_payload() {
        let fixture = tempfile::tempdir().unwrap();
        let source = fixture.path().join("source");
        let target = fixture.path().join("target");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&target).unwrap();
        fs::write(source.join("first"), b"source").unwrap();
        fs::write(source.join("conflict"), b"source").unwrap();
        fs::write(target.join("conflict"), b"keep").unwrap();
        assert!(transfer(&source, &target).is_err());
        assert!(!target.join("first").exists());
        assert_eq!(fs::read(target.join("conflict")).unwrap(), b"keep");
    }

    #[tokio::test]
    async fn relocate_rejects_game_directory_before_creating_a_destination() {
        let fixture = tempfile::tempdir().unwrap();
        let data = fixture.path().join("data");
        let game = fixture.path().join("game");
        fs::create_dir_all(&game).unwrap();
        let tools = ToolPaths::new(fixture.path().join("tools"));
        let app = Application::open(data, &tools).await.unwrap();
        app.add_project(&game).await.unwrap();
        let destination = game.join("data");
        assert!(app.relocate(&destination, true, &tools).await.is_err());
        assert!(!destination.exists());
    }
}
