use std::{
    ffi::OsString,
    io::BufWriter,
    path::{Path, PathBuf},
    time::Duration,
};

use crate::{
    CancellationToken, EngineError, ToolPaths, ToolProcess,
    protocol::{AdapterRequest, AdapterResponse},
};

// Embedded CPython ignores PYTHONPATH. Bootstrap restores the established package import roots.
const BOOTSTRAP: &str = "import runpy,sys,json,io,os;from pathlib import Path;configure_tempfiles('--isolated' in sys.argv[2:]);p=Path(sys.argv[1]).resolve();r=sys.stdin.read();d=json.loads(r);t=d.get('toolPaths') or {};l=t.get('engineLibrary');sys.path[:0]=[str(p.parent),str(p.parent.parent)]+([l,str(Path(l).parent/'native')] if l else []);os.environ['GAMEL10N_ADAPTER_HOST']=json.dumps(t.get('hostCommand'));os.environ['GAMEL10N_VNTEXTPATCH_COMMAND']=json.dumps(t.get('vnTextPatchCommand'));sys.dont_write_bytecode=True;protect_inputs(d.get('sourceDirectory'));sys.stdin=io.TextIOWrapper(io.BytesIO(r.encode('utf-8')),encoding='utf-8');sys.argv=[str(p)];runpy.run_path(str(p),run_name='__main__')";

#[derive(Clone, Debug)]
pub struct PythonAdapter {
    process: ToolProcess,
    paths: ToolPaths,
    entry: PathBuf,
    isolate_execution: bool,
}

impl PythonAdapter {
    pub fn new(paths: &ToolPaths, entry: &Path) -> Result<Self, EngineError> {
        if !entry.is_file() {
            return Err(EngineError::ToolMissing(entry.into()));
        }
        let entry = entry.canonicalize()?;
        Ok(Self {
            paths: paths.clone(),
            entry: entry.clone(),
            isolate_execution: cfg!(windows),
            process: ToolProcess::new(paths.python())
                .with_timeout(Duration::from_secs(600))
                .with_maximum_output_bytes(8 * 1024 * 1024)
                .with_arguments([
                    OsString::from("-I"),
                    OsString::from("-X"),
                    OsString::from("utf8"),
                    OsString::from("-c"),
                    OsString::from(format!(
                        "{}\n{}\n{BOOTSTRAP}",
                        include_str!("../python/isolation.py"),
                        include_str!("../python/readonly_inputs.py")
                    )),
                    entry.into_os_string(),
                ]),
        })
    }

    pub fn with_execution_isolation(mut self, enabled: bool) -> Self {
        self.isolate_execution = cfg!(windows) && enabled;
        self
    }

    pub async fn execute(
        &self,
        request: &AdapterRequest,
        cancellation: &CancellationToken,
    ) -> Result<AdapterResponse, EngineError> {
        let directory = tempfile::tempdir()?;
        let response_file = directory.path().join("response.json");
        let mut request = request.clone();
        request
            .extra
            .insert("responseFile".into(), serde_json::json!(response_file));
        if let Some(translations) = request.translations.take() {
            let path = directory.path().join("translations.json");
            let file = std::fs::File::create(&path)?;
            let mut writer = BufWriter::new(file);
            serde_json::to_writer(&mut writer, &translations)?;
            std::io::Write::flush(&mut writer)?;
            request
                .extra
                .insert("translationsFile".into(), serde_json::json!(path));
        }
        let mut process = self.process.clone();
        if self.isolate_execution {
            process = process.with_arguments([OsString::from("--isolated")]);
            let mut read_paths = vec![self.paths.root.clone()];
            if let Some(parent) = self.entry.parent() {
                read_paths.push(parent.into());
            }
            for key in ["adapterDirectory", "validationContextFile"] {
                if let Some(path) = request.extra.get(key).and_then(serde_json::Value::as_str) {
                    read_paths.push(path.into());
                }
            }
            if let Some(ranges) = request
                .extra
                .get("probeRanges")
                .and_then(serde_json::Value::as_array)
            {
                read_paths.extend(
                    ranges
                        .iter()
                        .filter_map(|range| range["dataPath"].as_str())
                        .map(PathBuf::from),
                );
            }
            if let Some(tools) = request.extra.get("toolPaths") {
                for key in ["engineLibrary", "hostCommand", "vnTextPatchCommand"] {
                    let paths = if let Some(arguments) = tools[key].as_array() {
                        arguments
                            .iter()
                            .filter_map(serde_json::Value::as_str)
                            .collect::<Vec<_>>()
                    } else {
                        tools[key].as_str().into_iter().collect()
                    };
                    for path in paths {
                        let path = Path::new(path);
                        if path.is_absolute() && path.exists() {
                            read_paths.push(if path.is_file() {
                                path.parent().unwrap_or(path).into()
                            } else {
                                path.into()
                            });
                        }
                    }
                }
            }
            let mut write_paths = vec![directory.path().to_path_buf()];
            if let Some(workspace) = self
                .entry
                .ancestors()
                .find(|path| path.ends_with("adaptation/current"))
            {
                write_paths.push(workspace.into());
            }
            if let Some(output) = &request.output_directory {
                std::fs::create_dir_all(output)?;
                write_paths.push(output.clone());
            }
            process = process
                .with_current_directory(directory.path())
                .with_isolation(self.paths.isolated_runner(), read_paths, write_paths)
                .with_isolated_inputs(vec![request.source_directory.clone()]);
        }
        let response = process
            .with_response_file(response_file)
            .execute::<_, AdapterResponse>(&request, cancellation)
            .await?;
        response.checked_for_operation(&request.operation)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[cfg(windows)]
    #[tokio::test]
    async fn input_handles_support_python_children_and_native_extract_build_validate() {
        let paths = ToolPaths::development();
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("ゲーム");
        std::fs::create_dir(&source).unwrap();
        let original = "[cm]\nこれは元の文章です。\n[p]\n";
        std::fs::write(source.join("script.ks"), format!("\u{feff}{original}")).unwrap();
        std::fs::write(source.join("data.bin"), b"untouched").unwrap();
        let outside = directory.path().join("private.txt");
        std::fs::write(&outside, b"private").unwrap();
        let workspace = directory.path().join("adaptation/current");
        std::fs::create_dir_all(&workspace).unwrap();
        let entry = workspace.join("adapter.py");
        std::fs::write(
            &entry,
            include_str!("../tests/fixtures/check_isolated_inputs.py"),
        )
        .unwrap();
        let mut request = AdapterRequest::new("validate", source.clone());
        request.output_directory = Some(workspace.join("output"));
        request.extra.insert(
            "canonicalSourceDirectory".into(),
            serde_json::json!(source.canonicalize().unwrap()),
        );
        request
            .extra
            .insert("outside".into(), serde_json::json!(outside));
        request.extra.insert(
            "inputBootstrap".into(),
            serde_json::json!(include_str!("../python/isolation.py")),
        );
        request.extra.insert(
            "toolPaths".into(),
            serde_json::json!({"vnTextPatchCommand":[paths.vntextpatch()]}),
        );
        let result = PythonAdapter::new(&paths, &entry)
            .unwrap()
            .execute(&request, &CancellationToken::new())
            .await
            .unwrap();
        assert!(result.validation.unwrap().passed);
        assert_eq!(
            std::fs::read_to_string(source.join("script.ks")).unwrap(),
            format!("\u{feff}{original}")
        );
        assert_eq!(std::fs::read_dir(&source).unwrap().count(), 2);
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn renpy_adapters_extract_build_and_validate_under_isolation() {
        let paths = ToolPaths::development();
        let directory = tempfile::tempdir().unwrap();
        let workspace = directory.path().join("adaptation/current");
        let package = workspace.join("tools");
        std::fs::create_dir_all(&package).unwrap();
        for (name, source) in [
            (
                "gamel10n_host.py",
                include_str!("../../../adapters/components/gamel10n_host.py"),
            ),
            (
                "engine_archive.py",
                include_str!("../../../adapters/components/engine_archive.py"),
            ),
            (
                "renpy_rpyc_adapter.py",
                include_str!("../../../adapters/components/renpy_rpyc_adapter.py"),
            ),
            (
                "renpy_mixed_adapter.py",
                include_str!("../../../adapters/components/renpy_mixed_adapter.py"),
            ),
        ] {
            std::fs::write(package.join(name), source).unwrap();
        }
        std::fs::write(
            package.join("adapter-template.json"),
            r#"{"schemaVersion":1,"templateVersion":1,"format":"Renpy"}"#,
        )
        .unwrap();
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        let entry = workspace.join("adapter.py");
        std::fs::write(&entry, r#"import contextlib,io,json,runpy,sys
from pathlib import Path
r=json.load(sys.stdin)
fixtures=Path(r['toolPaths']['engineLibrary']).parent
sys.path.insert(0,str(fixtures))
sys.argv=[str(fixtures/'check_renpy_rpyc_adapter.py'),r['adapterDirectory']]
output=io.StringIO()
with contextlib.redirect_stdout(output):
    runpy.run_path(sys.argv[0],run_name='__main__')
assert output.getvalue().strip()=='rpyc-adapter-ok'
Path(r['responseFile']).write_text(json.dumps(dict(protocolVersion=1,success=True,validation=dict(passed=True,issues=[],runtimeStatus='not-tested'))),encoding='utf-8')
"#).unwrap();
        let source = directory.path().join("game");
        std::fs::create_dir(&source).unwrap();
        let mut request = AdapterRequest::new("validate", source);
        request
            .extra
            .insert("adapterDirectory".into(), serde_json::json!(workspace));
        request.extra.insert(
            "toolPaths".into(),
            serde_json::json!({
                "engineLibrary":fixtures.join("renpy-rpyc-reference-e9b8018.zip"),
            }),
        );
        let result = PythonAdapter::new(&paths, &entry)
            .unwrap()
            .execute(&request, &CancellationToken::new())
            .await
            .unwrap();
        assert!(result.validation.unwrap().passed);
    }

    #[tokio::test]
    async fn resource_failures_allow_fallback_but_explicit_infrastructure_errors_stop_it() {
        let paths = ToolPaths::development();
        if !paths.python().is_file() {
            return;
        }
        let directory = tempfile::tempdir().expect("fixture");
        let entry = directory.path().join("adapter.py");
        for (operation, kind, recoverable) in [
            ("extract", "incompatible", true),
            ("extract", "", true),
            ("extract", "io", false),
            ("extract", "dependency", false),
            ("extract", "unknown-kind", false),
            ("build", "incompatible", true),
            ("build", "", true),
            ("build", "io", false),
            ("validate", "", true),
            ("probe", "", false),
        ] {
            std::fs::write(
                &entry,
                format!(
                    r#"import json,sys
r=json.load(sys.stdin)
print(json.dumps(dict(protocolVersion=1,success=False,error='resource error',errorKind='{kind}')))
"#
                ),
            )
            .expect("fixture");
            let result = PythonAdapter::new(&paths, &entry)
                .expect("adapter")
                .execute(
                    &AdapterRequest::new(operation, directory.path()),
                    &CancellationToken::new(),
                )
                .await
                .expect_err("failed operation");
            assert_eq!(
                matches!(result, EngineError::Incompatible { .. }),
                recoverable
            );
            if !recoverable {
                assert!(matches!(result, EngineError::Rejected(_)));
            }
        }
    }

    #[tokio::test]
    async fn adapters_receive_translations_file_and_return_file_responses() {
        let paths = ToolPaths::development();
        if !paths.python().is_file() {
            return;
        }
        let directory = tempfile::tempdir().expect("fixture");
        let entry = directory.path().join("adapter.py");
        for stream in ["sys.stdin", "sys.stdin.buffer"] {
            std::fs::write(&entry, r#"import json,sys
from pathlib import Path
r=json.load(REQUEST_STREAM)
assert r['fixture'] == '中文😀'
assert 'translations' not in r
values=json.loads(Path(r['translationsFile']).read_text(encoding='utf-8'))
assert values == {'stable-id':'测试译文'}
with open(r['responseFile'],'w',encoding='utf-8') as output:
    json.dump(dict(protocolVersion=1,success=True,validation=dict(passed=True,issues=[],runtimeStatus='not-tested')),output)
"#.replace("REQUEST_STREAM",stream)).expect("fixture");
            let mut request = AdapterRequest::new("validate", directory.path());
            request
                .extra
                .insert("fixture".into(), serde_json::json!("中文😀"));
            request.translations =
                Some(BTreeMap::from([("stable-id".into(), "测试译文".into())]).into());
            let response = PythonAdapter::new(&paths, &entry)
                .expect("adapter")
                .execute(&request, &CancellationToken::new())
                .await
                .expect("file protocol");
            assert!(response.validation.expect("validation").passed);
        }
    }
}
