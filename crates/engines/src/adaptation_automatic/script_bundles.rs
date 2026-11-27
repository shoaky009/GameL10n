//! Ren'Py and Kirikiri bundled script/archive template qualification.
use super::{extension, record, reject, supported};
use crate::adaptation::{Session, text};
use crate::adaptation_assets::{
    ARCHIVE_RPA3_COMPONENT, LOOSE_KIRIKIRI_KS_TEMPLATE, LOOSE_RENPY_RPYC_TEMPLATE,
    LOOSE_RENPY_TEMPLATE, MAX_ARCHIVE_ENTRIES, MAX_ARCHIVE_MANIFEST_BYTES,
    MAX_TEMPLATE_TOTAL_BYTES, RENPY_MIXED_TEMPLATE, RPA3_RENPY_RPYC_TEMPLATE, RPA3_RENPY_TEMPLATE,
};
use crate::discovery::resolve_relative;
use crate::{CancellationToken, EngineError, EngineService};
use serde_json::{Value, json};
use std::{collections::BTreeMap, fs, path::Path};

const MAX_RPYC_SCRIPT_BYTES: u64 = 32 * 1024 * 1024;
const MAX_RPY_SCRIPT_BYTES: u64 = 64 * 1024 * 1024;

fn is_other_translation_script(member: &str) -> bool {
    let parts: Vec<_> = member.replace('\\', "/").split('/').map(str::to_owned).collect();
    for index in 0..parts.len().saturating_sub(2) {
        if parts[index].eq_ignore_ascii_case("tl") {
            return !parts[index + 1].eq_ignore_ascii_case("none");
        }
    }
    false
}

pub(super) struct BundlePlan {
    pub(super) template_id: &'static str,
    pub(super) archives: Vec<String>,
    loose_renpy: usize,
    loose_compiled: usize,
    loose_kirikiri: usize,
    mixed: bool,
}

impl BundlePlan {
    pub(super) fn loose_scripts(&self) -> usize {
        self.loose_renpy + self.loose_compiled + self.loose_kirikiri
    }

    pub(super) fn archive_count(&self) -> usize {
        self.archives.len()
    }

    pub(super) fn has_archives(&self) -> bool {
        !self.archives.is_empty()
    }

    pub(super) fn allowed_extra_extensions(&self) -> &'static [&'static str] {
        if self.has_archives() { &["rpa"] } else { &[] }
    }

    pub(super) fn is_packed_resource(&self, path: &str) -> bool {
        self.has_archives() && extension(path) == "rpa"
    }
}

pub(super) fn has_eligible_candidate(candidates: &[Value]) -> bool {
    candidates
        .iter()
        .any(|candidate| matches!(candidate["format"].as_str(), Some("Renpy" | "KirikiriKs")))
}

pub(super) fn plan(paths: &BTreeMap<String, u64>) -> Result<BundlePlan, EngineError> {
    let by_extension = |ext: &str| {
        paths
            .keys()
            .filter(|path| extension(path) == ext)
            .cloned()
            .collect::<Vec<_>>()
    };
    let xp3 = by_extension("xp3");
    if !xp3.is_empty() {
        return Err(reject(format!(
            "XP3 缺少独立的无过滤器证据，不能根据零标记或可解包推断 filter=none；保留模型流程确认：{}",
            xp3.iter().take(8).cloned().collect::<Vec<_>>().join("、")
        )));
    }
    let archives = by_extension("rpa");
    let renpy = by_extension("rpy");
    let compiled = by_extension("rpyc");
    let kirikiri = by_extension("ks");
    if !kirikiri.is_empty() && (!archives.is_empty() || !renpy.is_empty() || !compiled.is_empty()) {
        return Err(reject("发现不同引擎的资源混用，需先确认加载范围。"));
    }
    let mixed = !renpy.is_empty() && !compiled.is_empty()
        || !archives.is_empty() && (!renpy.is_empty() || !compiled.is_empty());
    let template_id = if mixed {
        RENPY_MIXED_TEMPLATE
    } else if !archives.is_empty() {
        RPA3_RENPY_TEMPLATE
    } else if !compiled.is_empty() {
        LOOSE_RENPY_RPYC_TEMPLATE
    } else if !renpy.is_empty() {
        LOOSE_RENPY_TEMPLATE
    } else {
        LOOSE_KIRIKIRI_KS_TEMPLATE
    };
    Ok(BundlePlan {
        template_id,
        archives,
        loose_renpy: renpy.len(),
        loose_compiled: compiled.len(),
        loose_kirikiri: kirikiri.len(),
        mixed,
    })
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn inspect_archives(
    service: &EngineService,
    workspace: &Path,
    state: &mut Session,
    session_path: &Path,
    plan: &BundlePlan,
    mut template: Value,
    mut selected_bytes: u64,
    report: &mut Value,
    token: &CancellationToken,
    progress: crate::ProgressCallback<'_>,
) -> Result<Value, EngineError> {
    report["stage"] = json!("archive-inspection");
    report["archiveReadBytes"] = json!(0);
    record(state, session_path, "automaticTemplate", report.clone()).await?;
    let mut entries = 0_u64;
    let mut scripts = 0_u64;
    let mut sources = plan.loose_renpy;
    let mut compiled = plan.loose_compiled;
    let mut media = 0_u64;
    let mut read = 0_u64;
    let inspection = format!(
        "work/automatic-template-{}",
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
    );
    for (index, resource) in plan.archives.iter().enumerate() {
        EngineService::report(
            progress,
            "检查自动组合封包",
            format!("正在检查 {resource} 的全部成员及支持条件。"),
        );
        let output = format!("{inspection}/{index:02}");
        let evidence = format!("{output}/manifest.json");
        let inspected = service
            .run_component(
                workspace,
                state,
                &json!({
                    "id": ARCHIVE_RPA3_COMPONENT,
                    "operation": "inspect",
                    "archive": format!("resources/{resource}"),
                    "output": output,
                    "extensions": [".rpy", ".rpyc"],
                }),
                token,
            )
            .await?;
        report["evidenceFiles"]
            .as_array_mut()
            .unwrap()
            .push(json!(evidence));
        record(state, session_path, "automaticTemplate", report.clone()).await?;
        if inspected["success"] != true {
            return Err(reject(format!("{resource}：{}", inspected["error"])));
        }
        let manifest_path = resolve_relative(workspace, &evidence)?;
        if manifest_path.metadata()?.len() > MAX_ARCHIVE_MANIFEST_BYTES {
            return Err(reject("封包成员清单超过 8 MiB。"));
        }
        let manifest: Value = serde_json::from_slice(&fs::read(manifest_path)?)?;
        let members = manifest["entries"]
            .as_array()
            .ok_or_else(|| reject("封包清单未返回完整成员。"))?;
        entries += members.len() as u64;
        selected_bytes = selected_bytes
            .checked_add(
                manifest["selectedBytes"]
                    .as_u64()
                    .ok_or_else(|| reject("封包清单缺少脚本大小。"))?,
            )
            .ok_or_else(|| reject("封包脚本超出预算。"))?;
        read += manifest["readBytes"].as_u64().unwrap_or(0);
        if entries > MAX_ARCHIVE_ENTRIES || selected_bytes > MAX_TEMPLATE_TOTAL_BYTES {
            return Err(reject(
                "全部封包合计超过自动组合的 10,000 成员／256 MiB 脚本预算。",
            ));
        }
        let compiled_names: std::collections::BTreeSet<_> = members
            .iter()
            .filter_map(|member| member["name"].as_str())
            .filter(|name| extension(name) == "rpyc")
            .map(|name| name.replace('\\', "/").to_ascii_lowercase())
            .collect();
        let mut names = Vec::new();
        for member in members {
            let name = text(member, "name")?;
            let length = member["length"]
                .as_u64()
                .ok_or_else(|| reject("封包清单成员长度无效。"))?;
            if is_other_translation_script(name) {
                continue;
            }
            let normalized = name.replace('\\', "/").to_ascii_lowercase();
            match extension(name).as_str() {
                "rpyc" => {
                    if length > MAX_RPYC_SCRIPT_BYTES {
                        return Err(reject("单个 RPYC2 脚本超过 32 MiB 读取预算。"));
                    }
                    compiled += 1;
                }
                "rpy" => {
                    // Ren'Py skips archived .rpy when dir is None; ignore .rpy paired with .rpyc.
                    if compiled_names.contains(&format!("{normalized}c")) {
                        continue;
                    }
                    if length > MAX_RPY_SCRIPT_BYTES {
                        return Err(reject("单个 .rpy 脚本超过 64 MiB 读取预算。"));
                    }
                    sources += 1;
                }
                _ => {}
            }
            names.push(name);
        }
        supported(names, &template, &["rpyc"])?;
        let count = manifest["selectedCount"]
            .as_u64()
            .ok_or_else(|| reject("封包清单缺少脚本数量。"))?;
        scripts += count;
        if count == 0 {
            media += 1;
        }
        report["selectedBytes"] = json!(selected_bytes);
        report["archiveReadBytes"] = json!(read);
        report["archivedScripts"] = json!(scripts);
        report["mediaOnlyArchives"] = json!(media);
    }
    let id = if sources > 0 && compiled > 0 {
        RENPY_MIXED_TEMPLATE
    } else if scripts == 0 && plan.loose_renpy == 0 && plan.loose_compiled == 0 {
        return Err(reject(
            "全部资源中都没有模板支持的 .rpy 或 RPYC2 编译脚本；未生成或发布适配器。",
        ));
    } else if !plan.mixed && compiled > 0 {
        RPA3_RENPY_RPYC_TEMPLATE
    } else {
        text(&template, "id")?
    };
    template = crate::adaptation_assets::template_catalog()
        .into_iter()
        .find(|candidate| candidate["id"] == id)
        .unwrap();
    report["templateId"] = template["id"].clone();
    if service
        .disabled_families
        .contains(&format!("template.{}", text(&template, "id")?))
    {
        return Err(reject("模板已禁用，未自动组合。"));
    }
    Ok(template)
}
