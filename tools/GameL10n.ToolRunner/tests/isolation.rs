#![cfg(windows)]

use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader},
    path::Path,
    process::{Command, Stdio},
};
use windows::Win32::{
    Foundation::{CloseHandle, WAIT_OBJECT_0},
    System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject},
};

fn launch(request: &Value) -> std::process::Child {
    let mut child = Command::new(env!("CARGO_BIN_EXE_gamel10n-tool-runner"))
        .env("GAMEL10N_ISOLATION_TEST_SECRET", "must-not-inherit")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    serde_json::to_writer(child.stdin.take().unwrap(), request).unwrap();
    child
}

fn request(root: &Path, input: Value) -> Value {
    json!({
        "protocolVersion":1,
        "executable":env!("CARGO_BIN_EXE_isolation-fixture"),
        "arguments":["中文 path", "", "trailing\\", "a\\\"b"],
        "directory":root.join("work"),
        "readPaths":[root.join("inputs")],
        "writePaths":[root.join("work")],
        "input": input,
    })
}

fn fixture() -> tempfile::TempDir {
    let directory = tempfile::tempdir().unwrap();
    std::fs::create_dir(directory.path().join("inputs")).unwrap();
    std::fs::create_dir(directory.path().join("work")).unwrap();
    std::fs::write(directory.path().join("inputs/allowed.txt"), "allowed").unwrap();
    std::fs::write(directory.path().join("outside.txt"), "private").unwrap();
    directory
}

fn input(root: &Path) -> Value {
    json!({
        "allowedFile":root.join("inputs/allowed.txt"),
        "outsideFile":root.join("outside.txt"),
        "outsideOutput":root.join("outside-output.txt"),
        "allowedOutput":root.join("work/output.txt"),
    })
}

fn exited(pid: u32) -> bool {
    let Ok(process) = (unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, pid) }) else {
        return true;
    };
    let result = unsafe { WaitForSingleObject(process, 5_000) } == WAIT_OBJECT_0;
    unsafe {
        let _ = CloseHandle(process);
    }
    result
}

#[test]
fn file_permissions_environment_and_arguments_are_enforced() {
    let directory = fixture();
    let output = launch(&request(directory.path(), input(directory.path())))
        .wait_with_output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        result,
        json!({
            "allowedRead":"allowed", "outsideReadDenied":true, "outsideWriteDenied":true,
            "inputWriteDenied":true, "allowedWrite":true, "environment":null, "temporaryWrite":true,
            "arguments":["中文 path", "", "trailing\\", "a\\\"b"],
        }),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn child_processes_inherit_file_restrictions() {
    let directory = fixture();
    let mut input = input(directory.path());
    input["action"] = json!("nested");
    // The nested child receives the same inputs but performs ordinary inspection.
    let output = launch(&request(directory.path(), input))
        .wait_with_output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        (
            result["outsideReadDenied"].clone(),
            result["outsideWriteDenied"].clone()
        ),
        (json!(true), json!(true))
    );
}

#[test]
fn internet_and_private_network_capabilities_are_enabled() {
    let directory = fixture();
    let output = launch(&request(directory.path(), json!({"action":"network"})))
        .wait_with_output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap(),
        json!([true, true])
    );
}

#[test]
fn successful_parent_does_not_leave_descendants_running() {
    let directory = fixture();
    let output = launch(&request(directory.path(), json!({"action":"spawn"})))
        .wait_with_output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(exited(result["childPid"].as_u64().unwrap() as u32));
}

#[test]
fn killing_runner_terminates_the_entire_job() {
    let directory = fixture();
    let mut runner = launch(&request(
        directory.path(),
        json!({"action":"spawn", "wait":true}),
    ));
    let mut line = String::new();
    BufReader::new(runner.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let result: Value = serde_json::from_str(&line).unwrap();
    runner.kill().unwrap();
    runner.wait().unwrap();
    assert!(
        exited(result["processId"].as_u64().unwrap() as u32)
            && exited(result["childPid"].as_u64().unwrap() as u32)
    );
}

#[test]
fn child_abnormal_exit_is_a_tool_failure() {
    let directory = fixture();
    for code in [0xC000_0022u32, 0xC000_0005, 0xC000_0135] {
        let output = launch(&request(
            directory.path(),
            json!({"action":"exit", "code":code}),
        ))
        .wait_with_output()
        .unwrap();
        let diagnostic = String::from_utf8(output.stderr).unwrap();
        assert_eq!(output.status.code(), Some(1), "{diagnostic}");
        assert!(
            diagnostic.contains(&format!("0x{code:08X}")),
            "{diagnostic}"
        );
        assert!(
            !diagnostic.contains("GameL10n-Isolation-Failed:"),
            "{diagnostic}"
        );
    }
}

#[test]
fn invalid_authorization_fails_with_reason_and_settings_guidance() {
    let directory = fixture();
    let mut request = request(directory.path(), input(directory.path()));
    request["readPaths"] = json!([directory.path().canonicalize().unwrap().join("missing")]);
    let output = launch(&request).wait_with_output().unwrap();
    let error = String::from_utf8(output.stderr).unwrap();
    assert!(
        !output.status.success() && error.contains("程序设置") && error.contains("隔离运行环境"),
        "{error}"
    );
    assert!(
        error.contains("missing") && !error.contains(r"\\?\"),
        "{error}"
    );
    assert!(!directory.path().join("work/output.txt").exists());
}
