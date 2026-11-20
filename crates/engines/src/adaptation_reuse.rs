//! Bounded source inspection and selective reference copies of published adapters.
use super::{
    AdaptationRequest, Session, capability, number, optional_text, preflight_unmodified,
    readback_page, strings, text,
};
use crate::{
    CancellationToken, EngineError, PackageManifest,
    discovery::{glob_match, resolve_relative},
};
use gamel10n_translation::files::hash_bytes;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::Path,
};

const MAX_SOURCE_BYTES: usize = 1024 * 1024;
const MAX_COPY_BYTES: usize = 32 * 1024 * 1024;

pub(super) fn sampled_recipes(request: &AdaptationRequest<'_>, state: &Session) -> Vec<Value> {
    let mut samples: BTreeMap<String, Vec<&str>> = BTreeMap::new();
    let trials = state
        .extra
        .get("automaticArchiveTrials")
        .and_then(Value::as_array);
    for sample in trials.into_iter().flatten() {
        if let (Some(template), Some(archive)) =
            (sample["templateId"].as_str(), sample["archive"].as_str())
            && crate::adaptation_context::archive_sample_passed(sample)
            && !request.inputs.excludes(archive)
        {
            samples
                .entry(format!("template.{template}"))
                .or_default()
                .push(archive);
        }
    }
    if samples.is_empty() {
        return Vec::new();
    }
    let mut seen = BTreeSet::new();
    request.source_packages().filter_map(|directory| {
        let manifest = PackageManifest::load(directory).ok()?;
        let archives: Vec<_> = samples.get(&manifest.id)?.iter().copied()
            .filter(|path| manifest.resource_patterns.iter().any(|pattern| glob_match(pattern, path)))
            .collect();
        if archives.is_empty() || !seen.insert((manifest.id.clone(), manifest.version.clone())) {
            return None;
        }
        capability::arguments(directory, &manifest)?;
        Some(json!({
            "id":manifest.id, "version":manifest.version, "sampledArchives":archives,
            "nextAction":"优先复用已有中文回填实现。先按当前启动范围选择并暂存所需封包；不同目录的同名封包应先确认归属。用 copy_adapter 的 resourceFiles 传精确路径，由宿主重新验收当前资源"
        }))
    }).take(3).collect()
}

pub(super) fn resource_scope(
    manifest: &PackageManifest,
    game: &Path,
    args: &Value,
) -> Result<Vec<String>, EngineError> {
    if args.get("resourceFiles").is_none() {
        return Ok(manifest.resource_patterns.clone());
    }
    if manifest.manifest_version != 4 {
        return Err(EngineError::Rejected(
            "只有可执行的发布适配器可指定当前资源范围".into(),
        ));
    }
    let mut paths = strings(args, "resourceFiles")?;
    if paths.is_empty() || paths.len() > 1024 {
        return Err(EngineError::Protocol("请选择 1 至 1024 个资源路径".into()));
    }
    let mut seen = BTreeSet::new();
    for path in &mut paths {
        *path = path.replace('\\', "/");
        resolve_relative(game, path)?;
        let matches = if manifest.resource_patterns.is_empty() {
            manifest
                .resource_files
                .iter()
                .any(|file| file.eq_ignore_ascii_case(path))
        } else {
            manifest
                .resource_patterns
                .iter()
                .any(|pattern| glob_match(pattern, path))
        };
        if !matches || !seen.insert(path.to_lowercase()) {
            return Err(EngineError::Rejected(format!(
                "资源路径重复或超出来源适配器的资源条件：{path}"
            )));
        }
    }
    Ok(paths)
}

fn find<'a>(
    request: &'a AdaptationRequest<'_>,
    args: &Value,
) -> Result<(PackageManifest, &'a Path), EngineError> {
    let id = text(args, "id")?;
    let version = text(args, "version")?;
    for directory in request.source_packages() {
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
        .filter(|path| !super::components::is_release_source(path))
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
