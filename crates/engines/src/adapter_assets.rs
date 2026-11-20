//! Fixed public assets obtained through the shared download cache.
use std::{collections::BTreeMap, path::PathBuf};

use serde::{Deserialize, Serialize};

use crate::{CancellationToken, EngineError, EngineService, protocol::RequiredAssets};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Manifest {
    raw_base_url: String,
    revision: String,
    files: BTreeMap<String, PinnedFile>,
}

#[derive(Deserialize)]
struct PinnedFile {
    path: String,
    sha256: String,
}

#[derive(Serialize)]
pub(crate) struct PublicSource {
    url: String,
    sha256: String,
}

pub(crate) fn sources(
    catalog: &str,
) -> Result<Option<BTreeMap<String, PublicSource>>, EngineError> {
    let manifest = match catalog {
        "qlie-systemfont-assets" => include_str!("../../../third_party/qlie-fonts/sources.json"),
        _ => return Ok(None),
    };
    let manifest: Manifest = serde_json::from_str(manifest)?;
    Ok(Some(
        manifest
            .files
            .into_iter()
            .map(|(name, file)| {
                (
                    name,
                    PublicSource {
                        url: format!(
                            "{}/{}/{}",
                            manifest.raw_base_url, manifest.revision, file.path
                        ),
                        sha256: file.sha256,
                    },
                )
            })
            .collect(),
    ))
}

pub(crate) async fn resolve(
    service: &EngineService,
    required: &RequiredAssets,
    cancellation: &CancellationToken,
) -> Result<BTreeMap<String, PathBuf>, EngineError> {
    let sources = sources(&required.catalog)?
        .ok_or_else(|| EngineError::Protocol("适配器请求了未登记的素材来源".into()))?;
    if required.files.is_empty()
        || required.files.len() > sources.len()
        || required
            .files
            .iter()
            .any(|name| !sources.contains_key(name))
    {
        return Err(EngineError::Protocol(
            "适配器请求的素材不在登记清单内".into(),
        ));
    }
    let mut paths = BTreeMap::new();
    for name in &required.files {
        if paths.contains_key(name) {
            continue;
        }
        let source = &sources[name];
        let download = service
            .fetch_public_download(&source.url, false, Some(&source.sha256), cancellation)
            .await
            .map_err(|error| match error {
                EngineError::Research(message) | EngineError::Rejected(message) => {
                    EngineError::Research(format!(
                        "无法取得适配素材 {name}（{}）",
                        message.trim_end_matches(['。', '.'])
                    ))
                }
                other => other,
            })?;
        paths.insert(name.clone(), download.path.canonicalize()?);
    }
    Ok(paths)
}
