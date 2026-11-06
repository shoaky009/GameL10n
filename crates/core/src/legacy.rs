//! Read-only compatibility with the committed C# extraction cache.

use std::{collections::HashMap, fs, path::Path};

use gamel10n_engines::protocol::ExtractionResult;
use gamel10n_translation::{
    TranslationUnit,
    files::{hash_bytes, hash_text},
    validate_units,
};
use serde::Deserialize;
use serde_json::json;

use crate::{CoreError, GameProject, application::resolve_relative};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Manifest {
    schema_version: u32,
    source_fingerprint: String,
    adapter_id: String,
    adapter_version: String,
    resources: Vec<Resource>,
    unit_order: Vec<String>,
    warnings: Vec<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Resource {
    resource_path: String,
    file: String,
    sha256: String,
    unit_count: usize,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Shard {
    schema_version: u32,
    resource_path: String,
    units: Vec<TranslationUnit>,
}

pub(crate) fn load(
    project_directory: &Path,
    project: &GameProject,
) -> Result<Option<ExtractionResult>, CoreError> {
    let Some(summary) = project.extra.get("analysis") else {
        return Ok(None);
    };
    let Some(fingerprint) = summary
        .get("sourceFingerprint")
        .and_then(serde_json::Value::as_str)
    else {
        return Ok(None);
    };
    let adapter = summary
        .get("adapterId")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| CoreError::InvalidData("旧提取摘要缺少适配器 ID。".into()))?;
    let version = summary
        .get("adapterVersion")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| CoreError::InvalidData("旧提取摘要缺少适配器版本。".into()))?;
    // These contract values are ASCII. Preserve both the C# property order and compact encoding.
    if !fingerprint.is_ascii() || !adapter.is_ascii() || !version.is_ascii() {
        return Err(CoreError::InvalidData(
            "旧提取键包含未经验证的非 ASCII 标识，未重新计算或覆盖缓存。".into(),
        ));
    }
    let key = hash_text(
        &json!({"sourceFingerprint":fingerprint,"adapterId":adapter,"adapterVersion":version})
            .to_string(),
    );
    let directory = project_directory.join("extraction").join(key);
    let manifest_path = directory.join("manifest.json");
    if !manifest_path.is_file() {
        return Ok(None);
    }
    let bytes = fs::read(&manifest_path)?;
    let expected = fs::read_to_string(directory.join("manifest.sha256"))?;
    if hash_bytes(&bytes) != expected.trim() {
        return Err(CoreError::InvalidData("旧提取清单哈希不一致。".into()));
    }
    let manifest: Manifest = serde_json::from_slice(&bytes)
        .map_err(|error| CoreError::InvalidData(error.to_string()))?;
    if manifest.schema_version != 1
        || manifest.source_fingerprint != fingerprint
        || manifest.adapter_id != adapter
        || manifest.adapter_version != version
    {
        return Err(CoreError::InvalidData(
            "旧提取清单与项目摘要不一致。".into(),
        ));
    }
    let mut by_id = HashMap::with_capacity(manifest.unit_order.len());
    for resource in manifest.resources {
        let bytes = fs::read(resolve_relative(&directory, &resource.file)?)?;
        if hash_bytes(&bytes) != resource.sha256 {
            return Err(CoreError::InvalidData("旧提取资源哈希不一致。".into()));
        }
        let shard: Shard = serde_json::from_slice(&bytes)
            .map_err(|error| CoreError::InvalidData(error.to_string()))?;
        if shard.schema_version != 1
            || shard.resource_path != resource.resource_path
            || shard.units.len() != resource.unit_count
        {
            return Err(CoreError::InvalidData("旧提取资源清单不一致。".into()));
        }
        for unit in shard.units {
            if unit.resource_path != resource.resource_path
                || by_id.insert(unit.id.clone(), unit).is_some()
            {
                return Err(CoreError::InvalidData(
                    "旧提取结果包含重复身份或资源归属错误。".into(),
                ));
            }
        }
    }
    if by_id.len() != manifest.unit_order.len() {
        return Err(CoreError::InvalidData("旧提取文本顺序数量不一致。".into()));
    }
    let mut units = Vec::with_capacity(by_id.len());
    for id in manifest.unit_order {
        units.push(
            by_id.remove(&id).ok_or_else(|| {
                CoreError::InvalidData("旧提取文本顺序包含未知或重复 ID。".into())
            })?,
        );
    }
    validate_units(&units)?;
    Ok(Some(ExtractionResult {
        units,
        warnings: manifest.warnings,
    }))
}
