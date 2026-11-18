use serde_json::{Value, json};
use std::{
    fs::{File, TryLockError},
    io::{self, BufRead, BufReader, Read, Write},
    path::Path,
    process::{Command, Stdio},
};

fn inspect(request: &Value) -> Value {
    let path = |key: &str| Path::new(request[key].as_str().unwrap());
    let temporary = std::env::temp_dir().join("fixture.tmp");
    let temporary_write = std::fs::write(&temporary, b"temp");
    if let Err(error) = &temporary_write {
        eprintln!(
            "Temporary file {} (TEMP={:?}): {error}",
            temporary.display(),
            std::env::var_os("TEMP")
        );
    }
    json!({
        "allowedRead": std::fs::read_to_string(path("allowedFile")).ok(),
        "outsideReadDenied": std::fs::read(path("outsideFile")).is_err(),
        "outsideWriteDenied": std::fs::write(path("outsideOutput"), b"changed").is_err(),
        "inputWriteDenied": std::fs::write(path("allowedFile"), b"changed").is_err(),
        "allowedWrite": std::fs::write(path("allowedOutput"), b"output").is_ok(),
        "environment": std::env::var("GAMEL10N_ISOLATION_TEST_SECRET").ok(),
        "temporaryWrite": temporary_write.is_ok(),
        "arguments": std::env::args().skip(1).collect::<Vec<_>>(),
    })
}

#[cfg(windows)]
fn network_capabilities() -> Vec<bool> {
    use windows::{
        Win32::Security::{
            CheckTokenCapability, CreateWellKnownSid, PSID, SECURITY_MAX_SID_SIZE,
            WinCapabilityInternetClientServerSid, WinCapabilityPrivateNetworkClientServerSid,
        },
        core::BOOL,
    };
    [
        WinCapabilityInternetClientServerSid,
        WinCapabilityPrivateNetworkClientServerSid,
    ]
    .into_iter()
    .map(|kind| {
        let mut storage = [0u32; SECURITY_MAX_SID_SIZE as usize / 4];
        let sid = PSID(storage.as_mut_ptr().cast());
        let mut bytes = SECURITY_MAX_SID_SIZE;
        let mut granted = BOOL::default();
        unsafe {
            CreateWellKnownSid(kind, None, Some(sid), &mut bytes).unwrap();
            CheckTokenCapability(None, sid, &mut granted).unwrap();
        }
        granted.as_bool()
    })
    .collect()
}

#[expect(
    clippy::zombie_processes,
    reason = "The spawn fixture intentionally leaves a descendant for the runner's Job Object to terminate."
)]
fn main() {
    let mut arguments = std::env::args_os().skip(1);
    if arguments.next().as_deref() == Some("--linger".as_ref()) {
        let lock = File::options()
            .write(true)
            .open(arguments.next().unwrap())
            .unwrap();
        assert!(matches!(lock.try_lock(), Err(TryLockError::WouldBlock)));
        println!("ready");
        io::stdout().flush().unwrap();
        lock.lock().unwrap();
        return;
    }
    let mut bytes = Vec::new();
    io::stdin().read_to_end(&mut bytes).unwrap();
    let request: Value = serde_json::from_slice(&bytes).unwrap();
    match request["action"].as_str() {
        Some("exit") => std::process::exit(request["code"].as_u64().unwrap() as i32),
        #[cfg(windows)]
        Some("network") => println!("{}", json!(network_capabilities())),
        Some("nested") => {
            let mut request = request.clone();
            request.as_object_mut().unwrap().remove("action");
            let mut child = Command::new(std::env::current_exe().unwrap())
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            let mut input = child.stdin.take().unwrap();
            serde_json::to_writer(&mut input, &request).unwrap();
            drop(input);
            let output = child.wait_with_output().unwrap();
            if !output.status.success() {
                panic!("{}", String::from_utf8_lossy(&output.stderr));
            }
            print!("{}", String::from_utf8_lossy(&output.stdout));
        }
        Some("spawn") => {
            // AppContainer can create pipes even when it cannot open NUL.
            let mut child = Command::new(std::env::current_exe().unwrap())
                .arg("--linger")
                .arg(request["lockFile"].as_str().unwrap())
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            let mut ready = String::new();
            if BufReader::new(child.stdout.take().unwrap())
                .read_line(&mut ready)
                .unwrap()
                == 0
            {
                let output = child.wait_with_output().unwrap();
                panic!(
                    "Descendant exited before reporting readiness ({}): {}",
                    output.status,
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            assert_eq!(ready.trim_end(), "ready");
            println!(
                "{}",
                json!({"childPid":child.id(),"processId":std::process::id()})
            );
            io::stdout().flush().unwrap();
            if request["wait"] == true {
                let status = child.wait().unwrap();
                panic!("Descendant exited before the runner was terminated: {status}");
            }
        }
        _ => println!("{}", inspect(&request)),
    }
}
