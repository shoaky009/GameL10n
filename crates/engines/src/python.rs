use std::{ffi::OsString, path::Path};

use crate::{
    CancellationToken, EngineError, ToolPaths, ToolProcess,
    protocol::{AdapterRequest, AdapterResponse},
};

// Embedded CPython ignores PYTHONPATH. Bootstrap restores the established package import roots.
const BOOTSTRAP: &str = "import runpy,sys;from pathlib import Path;p=Path(sys.argv[1]).resolve();sys.path[:0]=[str(p.parent),str(p.parent.parent)];sys.argv=[str(p)];runpy.run_path(str(p),run_name='__main__')";

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
            process: ToolProcess::new(paths.python()).with_arguments([
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
        self.process
            .execute::<_, AdapterResponse>(request, cancellation)
            .await?
            .checked()
    }
}
