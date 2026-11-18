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

pub(crate) struct BuiltinFormat {
    pub key: &'static str,
    pub name: &'static str,
    pub extensions: &'static [&'static str],
}

// Shared format metadata for native discovery and intelligent adaptation.
pub(crate) const BUILTIN_FORMATS: &[BuiltinFormat] = &[
    BuiltinFormat {
        key: "AdvHd",
        name: "AdvHD / WS2",
        extensions: &["ws2"],
    },
    BuiltinFormat {
        key: "Age",
        name: "ArcGameEngine / BIN",
        extensions: &["bin"],
    },
    BuiltinFormat {
        key: "ArtemisAsb",
        name: "Artemis / ASB",
        extensions: &["asb"],
    },
    BuiltinFormat {
        key: "ArtemisAst",
        name: "Artemis / AST",
        extensions: &["ast"],
    },
    BuiltinFormat {
        key: "ArtemisTxt",
        name: "Artemis / text",
        extensions: &["art", "txt"],
    },
    BuiltinFormat {
        key: "CatSystem",
        name: "CatSystem2 / CST",
        extensions: &["cst"],
    },
    BuiltinFormat {
        key: "CSystem",
        name: "Cyberworks C,system / A0",
        extensions: &["a0"],
    },
    BuiltinFormat {
        key: "Ethornell",
        name: "Ethornell / BGI",
        extensions: &["bgi", ""],
    },
    BuiltinFormat {
        key: "Kaguya",
        name: "KaGuYa / message.dat",
        extensions: &["dat"],
    },
    BuiltinFormat {
        key: "KirikiriKs",
        name: "Kirikiri / KS",
        extensions: &["ks", "txt"],
    },
    BuiltinFormat {
        key: "KirikiriSoc",
        name: "Kirikiri / SOC",
        extensions: &["soc"],
    },
    BuiltinFormat {
        key: "KirikiriTjs",
        name: "Kirikiri / compiled TJS",
        extensions: &["tjs"],
    },
    BuiltinFormat {
        key: "Majiro",
        name: "Majiro / MJO",
        extensions: &["mjo"],
    },
    BuiltinFormat {
        key: "Musica",
        name: "Musica / SC",
        extensions: &["sc"],
    },
    BuiltinFormat {
        key: "Mware",
        name: "Mware / NUT",
        extensions: &["nut"],
    },
    BuiltinFormat {
        key: "Propeller",
        name: "Propeller / Stuff Script Engine / MSC",
        extensions: &["msc"],
    },
    BuiltinFormat {
        key: "Qlie",
        name: "QLIE / S",
        extensions: &["s"],
    },
    BuiltinFormat {
        key: "RealLive",
        name: "RealLive / scenario",
        extensions: &["rl", "txt"],
    },
    BuiltinFormat {
        key: "Renpy",
        name: "Ren'Py / RPY",
        extensions: &["rpy"],
    },
    BuiltinFormat {
        key: "ShSystem",
        name: "ShSystem / HST",
        extensions: &["hst"],
    },
    BuiltinFormat {
        key: "SilkysMap",
        name: "Silky's / AI6WIN / MAP",
        extensions: &["map"],
    },
    BuiltinFormat {
        key: "SilkysMes",
        name: "Silky's / AI6WIN / MES",
        extensions: &["mes"],
    },
    BuiltinFormat {
        key: "Softpal",
        name: "Softpal / SRC + DAT",
        extensions: &["src"],
    },
    BuiltinFormat {
        key: "SystemNnnDev",
        name: "SystemNNN / NNN",
        extensions: &["nnn"],
    },
    BuiltinFormat {
        key: "SystemNnnRelease",
        name: "SystemNNN / SPT",
        extensions: &["spt"],
    },
    BuiltinFormat {
        key: "TmrHiroAdvSystemCode",
        name: "TmrHiroAdvSystem / SRP",
        extensions: &["srp"],
    },
    BuiltinFormat {
        key: "TmrHiroAdvSystemText",
        name: "TmrHiroAdvSystem / text table",
        extensions: &["", "dat", "txt"],
    },
    BuiltinFormat {
        key: "Whale",
        name: "Whale / text",
        extensions: &["txt", ""],
    },
    BuiltinFormat {
        key: "Yuris",
        name: "YU-RIS / YBN",
        extensions: &["ybn"],
    },
];

impl BuiltinFormat {
    pub(crate) fn companions(&self) -> &'static [&'static str] {
        match self.key {
            "Age" => &["CHINIT.BIN"],
            "Softpal" => &["TEXT.DAT", "POINT.DAT"],
            "Yuris" => &["ysc.ybn"],
            _ => &[],
        }
    }
}

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
