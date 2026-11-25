use super::{Budget, extension, limit, preparation_skip};
use crate::adaptation::{AdaptationRequest, Session};
use crate::adaptation_assets::{QLIE_PACK_TEMPLATE, QLIE_PACK31_TEMPLATE};
use crate::adaptation_context::filepack_version;
use crate::discovery::Inventory;
use crate::{CancellationToken, EngineError, EngineService};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{Read, Seek, SeekFrom},
    path::Path,
    time::Duration,
};

pub(super) const FORMAT: &str = "Qlie";
const KEY_FILE: &str = "DLL/key.fkey";

fn record_trial_skip(state: &mut Session, code: &str, mut details: Value) {
    if state
        .extra
        .get("builtinDiscovery")
        .and_then(|discovery| discovery["candidates"].as_array())
        .is_some_and(|candidates| {
            candidates
                .iter()
                .any(|candidate| candidate["format"] == FORMAT)
        })
    {
        details["format"] = json!(FORMAT);
        state
            .preparation_skips
            .push(preparation_skip("archive-trial", code, details));
    }
}

fn version(path: &Path) -> Result<Option<String>, EngineError> {
    let mut file = File::open(path)?;
    if file.metadata()?.len() < 28 {
        return Ok(None);
    }
    file.seek(SeekFrom::End(-28))?;
    let mut footer = [0; 28];
    file.read_exact(&mut footer)?;
    Ok(filepack_version(&footer).map(str::to_owned))
}

fn probe_revision(components: &Path) -> Result<String, EngineError> {
    let mut hash = Sha256::new();
    for name in [
        "engine_archive.py",
        "qlie_archive.py",
        "qlie_pack31.py",
        "qlie_samples.py",
        "qlie_script.py",
    ] {
        let data = fs::read(components.join(name))?;
        hash.update(Sha256::digest(data));
    }
    Ok(format!("{:x}", hash.finalize()))
}

pub(super) struct AncillaryResources {
    pub(super) remaining: Vec<String>,
    pub(super) identical_copies: Vec<String>,
    pub(super) uninstall_archive: Option<String>,
}

pub(super) fn partition_ancillary<'a>(
    game: &Path,
    candidates: impl IntoIterator<Item = &'a String>,
    volumes: &[String],
    paths: &BTreeMap<String, u64>,
    cancellation: &CancellationToken,
) -> Result<AncillaryResources, EngineError> {
    let mut remaining = Vec::new();
    let mut identical_copies = Vec::new();
    let mut uninstall_archive = None;
    for path in candidates {
        if uninstall_companion(path) {
            if extension(path) == "pack" {
                uninstall_archive = Some(path.clone());
            }
        } else if identical_volume_copy(game, path, volumes, paths, cancellation)? {
            identical_copies.push(path.clone());
        } else {
            remaining.push(path.clone());
        }
    }
    Ok(AncillaryResources {
        remaining,
        identical_copies,
        uninstall_archive,
    })
}

fn uninstall_companion(path: &str) -> bool {
    path.eq_ignore_ascii_case("UnInstallData.pack") || path.eq_ignore_ascii_case("UnInstall.exe")
}

fn identical_volume_copy(
    game: &Path,
    relative: &str,
    volumes: &[String],
    paths: &BTreeMap<String, u64>,
    cancellation: &CancellationToken,
) -> Result<bool, EngineError> {
    let Some((_, name)) = relative.rsplit_once('/') else {
        return Ok(false);
    };
    if extension(name) != "pack" {
        return Ok(false);
    }
    let Some(installed) = volumes.iter().find(|path| {
        path.rsplit_once('/')
            .is_some_and(|(_, basename)| basename.eq_ignore_ascii_case(name))
    }) else {
        return Ok(false);
    };
    let length = paths[relative];
    if paths[installed] != length {
        return Ok(false);
    }
    let installed = crate::discovery::resolve_relative(game, installed)?;
    let copy = crate::discovery::resolve_relative(game, relative)?;
    Ok(super::checked_hash(&installed, length, cancellation)?
        == super::checked_hash(&copy, length, cancellation)?)
}

pub(super) async fn inspect(
    service: &EngineService,
    request: &AdaptationRequest<'_>,
    inventory: &Inventory,
    workspace: &Path,
    state: &mut Session,
    cancellation: &CancellationToken,
) -> Result<Option<Value>, EngineError> {
    if service
        .disabled_families
        .contains("engine.vntextpatch-qlie")
    {
        record_trial_skip(
            state,
            "family-disabled",
            json!({"adapterId":"engine.vntextpatch-qlie"}),
        );
        return Ok(None);
    }
    let files = || {
        inventory
            .files
            .iter()
            .map(|file| file.path.as_str())
            .filter(|path| !request.inputs.excludes(path))
    };
    // Only combine companions and volumes relative to the same launch directory.
    let archives: Vec<_> = files()
        .filter(|path| {
            path.rsplit_once('/').is_some_and(|(parent, file)| {
                parent.eq_ignore_ascii_case("GameData") && extension(file) == "pack"
            })
        })
        .take(17)
        .collect();
    if !(1..=16).contains(&archives.len()) {
        record_trial_skip(
            state,
            "launch-layout-out-of-range",
            json!({"archiveCandidates":archives.len(),"archiveLimit":16}),
        );
        return Ok(None);
    }

    let budget = Budget::new(cancellation, Duration::from_secs(20));
    let token = &budget.token;
    let mut template_id = None;
    let result = async {
        let resolve = |name: &str| {
            Ok((name.to_owned(), request.inputs.file(&request.game_directory, name)?))
        };
        let mut resolved = archives
            .iter()
            .copied()
            .map(resolve)
            .collect::<Result<BTreeMap<_, _>, EngineError>>()?;
        let _archives = crate::source::lock_inputs(&resolved, token)?;
        let versions: Vec<_> = archives
            .iter()
            .map(|path| version(&resolved[*path]))
            .collect::<Result<_, EngineError>>()?;
        if versions.iter().all(Option::is_none) {
            return Ok(None);
        }
        let expected = versions[0].as_deref();
        let same_version = versions.iter().all(|version| version.as_deref() == expected);
        template_id = match (same_version, expected) {
            (true, Some("3.0")) => Some(QLIE_PACK_TEMPLATE),
            (true, Some("3.1")) => Some(QLIE_PACK31_TEMPLATE),
            _ => None,
        };
        if template_id.is_none() {
            let volumes: Vec<_> = archives.iter().zip(versions).map(|(archive, version)| {
                json!({"archive":archive,"version":version})
            }).collect();
            return Ok(Some(json!({"status":"unsupported-layout","stage":"index","volumes":volumes,
                "error":"当前启动目录的 FilePack 版本没有匹配的索引布局"})));
        }
        let mut executables = Vec::new();
        let mut key_file = None;
        let mut companions = BTreeMap::new();
        if template_id == Some(QLIE_PACK_TEMPLATE) {
            let keys: Vec<_> = files().filter(|path| path.eq_ignore_ascii_case(KEY_FILE)).take(2).collect();
            if keys.len() != 1 {
                let code = if keys.len() == 2 {
                    "ambiguous-key-file"
                } else if request.inputs.excludes(KEY_FILE) {
                    "key-file-excluded"
                } else {
                    "missing-key-file"
                };
                record_trial_skip(state, code, json!({"requiredFile":KEY_FILE,"version":"3.0"}));
                return Ok(None);
            }
            executables = files().filter(|path| !path.contains('/') && extension(path) == "exe")
                .take(9).collect();
            if !(1..=8).contains(&executables.len()) {
                record_trial_skip(state, "launch-layout-out-of-range",
                    json!({"version":"3.0","executableCandidates":executables.len(),"executableLimit":8}));
                return Ok(None);
            }
            key_file = Some(keys[0]);
            companions = executables.iter().copied().chain(key_file)
                .map(resolve).collect::<Result<_, EngineError>>()?;
        }
        let _companions = crate::source::lock_inputs(&companions, token)?;
        resolved.extend(companions);
        let reference = service.adaptation_reference(workspace, state)?;
        let library = service.reference_library(&reference)?;
        let components = service.asset_root.join("adapters/components");
        let revision = probe_revision(&components)?;
        let previous = state.extra.get("automaticArchiveTrials").and_then(Value::as_array)
            .into_iter().flatten().find(|report| report["format"] == FORMAT
                && report["status"] == "sampled"
                && report["probeRevision"] == revision
                && report["libraryRevision"] == reference["revision"]);
        let mut report = service
            .helper(
                &json!({"operation":"archive_samples","format":FORMAT,"files":resolved,
                    "archives":archives,"executables":executables,"keyFile":key_file,"library":library,
                    "components":components,"previous":previous,"timeoutSeconds":10}),
                token,
            )
            .await?;
        report["libraryRevision"] = reference["revision"].clone();
        report["probeRevision"] = json!(revision);
        Ok::<_, EngineError>(Some(report))
    }
    .await;
    let mut report = match result {
        Ok(Some(report)) => report,
        Ok(None) => return Ok(None),
        Err(EngineError::Cancelled) if cancellation.is_cancelled() => {
            return Err(EngineError::Cancelled);
        }
        Err(error @ EngineError::Isolation(_)) => return Err(error),
        Err(EngineError::Cancelled | EngineError::Timeout) => json!({"status":"timed-out"}),
        Err(error) => json!({"status":"unavailable","error":limit(&error.to_string(),240)}),
    };
    report["format"] = json!(FORMAT);
    report["runtimeStatus"] = json!("not-tested");
    if let Some(template_id) = template_id.filter(|_| report["status"] == "sampled") {
        if service
            .disabled_families
            .contains(&format!("template.{template_id}"))
        {
            if let Some(report) = report.as_object_mut() {
                report.remove("templateId");
            }
            state.preparation_skips.push(preparation_skip(
                "template-selection",
                "template-disabled",
                json!({"format":FORMAT,"templateId":template_id}),
            ));
        } else {
            report["templateId"] = json!(template_id);
            report["nextAction"] = json!(format!(
                "复用 {template_id} 和本地验收配方，核对尚未确认的文字覆盖及游戏显示"
            ));
        }
    }
    Ok(Some(report))
}
