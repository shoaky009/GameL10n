//! Retain the native runtime used by a package without changing its dependency pin.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use gamel10n_translation::files::{hash_file, hash_text, read_json, write_json};
use serde_json::{Value, json};

use crate::{EngineError, EngineService, discovery::resolve_relative};

const EXECUTABLE: &str = "GameL10n.VnTextPatch.exe";
const RUNTIME_FILES: [&str; 2] = [EXECUTABLE, "capstone.dll"];
const RUNTIME_MANIFEST: &str = "runtime.json";
const NOTICES: [&str; 3] = [
    "VNTextPatch.LICENSE",
    "VNTextPatch.NOTICE.md",
    "Capstone.LICENSE.TXT",
];

struct NativePin {
    files: BTreeMap<String, String>,
}

impl NativePin {
    fn parse(value: &Value) -> Result<Self, EngineError> {
        let schema = value
            .get("schemaVersion")
            .or_else(|| value.get("schema"))
            .and_then(Value::as_u64);
        if schema == Some(1)
            || value
                .get("runtime")
                .and_then(Value::as_str)
                .is_some_and(|runtime| runtime != "native")
        {
            return Err(EngineError::Rejected(
                "该适配器固定到不支持的 DLL 工具；请改用原生工具依赖并重新发布。".into(),
            ));
        }
        if schema != Some(2)
            || value.get("runtime").and_then(Value::as_str) != Some("native")
            || value
                .get("entryPoint")
                .is_some_and(|entry| entry.as_str() != Some(EXECUTABLE))
        {
            return Err(EngineError::Protocol("原生解析器依赖记录无效。".into()));
        }
        let files = value
            .get("files")
            .and_then(Value::as_object)
            .ok_or_else(|| EngineError::Protocol("原生解析器依赖缺少文件哈希。".into()))?;
        let mut hashes = BTreeMap::new();
        for (relative, expected) in files {
            // The runtime manifest describes retained files; it is never a runtime dependency itself.
            if relative.eq_ignore_ascii_case(RUNTIME_MANIFEST) {
                return Err(EngineError::Protocol("原生依赖路径与运行记录冲突。".into()));
            }
            let expected = expected
                .as_str()
                .filter(|hash| {
                    hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit())
                })
                .ok_or_else(|| EngineError::Protocol("原生解析器依赖文件哈希无效。".into()))?;
            hashes.insert(relative.clone(), expected.to_ascii_lowercase());
        }
        if !hashes.contains_key(EXECUTABLE) {
            return Err(EngineError::Protocol("原生解析器依赖缺少入口哈希。".into()));
        }
        Ok(Self { files: hashes })
    }

    fn key(&self) -> Result<String, EngineError> {
        Ok(hash_text(&serde_json::to_string(&self.files)?))
    }
}

fn runtime_pin(files: &BTreeMap<String, String>) -> Value {
    json!({"schemaVersion":2,"runtime":"native","entryPoint":EXECUTABLE,"files":files})
}

fn file_hash(root: &Path, relative: &str) -> Result<String, EngineError> {
    let path = resolve_relative(root, relative)?;
    if !path.is_file() {
        return Err(EngineError::Rejected(format!(
            "缺少固定原生解析器文件：{}",
            path.display()
        )));
    }
    hash_file(&path).map_err(|error| EngineError::Protocol(error.to_string()))
}

fn verify_files(root: &Path, files: &BTreeMap<String, String>) -> Result<(), EngineError> {
    for (relative, expected) in files {
        if file_hash(root, relative)? != *expected {
            return Err(EngineError::Rejected(format!(
                "固定原生解析器文件校验失败：{}",
                root.join(relative).display()
            )));
        }
    }
    Ok(())
}

fn verify_retained(root: &Path, pin: &NativePin) -> Result<PathBuf, EngineError> {
    let manifest = resolve_relative(root, RUNTIME_MANIFEST)?;
    let runtime = NativePin::parse(
        &read_json(&manifest).map_err(|error| EngineError::Protocol(error.to_string()))?,
    )?;
    if RUNTIME_FILES
        .iter()
        .any(|relative| !runtime.files.contains_key(*relative))
        || pin
            .files
            .iter()
            .any(|(relative, expected)| runtime.files.get(relative) != Some(expected))
    {
        return Err(EngineError::Rejected(
            "保留的原生解析器运行记录与固定依赖不一致。".into(),
        ));
    }
    verify_files(root, &runtime.files)?;
    resolve_relative(root, EXECUTABLE)
}

impl EngineService {
    /// Validate and retain a complete native runtime for an unchanged schema-2 package pin.
    ///
    /// When the version is not already retained, `source_directory` must contain
    /// the pinned executable and its Capstone DLL.
    /// Existing retained versions are checked and reused without overwriting their files.
    pub fn retain_native_tools(
        &self,
        source_directory: &Path,
        pin: &Value,
    ) -> Result<PathBuf, EngineError> {
        let pin = NativePin::parse(pin)?;
        let root = resolve_relative(
            &self.shared_directory,
            &format!("native-vntextpatch/{}", pin.key()?),
        )?;
        if root.exists() {
            return verify_retained(&root, &pin);
        }
        let mut files = pin.files.clone();
        // Old schema-2 pins mention only the EXE. Keep its companion beside it as well;
        // the native executable independently checks Capstone's built-in content hash.
        for relative in RUNTIME_FILES {
            if !files.contains_key(relative) {
                files.insert(relative.into(), file_hash(source_directory, relative)?);
            }
        }
        verify_files(source_directory, &files)?;
        let parent = root
            .parent()
            .ok_or_else(|| EngineError::Protocol("原生工具保留目录无效。".into()))?;
        fs::create_dir_all(parent)?;
        let temporary = tempfile::Builder::new()
            .prefix(".native-vntextpatch-")
            .tempdir_in(parent)?;
        for relative in files.keys() {
            let target = resolve_relative(temporary.path(), relative)?;
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(resolve_relative(source_directory, relative)?, target)?;
        }
        for relative in NOTICES {
            let source = resolve_relative(source_directory, relative)?;
            if source.is_file() && !files.contains_key(relative) {
                fs::copy(source, temporary.path().join(relative))?;
            }
        }
        verify_files(temporary.path(), &files)?;
        write_json(
            &temporary.path().join(RUNTIME_MANIFEST),
            &runtime_pin(&files),
        )
        .map_err(|error| EngineError::Protocol(error.to_string()))?;
        if let Err(error) = fs::rename(temporary.path(), &root) {
            // A concurrent request may have published the same complete version first.
            if root.is_dir() {
                return verify_retained(&root, &pin);
            }
            return Err(error.into());
        }
        Ok(root.join(EXECUTABLE))
    }

    pub(crate) fn create_native_pin(&self) -> Result<Value, EngineError> {
        let source = self.tools.root.join("vntextpatch");
        let mut files = BTreeMap::new();
        for relative in RUNTIME_FILES {
            files.insert(relative.into(), file_hash(&source, relative)?);
        }
        let pin = runtime_pin(&files);
        self.retain_native_tools(&source, &pin)?;
        Ok(pin)
    }

    pub(crate) fn resolve_native_tools(&self, value: &Value) -> Result<PathBuf, EngineError> {
        let pin = NativePin::parse(value)?;
        let key = pin.key()?;
        let relative = format!("native-vntextpatch/{key}");
        for directory in [
            &self.shared_directory,
            &self.tools.root.join("engine-tools"),
        ] {
            let root = resolve_relative(directory, &relative)?;
            if root.exists() {
                return verify_retained(&root, &pin);
            }
        }
        let current = self.tools.root.join("vntextpatch");
        let mut matches_current = true;
        for (relative, expected) in &pin.files {
            let path = resolve_relative(&current, relative)?;
            if !path.is_file() || file_hash(&current, relative)? != *expected {
                matches_current = false;
                break;
            }
        }
        if matches_current {
            return self.retain_native_tools(&current, value);
        }
        Err(EngineError::Rejected(format!(
            "缺少固定原生解析器版本 {key}，请恢复该适配器使用的完整原生工具；当前工具不能替代固定版本。"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CancellationToken, ToolPaths};

    fn service(root: &Path) -> EngineService {
        let service = EngineService::new(root, ToolPaths::new(root.join("tools")))
            .with_shared_directory(root.join("shared"));
        fs::create_dir_all(service.tools.root.join("vntextpatch")).expect("native directory");
        service
    }

    fn native_runtime(root: &Path, executable: &[u8]) {
        fs::create_dir_all(root).expect("native directory");
        fs::write(root.join(EXECUTABLE), executable).expect("executable");
        fs::write(root.join("capstone.dll"), b"fixture decoder").expect("decoder");
        fs::write(root.join(NOTICES[0]), b"fixture license").expect("license");
    }

    fn old_pin(root: &Path) -> Value {
        // The original published schema-2 records omitted entryPoint and the companion DLL.
        json!({"schemaVersion":2,"runtime":"native","files":{EXECUTABLE:file_hash(root,EXECUTABLE).expect("hash")}})
    }

    fn package(root: &Path, pin: &Value) -> PathBuf {
        fs::create_dir_all(root.join("tools")).expect("package tools");
        write_json(&root.join("tools/vntextpatch-reference.json"), pin).expect("package pin");
        root.to_path_buf()
    }

    #[tokio::test]
    async fn pinned_runtime_survives_tool_upgrade_and_sets_every_command() {
        let temporary = tempfile::tempdir().expect("fixture");
        let service = service(temporary.path());
        let source = service.tools.root.join("vntextpatch");
        native_runtime(&source, b"old executable");
        let pin = old_pin(&source);
        let package = package(&temporary.path().join("package"), &pin);
        let original = fs::read(package.join("tools/vntextpatch-reference.json")).expect("pin");
        let retained = service
            .tool_paths(Some(&package), &CancellationToken::new())
            .await
            .expect("initial resolution");
        let old_executable = PathBuf::from(retained["vnTextPatch"].as_str().expect("path"));
        assert_ne!(old_executable, service.tools.vntextpatch());
        assert!(old_executable.with_file_name("capstone.dll").is_file());
        assert!(old_executable.with_file_name(NOTICES[0]).is_file());
        native_runtime(&source, b"new executable");
        let paths = service
            .tool_paths(Some(&package), &CancellationToken::new())
            .await
            .expect("fixed old version");
        assert_eq!(paths["vnTextPatch"], json!(old_executable));
        assert_eq!(paths["vnTextPatchCommand"], json!([old_executable]));
        assert_eq!(paths["hostCommand"], json!([old_executable]));
        assert_eq!(
            fs::read(old_executable).expect("retained"),
            b"old executable"
        );
        assert_eq!(
            fs::read(package.join("tools/vntextpatch-reference.json")).expect("unchanged pin"),
            original
        );
    }

    #[test]
    fn cached_executable_and_companion_tampering_are_rejected() {
        for relative in RUNTIME_FILES {
            let temporary = tempfile::tempdir().expect("fixture");
            let service = service(temporary.path());
            let source = service.tools.root.join("vntextpatch");
            native_runtime(&source, b"old executable");
            let pin = old_pin(&source);
            let executable = service
                .retain_native_tools(&source, &pin)
                .expect("retained");
            native_runtime(&source, b"new executable");
            fs::write(executable.with_file_name(relative), b"changed").expect("tamper");
            let error = service.resolve_native_tools(&pin).expect_err("must reject");
            assert!(
                matches!(error, EngineError::Rejected(message) if message.contains("校验失败")),
                "tampered {relative} must fail content verification"
            );
        }
    }

    #[test]
    fn missing_pinned_version_and_missing_companion_cannot_use_current_tool() {
        let temporary = tempfile::tempdir().expect("fixture");
        let service = service(temporary.path());
        let source = service.tools.root.join("vntextpatch");
        native_runtime(&source, b"old executable");
        let pin = old_pin(&source);
        native_runtime(&source, b"new executable");
        assert!(service.resolve_native_tools(&pin).is_err());
        let current_pin = old_pin(&source);
        fs::remove_file(source.join("capstone.dll")).expect("missing companion");
        assert!(service.resolve_native_tools(&current_pin).is_err());
    }

    #[test]
    fn old_and_complete_pins_keep_independent_versions_without_conflicting() {
        let temporary = tempfile::tempdir().expect("fixture");
        let service = service(temporary.path());
        let source = service.tools.root.join("vntextpatch");
        native_runtime(&source, b"old executable");
        let old_pin = old_pin(&source);
        let old = service
            .retain_native_tools(&source, &old_pin)
            .expect("legacy pin");
        let complete_pin = service.create_native_pin().expect("complete pin");
        assert!(complete_pin["files"]["capstone.dll"].is_string());
        let complete = service
            .resolve_native_tools(&complete_pin)
            .expect("complete version");
        assert_ne!(old, complete);
        native_runtime(&source, b"new executable");
        let new_pin = service.create_native_pin().expect("new version pin");
        let new = service.resolve_native_tools(&new_pin).expect("new version");
        assert_ne!(new, old);
        assert_eq!(service.resolve_native_tools(&old_pin).expect("old"), old);
        assert_eq!(
            service
                .resolve_native_tools(&complete_pin)
                .expect("complete"),
            complete
        );
    }

    #[test]
    fn bundled_retained_version_is_found_when_user_cache_is_empty() {
        let temporary = tempfile::tempdir().expect("fixture");
        let service = service(temporary.path());
        let source = service.tools.root.join("vntextpatch");
        native_runtime(&source, b"old executable");
        let pin = old_pin(&source);
        let bundled = service
            .clone()
            .with_shared_directory(service.tools.root.join("engine-tools"))
            .retain_native_tools(&source, &pin)
            .expect("bundled retained version");
        native_runtime(&source, b"new executable");
        assert_eq!(
            service.resolve_native_tools(&pin).expect("bundled"),
            bundled
        );
        assert!(!service.shared_directory.exists());
    }

    #[test]
    fn concurrent_retention_reuses_a_complete_version() {
        let temporary = tempfile::tempdir().expect("fixture");
        let service = service(temporary.path());
        let source = service.tools.root.join("vntextpatch");
        native_runtime(&source, b"old executable");
        let pin = old_pin(&source);
        std::thread::scope(|scope| {
            let requests: Vec<_> = (0..4)
                .map(|_| scope.spawn(|| service.retain_native_tools(&source, &pin)))
                .collect();
            let retained: Vec<_> = requests
                .into_iter()
                .map(|request| request.join().expect("request").expect("retained runtime"))
                .collect();
            assert!(retained.iter().all(|path| *path == retained[0]));
            assert_eq!(
                service
                    .resolve_native_tools(&pin)
                    .expect("complete runtime"),
                retained[0]
            );
        });
    }

    #[test]
    fn incomplete_legacy_or_unsafe_pins_are_rejected() {
        let temporary = tempfile::tempdir().expect("fixture");
        let service = service(temporary.path());
        let source = service.tools.root.join("vntextpatch");
        native_runtime(&source, b"old executable");
        let pin = old_pin(&source);
        for invalid in [
            json!({"schemaVersion":2,"runtime":"native","files":{}}),
            json!({"schemaVersion":1,"runtime":"dll","files":pin["files"]}),
            json!({"schemaVersion":2,"runtime":"native","entryPoint":"other.exe","files":pin["files"]}),
            json!({"schemaVersion":2,"runtime":"native","files":{EXECUTABLE:"invalid"}}),
            json!({"schemaVersion":2,"runtime":"native","files":{EXECUTABLE:pin["files"][EXECUTABLE],"../outside.dll":"0".repeat(64)}}),
        ] {
            assert!(service.resolve_native_tools(&invalid).is_err());
        }
        assert!(!service.shared_directory.exists());
    }
}
