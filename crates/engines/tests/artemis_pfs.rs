use std::{collections::BTreeMap, fs, process::Command};

use gamel10n_engines::{
    AdaptationRequest, AdaptationStatus, CancellationToken, EngineService, ToolPaths,
};
use gamel10n_translation::{
    files::{hash_file, read_json},
    provider::{OpenAiProvider, ProviderSettings},
};
use serde_json::Value;

const GAME: &str = r#"
import sys
from pathlib import Path
sys.path.insert(0, sys.argv[1])
from python.archives import pfs
root = Path(sys.argv[2])
block = '''  block_00000 = {
    text = { ja = { { name = {"名前"}, "原文",
      -- keep this comment in the message table
      {"rt2"}, "続き", {"rt2"}, },
    }, },
    select = { ja = { "選択" }, },
  },
'''
script = ('astver = 2.0\nast = {\n' +
          ''.join(block.replace("block_00000", f"block_{i:05}") for i in range(360)) +
          '''
  label = { top = { block = "block_00000", label = 1 }, },
}
''').encode("utf-8-sig")
members = [("script\\story.ast", script), ("voice.bin", b"voice")]
(root / "story.pfs").write_bytes(pfs.build(members, trailing=b"\0" * 70000, index_order=[1, 0]))
(root / "story.pfs.000").write_bytes(pfs.build([("picture.bin", b"image")]))
"#;

#[tokio::test]
async fn automatic_recognition_verifies_pfs_replacement_before_model_calls() {
    let tools = ToolPaths::development();
    if !tools.python().is_file() {
        return;
    }
    let root = tempfile::tempdir().expect("fixture");
    let game = root.path().join("game");
    let project = root.path().join("project");
    let extensions = root.path().join("extensions");
    fs::create_dir(&game).expect("game");
    let references = tools.asset_root().join("third_party/engine-tools");
    let pin: Value = read_json(&references.join("current.json")).expect("tool pin");
    let library = references
        .join("references")
        .join(pin["revision"].as_str().expect("revision"))
        .join("library.zip");
    let created = Command::new(tools.python())
        .args(["-I", "-X", "utf8", "-c", GAME])
        .arg(library)
        .arg(&game)
        .output()
        .expect("fixture builder");
    assert!(created.status.success(), "{:?}", created);
    let originals = [
        hash_file(&game.join("story.pfs")).unwrap(),
        hash_file(&game.join("story.pfs.000")).unwrap(),
    ];
    let service = EngineService::new(tools.asset_root(), tools.clone())
        .with_shared_directory(extensions.join("engine-tools"));
    let provider = OpenAiProvider::new(ProviderSettings {
        endpoint: "http://127.0.0.1:1/v1".into(),
        api_key: "local-test".into(),
        model: "fixture".into(),
        protocol: Default::default(),
        translation_format: Default::default(),
        analysis_reasoning_effort: None,
    })
    .expect("provider");
    let request = AdaptationRequest {
        game_id: "artemis-fixture".into(),
        game_directory: game.clone(),
        inputs: Default::default(),
        project_directory: project.clone(),
        package_directory: extensions.clone(),
        package_directories: vec![],
        reference_package_directories: vec![],
        user_instruction: String::new(),
        objective: "develop".into(),
        improve_existing: false,
        require_model_analysis: false,
        resume_session: false,
        reuse_failures: vec![],
        builtin_probes: vec![],
        discovery_inventory: None,
        limits: Default::default(),
        baseline_units: &[],
    };
    let result = service
        .adapt(&request, &provider, &CancellationToken::new(), None)
        .await
        .expect("adaptation");
    assert_eq!(result.status, AdaptationStatus::Activated, "{result:?}");
    assert_eq!(result.api_requests.unwrap_or(0), 0);
    assert_eq!(result.usage.input_tokens + result.usage.output_tokens, 0);
    assert!(!result.pending_runtime_checks.is_empty());
    let extraction = result
        .verified_extraction
        .expect("verified extraction")
        .into_matching_extraction(
            &service,
            result.adapter.as_ref().expect("published adapter"),
            &BTreeMap::from([("story.pfs".into(), originals[0].clone())]),
        )
        .expect("check extraction fingerprint")
        .expect("matching extraction");
    assert_eq!(extraction.units.len(), 1080);
    assert!(extraction.units.iter().any(|unit| unit.kind == "name"));
    assert_eq!(
        originals,
        [
            hash_file(&game.join("story.pfs")).unwrap(),
            hash_file(&game.join("story.pfs.000")).unwrap(),
        ]
    );
    let session: Value =
        read_json(&project.join("adaptation/current/session.json")).expect("session");
    assert_eq!(session["automaticTemplate"]["mediaOnlyArchives"], 1);
    assert_eq!(session["automaticTemplate"]["archivedScripts"], 1);
    assert!(
        !project
            .join("adaptation/current/src/tools/vntextpatch-reference.json")
            .exists()
    );
}
