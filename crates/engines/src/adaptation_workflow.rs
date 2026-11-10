//! Deterministic continuation of known verification work; unknown repairs stay with the model.
use super::*;

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, rename_all = "camelCase")]
pub(crate) struct Workflow {
    pub verification_args: Option<Value>,
    pub needs_verification: bool,
    pub last_attempt: Option<String>,
    pub running: Option<String>,
    pub evidence_revision: u64,
    pub observations: BTreeSet<String>,
    #[serde(skip)]
    pub last_decision: Option<String>,
}

pub(super) struct Step {
    pub arguments: Value,
    pub fingerprint: String,
}

fn key(state: &Session, workspace: &Path, args: &Value) -> Result<String, EngineError> {
    Ok(hash_text(&json!({"implementation":workspace_fingerprint(workspace,state)?,"inputs":repair::input_key(state),"arguments":args}).to_string()))
}

pub(super) fn observe(
    state: &mut Session,
    workspace: &Path,
    name: &str,
    args: &Value,
    result: &Value,
) {
    let success = history::successful(result);
    if name == "compose_adapter_template" && success && result["testArguments"].is_object() {
        state.workflow.verification_args = Some(result["testArguments"].clone());
        state.workflow.needs_verification = true;
    } else if name == "test_adapter" && state.family["key"].is_string() {
        // Use host state after argument validation; never replay an unvalidated release selection.
        let mut normalized = json!({"kind":state.family["kind"],"key":state.family["key"],"name":state.family["name"],"resourcePatterns":state.resource_patterns});
        for property in ["trialFiles", "discoveryOnly"] {
            if let Some(value) = args.get(property) {
                normalized[property] = value.clone();
            }
        }
        state.workflow.last_attempt = key(state, workspace, &normalized).ok();
        if result["passed"] == true
            && (args["discoveryOnly"] == true || args["trialFiles"].is_array())
        {
            if let Some(args) = normalized.as_object_mut() {
                args.remove("trialFiles");
                args.remove("discoveryOnly");
            }
            state.workflow.needs_verification = true;
        } else {
            state.workflow.needs_verification = false;
        }
        state.workflow.verification_args = Some(normalized);
    } else if success
        && result["changed"] == true
        && matches!(name, "write_file" | "replace_text" | "remove_file")
        && args["path"]
            .as_str()
            .is_some_and(|path| path.replace('\\', "/").starts_with("src/"))
    {
        state.workflow.needs_verification = true;
    }

    if matches!(
        name,
        "report_progress" | "checkpoint" | "load_tool_groups" | "finish"
    ) {
        return;
    }
    if matches!(
        name,
        "write_file" | "replace_text" | "remove_file" | "update_release_files"
    ) {
        return;
    }
    let evidence = if name == "test_adapter" {
        json!({"passed":result["passed"],"stage":result["stage"],"issues":result["issues"],"error":result["error"],"diagnostics":result["diagnostics"]})
    } else {
        result.clone()
    };
    let signature =
        hash_text(&json!({"tool":name,"arguments":args,"evidence":evidence}).to_string());
    if state.workflow.observations.insert(signature) {
        state.workflow.evidence_revision = state.workflow.evidence_revision.saturating_add(1);
    }
    // The guard is deliberately conservative when its bounded observation set fills up.
    if state.workflow.observations.len() > 512 {
        state.workflow.observations.clear();
    }
}

pub(super) fn next(state: &mut Session, workspace: &Path) -> Result<Option<Step>, EngineError> {
    if !state.workflow.needs_verification
        || state.workflow.running.is_some()
        || !state.pending_calls.is_empty()
    {
        return Ok(None);
    }
    let Some(arguments) = state.workflow.verification_args.clone() else {
        return Ok(None);
    };
    if state.release_files.is_empty()
        || state
            .extra
            .get("resourceHashes")
            .and_then(Value::as_object)
            .is_none_or(|hashes| hashes.is_empty())
        || state.release_files.iter().any(|file| {
            !resolve_relative(workspace, &workspace_file(&file.path))
                .is_ok_and(|file| file.is_file())
        })
    {
        return Ok(None);
    }
    let fingerprint = key(state, workspace, &arguments)?;
    state.workflow.needs_verification = false;
    if state.workflow.last_attempt.as_ref() == Some(&fingerprint) {
        return Ok(None);
    }
    Ok(Some(Step {
        arguments,
        fingerprint,
    }))
}

pub(super) fn decision_key(
    state: &Session,
    workspace: &Path,
) -> Result<Option<String>, EngineError> {
    let input = repair::input_key(state);
    let tickets: Vec<_> = state
        .repairs
        .iter()
        .filter(|ticket| {
            ticket.input_fingerprint == input && ticket.status != repair::Status::Passed
        })
        .map(|ticket| json!({"id":ticket.id,"evidence":ticket.evidence,"approach":ticket.approach}))
        .collect();
    if tickets.is_empty() {
        return Ok(None);
    }
    Ok(Some(hash_text(&json!({"tickets":tickets,"inputs":input,"implementation":workspace_fingerprint(workspace,state)?,"evidenceRevision":state.workflow.evidence_revision,"instruction":state.user_instruction}).to_string())))
}

pub(super) fn recover(state: &mut Session) {
    if let Some(interrupted) = state.workflow.running.take() {
        state.workflow.last_attempt = Some(interrupted);
        state.workflow.needs_verification = false;
        state.messages.push(json!({"role":"user","content":"宿主自动验收被中断，结果未知，未取得通过资格。请检查已保存证据后决定是否重新验收。"}));
        repair::patched(state);
        state.verified_package = None;
        state.verified_fingerprint = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture(root: &Path) -> Session {
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/adapter.py"), "# writer").unwrap();
        let mut state = Session {
            family: json!({"kind":"engine","key":"fixture","name":"Fixture"}),
            resource_patterns: vec!["*.json".into()],
            ..Session::default()
        };
        state.release_files.push(ReleaseFile {
            path: "src/adapter.py".into(),
            role: "source".into(),
            purpose: "adapter".into(),
        });
        state
            .extra
            .insert("resourceHashes".into(), json!({"text.json":"input"}));
        state
    }

    #[test]
    fn trial_success_promotes_to_full_and_same_failed_version_is_not_repeated() {
        let root = tempfile::tempdir().unwrap();
        let mut state = fixture(root.path());
        let args = json!({"trialFiles":["text.json"]});
        observe(
            &mut state,
            root.path(),
            "test_adapter",
            &args,
            &json!({"passed":true,"stage":"trial"}),
        );
        let step = next(&mut state, root.path()).unwrap().unwrap();
        assert!(step.arguments.get("trialFiles").is_none());
        observe(
            &mut state,
            root.path(),
            "test_adapter",
            &step.arguments,
            &json!({"passed":false,"issues":[],"diagnostics":["failure"]}),
        );
        state.workflow.needs_verification = true;
        assert!(next(&mut state, root.path()).unwrap().is_none());
        fs::write(root.path().join("src/adapter.py"), "# repaired writer").unwrap();
        observe(
            &mut state,
            root.path(),
            "replace_text",
            &json!({"path":"src/adapter.py"}),
            &json!({"success":true,"changed":true}),
        );
        assert!(next(&mut state, root.path()).unwrap().is_some());
    }

    #[test]
    fn interruption_does_not_replay_or_grant_acceptance() {
        let root = tempfile::tempdir().unwrap();
        let mut state = fixture(root.path());
        state.workflow.running = Some("uncertain".into());
        state.workflow.needs_verification = true;
        recover(&mut state);
        assert!(!state.workflow.needs_verification);
        assert!(state.verified_package.is_none());
        assert!(
            state.messages[0]["content"]
                .as_str()
                .unwrap()
                .contains("结果未知")
        );
    }
}
