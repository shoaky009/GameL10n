//! Reusable adapter component metadata and the component-specific source whitelist.
use super::{ReleaseFile, Session, select_release, strings, text};
use crate::{CancellationToken, EngineError, EngineService, discovery::resolve_relative};
use gamel10n_translation::files::hash_bytes;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{collections::BTreeSet, fs, io::Write, path::Path};

const VERSION: &str = "2.6.0";
const USAGE: &str = "copy_adapter_components 按需复制 Python 辅助文件或预编译运行组件。work/ 中的诊断工具仅用于分析，不进入发布清单；src/ 中的组件供适配器复用。支持的 JSON CLI 可用 run_engine_python 直接运行。组件本身不保证当前游戏兼容，仍需资源回填及运行显示验证。";

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Component {
    id: String,
    name: String,
    scope: String,
    origin: String,
    api: String,
    example: String,
    files: Vec<ComponentFile>,
    engine_families: Vec<String>,
}

#[derive(Deserialize)]
struct ComponentFile {
    name: String,
    path: String,
    binary: bool,
}

fn catalog() -> Result<Vec<Component>, EngineError> {
    Ok(serde_json::from_str(include_str!(
        "../prompts/adapter-components.json"
    ))?)
}

fn find<'a>(components: &'a [Component], id: &str) -> Result<&'a Component, EngineError> {
    components
        .iter()
        .find(|component| component.id.eq_ignore_ascii_case(id))
        .ok_or_else(|| {
            EngineError::Rejected("未知复用组件，请从 list_adapters 的 components 中选择。".into())
        })
}

pub(super) fn matches_shipped_dependency(
    service: &EngineService,
    path: &str,
    sha256: &str,
) -> Result<bool, EngineError> {
    let file = catalog()?
        .into_iter()
        .flat_map(|component| component.files)
        .find(|file| file.path == path);
    let Some(file) = file else {
        return Ok(false);
    };
    if file.binary {
        let original = service
            .tools
            .root
            .join("adapter-components")
            .join(file.name);
        return Ok(original.is_file()
            && gamel10n_translation::files::hash_file(&original).map_err(super::file_error)?
                == sha256);
    }
    Ok(hash_bytes(source(&file.name)?) == sha256)
}

pub(super) fn list(query: &str) -> Result<Vec<Value>, EngineError> {
    let query = query.to_lowercase();
    Ok(catalog()?
        .into_iter()
        .filter(|component| {
            query.trim().is_empty()
                || format!(
                    "{} {} {} {} {}",
                    component.id, component.name, component.scope, component.origin,
                    component.engine_families.join(" ")
                )
                .to_lowercase()
                .contains(&query)
        })
        .map(|component| {
            json!({"id":component.id,"version":VERSION,"name":component.name,"scope":component.scope})
        })
        .collect())
}

pub(super) fn related(root: Option<&str>) -> Result<Vec<Value>, EngineError> {
    Ok(catalog()?.into_iter().filter(|component| root.is_some_and(|root|component.engine_families.iter().any(|family|family.eq_ignore_ascii_case(root)))).map(|component|json!({"id":component.id,"version":VERSION,"scope":component.scope,"inspectCall":{"tool":"inspect_adapter_component","arguments":{"id":component.id}}})).collect())
}

fn describe(component: &Component, service: &EngineService) -> Value {
    json!({
        "id":component.id,"version":VERSION,"name":component.name,"scope":component.scope,
        "origin":component.origin,"api":component.api,"example":component.example,
        "files":component.files.iter().map(|file|json!({"path":file.path,"binary":file.binary,"available":!file.binary||service.tools.root.join("adapter-components").join(&file.name).is_file()})).collect::<Vec<_>>(),
        "usage":USAGE
    })
}

fn source(name: &str) -> Result<&'static [u8], EngineError> {
    match name {
        "windows_font_metrics.py" => Ok(include_bytes!(
            "../../../adapters/components/windows_font_metrics.py"
        )),
        "kaguya_tblstr.py" => Ok(include_bytes!(
            "../../../adapters/components/kaguya_tblstr.py"
        )),
        "xp3_adler.py" => Ok(include_bytes!("../../../adapters/components/xp3_adler.py")),
        "sjis_tunnel.py" => Ok(include_bytes!(
            "../../../adapters/components/sjis_tunnel.py"
        )),
        "vntextproxy_runtime.py" => Ok(include_bytes!(
            "../../../adapters/components/vntextproxy_runtime.py"
        )),
        "gdiplus_font_bridge.py" => Ok(include_bytes!(
            "../../../adapters/components/gdiplus_font_bridge.py"
        )),
        "GameL10n.FontBridge.LICENSE" => {
            Ok(include_bytes!("../../../tools/GameL10n.FontBridge/LICENSE"))
        }
        "VNTextProxy-LICENSE.txt" => Ok(include_bytes!(
            "../../../adapters/components/VNTextProxy-LICENSE.txt"
        )),
        "kirikiri_scn_text.py" => Ok(include_bytes!(
            "../../../adapters/components/kirikiri_scn_text.py"
        )),
        "kirikiri_tft.py" => Ok(include_bytes!(
            "../../../adapters/components/kirikiri_tft.py"
        )),
        "gamel10n_host.py" => Ok(include_bytes!(
            "../../../adapters/components/gamel10n_host.py"
        )),
        "QlieLauncher.Cargo.toml" => Ok(include_bytes!(
            "../../../tools/GameL10n.QlieLauncher/Cargo.toml"
        )),
        _ => Err(EngineError::Rejected(format!("内置组件源码缺失：{name}"))),
    }
}

fn page(id: &str, path: &str, content: &str, args: &Value) -> Result<Value, EngineError> {
    let integer = |name: &str, default| {
        args.get(name).map_or(Ok(default), |value| {
            value
                .as_i64()
                .ok_or_else(|| EngineError::Protocol(format!("{name} 必须是整数。")))
        })
    };
    let offset = integer("offset", 0)?;
    let length = integer("length", 12000)?;
    if !(0..=i64::from(i32::MAX)).contains(&offset) || !(1..=24000).contains(&length) {
        return Err(EngineError::Protocol(
            "offset 必须非负，length 必须为 1 到 24000。".into(),
        ));
    }
    let units: Vec<_> = content.encode_utf16().collect();
    let start = (offset as usize).min(units.len());
    let next = (start + length as usize).min(units.len());
    Ok(
        json!({"id":id,"version":VERSION,"path":path,"offset":start,"nextOffset":next,"totalLength":units.len(),"more":next<units.len(),"content":String::from_utf16_lossy(&units[start..next])}),
    )
}

pub(super) fn inspect(
    service: &EngineService,
    args: &Value,
    cancellation: &CancellationToken,
) -> Result<Value, EngineError> {
    if cancellation.is_cancelled() {
        return Err(EngineError::Cancelled);
    }
    let components = catalog()?;
    let id = text(args, "id")?;
    let component = find(&components, id)?;
    let Some(path) = args.get("path").and_then(Value::as_str) else {
        return Ok(describe(component, service));
    };
    let path = path.replace('\\', "/");
    let file = component
        .files
        .iter()
        .find(|file| !file.binary && file.path == path)
        .ok_or_else(|| EngineError::Rejected("源码路径不在此组件索引中。".into()))?;
    page(
        id,
        &file.path,
        &String::from_utf8_lossy(source(&file.name)?),
        args,
    )
}

pub(super) fn copy(
    service: &EngineService,
    workspace: &Path,
    state: &mut Session,
    args: &Value,
) -> Result<Value, EngineError> {
    let ids = strings(args, "ids")?;
    if ids.is_empty() || ids.len() > 4 {
        return Err(EngineError::Protocol("每次选择 1 到 4 个组件。".into()));
    }
    let components = catalog()?;
    let mut selected = Vec::new();
    let mut seen = BTreeSet::new();
    for id in ids {
        if seen.insert(id.to_lowercase()) {
            selected.push(find(&components, &id)?);
        }
    }
    let mut files = Vec::new();
    let mut destinations = BTreeSet::new();
    for component in &selected {
        for file in &component.files {
            if !destinations.insert(file.path.to_lowercase()) {
                continue;
            }
            let content = if file.binary {
                let path = service
                    .tools
                    .root
                    .join("adapter-components")
                    .join(&file.name);
                if !path.is_file() {
                    return Err(EngineError::Rejected(format!(
                        "缺少预编译组件 {}，请使用完整发行包；开发环境按 docs/development.md 准备工具。",
                        file.name
                    )));
                }
                fs::read(path)?
            } else {
                source(&file.name)?.to_vec()
            };
            files.push((file.path.clone(), content));
        }
        if component.id == "qlie-japanese-font-launcher" {
            // Retain the launcher's complete Rust project beside the pinned binary.
            for (name, content) in [
                ("LICENSE", include_bytes!("../../../LICENSE").as_slice()),
                (
                    "Cargo.lock",
                    include_bytes!("../../../tools/GameL10n.QlieLauncher/Cargo.lock").as_slice(),
                ),
                (
                    "src/main.rs",
                    include_bytes!("../../../tools/GameL10n.QlieLauncher/src/main.rs").as_slice(),
                ),
                (
                    "src/config.rs",
                    include_bytes!("../../../tools/GameL10n.QlieLauncher/src/config.rs").as_slice(),
                ),
                (
                    "src/font.rs",
                    include_bytes!("../../../tools/GameL10n.QlieLauncher/src/font.rs").as_slice(),
                ),
                (
                    "src/windows.rs",
                    include_bytes!("../../../tools/GameL10n.QlieLauncher/src/windows.rs")
                        .as_slice(),
                ),
                (
                    "src/self_test.rs",
                    include_bytes!("../../../tools/GameL10n.QlieLauncher/src/self_test.rs")
                        .as_slice(),
                ),
            ] {
                files.push((
                    format!("src/tools/launcher-source/{name}"),
                    content.to_vec(),
                ));
            }
        }
    }
    let root = fs::canonicalize(workspace)?;
    let mut reused = BTreeSet::new();
    let mut selection = state.release_files.clone();
    for (relative, content) in &files {
        let path = resolve_relative(&root, relative)?;
        if path.is_dir() {
            return Err(EngineError::Rejected(format!(
                "组件目标已是目录：{relative}"
            )));
        }
        let mut parent = path.parent();
        while let Some(directory) = parent.filter(|directory| directory.starts_with(&root)) {
            if directory.is_file() {
                return Err(EngineError::Rejected(format!(
                    "组件目录被文件占用：{relative}"
                )));
            }
            parent = directory.parent();
        }
        if path.is_file() {
            if fs::read(&path)? != *content {
                return Err(EngineError::Rejected(format!(
                    "组件文件已有不同内容，本次未写入文件：{relative}。请先检查已有实现，保留必要修改。"
                )));
            }
            reused.insert(relative.clone());
        }
        if !relative.starts_with("src/")
            || (relative.ends_with(".cs") && !relative.starts_with("src/tools/launcher-source/"))
        {
            continue;
        }
        select_release(
            &mut selection,
            ReleaseFile {
                path: relative.trim_start_matches("src/").into(),
                role: if relative.starts_with("src/tools/launcher-source/")
                    || relative.ends_with("/VNTextProxy-LICENSE.txt")
                    || relative.ends_with("/GameL10n.FontBridge.LICENSE")
                {
                    "license"
                } else if relative.ends_with(".py") {
                    "runtime"
                } else {
                    "asset"
                }
                .into(),
                purpose: if relative.starts_with("src/tools/launcher-source/") {
                    "保留启动器对应源码与构建说明。"
                } else {
                    "内置文本、编码、字体或启动组件"
                }
                .into(),
            },
        )?;
    }
    for (relative, content) in &files {
        if reused.contains(relative) {
            continue;
        }
        let path = resolve_relative(&root, relative)?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)?
            .write_all(content)?;
    }
    state.release_files = selection;
    Ok(
        json!({"components":selected.iter().map(|component|describe(component,service)).collect::<Vec<_>>(),"files":files.iter().map(|(relative,content)|json!({"path":relative,"reused":reused.contains(relative),"sha256":hash_bytes(content)})).collect::<Vec<_>>()}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ToolPaths;

    fn service(root: &Path) -> EngineService {
        EngineService::new(root, ToolPaths::new(root.join("tools")))
    }

    #[test]
    fn component_metadata_preserves_the_original_catalog_and_search_fields() {
        let root = tempfile::tempdir().expect("fixture");
        let service = service(root.path());
        let components = list("").expect("catalog");
        assert_eq!(components.len(), 12);
        assert!(components.iter().all(|component| {
            component.as_object().expect("metadata").len() == 4
                && component["version"] == VERSION
                && component["name"]
                    .as_str()
                    .is_some_and(|name| !name.is_empty())
                && component["scope"]
                    .as_str()
                    .is_some_and(|scope| !scope.is_empty())
        }));
        assert_eq!(
            list("ARCUSMAXIMUS").expect("origin search")[0]["id"],
            "sjis-tunnel"
        );
        assert!(
            list("mappingInput")
                .expect("API is not a list search field")
                .is_empty()
        );
        let result = inspect(
            &service,
            &json!({"id":"XP3-ADLER"}),
            &CancellationToken::new(),
        )
        .expect("description");
        assert_eq!(result["id"], "xp3-adler");
        assert_eq!(result["name"], "XP3 Adler 派生过滤器检测");
        assert!(
            result["api"]
                .as_str()
                .expect("API")
                .contains("member_offset=...")
        );
        assert!(
            result["example"]
                .as_str()
                .expect("example")
                .starts_with("copy_adapter_components(ids=[\"xp3-adler\"])\n")
        );
        assert_eq!(
            result["files"],
            json!([{"path":"src/tools/xp3_adler.py","binary":false,"available":true}])
        );
        assert_eq!(result["usage"], USAGE);
        let related = related(Some("kirikiri")).expect("related components");
        assert_eq!(
            related
                .iter()
                .map(|item| item["id"].as_str().expect("id"))
                .collect::<Vec<_>>(),
            [
                "windows-font-metrics",
                "xp3-adler",
                "kirikiri-scn-text",
                "kirikiri-tft",
            ]
        );
        assert_eq!(
            related[1]["inspectCall"],
            json!({"tool":"inspect_adapter_component","arguments":{"id":"xp3-adler"}})
        );
    }

    #[test]
    fn component_source_reads_require_the_selected_components_exact_nonbinary_path() {
        let root = tempfile::tempdir().expect("fixture");
        let service = service(root.path());
        let cancellation = CancellationToken::new();
        for args in [
            json!({"id":"xp3-adler","path":"src/tools/sjis_tunnel.py"}),
            json!({"id":"xp3-adler","path":"xp3_adler.py"}),
            json!({"id":"xp3-adler","path":"src/tools/XP3_ADLER.py"}),
            json!({"id":"xp3-adler","path":"src/tools/../tools/xp3_adler.py"}),
            json!({"id":"qlie-japanese-font-launcher","path":"src/tools/QlieLauncher.exe"}),
            json!({"path":"src/tools/xp3_adler.py"}),
            json!({"id":"missing","path":"src/tools/xp3_adler.py"}),
        ] {
            assert!(
                inspect(&service, &args, &cancellation).is_err(),
                "accepted invalid source request: {args}"
            );
        }
        let result = inspect(
            &service,
            &json!({"id":"xp3-adler","path":"src\\tools\\xp3_adler.py","length":90}),
            &cancellation,
        )
        .expect("source read");
        assert_eq!(result["path"], "src/tools/xp3_adler.py");
        assert_eq!(result["nextOffset"], 90);
        assert_eq!(
            result["content"],
            String::from_utf8_lossy(source("xp3_adler.py").expect("embedded source"))
                .chars()
                .take(90)
                .collect::<String>()
        );
        cancellation.cancel();
        assert!(matches!(
            inspect(&service, &json!({"id":"xp3-adler"}), &cancellation),
            Err(EngineError::Cancelled)
        ));
    }

    #[test]
    fn component_source_paging_counts_utf16_units_and_clamps_at_end() {
        let result = page("id", "path", "a😀b中", &json!({"offset":1,"length":2})).expect("page");
        assert_eq!(
            result,
            json!({"id":"id","version":VERSION,"path":"path","offset":1,"nextOffset":3,"totalLength":5,"more":true,"content":"😀"})
        );
        let result =
            page("id", "path", "a😀b中", &json!({"offset":200,"length":1})).expect("clamped page");
        assert_eq!(
            result,
            json!({"id":"id","version":VERSION,"path":"path","offset":5,"nextOffset":5,"totalLength":5,"more":false,"content":""})
        );
        assert_eq!(
            page("id", "path", &"a".repeat(13000), &json!({})).expect("default page")["nextOffset"],
            12000
        );
        for args in [
            json!({"offset":-1}),
            json!({"offset":2147483648_i64}),
            json!({"offset":1.5}),
            json!({"length":0}),
            json!({"length":24001}),
            json!({"length":"2"}),
        ] {
            assert!(
                page("id", "path", "sample", &args).is_err(),
                "invalid page: {args}"
            );
        }
        assert!(page("id", "path", "sample", &json!({"length":24000})).is_ok());
    }

    #[test]
    fn component_copy_preflights_the_whole_selection_and_returns_hashes_for_reused_files() {
        let root = tempfile::tempdir().expect("fixture");
        let service = service(root.path());
        let workspace = root.path().join("workspace");
        fs::create_dir_all(workspace.join("src/tools")).expect("workspace");
        let target = workspace.join("src/tools/xp3_adler.py");
        fs::write(&target, "local edits").expect("local source");
        let mut state = Session::default();
        assert!(
            copy(
                &service,
                &workspace,
                &mut state,
                &json!({"ids":["windows-font-metrics","xp3-adler"]})
            )
            .is_err()
        );
        assert!(!workspace.join("work/windows_font_metrics.py").exists());
        assert!(state.release_files.is_empty());
        fs::write(&target, source("xp3_adler.py").expect("source")).expect("matching source");
        let result = copy(
            &service,
            &workspace,
            &mut state,
            &json!({"ids":["windows-font-metrics","xp3-adler","XP3-ADLER"]}),
        )
        .expect("copy");
        assert_eq!(
            result["components"].as_array().expect("components").len(),
            2
        );
        assert_eq!(result["files"][0]["reused"], false);
        assert_eq!(
            result["files"][1],
            json!({"path":"src/tools/xp3_adler.py","reused":true,"sha256":hash_bytes(source("xp3_adler.py").expect("source"))})
        );
        assert_eq!(state.release_files.len(), 1);
        assert_eq!(state.release_files[0].path, "tools/xp3_adler.py");
    }

    #[test]
    fn proxy_runtime_component_is_inspectable_and_copies_helper_with_required_licenses() {
        let root = tempfile::tempdir().expect("fixture");
        let service = service(root.path());
        let cancellation = CancellationToken::new();
        let args = json!({"id":"vntextproxy-runtime","path":"src/tools/vntextproxy_runtime.py"});
        let inspected = inspect(&service, &args, &cancellation).expect("helper source");
        assert!(
            inspected["content"]
                .as_str()
                .expect("content")
                .contains("ARCHIVE_SHA256")
        );
        assert!(
            related(Some("yuris"))
                .expect("related")
                .iter()
                .any(|item| item["id"] == "vntextproxy-runtime")
        );
        let workspace = root.path().join("workspace");
        fs::create_dir_all(&workspace).expect("workspace");
        let mut state = Session::default();
        copy(
            &service,
            &workspace,
            &mut state,
            &json!({"ids":["vntextproxy-runtime"]}),
        )
        .expect("copy");
        assert!(workspace.join("src/tools/vntextproxy_runtime.py").is_file());
        let notice = fs::read_to_string(workspace.join("src/tools/VNTextProxy-LICENSE.txt"))
            .expect("licenses");
        assert!(notice.contains("Copyright (c) 2021 arcusmaximus"));
        assert!(notice.contains("Copyright (c) Microsoft Corporation"));
        assert!(
            state
                .release_files
                .iter()
                .any(|file| file.path == "tools/VNTextProxy-LICENSE.txt" && file.role == "license")
        );
        assert!(
            state
                .release_files
                .iter()
                .all(|file| !file.path.ends_with(".dll"))
        );
    }

    #[test]
    fn launcher_metadata_and_copy_preserve_the_native_binary_and_its_source_provenance() {
        let root = tempfile::tempdir().expect("fixture");
        let service = service(root.path());
        let args = json!({"id":"qlie-japanese-font-launcher"});
        let description = inspect(&service, &args, &CancellationToken::new()).expect("description");
        assert_eq!(description["files"][1]["available"], false);
        assert!(
            description["scope"]
                .as_str()
                .expect("scope")
                .contains("x64 Rust")
        );
        let workspace = root.path().join("workspace");
        fs::create_dir_all(&workspace).expect("workspace");
        let mut state = Session::default();
        assert!(
            copy(
                &service,
                &workspace,
                &mut state,
                &json!({"ids":["qlie-japanese-font-launcher"]})
            )
            .is_err()
        );
        assert!(!workspace.join("src").exists());
        fs::create_dir_all(service.tools.root.join("adapter-components")).expect("tools");
        fs::write(
            service
                .tools
                .root
                .join("adapter-components/QlieLauncher.exe"),
            b"native launcher fixture",
        )
        .expect("binary");
        let result = copy(
            &service,
            &workspace,
            &mut state,
            &json!({"ids":["qlie-japanese-font-launcher"]}),
        )
        .expect("copy");
        assert_eq!(result["components"][0]["files"][1]["available"], true);
        assert!(
            workspace
                .join("src/tools/launcher-source/Cargo.toml")
                .is_file()
        );
        assert!(
            workspace
                .join("src/tools/launcher-source/src/windows.rs")
                .is_file()
        );
        assert!(
            state
                .release_files
                .iter()
                .any(|file| file.path == "tools/QlieLauncher.exe" && file.role == "asset")
        );
        assert_eq!(
            state
                .release_files
                .iter()
                .filter(|file| file.role == "license")
                .count(),
            8
        );
        assert!(
            !state
                .release_files
                .iter()
                .any(|file| file.path.ends_with(".cs"))
        );
    }
}
