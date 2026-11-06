use std::path::Path;

use crate::{
    CancellationToken, EngineError, ToolPaths, ToolProcess,
    protocol::{AdapterRequest, AdapterResponse},
};

/// Keys match the existing native parser catalog; they are not game identities.
pub const FORMATS: &[&str] = &[
    "AdvHd",
    "ArtemisAsb",
    "ArtemisAst",
    "ArtemisTxt",
    "Age",
    "CatSystem",
    "CSystem",
    "Ethornell",
    "Kaguya",
    "KirikiriKs",
    "KirikiriSoc",
    "KirikiriTjs",
    "Majiro",
    "Musica",
    "Mware",
    "Propeller",
    "Qlie",
    "RealLive",
    "Renpy",
    "ShSystem",
    "SilkysMap",
    "SilkysMes",
    "Softpal",
    "SystemNnnDev",
    "SystemNnnRelease",
    "TmrHiroAdvSystemCode",
    "TmrHiroAdvSystemText",
    "Whale",
    "Yuris",
];

#[derive(Clone, Debug)]
pub struct VnTextPatch {
    process: ToolProcess,
}

impl VnTextPatch {
    pub fn new(paths: &ToolPaths) -> Self {
        Self {
            process: ToolProcess::new(paths.vntextpatch()),
        }
    }

    pub fn from_executable(executable: impl Into<std::path::PathBuf>) -> Self {
        Self {
            process: ToolProcess::new(executable),
        }
    }

    pub async fn execute(
        &self,
        request: &AdapterRequest,
        cancellation: &CancellationToken,
    ) -> Result<AdapterResponse, EngineError> {
        let format = request
            .format
            .as_deref()
            .ok_or_else(|| EngineError::Protocol("缺少脚本格式。".into()))?;
        if !FORMATS.contains(&format) {
            return Err(EngineError::Protocol(format!("不支持的脚本格式：{format}")));
        }
        self.process
            .execute::<_, AdapterResponse>(request, cancellation)
            .await?
            .checked()
    }

    pub async fn probe(
        &self,
        source: &Path,
        format: &str,
        cancellation: &CancellationToken,
    ) -> Result<AdapterResponse, EngineError> {
        let mut request = AdapterRequest::new("probe", source);
        request.format = Some(format.into());
        self.execute(&request, cancellation).await
    }
}
