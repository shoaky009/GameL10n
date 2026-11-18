//! Native window, launcher and OCR acceptance fixtures.
//!
//! Build the `game_fixture` example and set `GAMEL10N_GAME_CHECK_FIXTURE` to its EXE path.
//! On an interactive Windows desktop with English OCR, run this crate's tests with
//! `cargo test --test runtime -- --ignored --test-threads=1` so the fixtures share the desktop in turn.
#![cfg(windows)]
use serde_json::{Value, json};
use std::{
    io::Write,
    process::{Command, Stdio},
};

struct GameFixture {
    directory: tempfile::TempDir,
}
impl GameFixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let executable = std::env::var_os("GAMEL10N_GAME_CHECK_FIXTURE")
            .expect("Build the game_fixture example and set GAMEL10N_GAME_CHECK_FIXTURE.");
        for name in ["Game.exe", "Launcher.exe"] {
            std::fs::copy(&executable, directory.path().join(name)).unwrap();
        }
        Self { directory }
    }
    fn request(&self, mode: &str) -> Value {
        json!({"mode":mode,"executable":self.directory.path().join("Game.exe"),"gameExecutable":self.directory.path().join("Game.exe"),"arguments":"","workingDirectory":self.directory.path(),"gameDirectory":self.directory.path(),"settings":{"language":"en","timeoutSeconds":12},"expected":["Welcome to the translated world"]})
    }
}
impl Drop for GameFixture {
    fn drop(&mut self) {
        use windows::{
            Win32::{
                Foundation::{CloseHandle, WAIT_OBJECT_0},
                System::Threading::*,
            },
            core::PWSTR,
        };
        for name in ["Game", "Launcher"] {
            let Ok(pid) =
                std::fs::read_to_string(self.directory.path().join(format!("{name}.pid")))
            else {
                continue;
            };
            let Ok(pid) = pid.parse::<u32>() else {
                continue;
            };
            unsafe {
                if let Ok(process) = OpenProcess(
                    PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_TERMINATE | PROCESS_SYNCHRONIZE,
                    false,
                    pid,
                ) {
                    let mut path = [0u16; 32768];
                    let mut length = path.len() as u32;
                    if QueryFullProcessImageNameW(
                        process,
                        PROCESS_NAME_WIN32,
                        PWSTR(path.as_mut_ptr()),
                        &mut length,
                    )
                    .is_ok()
                        && std::path::Path::new(&String::from_utf16_lossy(&path[..length as usize]))
                            == self.directory.path().join(format!("{name}.exe"))
                    {
                        let _ = TerminateProcess(process, 0);
                        assert_eq!(WaitForSingleObject(process, 3000), WAIT_OBJECT_0);
                    }
                    let _ = CloseHandle(process);
                }
            }
        }
    }
}

fn check(request: &Value) -> Vec<Value> {
    use std::os::windows::process::CommandExt;
    let mut child = Command::new(env!("CARGO_BIN_EXE_gamel10n-game-check"))
        .creation_flags(0x08000000)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    serde_json::to_writer(&mut stdin, request).unwrap();
    stdin.flush().unwrap();
    drop(stdin);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(35);
    while child.try_wait().unwrap().is_none() {
        if std::time::Instant::now() >= deadline {
            child.kill().unwrap();
            panic!("Native check did not finish within 35 seconds.");
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[test]
#[ignore = "Requires a Windows desktop with graphics capture and English OCR, and the game_fixture example."]
fn launcher_handoff_visible_translation_mismatch_and_empty_regions_have_distinct_results() {
    {
        let fixture = GameFixture::new();
        let mut request = fixture.request("launch");
        request["executable"] = json!(fixture.directory.path().join("Launcher.exe"));
        request["arguments"] = json!(format!(
            "--handoff=\"{}\"",
            fixture.directory.path().join("Game.exe").display()
        ));
        let results = check(&request);
        assert_eq!(results[0]["value"]["status"], "passed", "{results:?}");
    }
    let fixture = GameFixture::new();
    let mut request = fixture.request("launchAndText");
    request["executable"] = json!(fixture.directory.path().join("Launcher.exe"));
    request["arguments"] = json!(format!(
        "--handoff=\"{}\" --keep-window",
        fixture.directory.path().join("Game.exe").display()
    ));
    let results = check(&request);
    assert_eq!(results.len(), 2, "{results:?}");
    assert_eq!(results[0]["value"]["status"], "passed", "{results:?}");
    assert_eq!(results[1]["value"]["status"], "passed", "{results:?}");
    let mut current = fixture.request("text");
    current["expected"] = json!(["This sentence is absent from the game"]);
    let results = check(&current);
    assert_eq!(results[0]["kind"], "text");
    assert_eq!(results[0]["value"]["status"], "failed", "{results:?}");
    current["settings"]["region"] = json!({"left":0,"top":85,"width":100,"height":15});
    let results = check(&current);
    assert_eq!(results[0]["value"]["status"], "inconclusive", "{results:?}");
    let results = check(&fixture.request("launch"));
    assert_eq!(results[0]["value"]["status"], "inconclusive", "{results:?}");
}

#[test]
#[ignore = "Requires the game_fixture example."]
fn early_exit_and_invalid_executables_do_not_pass_launch_checks() {
    let fixture = GameFixture::new();
    let mut request = fixture.request("launch");
    request["arguments"] = json!("--exit=7");
    let results = check(&request);
    assert_eq!(results[0]["value"]["status"], "failed", "{results:?}");
    std::fs::write(
        fixture.directory.path().join("Game.exe"),
        "invalid executable",
    )
    .unwrap();
    let results = check(&request);
    assert_eq!(results[0]["value"]["status"], "failed", "{results:?}");
}

#[test]
#[ignore = "Requires a Windows desktop and the game_fixture example."]
fn owned_error_dialogs_fail_startup_and_cleanup_rechecks_process_identity() {
    let fixture = GameFixture::new();
    let mut request = fixture.request("launch");
    request["arguments"] = json!("--error-dialog");
    let results = check(&request);
    let error = results
        .iter()
        .find(|e| e["kind"] == "errorWindow")
        .expect("Error window identity")["value"]
        .clone();
    assert_eq!(
        results.last().unwrap()["value"]["status"],
        "failed",
        "{results:?}"
    );
    assert!(
        results.last().unwrap()["value"]["message"]
            .as_str()
            .unwrap()
            .contains("cannot load its resources"),
        "{results:?}"
    );
    fn cleanup(errors: Value) {
        use std::os::windows::process::CommandExt;
        let mut child = Command::new(env!("CARGO_BIN_EXE_gamel10n-game-check"))
            .arg("--cleanup")
            .creation_flags(0x08000000)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        serde_json::to_writer(child.stdin.take().unwrap(), &errors).unwrap();
        assert!(child.wait().unwrap().success());
    }
    let window =
        windows::Win32::Foundation::HWND(error["window"].as_u64().unwrap() as usize as *mut _);
    let mut wrong = error.clone();
    wrong["processStarted"] = json!(wrong["processStarted"].as_u64().unwrap() + 1);
    cleanup(json!([wrong]));
    assert!(unsafe { windows::Win32::UI::WindowsAndMessaging::IsWindow(Some(window)).as_bool() });
    cleanup(json!([error]));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while unsafe { windows::Win32::UI::WindowsAndMessaging::IsWindow(Some(window)).as_bool() }
        && std::time::Instant::now() < deadline
    {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(!unsafe { windows::Win32::UI::WindowsAndMessaging::IsWindow(Some(window)).as_bool() });
    let late = GameFixture::new();
    let mut request = late.request("launchAndText");
    request["arguments"] = json!("--late-error");
    request["settings"]["steps"] = json!([{"action":"wait","milliseconds":4000}]);
    let results = check(&request);
    assert!(
        results
            .iter()
            .any(|e| e["kind"] == "launch" && e["value"]["status"] == "failed"),
        "{results:?}"
    );
    let error = results.iter().find(|e| e["kind"] == "errorWindow").unwrap()["value"].clone();
    cleanup(json!([error]));
    let pid: u32 = std::fs::read_to_string(late.directory.path().join("Game.pid"))
        .unwrap()
        .parse()
        .unwrap();
    unsafe {
        use windows::Win32::{Foundation::CloseHandle, System::Threading::*};
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).unwrap();
        let mut exit = 0;
        GetExitCodeProcess(process, &mut exit).unwrap();
        assert_eq!(
            exit, 259,
            "The normal game process must remain open after its error dialog closes."
        );
        let _ = CloseHandle(process);
    }
}

#[test]
#[ignore = "Requires a Windows desktop with English OCR and the game_fixture example."]
fn automatic_location_closes_the_launched_process_tree() {
    let fixture = GameFixture::new();
    let mut request = fixture.request("locateText");
    request["closeAfter"] = json!(true);
    request["executable"] = json!(fixture.directory.path().join("Launcher.exe"));
    request["arguments"] = json!(format!(
        "--handoff=\"{}\" --keep-window",
        fixture.directory.path().join("Game.exe").display()
    ));
    let results = check(&request);
    assert!(
        results.iter().any(|e| e["kind"] == "visibleText"
            && e["value"]
                .as_str()
                .is_some_and(|text| text.contains("Welcome"))),
        "{results:?}"
    );
    assert!(
        results
            .iter()
            .any(|e| e["kind"] == "closed" && e["value"]["status"] == "passed"),
        "{results:?}"
    );
    for name in ["Game", "Launcher"] {
        let pid: u32 =
            std::fs::read_to_string(fixture.directory.path().join(format!("{name}.pid")))
                .unwrap()
                .parse()
                .unwrap();
        unsafe {
            use windows::Win32::{
                Foundation::{CloseHandle, WAIT_OBJECT_0},
                System::Threading::*,
            };
            if let Ok(process) = OpenProcess(PROCESS_SYNCHRONIZE, false, pid) {
                assert_eq!(
                    WaitForSingleObject(process, 0),
                    WAIT_OBJECT_0,
                    "{name} should be closed"
                );
                let _ = CloseHandle(process);
            }
        }
    }
}
