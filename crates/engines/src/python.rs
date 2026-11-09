use std::{ffi::OsString, io::BufWriter, path::Path, time::Duration};

use crate::{
    CancellationToken, EngineError, ToolPaths, ToolProcess,
    protocol::{AdapterRequest, AdapterResponse},
};

// Embedded CPython ignores PYTHONPATH. Bootstrap restores the established package import roots.
const BOOTSTRAP: &str = "import runpy,sys,json,io,os;from pathlib import Path;p=Path(sys.argv[1]).resolve();r=sys.stdin.read();d=json.loads(r);t=d.get('toolPaths') or {};l=t.get('engineLibrary');sys.path[:0]=[str(p.parent),str(p.parent.parent)]+([l,str(Path(l).parent/'native')] if l else []);os.environ['GAMEL10N_ADAPTER_HOST']=json.dumps(t.get('hostCommand'));os.environ['GAMEL10N_VNTEXTPATCH_COMMAND']=json.dumps(t.get('vnTextPatchCommand'));sys.dont_write_bytecode=True;sys.stdin=io.StringIO(r);sys.argv=[str(p)];runpy.run_path(str(p),run_name='__main__')";

#[derive(Clone, Debug)]
pub struct PythonAdapter {
    process: ToolProcess,
}

impl PythonAdapter {
    pub fn new(paths: &ToolPaths, entry: &Path) -> Result<Self, EngineError> {
        if !entry.is_file() {
            return Err(EngineError::ToolMissing(entry.into()));
        }
        let entry = entry.canonicalize()?;
        Ok(Self {
            process: ToolProcess::new(paths.python())
                .with_timeout(Duration::from_secs(600))
                .with_maximum_output_bytes(8 * 1024 * 1024)
                .with_arguments([
                    OsString::from("-I"),
                    OsString::from("-X"),
                    OsString::from("utf8"),
                    OsString::from("-c"),
                    OsString::from(BOOTSTRAP),
                    entry.into_os_string(),
                ]),
        })
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
        let response = self
            .process
            .clone()
            .with_response_file(response_file)
            .execute::<_, AdapterResponse>(&request, cancellation)
            .await?;
        response.checked().map_err(|error| match error {
            EngineError::Rejected(message) if request.operation == "extract" => {
                EngineError::Incompatible {
                    stage: "extraction",
                    message,
                }
            }
            error => error,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[tokio::test]
    async fn adapters_receive_translations_file_and_return_file_responses() {
        let paths = ToolPaths::development();
        if !paths.python().is_file() {
            return;
        }
        let directory = tempfile::tempdir().expect("fixture");
        let entry = directory.path().join("adapter.py");
        std::fs::write(&entry, r#"import json,sys
from pathlib import Path
r=json.load(sys.stdin)
assert 'translations' not in r
values=json.loads(Path(r['translationsFile']).read_text(encoding='utf-8'))
assert values == {'stable-id':'测试译文'}
with open(r['responseFile'],'w',encoding='utf-8') as output:
    json.dump(dict(protocolVersion=1,success=True,validation=dict(passed=True,issues=[],runtimeStatus='not-tested')),output)
"#).expect("fixture");
        let mut request = AdapterRequest::new("validate", directory.path());
        request.translations = Some(BTreeMap::from([("stable-id".into(), "测试译文".into())]));
        let response = PythonAdapter::new(&paths, &entry)
            .expect("adapter")
            .execute(&request, &CancellationToken::new())
            .await
            .expect("file protocol");
        assert!(response.validation.expect("validation").passed);
    }
}
