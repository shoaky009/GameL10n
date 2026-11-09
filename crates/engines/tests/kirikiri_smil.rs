use std::{fs, path::Path, process::Command};

use gamel10n_engines::{
    AdapterHandle, CancellationToken, EngineService, ToolPaths, discovery::Inventory,
};
use gamel10n_translation::files::{hash_file, write_json};
use serde_json::{Value, json};

fn service(shared: &Path) -> EngineService {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let pin: Value = serde_json::from_str(include_str!(
        "../../../third_party/engine-tools/current.json"
    ))
    .unwrap();
    let relative = format!(
        "references/{}/library.zip",
        pin["revision"].as_str().unwrap()
    );
    let library = shared.join(&relative);
    fs::create_dir_all(library.parent().unwrap()).unwrap();
    fs::copy(
        root.join("third_party/engine-tools").join(relative),
        library,
    )
    .unwrap();
    EngineService::new(root, ToolPaths::development()).with_shared_directory(shared)
}

#[tokio::test]
async fn smil_discovery_preserves_preferred_adapters_and_verifies_unicode_rebuilds() {
    let root = tempfile::tempdir().unwrap();
    let service = service(&root.path().join("shared"));
    if !service.tools.python().is_file() {
        return;
    }
    let source = root.path().join("source");
    let status = Command::new(service.tools.python())
        .args(["-I", "-X", "utf8"])
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("python/test_xp3_smil.py"))
        .arg("--fixture-directory")
        .arg(&source)
        .output()
        .unwrap();
    assert!(
        status.status.success(),
        "{}",
        String::from_utf8_lossy(&status.stderr)
    );
    let original_hash = hash_file(&source.join("data.xp3")).unwrap();
    let package = root.path().join("legacy");
    fs::create_dir_all(package.join("src")).unwrap();
    let entry = package.join("src/adapter.py");
    fs::write(&entry, r#"import json,sys
r=json.load(sys.stdin)
print(json.dumps(dict(protocolVersion=1,success=True,probe=dict(engineId='fixture',adapterId='engine.legacy',adapterVersion='1',description='fixture',requiredFiles=['data.xp3'],warnings=[]))))
"#).unwrap();
    write_json(&package.join("package.json"), &json!({
        "manifestVersion":4,"protocolVersion":1,"id":"engine.legacy","version":"1",
        "resourceFiles":["data.xp3"],"packageFiles":{"src/adapter.py":hash_file(&entry).unwrap()},
    })).unwrap();
    let inventory = Inventory::scan(&source).unwrap();
    let token = CancellationToken::new();
    let discovery = service
        .discover_with_inventory(
            &source,
            std::slice::from_ref(&package),
            &inventory,
            &token,
            None,
        )
        .await
        .unwrap();
    assert!(matches!(
        discovery.adapter,
        Some(AdapterHandle::BuiltinKirikiriSmil)
    ));
    let probe = discovery.probe.unwrap();
    assert_eq!(probe.required_files, ["data.xp3"]);
    let result = service
        .verify_probed(
            &AdapterHandle::BuiltinKirikiriSmil,
            &source,
            &probe,
            &root.path().join("verification"),
            &[],
            &token,
            None,
        )
        .await
        .unwrap();
    assert!(result.passed, "{:?}", result.diagnostics);
    assert!(result.original_roundtrip_passed);
    assert_eq!(result.units, 6);
    assert!(!result.runtime_tested);
    assert_eq!(hash_file(&source.join("data.xp3")).unwrap(), original_hash);
    let preferred = service
        .clone()
        .with_preferred_adapter(Some("engine.legacy".into()))
        .discover_with_inventory(
            &source,
            std::slice::from_ref(&package),
            &inventory,
            &token,
            None,
        )
        .await
        .unwrap();
    assert_eq!(preferred.adapter.unwrap().id(), "engine.legacy");
    let missing_preferred = service
        .clone()
        .with_preferred_adapter(Some("engine.legacy".into()))
        .discover_with_inventory(&source, &[], &inventory, &token, None)
        .await
        .unwrap();
    assert!(missing_preferred.adapter.is_none());
    let disabled = service
        .clone()
        .with_disabled_families(["engine.kirikiri-smil-smile".into()])
        .discover_with_inventory(
            &source,
            std::slice::from_ref(&package),
            &inventory,
            &token,
            None,
        )
        .await
        .unwrap();
    assert_eq!(disabled.adapter.unwrap().id(), "engine.legacy");
}

#[tokio::test]
#[ignore = "Requires GAMEL10N_SMIL_SAMPLE pointing to a compatible XP3 archive."]
async fn local_smil_sample_passes_resource_verification() {
    let path = std::env::var_os("GAMEL10N_SMIL_SAMPLE").expect("GAMEL10N_SMIL_SAMPLE");
    let path = Path::new(&path).canonicalize().unwrap();
    let source = path.parent().unwrap();
    let resource = path.file_name().unwrap().to_str().unwrap().to_owned();
    let root = tempfile::tempdir().unwrap();
    let service = service(&root.path().join("shared"));
    let token = CancellationToken::new();
    let probe = service
        .probe_bounded(
            &AdapterHandle::BuiltinKirikiriSmil,
            source,
            &[resource],
            &token,
        )
        .await
        .unwrap()
        .expect("compatible SMIL sample");
    let report =
        |event: gamel10n_engines::EngineProgress| eprintln!("{}: {}", event.stage, event.message);
    let result = service
        .verify_probed(
            &AdapterHandle::BuiltinKirikiriSmil,
            source,
            &probe,
            &root.path().join("verification"),
            &[],
            &token,
            Some(&report),
        )
        .await
        .unwrap();
    eprintln!("{}", serde_json::to_string(&result).unwrap());
    assert!(result.passed, "{:?}", result.diagnostics);
    assert!(result.original_roundtrip_passed);
    assert!(result.units > 0);
    assert!(!result.runtime_tested);
}
