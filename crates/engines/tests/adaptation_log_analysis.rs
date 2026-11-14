#![cfg(windows)]

use serde_json::{Value, json};
use std::{fs, path::Path, process::Command};

fn analyze(entries: &[Value], detail: bool) -> Value {
    let project = tempfile::tempdir().unwrap();
    fs::create_dir(project.path().join("logs")).unwrap();
    let rows = entries.iter().enumerate().map(|(index, entry)| {
        json!({"schemaVersion":2,"runId":"fixture-run","at":format!("2025-01-01T00:00:{index:02}Z"),"turn":1,"entry":entry}).to_string()
    }).collect::<Vec<_>>().join("\n");
    fs::write(
        project.path().join("logs/adaptation-20250101.jsonl"),
        rows + "\n",
    )
    .unwrap();
    let script =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/analyze-adaptation-log.ps1");
    let mut command = Command::new("pwsh");
    command
        .args(["-NoProfile", "-File"])
        .arg(script)
        .arg("-ProjectDirectory")
        .arg(project.path());
    if detail {
        command.arg("-Detail");
    }
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn metrics(checks: u64, samples: u64, bytes: u64, hits: u64, ms: u64) -> Value {
    json!({"checks":checks,"sampleCount":samples,"sampleBytes":bytes,"cacheHits":hits,"elapsedMs":ms})
}

#[test]
fn sample_time_is_a_measured_subset_of_parent_time_and_builds_are_comparable() {
    let entries = [
        json!({"operation":"run-start","softwareVersion":"0.1.0","buildGitHash":"fixture-build","maximumOutputTokens":64000,"analysisRequestTimeoutMs":600000}),
        json!({"operation":"local-preparation","elapsedMs":1000,"sampleChecks":metrics(1,2,300,1,200)}),
        json!({"operation":"model","status":"completed","elapsedMs":1500,"apiMs":1000,"waitingMs":450}),
        json!({"operation":"tool-end","tool":"run_engine_python","call":{"id":"probe","name":"run_engine_python"},"success":true,"elapsedMs":5000,"sampleChecks":metrics(2,3,600,0,1000)}),
    ];
    for detail in [false, true] {
        let result = analyze(&entries, detail);
        assert_eq!(result["runs"][0]["buildGitHash"], "fixture-build");
        assert_eq!(result["runs"][0]["maximumOutputTokens"], 64000);
        assert_eq!(result["runs"][0]["analysisRequestTimeoutMs"], 600000);
        assert_eq!(result["timing"]["toolSeconds"], 5.0);
        assert_eq!(result["timing"]["apiSeconds"], 1.0);
        assert_eq!(result["timing"]["waitingSeconds"], 0.45);
        let samples = &result["sampleChecks"];
        assert_eq!(samples["seconds"], 1.2);
        assert_eq!(samples["sampleCount"], 5.0);
        assert_eq!(samples["sampleBytes"], 900.0);
        assert_eq!(samples["cacheHits"], 1.0);
        assert_eq!(samples["cacheHitRate"], 0.3333);
        assert_eq!(samples["localPreparationSeconds"], 0.2);
        assert_eq!(samples["toolSeconds"], 1.0);
        assert_eq!(samples["unmeasuredScopes"], 0);
        if detail {
            assert_eq!(result["sampleCheckEvents"].as_array().unwrap().len(), 2);
        }
    }
}

#[test]
fn legacy_and_mixed_logs_keep_unmeasured_categories_unknown() {
    for measured_scope in [None, Some("local-preparation"), Some("tool-end")] {
        let mut entries = vec![
            json!({"operation":"run-start","softwareVersion":"0.1.0","buildGitHash":"unknown"}),
            json!({"operation":"local-preparation","elapsedMs":1000}),
            json!({"operation":"tool-end","tool":"run_engine_python","call":{"id":"probe","name":"run_engine_python"},"elapsedMs":5000}),
            json!({"operation":"model","status":"completed","elapsedMs":1500}),
        ];
        if let Some(scope) = measured_scope {
            let index = if scope == "local-preparation" { 1 } else { 2 };
            entries[index]["sampleChecks"] = metrics(1, 1, 100, 0, 200);
        }
        let result = analyze(&entries, false);
        assert_eq!(result["coverage"]["missingBuildIdentity"], 1);
        assert!(result["runs"][0]["maximumOutputTokens"].is_null());
        assert!(result["runs"][0]["analysisRequestTimeoutMs"].is_null());
        assert!(result["timing"]["apiSeconds"].is_null());
        assert!(result["timing"]["waitingSeconds"].is_null());
        let summary = &result["sampleChecks"];
        if measured_scope.is_none() {
            for field in [
                "seconds",
                "sampleCount",
                "sampleBytes",
                "cacheHits",
                "localPreparationSeconds",
                "toolSeconds",
            ] {
                assert!(summary[field].is_null(), "{field}: {summary}");
            }
            assert_eq!(summary["unmeasuredScopes"], 2);
        } else {
            let missing = if measured_scope == Some("tool-end") {
                "localPreparationSeconds"
            } else {
                "toolSeconds"
            };
            assert!(summary[missing].is_null(), "{summary}");
            assert_eq!(summary["unmeasuredScopes"], 1);
            assert_eq!(summary["cacheHits"], 0.0);
        }
    }
}
