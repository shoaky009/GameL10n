use std::{fs, path::PathBuf};

use gamel10n_engines::{
    AdapterHandle, CancellationToken, EngineService, PackageManifest, ToolPaths,
    protocol::AdapterRequest,
};
use gamel10n_translation::files::{hash_file, write_json};
use serde_json::json;

const ADAPTER: &str = r#"import json,sys
from pathlib import Path
r=json.load(sys.stdin);root=Path(r['sourceDirectory']);op=r['operation']
data=json.loads((root/'text.json').read_text(encoding='utf8'))
out={'protocolVersion':1,'success':True}
if op=='probe':
 out['probe']={'engineId':'fixture','adapterId':'engine.fixture','adapterVersion':'1','description':'fixture','requiredFiles':['text.json'],'warnings':[]}
elif op=='extract':
 out['extraction']={'units':[{'id':str(i),'resourcePath':'text.json','locator':str(i),'sourceText':v,'context':'stable context','kind':'dialogue'} for i,v in enumerate(data)],'warnings':[]}
elif op=='build':
 target=Path(r['outputDirectory']);target.mkdir(parents=True,exist_ok=True)
 translations=json.loads(Path(r['translationsFile']).read_text(encoding='utf8')) if r.get('translationsFile') else r.get('translations',{})
 changed=[translations.get(str(i),v) for i,v in enumerate(data)]
 (target/'text.json').write_bytes((root/'text.json').read_bytes() if changed==data else json.dumps(changed,ensure_ascii=False).encode('utf8'))
 out['build']={'files':['text.json'],'warnings':[]}
elif op=='validate':out['validation']={'passed':True,'issues':[],'runtimeStatus':'not-tested'}
print(json.dumps(out,ensure_ascii=False))
"#;

fn service(shared: PathBuf) -> EngineService {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    EngineService::new(&root, ToolPaths::development()).with_shared_directory(shared)
}

#[tokio::test]
async fn overlay_verification_checks_effective_text_and_binds_read_only_patch_bytes() {
    let fixture = tempfile::tempdir().expect("fixture");
    let service = service(fixture.path().join("shared"));
    if !service.tools.python().is_file() {
        return;
    }
    for (case, expected_error) in [
        ("valid", None),
        ("nonempty-original", None),
        ("ignore-overlay", Some("未保留文本")),
        ("drop-untouched", Some("改变了文本数量")),
        ("mutate-patch", Some("修改了补丁输出")),
    ] {
        let directory = fixture.path().join(case);
        let source = directory.join("source");
        fs::create_dir_all(&source).expect("source");
        fs::write(source.join("text.json"), r#"["one","two","three","four"]"#).expect("text");
        // Unselected volumes affect naming, but their contents are never staged or hashed.
        fs::write(source.join("text.json.000"), b"unrelated media").expect("media");
        let original_hash = hash_file(&source.join("text.json")).expect("hash");
        let entry = directory.join("adapter.py");
        fs::write(
            &entry,
            include_str!("fixtures/overlay_adapter.py")
                .replace("CASE = 'valid'", &format!("CASE = '{case}'")),
        )
        .expect("adapter");
        let adapter = AdapterHandle::Python {
            entry,
            package_directory: None,
            id: "engine.fixture".into(),
            version: "1".into(),
        };
        let token = CancellationToken::new();
        let mut request = AdapterRequest::new("probe", &source);
        request.resource_files = Some(vec!["text.json".into(), "text.json.000".into()]);
        let probe = service
            .execute(&adapter, &request, &token)
            .await
            .expect("probe")
            .probe
            .expect("probe result");
        assert_eq!(probe.build_hints["patchPath"], "text.json.001");
        let verification = directory.join("verification");
        let result = service
            .verify_probed(&adapter, &source, &probe, &verification, &[], &token, None)
            .await
            .expect("verification");
        assert_eq!(
            result.passed,
            expected_error.is_none(),
            "{case}: {:?}",
            result.diagnostics
        );
        assert!(
            result.original_roundtrip_passed,
            "{case}: {:?}",
            result.diagnostics
        );
        if let Some(error) = expected_error {
            assert!(
                result
                    .diagnostics
                    .iter()
                    .any(|message| message.contains(error)),
                "{case}: {:?}",
                result.diagnostics
            );
        } else {
            assert!(verification.join("partial/text.json.001").is_file());
            assert!(!verification.join("partial/text.json").exists());
            assert!(!verification.join("sources/text.json.000").exists());
            assert_eq!(
                verification
                    .join("original")
                    .read_dir()
                    .expect("original output")
                    .count(),
                usize::from(case == "nonempty-original")
            );
        }
        assert_eq!(
            hash_file(&source.join("text.json")).expect("hash"),
            original_hash
        );
        assert!(!result.runtime_tested);
    }
}

#[tokio::test]
async fn sfa16_original_roundtrip_does_not_deploy_the_source_executable() {
    let fixture = tempfile::tempdir().expect("fixture");
    let service = service(fixture.path().join("shared"));
    if !service.tools.python().is_file() {
        return;
    }
    let source = fixture.path().join("source");
    fs::create_dir(&source).expect("fixture");
    // One AOS member with a single-leaf Huffman tree decoding to "AAAA".
    let base = 0x111 + 40;
    let mut archive = vec![0u8; base];
    archive[4..8].copy_from_slice(&(base as u32).to_le_bytes());
    archive[8..12].copy_from_slice(&40u32.to_le_bytes());
    archive[0x111..0x11b].copy_from_slice(b"sample.scr");
    archive[base - 4..base].copy_from_slice(&6u32.to_le_bytes());
    archive.extend_from_slice(&[4, 0, 0, 0, 0x20, 0x80]);
    fs::write(source.join("scr.aos"), &archive).expect("fixture");
    fs::write(
        source.join("sfa16.exe"),
        b"original executable stays in source",
    )
    .expect("fixture");
    let executable_hash = hash_file(&source.join("sfa16.exe")).expect("hash");
    let output = fixture.path().join("output");
    let mut request = AdapterRequest::new("build", &source);
    request.resource_files = Some(vec!["scr.aos".into(), "sfa16.exe".into()]);
    request.output_directory = Some(output.clone());
    let response = service
        .execute(
            &AdapterHandle::BuiltinSfa16,
            &request,
            &CancellationToken::new(),
        )
        .await
        .expect("source roundtrip must pass the host contract");
    let build = response.build.expect("build result");
    assert_eq!(build.files, ["scr.aos"]);
    assert!(build.launch_executable.is_none());
    assert_eq!(fs::read(output.join("scr.aos")).expect("archive"), archive);
    assert!(!output.join("sfa16.exe").exists());
    assert!(!output.join("sfa16.zh.exe").exists());
    assert_eq!(
        hash_file(&source.join("sfa16.exe")).expect("hash"),
        executable_hash
    );
}

#[tokio::test]
async fn independently_verifies_original_chinese_and_partial_rebuilding() {
    let fixture = tempfile::tempdir().expect("fixture");
    let service = service(fixture.path().join("shared"));
    if !service.tools.python().is_file() {
        return;
    }
    let game = fixture.path().join("game");
    fs::create_dir(&game).expect("fixture");
    fs::write(
        game.join("text.json"),
        "[\"日本語\",\"dialogue\",\"third\",\"untouched\"]\n",
    )
    .expect("fixture");
    let entry = fixture.path().join("adapter.py");
    fs::write(&entry, ADAPTER).expect("fixture");
    let adapter = AdapterHandle::Python {
        entry,
        package_directory: None,
        id: "fixture".into(),
        version: "1".into(),
    };
    let before = hash_file(&game.join("text.json")).expect("hash");
    let result = service
        .verify(
            &adapter,
            &game,
            &["text.json".into()],
            &fixture.path().join("verify"),
            &[],
            &CancellationToken::new(),
            None,
        )
        .await
        .expect("verification");
    assert!(result.passed, "{:?}", result.diagnostics);
    assert_eq!(hash_file(&game.join("text.json")).expect("hash"), before);
    assert!(!result.runtime_tested);
}

#[tokio::test]
async fn validation_context_reuses_real_extractions_and_still_rejects_output_mutation() {
    let fixture = tempfile::tempdir().expect("fixture");
    let service = service(fixture.path().join("shared"));
    if !service.tools.python().is_file() {
        return;
    }
    let script = ADAPTER
        .replace(
            "out['extraction']=",
            "out['validationContextVersion']=1\n out['extraction']=",
        )
        .replace(
            "out['build']={'files':['text.json'],'warnings':[]}",
            "out['build']={'files':['text.json'],'warnings':[]}\n if changed!=data:\n  (target/'encoding.map').write_text('generated companion',encoding='utf8')\n  out['build']['files'].append('encoding.map')",
        )
        .replace(
            "elif op=='validate':out['validation']={'passed':True,'issues':[],'runtimeStatus':'not-tested'}",
            r#"elif op=='validate':
 context_path=Path(r['validationContextFile'])
 assert context_path.is_absolute()
 context=json.loads(context_path.read_text(encoding='utf8'))
 target=Path(r['outputDirectory'])
 assert context['schemaVersion']==1
 assert Path(context['sourceDirectory']).resolve()==root.resolve()
 assert Path(context['outputDirectory']).resolve()==target.resolve()
 assert context['resourceFiles']==r['resourceFiles']==['text.json']
 assert set(context['outputFiles'])=={'text.json','encoding.map'}
 assert (target/'encoding.map').read_text(encoding='utf8')=='generated companion'
 translations=json.loads(Path(r['translationsFile']).read_text(encoding='utf8'))
 original=context['originalUnits'];modified=context['outputUnits']
 assert [u['id'] for u in original]==[str(i) for i in range(len(data))]
 assert [u['sourceText'] for u in original]==data
 assert [u['sourceText'] for u in modified]==json.loads((target/'text.json').read_text(encoding='utf8'))
 after={u['id']:u for u in modified}
 for unit in original:
  actual=after[unit['id']]
  assert actual['sourceText']==translations.get(unit['id'],unit['sourceText'])
  assert all(actual[key]==unit[key] for key in ('resourcePath','locator','context','kind'))
 unchanged=sum(unit['id'] not in translations for unit in original)
 with Path(__file__).with_name('contexts.jsonl').open('a',encoding='utf8') as log:
  log.write(json.dumps(dict(path=str(context_path),unchanged=unchanged))+'\n')
 if CORRUPT_OUTPUT:
  (target/'text.json').write_text('[]',encoding='utf8')
 out['validation']={'passed':True,'issues':[],'runtimeStatus':'not-tested'}"#,
        );
    for corrupt_output in [false, true] {
        let case = fixture.path().join(if corrupt_output {
            "mutating-validator"
        } else {
            "readonly-validator"
        });
        let game = case.join("game");
        fs::create_dir_all(&game).expect("fixture");
        fs::write(game.join("text.json"), r#"["one","two","three","four"]"#).expect("fixture");
        let source_hash = hash_file(&game.join("text.json")).expect("hash");
        let entry = case.join("adapter.py");
        fs::write(
            &entry,
            script.replace(
                "CORRUPT_OUTPUT",
                if corrupt_output { "True" } else { "False" },
            ),
        )
        .expect("adapter");
        let adapter = AdapterHandle::Python {
            entry,
            package_directory: None,
            id: "fixture".into(),
            version: "1".into(),
        };
        let result = service
            .verify(
                &adapter,
                &game,
                &["text.json".into()],
                &case.join("verify"),
                &[],
                &CancellationToken::new(),
                None,
            )
            .await
            .expect("verification");
        assert_eq!(result.passed, !corrupt_output, "{:?}", result.diagnostics);
        let contexts: Vec<serde_json::Value> = fs::read_to_string(case.join("contexts.jsonl"))
            .expect("validator used the context")
            .lines()
            .map(|line| serde_json::from_str(line).expect("context record"))
            .collect();
        assert_eq!(contexts.len(), if corrupt_output { 1 } else { 2 });
        assert!(contexts.iter().any(|context| context["unchanged"] == 2));
        for context in contexts {
            let path = PathBuf::from(context["path"].as_str().expect("context path"));
            assert!(!path.exists(), "context file must expire after validate");
            assert!(
                !path.parent().expect("context directory").exists(),
                "context directory must be removed on success and failure"
            );
        }
        if corrupt_output {
            assert!(
                result
                    .diagnostics
                    .iter()
                    .any(|message| message.contains("修改了补丁输出")),
                "{:?}",
                result.diagnostics
            );
        }
        assert_eq!(
            hash_file(&game.join("text.json")).expect("hash"),
            source_hash
        );
    }
}

#[tokio::test]
async fn partial_translation_must_preserve_untouched_units() {
    let fixture = tempfile::tempdir().expect("fixture");
    let service = service(fixture.path().join("shared"));
    if !service.tools.python().is_file() {
        return;
    }
    let game = fixture.path().join("game");
    fs::create_dir(&game).expect("fixture");
    fs::write(
        game.join("text.json"),
        "[\"one\",\"two\",\"three\",\"four\"]",
    )
    .expect("fixture");
    let entry = fixture.path().join("adapter.py");
    fs::write(
        &entry,
        ADAPTER.replace(".get(str(i),v)", ".get(str(i),'CORRUPTED')"),
    )
    .expect("fixture");
    let adapter = AdapterHandle::Python {
        entry,
        package_directory: None,
        id: "fixture".into(),
        version: "1".into(),
    };
    let result = service
        .verify(
            &adapter,
            &game,
            &["text.json".into()],
            &fixture.path().join("verify"),
            &[],
            &CancellationToken::new(),
            None,
        )
        .await
        .expect("verification");
    assert!(!result.passed);
    assert!(
        result
            .diagnostics
            .iter()
            .any(|item| item.contains("部分试译"))
    );
}

#[test]
fn changed_selected_source_rejects_a_published_manifest() {
    let fixture = tempfile::tempdir().expect("fixture");
    fs::create_dir(fixture.path().join("src")).expect("fixture");
    fs::write(fixture.path().join("src/adapter.py"), ADAPTER).expect("fixture");
    write_json(&fixture.path().join("package.json"),&json!({"id":"engine.fixture","version":"1","manifestVersion":4,"protocolVersion":1,"resourcePatterns":["*.json"],"packageFiles":{"src/adapter.py":hash_file(&fixture.path().join("src/adapter.py")).expect("hash")}})).expect("fixture");
    let manifest = PackageManifest::load(fixture.path()).expect("manifest");
    fs::write(fixture.path().join("src/adapter.py"), "print('changed')").expect("change");
    assert!(manifest.handle(fixture.path()).is_err());
}

#[tokio::test]
async fn partial_translation_reports_speaker_drift_without_a_text_mismatch() {
    let fixture = tempfile::tempdir().expect("fixture");
    let service = service(fixture.path().join("shared"));
    if !service.tools.python().is_file() {
        return;
    }
    let game = fixture.path().join("game");
    fs::create_dir(&game).expect("fixture");
    fs::write(
        game.join("text.json"),
        r#"["name","dialogue","third","untouched"]"#,
    )
    .expect("fixture");
    let entry = fixture.path().join("adapter.py");
    fs::write(
        &entry,
        ADAPTER.replace(
            "'kind':'dialogue'",
            "'kind':'dialogue','speaker':data[0] if i==1 else None",
        ),
    )
    .expect("adapter");
    let adapter = AdapterHandle::Python {
        entry,
        package_directory: None,
        id: "fixture".into(),
        version: "1".into(),
    };
    let result = service
        .verify(
            &adapter,
            &game,
            &["text.json".into()],
            &fixture.path().join("verify"),
            &[],
            &CancellationToken::new(),
            None,
        )
        .await
        .expect("verification");
    assert!(!result.passed);
    assert!(result.original_roundtrip_passed);
    let issue = &result.issues[0];
    assert_eq!(issue.stage, "partial-translation-re-extraction");
    assert_eq!(
        issue.code,
        gamel10n_engines::VerificationIssueCode::IdentityMismatch
    );
    assert_eq!(issue.field_differences.len(), 1);
    assert_eq!(issue.field_differences[0].field, "speaker");
    assert_eq!(issue.expected, "\"name\"");
    assert_ne!(issue.expected, issue.actual);
    assert!(
        result
            .diagnostics
            .iter()
            .any(|message| message.contains("speaker"))
    );
}
