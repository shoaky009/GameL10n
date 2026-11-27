//! Resolve native dependencies and record the runtime used for adapter verification.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use gamel10n_translation::{
    files::{hash_file, hash_text, read_json},
    storage::SHA256_HEX_LEN,
};
use serde_json::{Value, json};

use crate::{
    EngineError, EngineService, PackageManifest, discovery::resolve_relative,
    protocol::STAGE_RUNTIME,
};

pub(crate) const VNTEXTPATCH_DIR: &str = "vntextpatch";
pub(crate) const EXECUTABLE: &str = "GameL10n.VnTextPatch.exe";
const RUNTIME_FILES: [&str; 2] = [EXECUTABLE, "capstone.dll"];
pub(crate) const RUNTIME_MANIFEST: &str = "runtime.json";
pub(crate) const PIN_PATH: &str = "tools/vntextpatch-reference.json";
pub(crate) const PIN_PURPOSE: &str = "记录验收时使用的原生解析器与组件宿主版本。";
const LEGACY_PIN_SCHEMA: u64 = 1;
const NATIVE_PIN_SCHEMA: u64 = 2;
const NATIVE_RUNTIME: &str = "native";

struct NativePin {
    files: BTreeMap<String, String>,
}

impl NativePin {
    fn schema(value: &Value) -> Option<u64> {
        value
            .get("schemaVersion")
            .or_else(|| value.get("schema"))
            .and_then(Value::as_u64)
    }

    fn parse(value: &Value) -> Result<Self, EngineError> {
        let schema = Self::schema(value);
        if schema == Some(LEGACY_PIN_SCHEMA)
            || value
                .get("runtime")
                .and_then(Value::as_str)
                .is_some_and(|runtime| runtime != NATIVE_RUNTIME)
        {
            return Err(EngineError::Incompatible {
                stage: STAGE_RUNTIME,
                message: "适配器依赖的旧 DLL 工具已不再支持，请重新识别游戏".into(),
            });
        }
        if schema != Some(NATIVE_PIN_SCHEMA)
            || value.get("runtime").and_then(Value::as_str) != Some(NATIVE_RUNTIME)
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
            // A runtime manifest cannot be its own hashed dependency.
            if relative.eq_ignore_ascii_case(RUNTIME_MANIFEST) {
                return Err(EngineError::Protocol("原生依赖路径与运行记录冲突。".into()));
            }
            let expected = expected
                .as_str()
                .filter(|hash| {
                    hash.len() == SHA256_HEX_LEN && hash.bytes().all(|byte| byte.is_ascii_hexdigit())
                })
                .ok_or_else(|| EngineError::Protocol("原生解析器依赖文件哈希无效。".into()))?;
            hashes.insert(relative.clone(), expected.to_ascii_lowercase());
        }
        if !hashes.contains_key(EXECUTABLE) {
            return Err(EngineError::Protocol("原生解析器依赖缺少入口哈希。".into()));
        }
        Ok(Self { files: hashes })
    }

    fn required_files(&self) -> impl Iterator<Item = &str> {
        RUNTIME_FILES.into_iter().chain(
            self.files
                .keys()
                .map(String::as_str)
                .filter(|name| !RUNTIME_FILES.contains(name)),
        )
    }
}

fn runtime_pin(files: &BTreeMap<String, String>) -> Value {
    json!({"schemaVersion":NATIVE_PIN_SCHEMA,"runtime":NATIVE_RUNTIME,"entryPoint":EXECUTABLE,"files":files})
}

fn required_file(root: &Path, relative: &str) -> Result<PathBuf, EngineError> {
    let path = resolve_relative(root, relative)?;
    if !path.is_file() {
        return Err(EngineError::ToolMissing(path));
    }
    Ok(path)
}

fn file_hash(root: &Path, relative: &str) -> Result<String, EngineError> {
    hash_file(&required_file(root, relative)?)
        .map_err(|error| EngineError::Protocol(error.to_string()))
}

impl EngineService {
    fn native_hashes<'a>(
        &self,
        files: impl IntoIterator<Item = &'a str>,
    ) -> Result<BTreeMap<String, String>, EngineError> {
        let current = self.tools.root.join(VNTEXTPATCH_DIR);
        files
            .into_iter()
            .map(|name| Ok((name.to_owned(), file_hash(&current, name)?)))
            .collect()
    }

    pub(crate) fn create_native_pin(&self) -> Result<Value, EngineError> {
        Ok(runtime_pin(&self.native_hashes(RUNTIME_FILES)?))
    }

    pub(super) fn verification_package_hash(
        &self,
        manifest: &PackageManifest,
        directory: &Path,
    ) -> Result<String, EngineError> {
        let path = directory.join(PIN_PATH);
        if !path.try_exists()? {
            return Ok(hash_text(&serde_json::to_string(manifest)?));
        }
        // Cached extraction and validation results belong to the runtime that produced them.
        let value = read_json(&path).map_err(crate::adaptation::file_error)?;
        let files = self.native_hashes(NativePin::parse(&value)?.required_files())?;
        Ok(hash_text(&serde_json::to_string(&(manifest, files))?))
    }

    pub(crate) fn resolve_native_tools(&self, value: &Value) -> Result<PathBuf, EngineError> {
        let pin = NativePin::parse(value)?;
        let current = self.tools.root.join(VNTEXTPATCH_DIR);
        // Hashes identify the build used for verification. Compatible adapters use the installed runtime.
        for relative in pin.required_files() {
            required_file(&current, relative)?;
        }
        Ok(self.tools.vntextpatch())
    }

    pub(super) fn updated_native_pin(&self, value: &Value) -> Result<Option<Value>, EngineError> {
        if NativePin::schema(value) == Some(LEGACY_PIN_SCHEMA) {
            return self.create_native_pin().map(Some);
        }
        let pin = NativePin::parse(value)?;
        let current = self.native_hashes(pin.required_files())?;
        Ok((pin.files != current).then(|| runtime_pin(&current)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CancellationToken, ToolPaths};
    use gamel10n_translation::files::write_json;
    use std::fs;

    fn service(root: &Path) -> EngineService {
        EngineService::new(root, ToolPaths::new(root.join("tools")))
            .with_shared_directory(root.join("shared"))
    }

    fn native_runtime(root: &Path, executable: &[u8]) {
        fs::create_dir_all(root).expect("native directory");
        fs::write(root.join(EXECUTABLE), executable).expect("executable");
        fs::write(root.join("capstone.dll"), b"fixture decoder").expect("decoder");
    }

    fn old_pin(root: &Path) -> Value {
        // Published schema-2 records can omit entryPoint and the companion DLL.
        json!({"schemaVersion":2,"runtime":"native","files":{EXECUTABLE:file_hash(root,EXECUTABLE).expect("hash")}})
    }

    fn package(root: &Path, pin: &Value) -> PathBuf {
        fs::create_dir_all(root.join("tools")).expect("package tools");
        write_json(&root.join("tools/vntextpatch-reference.json"), pin).expect("package pin");
        root.to_path_buf()
    }

    fn retained_runtime(service: &EngineService, pin: &Value) {
        let files = NativePin::parse(pin).expect("pin").files;
        let key = hash_text(&serde_json::to_string(&files).expect("serialized files"));
        for base in [
            &service.shared_directory,
            &service.tools.root.join("engine-tools"),
        ] {
            let root = base.join("native-vntextpatch").join(&key);
            native_runtime(&root, b"old executable");
            let files = RUNTIME_FILES
                .into_iter()
                .map(|relative| (relative.into(), file_hash(&root, relative).expect("hash")))
                .collect();
            write_json(&root.join(RUNTIME_MANIFEST), &runtime_pin(&files))
                .expect("retained manifest");
        }
    }

    #[tokio::test]
    async fn published_package_uses_the_upgraded_runtime_and_keeps_its_verification_record() {
        let temporary = tempfile::tempdir().expect("fixture");
        let service = service(temporary.path());
        let source = service.tools.root.join("vntextpatch");
        native_runtime(&source, b"old executable");
        let pin = old_pin(&source);
        let package = package(&temporary.path().join("package"), &pin);
        let original = fs::read(package.join("tools/vntextpatch-reference.json")).expect("pin");
        let manifest: PackageManifest = serde_json::from_value(
            json!({"id":"engine.fixture","version":"1","manifestVersion":4}),
        )
        .unwrap();
        let previous = service
            .verification_package_hash(&manifest, &package)
            .unwrap();
        retained_runtime(&service, &pin);
        native_runtime(&source, b"new executable");
        let paths = service
            .tool_paths(Some(&package), &CancellationToken::new())
            .await
            .expect("current runtime");
        let executable = service.tools.vntextpatch();
        let path: PathBuf =
            serde_json::from_value(paths["vnTextPatch"].clone()).expect("runtime path");
        assert_eq!(path, executable);
        for key in ["vnTextPatchCommand", "hostCommand"] {
            let [program]: [PathBuf; 1] = serde_json::from_value(paths[key].clone()).expect(key);
            assert_eq!(program, executable);
        }
        assert_ne!(
            service
                .verification_package_hash(&manifest, &package)
                .unwrap(),
            previous
        );
        assert_eq!(
            fs::read(package.join("tools/vntextpatch-reference.json")).expect("unchanged pin"),
            original
        );
    }

    #[tokio::test]
    async fn discovery_skips_unsupported_dll_packages_before_launching_the_adapter() {
        let temporary = tempfile::tempdir().expect("fixture");
        let service = service(temporary.path()).with_ordered_packages_only();
        let source = service.tools.root.join("vntextpatch");
        native_runtime(&source, b"new executable");
        let pin = json!({"schemaVersion":1,"runtime":"dll"});
        let package = package(&temporary.path().join("package"), &pin);
        fs::create_dir_all(package.join("src")).expect("source directory");
        fs::write(
            package.join("src/adapter.py"),
            b"raise AssertionError('obsolete adapter must not execute')",
        )
        .expect("entry");
        let pin_path = "tools/vntextpatch-reference.json";
        write_json(&package.join("package.json"), &json!({
            "id":"engine.obsolete", "version":"1", "manifestVersion":4,
            "protocolVersion":1, "resourcePatterns":["*.dat"],
            "packageFiles":{"src/adapter.py":file_hash(&package,"src/adapter.py").expect("entry hash"), pin_path:file_hash(&package,pin_path).expect("pin hash")}
        })).expect("manifest");
        let game = temporary.path().join("game");
        fs::create_dir_all(&game).expect("game directory");
        fs::write(game.join("story.dat"), b"fixture").expect("resource");
        let result = service
            .discover(&game, &[package], &CancellationToken::new(), None)
            .await
            .expect("skip obsolete package");
        assert!(result.adapter.is_none());
        assert!(
            result
                .diagnostics
                .iter()
                .any(|message| message.contains("重新识别"))
        );
    }

    #[test]
    fn both_pin_shapes_run_after_a_companion_update() {
        let temporary = tempfile::tempdir().expect("fixture");
        let service = service(temporary.path());
        let source = service.tools.root.join("vntextpatch");
        native_runtime(&source, b"current executable");
        let old = old_pin(&source);
        let complete = service.create_native_pin().expect("complete pin");
        assert_eq!(complete["schemaVersion"], 2);
        assert_eq!(complete["runtime"], "native");
        assert!(complete["files"]["capstone.dll"].is_string());
        fs::write(source.join("capstone.dll"), b"changed companion").expect("change companion");
        for pin in [&old, &complete] {
            assert_eq!(
                service.resolve_native_tools(pin).expect("current runtime"),
                service.tools.vntextpatch()
            );
        }
        assert!(!service.shared_directory.exists());
        assert_eq!(
            service.updated_native_pin(&complete).unwrap(),
            Some(service.create_native_pin().unwrap())
        );
    }

    #[test]
    fn missing_current_files_cannot_fall_back_to_retained_tools() {
        for relative in RUNTIME_FILES {
            let temporary = tempfile::tempdir().expect("fixture");
            let service = service(temporary.path());
            let source = service.tools.root.join("vntextpatch");
            native_runtime(&source, b"old executable");
            let pin = old_pin(&source);
            retained_runtime(&service, &pin);
            fs::remove_file(source.join(relative)).expect("remove current file");
            assert!(
                matches!(service.resolve_native_tools(&pin), Err(EngineError::ToolMissing(path)) if path == source.join(relative))
            );
        }
    }

    #[test]
    fn incomplete_legacy_or_unsafe_pins_are_rejected() {
        let temporary = tempfile::tempdir().expect("fixture");
        let service = service(temporary.path());
        let source = service.tools.root.join("vntextpatch");
        native_runtime(&source, b"current executable");
        let pin = old_pin(&source);
        let legacy = json!({"schemaVersion":1,"runtime":"dll","files":pin["files"]});
        for invalid in [
            json!({"schemaVersion":2,"runtime":"native","files":{}}),
            legacy.clone(),
            json!({"schemaVersion":2,"runtime":"native","entryPoint":"other.exe","files":pin["files"]}),
            json!({"schemaVersion":2,"runtime":"native","files":{EXECUTABLE:"invalid"}}),
            json!({"schemaVersion":2,"runtime":"native","files":{EXECUTABLE:pin["files"][EXECUTABLE],"../outside.dll":"0".repeat(64)}}),
        ] {
            assert!(service.resolve_native_tools(&invalid).is_err());
        }
        let draft_pin = service.updated_native_pin(&legacy).unwrap().unwrap();
        assert_eq!(
            service.resolve_native_tools(&draft_pin).unwrap(),
            source.join(EXECUTABLE)
        );
        assert!(!service.shared_directory.exists());
    }
}
