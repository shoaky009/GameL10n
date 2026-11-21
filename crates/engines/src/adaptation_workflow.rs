//! Deterministic continuation of known verification work; unknown repairs stay with the model.
use super::*;

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, rename_all = "camelCase")]
pub(crate) struct Workflow {
    pub verification_args: Option<Value>,
    pub needs_verification: bool,
    pub last_attempt: Option<String>,
    pub running: Option<String>,
}

pub(super) struct Step {
    pub arguments: Value,
    pub fingerprint: String,
}

fn key(state: &Session, workspace: &Path, args: &Value) -> Result<String, EngineError> {
    Ok(hash_text(&json!({"implementation":workspace_fingerprint(workspace,state)?,"inputs":repair::input_key(state),"arguments":args,"coverageSamples":state.text_coverage["samples"]}).to_string()))
}

pub(super) fn register(state: &mut Session, arguments: Value) {
    if state.workflow.verification_args.as_ref() != Some(&arguments) {
        state.workflow.verification_args = Some(arguments);
        state.workflow.needs_verification = true;
        state.verified_package = None;
        state.verified_fingerprint = None;
    }
}

pub(super) fn observe(
    state: &mut Session,
    workspace: &Path,
    name: &str,
    args: &Value,
    result: &Value,
) {
    let success = history::successful(result);
    if matches!(name, "compose_adapter_template" | "copy_adapter")
        && success
        && result["testArguments"].is_object()
    {
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
        // Source edits alone do not establish which of the staged files hold the main text.
        let needs_scope_confirmation = result["coveragePending"] == true
            && state.text_coverage["samples"]
                .as_array()
                .is_none_or(Vec::is_empty)
            && state.extra["resourceHashes"]
                .as_object()
                .is_some_and(|staged| {
                    staged.keys().any(|path| {
                        !state
                            .resource_patterns
                            .iter()
                            .any(|pattern| crate::discovery::glob_match(pattern, path))
                    })
                });
        state.workflow.verification_args = (!needs_scope_confirmation).then_some(normalized);
    } else if success
        && result["changed"] == true
        && matches!(name, "write_file" | "replace_text" | "remove_file")
        && args["path"]
            .as_str()
            .is_some_and(|path| path.replace('\\', "/").starts_with("src/"))
    {
        state.workflow.needs_verification = true;
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
        || !workspace.join("src/adapter.py").is_file()
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

pub(super) fn recover(state: &mut Session) {
    if state.verified_package.is_some()
        && state
            .extra
            .get("verificationCache")
            .is_some_and(|cache| cache["revision"] != verification::VERIFIER_REVISION)
    {
        state.verified_package = None;
        state.verified_fingerprint = None;
        state.workflow.last_attempt = None;
        state.workflow.needs_verification = state.workflow.verification_args.is_some();
    }
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
    fn coverage_review_keeps_rechecks_when_all_staged_resources_are_selected() {
        let root = tempfile::tempdir().unwrap();
        let mut state = fixture(root.path());
        observe(
            &mut state,
            root.path(),
            "test_adapter",
            &json!({"resourcePatterns":["*.json"]}),
            &json!({"passed":true,"stage":"finish-ready","coveragePending":true}),
        );
        fs::write(root.path().join("src/adapter.py"), "# fixed parser").unwrap();
        observe(
            &mut state,
            root.path(),
            "write_file",
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
