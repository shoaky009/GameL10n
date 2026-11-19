//! Verify package pins against the current application-provided native runtime.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use gamel10n_translation::files::hash_file;
use serde_json::{Value, json};

use crate::{EngineError, EngineService, discovery::resolve_relative};

const EXECUTABLE: &str = "GameL10n.VnTextPatch.exe";
const RUNTIME_FILES: [&str; 2] = [EXECUTABLE, "capstone.dll"];
const RUNTIME_MANIFEST: &str = "runtime.json";
pub(crate) const PIN_PATH: &str = "tools/vntextpatch-reference.json";
pub(crate) const PIN_PURPOSE: &str = "固定原生脚本、编码与字体组件宿主。";

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
        if schema == Some(1)
            || value
                .get("runtime")
                .and_then(Value::as_str)
                .is_some_and(|runtime| runtime != "native")
        {
            return Err(EngineError::Incompatible {
                stage: "runtime",
                message: "适配器依赖的旧 DLL 工具已不再支持，请重新识别游戏".into(),
            });
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
            // A runtime manifest cannot be its own hashed dependency.
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
}

fn runtime_pin(files: &BTreeMap<String, String>) -> Value {
    json!({"schemaVersion":2,"runtime":"native","entryPoint":EXECUTABLE,"files":files})
}

fn file_hash(root: &Path, relative: &str) -> Result<String, EngineError> {
    let path = resolve_relative(root, relative)?;
    if !path.is_file() {
        return Err(EngineError::ToolMissing(path));
    }
    hash_file(&path).map_err(|error| EngineError::Protocol(error.to_string()))
}

impl EngineService {
    pub(crate) fn create_native_pin(&self) -> Result<Value, EngineError> {
        let source = self.tools.root.join("vntextpatch");
        let mut files = BTreeMap::new();
        for relative in RUNTIME_FILES {
            files.insert(relative.into(), file_hash(&source, relative)?);
        }
        Ok(runtime_pin(&files))
    }

    pub(crate) fn resolve_native_tools(&self, value: &Value) -> Result<PathBuf, EngineError> {
        let pin = NativePin::parse(value)?;
        let current = self.tools.root.join("vntextpatch");
        for relative in RUNTIME_FILES {
            let path = resolve_relative(&current, relative)?;
            if !path.is_file() {
                return Err(EngineError::ToolMissing(path));
            }
        }
        for (relative, expected) in &pin.files {
            let path = resolve_relative(&current, relative)?;
            if !path.is_file() || file_hash(&current, relative)? != *expected {
                return Err(EngineError::Incompatible {
                    stage: "runtime",
                    message:
                        "适配器固定的原生工具与当前版本不同，请重新识别游戏并使用当前工具重新验收"
                            .into(),
                });
            }
        }
        resolve_relative(&current, EXECUTABLE)
    }

    pub(super) fn updated_native_pin(&self, value: &Value) -> Result<Option<Value>, EngineError> {
        if NativePin::schema(value) == Some(1) {
            return self.create_native_pin().map(Some);
        }
        match self.resolve_native_tools(value) {
            Ok(_) => return Ok(None),
            Err(EngineError::Incompatible {
                stage: "runtime", ..
            }) => {}
            Err(error) => return Err(error),
        }
        // A draft can be reverified against a new build of the same native runtime.
        let mut files = NativePin::parse(value)?.files;
        for relative in RUNTIME_FILES {
            files.entry(relative.into()).or_default();
        }
        let current = self.tools.root.join("vntextpatch");
        for (relative, hash) in &mut files {
            *hash = file_hash(&current, relative)?;
        }
        Ok(Some(runtime_pin(&files)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CancellationToken, ToolPaths};
    use gamel10n_translation::files::{hash_text, write_json};
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
    async fn tool_upgrade_rejects_retained_old_runtime_without_rewriting_the_package() {
        let temporary = tempfile::tempdir().expect("fixture");
        let service = service(temporary.path());
        let source = service.tools.root.join("vntextpatch");
        native_runtime(&source, b"old executable");
        let pin = old_pin(&source);
        let package = package(&temporary.path().join("package"), &pin);
        let original = fs::read(package.join("tools/vntextpatch-reference.json")).expect("pin");
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
        retained_runtime(&service, &pin);
        native_runtime(&source, b"new executable");
        let error = service
            .tool_paths(Some(&package), &CancellationToken::new())
            .await
            .expect_err("obsolete runtime");
        assert!(
            matches!(error, EngineError::Incompatible { stage: "runtime", message } if message.contains("重新识别"))
        );
        assert_eq!(
            fs::read(package.join("tools/vntextpatch-reference.json")).expect("unchanged pin"),
            original
        );
        let new_pin = service.create_native_pin().expect("current pin");
        assert_eq!(
            service.resolve_native_tools(&new_pin).expect("new runtime"),
            executable
        );
    }

    #[tokio::test]
    async fn discovery_skips_obsolete_package_before_launching_its_adapter() {
        let temporary = tempfile::tempdir().expect("fixture");
        let service = service(temporary.path()).with_ordered_packages_only();
        let source = service.tools.root.join("vntextpatch");
        native_runtime(&source, b"old executable");
        let pin = old_pin(&source);
        retained_runtime(&service, &pin);
        native_runtime(&source, b"new executable");
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
    fn both_pin_shapes_remain_readable_and_complete_pins_verify_the_companion() {
        let temporary = tempfile::tempdir().expect("fixture");
        let service = service(temporary.path());
        let source = service.tools.root.join("vntextpatch");
        native_runtime(&source, b"current executable");
        let old = old_pin(&source);
        let complete = service.create_native_pin().expect("complete pin");
        assert_eq!(complete["schemaVersion"], 2);
        assert_eq!(complete["runtime"], "native");
        assert!(complete["files"]["capstone.dll"].is_string());
        for pin in [&old, &complete] {
            assert_eq!(
                service.resolve_native_tools(pin).expect("current runtime"),
                service.tools.vntextpatch()
            );
        }
        assert!(!service.shared_directory.exists());
        fs::write(source.join("capstone.dll"), b"changed companion").expect("change companion");
        assert!(matches!(
            service.resolve_native_tools(&complete),
            Err(EngineError::Incompatible {
                stage: "runtime",
                ..
            })
        ));
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
