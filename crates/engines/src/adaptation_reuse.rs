//! Bounded source inspection and selective reference copies of published adapters.
use super::{
    AdaptationRequest, number, optional_text, preflight_unmodified, readback_page, strings, text,
};
use crate::{CancellationToken, EngineError, PackageManifest, discovery::resolve_relative};
use gamel10n_translation::files::hash_bytes;
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::Path,
};

const MAX_SOURCE_BYTES: usize = 1024 * 1024;
const MAX_COPY_BYTES: usize = 32 * 1024 * 1024;

fn find<'a>(
    request: &'a AdaptationRequest<'_>,
    args: &Value,
) -> Result<(PackageManifest, &'a Path), EngineError> {
    let id = text(args, "id")?;
    let version = text(args, "version")?;
    for directory in &request.package_directories {
        if let Ok(manifest) = PackageManifest::load(directory)
            && manifest.id == id
            && manifest.version == version
        {
            return Ok((manifest, directory));
        }
    }
    Err(EngineError::Rejected(
        "没有找到所选发布版本。内置解析器请使用 inspect_builtin_adapter。".into(),
    ))
}

fn read_registered(
    directory: &Path,
    manifest: &PackageManifest,
    relative: &str,
    maximum: usize,
) -> Result<Vec<u8>, EngineError> {
    let path = resolve_relative(directory, relative)?;
    let expected = manifest
        .package_files
        .get(relative)
        .ok_or_else(|| EngineError::Rejected("只能读取或复制发布清单中登记的文件。".into()))?;
    let mut content = Vec::new();
    File::open(path)?
        .take(maximum as u64 + 1)
        .read_to_end(&mut content)?;
    if content.len() > maximum {
        return Err(EngineError::Rejected(format!(
            "所选文件超出本次读取或复制的大小限制：{relative}"
        )));
    }
    if hash_bytes(&content) != *expected {
        return Err(EngineError::Rejected(format!(
            "适配器文件已变化，请恢复原发布包：{relative}"
        )));
    }
    Ok(content)
}

pub(super) fn inspect(request: &AdaptationRequest<'_>, args: &Value) -> Result<Value, EngineError> {
    let (manifest, directory) = find(request, args)?;
    if let Some(path) = args.get("path") {
        let relative = path
            .as_str()
            .ok_or_else(|| EngineError::Protocol("path 必须为字符串。".into()))?
            .replace('\\', "/");
        let bytes = read_registered(directory, &manifest, &relative, MAX_SOURCE_BYTES)?;
        let content = std::str::from_utf8(&bytes)
            .ok()
            .filter(|content| !content.contains('\0'))
            .ok_or_else(|| {
                EngineError::Rejected("所选文件不是 UTF-8 文本，不能作为源码读取。".into())
            })?;
        let mut page = readback_page(content, args, 0, true)?;
        page["id"] = json!(manifest.id);
        page["version"] = json!(manifest.version);
        page["path"] = json!(relative);
        page["referenceOnly"] = json!(true);
        return Ok(page);
    }
    let query = optional_text(args, "query", "")?.to_lowercase();
    if query.len() > 1024 {
        return Err(EngineError::Protocol("文件查询条件过长。".into()));
    }
    let files: Vec<_> = manifest
        .package_files
        .keys()
        .filter(|path| query.is_empty() || path.to_lowercase().contains(&query))
        .collect();
    let offset = number(args, "offset", 0, 0, files.len() as u64)? as usize;
    let length = number(args, "length", 40, 1, 100)? as usize;
    let next = files.len().min(offset + length);
    let page = files[offset..next]
        .iter()
        .map(|relative| {
            let bytes = resolve_relative(directory, relative)
                .ok()
                .and_then(|path| fs::metadata(path).ok())
                .filter(|metadata| metadata.is_file())
                .map(|metadata| metadata.len());
            json!({"path":relative,"bytes":bytes})
        })
        .collect::<Vec<_>>();
    Ok(json!({
        "id":manifest.id,"version":manifest.version,"manifestVersion":manifest.manifest_version,
        "family":manifest.family,"files":page,"fileCount":files.len(),
        "capabilityProfile":super::capability::describe(directory, &manifest),
        "nextOffset":next,"more":next<files.len(),"referenceOnly":true,
        "usage":"先按文件名定位所需模块，再用 path 和 query/offset/length 读取片段。文件索引不校验全包，仅核验实际读取或复制的文件。copy_adapter(files=[发布路径]) 将所选文件复制到 work/adapter-references/，不覆盖当前源码、不登记依赖或授予兼容资格；按模块核对输入、导入、共享依赖和许可后接入 src，再经 test_adapter 验收。"
    }))
}

pub(super) fn copy_selected(
    request: &AdaptationRequest<'_>,
    workspace: &Path,
    args: &Value,
    cancellation: &CancellationToken,
) -> Result<Value, EngineError> {
    let selected = strings(args, "files")?;
    if selected.is_empty() || selected.len() > 32 {
        return Err(EngineError::Protocol(
            "每次请选择 1 至 32 个发布文件。".into(),
        ));
    }
    let (manifest, directory) = find(request, args)?;
    let prefix = format!(
        "work/adapter-references/{}/{}",
        manifest.id, manifest.version
    );
    let target = resolve_relative(workspace, &prefix)?;
    let mut files = Vec::new();
    let mut seen = BTreeSet::new();
    let mut remaining = MAX_COPY_BYTES;
    for relative in selected {
        if cancellation.is_cancelled() {
            return Err(EngineError::Cancelled);
        }
        let relative = relative.replace('\\', "/");
        if !seen.insert(relative.to_lowercase()) {
            return Err(EngineError::Protocol("所选发布文件不能重复。".into()));
        }
        let content = read_registered(directory, &manifest, &relative, remaining)?;
        remaining -= content.len();
        let path = resolve_relative(&target, &relative)?;
        preflight_unmodified(&path, &content)?;
        files.push((relative, path, content));
    }
    // Check every selected input and conflict before creating reference files.
    let mut copied = Vec::new();
    for (relative, path, content) in files {
        if cancellation.is_cancelled() {
            return Err(EngineError::Cancelled);
        }
        if !path.exists() {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(path)?
                .write_all(&content)?;
        }
        copied.push(
            json!({"source":relative,"path":format!("{prefix}/{relative}"),"bytes":content.len()}),
        );
    }
    Ok(json!({
        "success":true,"id":manifest.id,"version":manifest.version,"referenceOnly":true,"files":copied,
        "nextAction":"所选文件仅供参考，当前适配器、身份、依赖和发布清单均未变更。保留已验证模块，只替换当前不兼容的部分；核对导入、共享依赖与许可，将所需实现接入 src 后按现有流程 test_adapter。不要直接发布 work/ 或把来源验收当作当前游戏通过。"
    }))
}
