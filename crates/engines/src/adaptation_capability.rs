//! Executable reuse contracts beside existing published packages; evidence is never portable acceptance.
use super::*;
use std::io::Read;

const FILE: &str = "capability-profile.json";
const CONTRACTS: [&str; 5] = [
    "stable-text-identity",
    "original-roundtrip",
    "partial-chinese-readback",
    "full-chinese-readback",
    "original-resource-integrity",
];

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Profile {
    schema_version: u32,
    verifier_revision: u32,
    package_fingerprint: String,
    adapter_id: String,
    adapter_version: String,
    executor: String,
    contracts: Vec<String>,
}

fn fingerprint(manifest: &PackageManifest) -> Result<String, EngineError> {
    Ok(hash_text(&serde_json::to_string(manifest)?))
}

fn recipe(manifest: &PackageManifest) -> Result<Value, EngineError> {
    if manifest.manifest_version != 4 || !manifest.package_files.contains_key("src/adapter.py") {
        return Err(EngineError::Rejected(
            "该发布包没有可直接复验的 Python 入口。".into(),
        ));
    }
    let kind = text(&manifest.family, "kind")?;
    let key = text(&manifest.family, "key")?;
    let name = text(&manifest.family, "name")?;
    validate_family(kind, key, name)?;
    if manifest.id != format!("{kind}.{key}") || manifest.resource_patterns.is_empty() {
        return Err(EngineError::Rejected(
            "适配器身份或资源条件不足，需明确当前游戏的验收参数。".into(),
        ));
    }
    for pattern in &manifest.resource_patterns {
        validate_pattern(pattern)?;
    }
    if let Some(recognition) = &manifest.recognition {
        recognition.validate()?;
    }
    Ok(
        json!({"kind":kind,"key":key,"name":name,"resourcePatterns":manifest.resource_patterns,"recognition":manifest.recognition}),
    )
}

pub(super) fn publish(
    directory: &Path,
    manifest: &PackageManifest,
    result: &VerificationResult,
) -> Result<(), EngineError> {
    if !result.passed
        || !result.original_roundtrip_passed
        || !result.issues.is_empty()
        || ![
            "partial-translation-re-extraction",
            "full-translation-re-extraction",
            "original-resource-check",
        ]
        .iter()
        .all(|stage| result.stage_timings_ms.contains_key(*stage))
        || recipe(manifest).is_err()
        || manifest.package_files.contains_key(FILE)
    {
        return Ok(());
    }
    let profile = Profile {
        schema_version: 1,
        verifier_revision: verification::VERIFIER_REVISION,
        package_fingerprint: fingerprint(manifest)?,
        adapter_id: manifest.id.clone(),
        adapter_version: manifest.version.clone(),
        executor: "host-full-resource-verifier".into(),
        contracts: CONTRACTS
            .iter()
            .map(|contract| (*contract).into())
            .collect(),
    };
    write_json(&resolve_relative(directory, FILE)?, &profile).map_err(file_error)
}

fn read(directory: &Path, manifest: &PackageManifest) -> Result<Profile, EngineError> {
    if manifest.package_files.contains_key(FILE) {
        return Err(EngineError::Rejected("能力资料路径属于适配器负载。".into()));
    }
    let file = fs::File::open(resolve_relative(directory, FILE)?)?;
    let mut content = Vec::new();
    file.take(128 * 1024 + 1).read_to_end(&mut content)?;
    if content.len() > 128 * 1024 {
        return Err(EngineError::Rejected("能力资料超出大小限制。".into()));
    }
    let profile: Profile = serde_json::from_slice(&content)?;
    if profile.schema_version != 1
        || profile.verifier_revision != verification::VERIFIER_REVISION
        || profile.adapter_id != manifest.id
        || profile.adapter_version != manifest.version
        || profile.package_fingerprint != fingerprint(manifest)?
        || profile.executor != "host-full-resource-verifier"
        || profile.contracts != CONTRACTS
    {
        return Err(EngineError::Rejected(
            "能力资料的包、契约或验收版本不匹配，继续使用原适配器流程。".into(),
        ));
    }
    recipe(manifest)?;
    Ok(profile)
}

pub(super) fn describe(directory: &Path, manifest: &PackageManifest) -> Value {
    match read(directory, manifest) {
        Ok(profile) => {
            json!({"status":"available","profile":profile,"conditions":{"resourcePatterns":manifest.resource_patterns,"recognition":manifest.recognition},"recipe":{"tool":"test_adapter","arguments":recipe(manifest).ok()},"currentGameAccepted":false,"runtimeTested":false,"limitation":"The recipe reuses executable package code and dependencies. Source-game evidence cannot accept another game's resources; staged current inputs and full verification remain required."})
        }
        Err(EngineError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            json!({"status":"not-recorded","currentGameAccepted":false})
        }
        Err(error) => {
            json!({"status":"unavailable","reason":error.to_string(),"currentGameAccepted":false})
        }
    }
}

pub(super) fn arguments(directory: &Path, manifest: &PackageManifest) -> Option<Value> {
    read(directory, manifest).ok()?;
    recipe(manifest).ok()
}

/// Derived from canonical host state, not a second database of model-authored facts.
pub(super) fn gaps(state: &Session, workspace: &Path) -> Value {
    let mut gaps = Vec::new();
    if state
        .extra
        .get("resourceHashes")
        .and_then(Value::as_object)
        .is_none_or(|hashes| hashes.is_empty())
    {
        gaps.push(json!({"capability":"verification-inputs","reason":"No registered current resources","resolution":"stage_resources with confirmed resource paths"}));
    }
    if !workspace.join("src/adapter.py").is_file() {
        gaps.push(json!({"capability":"adapter-implementation","reason":"No executable adapter entry","resolution":"reuse a compatible parser/template/package or implement the unknown layer"}));
    }
    if state.workflow.verification_args.is_none() && state.verified_package.is_none() {
        gaps.push(json!({"capability":"verification-recipe","reason":"Current family/resource conditions have not been supplied","resolution":"include testArguments when writing src/adapter.py, or call test_adapter once; the host continues known checks after edits"}));
    }
    let input = repair::input_key(state);
    for ticket in state
        .repairs
        .iter()
        .rev()
        .filter(|ticket| {
            ticket.input_fingerprint == input && ticket.status != repair::Status::Passed
        })
        .take(8)
    {
        gaps.push(json!({"capability":"verified-roundtrip","ticketId":ticket.id,"contract":ticket.issue.code,"stage":ticket.issue.stage,"resolution":"repair the failing contract or provide new discriminating evidence"}));
    }
    if state.verified_package.is_some() {
        gaps.push(json!({"capability":"publication-decision","resolution":"finish when the current objective is complete; disclose remaining coverage and runtime checks"}));
    } else if gaps.is_empty() {
        gaps.push(json!({"capability":"full-resource-verification","resolution":"host executes the saved recipe; otherwise use test_adapter"}));
    }
    json!({"gaps":gaps,"runtimeChecks":coverage_list(state,"runtimeChecks"),"runtimeChecksBlockResourceAcceptance":false})
}

#[cfg(test)]
mod tests {
    use super::*;
    fn manifest() -> PackageManifest {
        serde_json::from_value(json!({"id":"engine.fixture","version":"1","manifestVersion":4,"family":{"kind":"engine","key":"fixture","name":"Fixture"},"resourcePatterns":["*.json"],"packageFiles":{"src/adapter.py":"source-hash"}})).unwrap()
    }
    fn verified() -> VerificationResult {
        VerificationResult {
            passed: true,
            original_roundtrip_passed: true,
            stage_timings_ms: BTreeMap::from([
                ("partial-translation-re-extraction".into(), 0),
                ("full-translation-re-extraction".into(), 0),
                ("original-resource-check".into(), 0),
            ]),
            ..VerificationResult::default()
        }
    }
    #[test]
    fn profile_binds_executable_contract_and_conditions_but_never_accepts_current_game() {
        let root = tempfile::tempdir().unwrap();
        let mut manifest = manifest();
        publish(root.path(), &manifest, &VerificationResult::default()).unwrap();
        assert!(arguments(root.path(), &manifest).is_none());
        publish(root.path(), &manifest, &verified()).unwrap();
        let first = fs::read(root.path().join(FILE)).unwrap();
        assert_eq!(arguments(root.path(), &manifest).unwrap()["key"], "fixture");
        assert_eq!(
            describe(root.path(), &manifest)["currentGameAccepted"],
            false
        );
        publish(root.path(), &manifest, &verified()).unwrap();
        assert_eq!(fs::read(root.path().join(FILE)).unwrap(), first);
        manifest.resource_patterns = vec!["*.other".into()];
        assert!(arguments(root.path(), &manifest).is_none());
        assert_eq!(describe(root.path(), &manifest)["status"], "unavailable");
    }

    #[test]
    fn unknown_verifier_contracts_and_malformed_profiles_fall_back_to_existing_flow() {
        let root = tempfile::tempdir().unwrap();
        let manifest = manifest();
        publish(root.path(), &manifest, &verified()).unwrap();
        let path = root.path().join(FILE);
        let mut profile: Value = read_json(&path).unwrap();
        profile["verifierRevision"] = json!(0);
        write_json(&path, &profile).unwrap();
        assert!(arguments(root.path(), &manifest).is_none());
        fs::write(path, b"malformed").unwrap();
        assert!(arguments(root.path(), &manifest).is_none());
    }
}
