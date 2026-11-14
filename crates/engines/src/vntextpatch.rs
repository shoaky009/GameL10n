use std::path::Path;

use crate::{
    CancellationToken, EngineError, ToolPaths, ToolProcess,
    discovery::Inventory,
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
        if cancellation.is_cancelled() {
            return Err(EngineError::Cancelled);
        }
        let implicit = if request.resource_files.is_none() {
            let mut request = request.clone();
            request.resource_files =
                Some(Inventory::scan(&request.source_directory)?.candidate_resources()?);
            Some(request)
        } else {
            None
        };
        let request = implicit.as_ref().unwrap_or(request);
        self.process
            .execute::<_, AdapterResponse>(request, cancellation)
            .await?
            .checked_for_operation(&request.operation)
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[tokio::test]
    async fn implicit_probe_and_extraction_exclude_documents_while_explicit_resources_remain_valid()
    {
        let paths = ToolPaths::development();
        if !paths.vntextpatch().is_file() {
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let script = "\u{feff}*start\nこれは物語の台詞です。[p]\n";
        fs::write(root.path().join("story.txt"), script).unwrap();
        fs::write(root.path().join("ReAdMe.TxT"), script).unwrap();
        let worker = VnTextPatch::new(&paths);
        let token = CancellationToken::new();
        let probe = worker
            .probe(root.path(), "KirikiriKs", &token)
            .await
            .unwrap()
            .probe
            .unwrap();
        assert_eq!(probe.required_files, ["story.txt"]);
        let mut request = AdapterRequest::new("extract", root.path());
        request.format = Some("KirikiriKs".into());
        let extraction = worker
            .execute(&request, &token)
            .await
            .unwrap()
            .extraction
            .unwrap();
        assert!(!extraction.units.is_empty());
        assert!(
            extraction
                .units
                .iter()
                .all(|unit| unit.resource_path == "story.txt")
        );
        request.resource_files = Some(vec!["ReAdMe.TxT".into()]);
        let explicit = worker
            .execute(&request, &token)
            .await
            .unwrap()
            .extraction
            .unwrap();
        assert!(!explicit.units.is_empty());
        assert!(
            explicit
                .units
                .iter()
                .all(|unit| unit.resource_path == "ReAdMe.TxT")
        );
        assert_eq!(
            fs::read_to_string(root.path().join("ReAdMe.TxT")).unwrap(),
            script
        );
    }
}
