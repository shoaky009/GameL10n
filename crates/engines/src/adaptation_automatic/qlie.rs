use super::{Budget, extension, limit};
use crate::adaptation::{AdaptationRequest, Session};
use crate::discovery::Inventory;
use crate::{CancellationToken, EngineError, EngineService};
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::Path, time::Duration};

pub(super) const FORMAT: &str = "Qlie";

pub(super) async fn inspect(
    service: &EngineService,
    request: &AdaptationRequest<'_>,
    inventory: &Inventory,
    workspace: &Path,
    state: &Session,
    cancellation: &CancellationToken,
) -> Result<Option<Value>, EngineError> {
    if service
        .disabled_families
        .contains("engine.vntextpatch-qlie")
    {
        return Ok(None);
    }
    let files = || {
        inventory
            .files
            .iter()
            .map(|file| file.path.as_str())
            .filter(|path| !request.inputs.excludes(path))
    };
    let mut keys = files().filter(|path| path.eq_ignore_ascii_case("DLL/key.fkey"));
    let Some(key_file) = keys.next() else {
        return Ok(None);
    };
    if keys.next().is_some() {
        return Ok(None);
    }
    // Only combine companions and volumes relative to the same launch directory.
    let archives: Vec<_> = files()
        .filter(|path| {
            path.rsplit_once('/').is_some_and(|(parent, file)| {
                parent.eq_ignore_ascii_case("GameData") && extension(file) == "pack"
            })
        })
        .take(17)
        .collect();
    let executables: Vec<_> = files()
        .filter(|path| !path.contains('/') && extension(path) == "exe")
        .take(9)
        .collect();
    if !(1..=16).contains(&archives.len()) || !(1..=8).contains(&executables.len()) {
        return Ok(None);
    }

    let budget = Budget::new(cancellation, Duration::from_secs(20));
    let token = &budget.token;
    let result = async {
        let resolved = archives
            .iter()
            .chain(&executables)
            .copied()
            .chain([key_file])
            .map(|name| Ok((name.to_owned(), request.inputs.file(&request.game_directory, name)?)))
            .collect::<Result<BTreeMap<_, _>, EngineError>>()?;
        let _inputs = crate::source::lock_inputs(&resolved, token)?;
        let reference = service.adaptation_reference(workspace, state)?;
        let library = service.reference_library(&reference)?;
        let mut report = service
            .helper(
                &json!({"operation":"archive_samples","format":FORMAT,"files":resolved,
                    "archives":archives,"executables":executables,"keyFile":key_file,"library":library,
                    "components":service.asset_root.join("adapters/components"),"timeoutSeconds":10}),
                token,
            )
            .await?;
        report["libraryRevision"] = reference["revision"].clone();
        Ok::<_, EngineError>(report)
    }
    .await;
    let mut report = match result {
        Ok(report) => report,
        Err(EngineError::Cancelled) if cancellation.is_cancelled() => {
            return Err(EngineError::Cancelled);
        }
        Err(error @ EngineError::Isolation(_)) => return Err(error),
        Err(EngineError::Cancelled | EngineError::Timeout) => json!({"status":"timed-out"}),
        Err(error) => json!({"status":"unavailable","error":limit(&error.to_string(),240)}),
    };
    report["format"] = json!(FORMAT);
    report["runtimeStatus"] = json!("not-tested");
    Ok(Some(report))
}
