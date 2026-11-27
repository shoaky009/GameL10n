use std::{
    ffi::OsString,
    future::Future,
    io::BufReader,
    path::PathBuf,
    process::Stdio,
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};

use serde::{Serialize, de::DeserializeOwned};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::Command,
    sync::{Notify, Semaphore},
};
use tokio_util::sync::CancellationToken;

use crate::{EngineError, protocol::PROTOCOL_VERSION};

const DEFAULT_PROCESS_TIMEOUT: Duration = Duration::from_secs(300);
const ISOLATION_SHUTDOWN_GRACE: Duration = Duration::from_secs(30);
const MAX_RESPONSE_BYTES: u64 = 256 * 1024 * 1024;
const DEFAULT_STDERR_BYTES: u64 = 2 * 1024 * 1024;
const MAX_STDERR_DIAGNOSTIC_CHARS: usize = 4000;
const WINDOWS_ABNORMAL_EXIT_MIN: u32 = 0x8000_0000;
#[cfg(windows)]
const WIN32_CREATE_NO_WINDOW: u32 = 0x0800_0000;

fn process_slots() -> &'static Semaphore {
    static SLOTS: OnceLock<Semaphore> = OnceLock::new();
    SLOTS.get_or_init(|| {
        let permits = std::thread::available_parallelism()
            .map(|count| count.get().clamp(2, 6))
            .unwrap_or(4);
        Semaphore::new(permits)
    })
}

tokio::task_local! {
    static CURRENT_BUDGET: Arc<ProcessBudget>;
}

struct BudgetState {
    limit: Duration,
    active_elapsed: Duration,
    active_started: Option<Instant>,
    paused_waiters: usize,
}

/// Tracks active execution budget while excluding time spent queued for `process_slots()`.
pub(crate) struct ProcessBudget {
    state: Mutex<BudgetState>,
    changed: Notify,
}

struct BudgetPauseGuard {
    budget: Arc<ProcessBudget>,
}

impl Drop for BudgetPauseGuard {
    fn drop(&mut self) {
        self.budget.resume();
    }
}

impl ProcessBudget {
    pub(crate) fn new(limit: Duration) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(BudgetState {
                limit,
                active_elapsed: Duration::ZERO,
                active_started: Some(Instant::now()),
                paused_waiters: 0,
            }),
            changed: Notify::new(),
        })
    }

    pub(crate) async fn scope<F: Future>(budget: Arc<Self>, future: F) -> F::Output {
        CURRENT_BUDGET.scope(budget, future).await
    }

    pub(crate) fn active_elapsed(&self) -> Duration {
        let state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.active_elapsed
            + state
                .active_started
                .map_or(Duration::ZERO, |started| started.elapsed())
    }

    fn pause(self: &Arc<Self>) -> BudgetPauseGuard {
        {
            let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
            if state.paused_waiters == 0
                && let Some(started) = state.active_started.take()
            {
                state.active_elapsed += started.elapsed();
            }
            state.paused_waiters += 1;
        }
        self.changed.notify_waiters();
        BudgetPauseGuard {
            budget: Arc::clone(self),
        }
    }

    fn resume(&self) {
        {
            let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
            state.paused_waiters = state.paused_waiters.saturating_sub(1);
            if state.paused_waiters == 0 && state.active_started.is_none() {
                state.active_started = Some(Instant::now());
            }
        }
        self.changed.notify_waiters();
    }

    pub(crate) async fn expired(&self) {
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let remaining = {
                let state = self.state.lock().unwrap_or_else(|error| error.into_inner());
                let elapsed = state.active_elapsed
                    + state
                        .active_started
                        .map_or(Duration::ZERO, |started| started.elapsed());
                let remaining = state.limit.saturating_sub(elapsed);
                if remaining.is_zero() {
                    return;
                }
                (state.paused_waiters == 0).then_some(remaining)
            };
            match remaining {
                None => notified.await,
                Some(remaining) => {
                    tokio::select! {
                        biased;
                        () = notified => continue,
                        () = tokio::time::sleep(remaining) => continue,
                    }
                }
            }
        }
    }
}

fn pause_current_budget() -> Option<BudgetPauseGuard> {
    CURRENT_BUDGET.try_with(|budget| budget.pause()).ok()
}

/// JSON subprocess execution, optionally through the isolated native runner.
#[derive(Clone, Debug)]
pub struct ToolProcess {
    executable: PathBuf,
    arguments: Vec<OsString>,
    timeout: Duration,
    directory: Option<PathBuf>,
    response_file: Option<PathBuf>,
    maximum_output_bytes: Option<u64>,
    preserve_failure_response: bool,
    isolation: Option<ProcessIsolation>,
}

#[derive(Clone, Debug)]
struct ProcessIsolation {
    runner: PathBuf,
    read_paths: Vec<PathBuf>,
    input_paths: Vec<PathBuf>,
    write_paths: Vec<PathBuf>,
}

impl ToolProcess {
    pub fn new(executable: impl Into<PathBuf>) -> Self {
        Self {
            executable: executable.into(),
            arguments: Vec::new(),
            timeout: DEFAULT_PROCESS_TIMEOUT,
            directory: None,
            response_file: None,
            maximum_output_bytes: None,
            preserve_failure_response: false,
            isolation: None,
        }
    }

    pub fn with_arguments(mut self, arguments: impl IntoIterator<Item = OsString>) -> Self {
        self.arguments.extend(arguments);
        self
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn with_current_directory(mut self, directory: impl Into<PathBuf>) -> Self {
        self.directory = Some(directory.into());
        self
    }

    /// The host supplies access roots; the child receives only the tool request.
    pub fn with_isolation(
        mut self,
        runner: PathBuf,
        read_paths: Vec<PathBuf>,
        write_paths: Vec<PathBuf>,
    ) -> Self {
        self.isolation = Some(ProcessIsolation {
            runner,
            read_paths,
            input_paths: Vec::new(),
            write_paths,
        });
        self
    }

    /// Supply game inputs through read-only handles, without changing their ACLs.
    pub fn with_isolated_inputs(mut self, paths: Vec<PathBuf>) -> Self {
        if let Some(isolation) = &mut self.isolation {
            isolation.input_paths = paths;
        }
        self
    }

    /// Read a file response when produced, retaining stdout JSON for legacy tools.
    pub fn with_response_file(mut self, path: impl Into<PathBuf>) -> Self {
        self.response_file = Some(path.into());
        self
    }

    /// Bound stdout and stderr together; file responses are streamed separately.
    pub fn with_maximum_output_bytes(mut self, maximum: u64) -> Self {
        self.maximum_output_bytes = Some(maximum);
        self
    }

    /// Keep structured tool diagnostics on a nonzero exit. Only an explicit
    /// `success: false` response is returned; callers must still check the protocol.
    pub(crate) fn with_failure_response(mut self) -> Self {
        self.preserve_failure_response = true;
        self
    }

    pub async fn execute<Req: Serialize, Res: DeserializeOwned + Send + 'static>(
        &self,
        request: &Req,
        cancellation: &CancellationToken,
    ) -> Result<Res, EngineError> {
        if cancellation.is_cancelled() {
            return Err(EngineError::Cancelled);
        }
        if !self.executable.is_file() {
            return Err(EngineError::ToolMissing(self.executable.clone()));
        }
        let (executable, input) = if let Some(isolation) = &self.isolation {
            if !isolation.runner.is_file() {
                return Err(EngineError::Isolation(format!(
                    "无法启动执行隔离，缺少原生工具：{}。请修复运行工具，或在程序设置中明确关闭隔离。",
                    isolation.runner.display()
                )));
            }
            let arguments = self
                .arguments
                .iter()
                .map(|argument| {
                    argument.to_str().ok_or_else(|| {
                        EngineError::Protocol("隔离执行参数必须是有效的 Unicode。".into())
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            let input = serde_json::to_vec(&serde_json::json!({
                "protocolVersion": PROTOCOL_VERSION,
                "executable": self.executable,
                "arguments": arguments,
                "directory": self.directory,
                "readPaths": isolation.read_paths,
                "inputPaths": isolation.input_paths,
                "writePaths": isolation.write_paths,
                "input": request,
            }))?;
            (&isolation.runner, input)
        } else {
            (&self.executable, serde_json::to_vec(request)?)
        };
        let response_file = self.response_file.clone();
        let maximum_output_bytes = self.maximum_output_bytes;
        let preserve_failure_response = self.preserve_failure_response;
        let mut command = Command::new(executable);
        if self.isolation.is_some() {
            command.arg("--control-stdin");
        } else {
            command.args(&self.arguments);
        }
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(self.isolation.is_none());
        if let Some(directory) = &self.directory {
            command.current_dir(directory);
        }
        #[cfg(windows)]
        command.creation_flags(WIN32_CREATE_NO_WINDOW); // CLI helpers must not open consoles.
        #[cfg(unix)]
        command.process_group(0);
        let budget_pause = pause_current_budget();
        let _permit = tokio::select! {
            biased;
            _ = cancellation.cancelled() => return Err(EngineError::Cancelled),
            permit = process_slots().acquire() => {
                permit.map_err(|_| EngineError::Protocol("子进程调度器不可用。".into()))?
            }
        };
        drop(budget_pause);
        let isolated = self.isolation.is_some();
        let mut child = command.spawn().map_err(|error| {
            if isolated {
                EngineError::Isolation(format!("无法启动原生隔离工具 {}：{error}。请修复运行工具与目录权限，或在程序设置中关闭隔离后重试。", executable.display()))
            } else {
                EngineError::Io(error)
            }
        })?;
        let process_id = child.id();
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| EngineError::Protocol("工具的标准输入不可用。".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| EngineError::Protocol("工具的标准输出不可用。".into()))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| EngineError::Protocol("工具的错误输出不可用。".into()))?;
        let mut control = Some(stdin);
        let mut process_finished = false;
        let work = async {
            // Read both pipes while writing; fail promptly if a helper floods either pipe.
            let write = async {
                let mut stdin = control.take().expect("child stdin");
                stdin.write_all(&input).await?;
                if isolated {
                    stdin.write_all(b"\n").await?;
                    control = Some(stdin);
                }
                Ok::<(), std::io::Error>(())
            };
            let (written, stdout, stderr, status) = tokio::try_join!(
                async { Ok::<_, EngineError>(write.await) },
                read_limited(stdout, maximum_output_bytes.unwrap_or(MAX_RESPONSE_BYTES)),
                read_limited(stderr, maximum_output_bytes.unwrap_or(DEFAULT_STDERR_BYTES)),
                async { Ok::<_, EngineError>(child.wait().await?) },
            )?;
            process_finished = true;
            if maximum_output_bytes
                .is_some_and(|maximum| stdout.len() as u64 + stderr.len() as u64 > maximum)
            {
                return Err(EngineError::Protocol("工具输出超过大小限制。".into()));
            }
            if !status.success() {
                let diagnostic = String::from_utf8_lossy(&stderr);
                if isolated
                    && let Some((_, reason)) = diagnostic.split_once("GameL10n-Isolation-Failed: ")
                {
                    return Err(EngineError::Isolation(reason.trim().to_owned()));
                }
                if isolated
                    && let Some(code) = status
                        .code()
                        .filter(|code| *code as u32 >= WINDOWS_ABNORMAL_EXIT_MIN)
                {
                    return Err(EngineError::Isolation(format!(
                        "原生隔离工具无法正常启动或运行，Windows 退出码为 0x{:08X}。请恢复完整运行工具并检查 Windows 环境，或在程序设置中关闭执行隔离后重试。",
                        code as u32
                    )));
                }
                if preserve_failure_response
                    && let Some(value) = read_response::<serde_json::Value>(response_file).await?
                    && value.get("success").and_then(serde_json::Value::as_bool) == Some(false)
                {
                    return Ok(serde_json::from_value(value)?);
                }
                let parsed = serde_json::from_slice::<serde_json::Value>(&stdout).ok();
                let message = parsed
                    .as_ref()
                    .and_then(|value| value.get("error"))
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned)
                    .unwrap_or_else(|| {
                        String::from_utf8_lossy(&stderr)
                            .chars()
                            .take(MAX_STDERR_DIAGNOSTIC_CHARS)
                            .collect()
                    });
                return Err(EngineError::Rejected(if message.is_empty() {
                    format!("工具退出：{status}")
                } else {
                    message
                }));
            }
            written?;
            if let Some(response) = read_response(response_file).await? {
                return Ok(response);
            }
            Ok(serde_json::from_slice::<Res>(&stdout)?)
        };
        let mut work = Box::pin(work);
        let result = tokio::select! {
            biased;
            () = cancellation.cancelled() => Err(EngineError::Cancelled),
            result = tokio::time::timeout(self.timeout, &mut work) => result.unwrap_or(Err(EngineError::Timeout)),
        };
        drop(work);
        drop(control);
        if !process_finished && let Some(pid) = process_id {
            // EOF lets the runner stop its job and revoke its ACLs before forced termination.
            if isolated
                && matches!(
                    tokio::time::timeout(ISOLATION_SHUTDOWN_GRACE, child.wait()).await,
                    Ok(Ok(_))
                )
            {
                return result;
            }
            terminate_process_tree(pid).await;
            if let Err(error) = child.kill().await
                && !matches!(child.try_wait(), Ok(Some(_)))
            {
                eprintln!("无法确认工具进程 {pid} 已退出：{error}");
            }
        }
        result
    }
}

async fn read_response<T: DeserializeOwned + Send + 'static>(
    path: Option<PathBuf>,
) -> Result<Option<T>, EngineError> {
    let Some(path) = path else {
        return Ok(None);
    };
    tokio::task::spawn_blocking(move || {
        use std::io::Read;
        let max_mib = MAX_RESPONSE_BYTES / (1024 * 1024);
        let metadata = match path.symlink_metadata() {
            Ok(metadata) => metadata,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        if !metadata.is_file() || crate::discovery::is_link(&metadata) {
            return Err(EngineError::Protocol("适配器响应必须为普通文件。".into()));
        }
        let file = std::fs::File::open(path)?;
        if file.metadata()?.len() > MAX_RESPONSE_BYTES {
            return Err(EngineError::Protocol(format!(
                "适配器文件响应超过 {max_mib} MiB。"
            )));
        }
        let mut reader = BufReader::new(file).take(MAX_RESPONSE_BYTES + 1);
        let response = serde_json::from_reader(&mut reader)?;
        if reader.limit() == 0 {
            return Err(EngineError::Protocol(format!(
                "适配器文件响应超过 {max_mib} MiB。"
            )));
        }
        Ok(Some(response))
    })
    .await
    .map_err(|error| EngineError::Protocol(format!("响应读取任务失败：{error}")))?
}

async fn terminate_process_tree(process_id: u32) {
    #[cfg(windows)]
    {
        let mut command = Command::new("taskkill.exe");
        command
            .args(["/PID", &process_id.to_string(), "/T", "/F"])
            .creation_flags(WIN32_CREATE_NO_WINDOW)
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let _ = tokio::time::timeout(Duration::from_secs(5), command.status()).await;
    }
    #[cfg(unix)]
    {
        let _ = Command::new("kill")
            .args(["-KILL", "--", &format!("-{process_id}")])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await;
    }
}

async fn read_limited(stream: impl AsyncRead + Unpin, limit: u64) -> Result<Vec<u8>, EngineError> {
    let mut bytes = Vec::new();
    stream.take(limit + 1).read_to_end(&mut bytes).await?;
    if bytes.len() as u64 > limit {
        return Err(EngineError::Protocol("工具输出超过大小限制。".into()));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    #[tokio::test]
    async fn missing_isolation_runner_never_falls_back_to_the_original_program() {
        let directory = tempfile::tempdir().unwrap();
        let process = ToolProcess::new(std::env::current_exe().unwrap()).with_isolation(
            directory.path().join("missing-runner.exe"),
            vec![],
            vec![],
        );
        let error = process
            .execute::<_, Value>(&json!({}), &CancellationToken::new())
            .await
            .unwrap_err();
        assert!(
            matches!(error, EngineError::Isolation(message) if message.contains("程序设置") && message.contains("缺少原生工具"))
        );
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn isolation_startup_errors_keep_only_the_specific_reason() {
        let paths = crate::ToolPaths::development();
        let directory = tempfile::tempdir().unwrap();
        let process = ToolProcess::new(paths.python()).with_isolation(
            paths.isolated_runner(),
            vec![directory.path().join("missing-input")],
            vec![],
        );
        let error = process
            .execute::<_, Value>(&json!({}), &CancellationToken::new())
            .await
            .unwrap_err();
        assert!(
            matches!(&error, EngineError::Isolation(message) if message.contains("missing-input") && message.lines().count() == 1),
            "{error}"
        );
    }

    fn python(script: &str) -> Option<ToolProcess> {
        let executable = crate::ToolPaths::development().python();
        executable.is_file().then(|| {
            ToolProcess::new(executable).with_arguments([
                "-I".into(),
                "-X".into(),
                "utf8".into(),
                "-c".into(),
                script.into(),
            ])
        })
    }

    #[tokio::test]
    async fn failed_native_tools_keep_diagnostics_without_accepting_false_success() {
        let Some(process) = python(
            "import json,sys; r=json.load(sys.stdin); json.dump({'success':r['success'],'diagnostic':{'errorType':'InvalidHeader'}},open(r['responseFile'],'w')); sys.exit(1)",
        ) else {
            return;
        };
        let directory = tempfile::tempdir().expect("fixture");
        let path = directory.path().join("response.json");
        let process = process.with_response_file(&path).with_failure_response();
        let failure: Value = process
            .execute(
                &json!({"responseFile":path,"success":false}),
                &CancellationToken::new(),
            )
            .await
            .expect("structured failure");
        assert_eq!(failure["diagnostic"]["errorType"], "InvalidHeader");
        let result = process
            .execute::<_, Value>(
                &json!({"responseFile":path,"success":true}),
                &CancellationToken::new(),
            )
            .await;
        assert!(matches!(result, Err(EngineError::Rejected(_))));
    }

    #[tokio::test]
    async fn file_responses_can_exceed_the_diagnostic_output_budget() {
        let Some(process) = python(
            "import json,sys; r=json.load(sys.stdin); f=open(r['responseFile'],'w',encoding='utf-8'); json.dump({'payload':'x'*(9*1024*1024)},f); f.close(); print('small diagnostic')",
        ) else {
            return;
        };
        let directory = tempfile::tempdir().expect("fixture");
        let path = directory.path().join("response.json");
        let response: Value = process
            .with_maximum_output_bytes(8 * 1024 * 1024)
            .with_response_file(&path)
            .execute(&json!({"responseFile":path}), &CancellationToken::new())
            .await
            .expect("streamed file response");
        assert_eq!(
            response["payload"].as_str().expect("payload").len(),
            9 * 1024 * 1024
        );
    }

    #[tokio::test]
    async fn adapters_that_omit_the_response_file_can_return_legacy_stdout_json() {
        let Some(process) =
            python("import json,sys; json.load(sys.stdin); print('{\"legacy\":true}')")
        else {
            return;
        };
        let directory = tempfile::tempdir().expect("fixture");
        let path = directory.path().join("response.json");
        let response: Value = process
            .with_response_file(&path)
            .execute(&json!({"responseFile":path}), &CancellationToken::new())
            .await
            .expect("stdout fallback");
        assert_eq!(response["legacy"], true);
    }

    #[tokio::test]
    async fn stdout_and_stderr_share_one_output_budget() {
        let Some(process) =
            python("import sys; sys.stdout.write(' '*4096); sys.stderr.write(' '*4097)")
        else {
            return;
        };
        let result = process
            .with_maximum_output_bytes(8192)
            .execute::<_, Value>(&json!({}), &CancellationToken::new())
            .await;
        assert!(
            matches!(result, Err(EngineError::Protocol(message)) if message.contains("大小限制"))
        );
    }

    #[tokio::test]
    async fn invalid_file_responses_cannot_be_hidden_by_successful_stdout() {
        let Some(process) = python(
            "import json,sys; r=json.load(sys.stdin); open(r['responseFile'],'w').write('invalid'); print('{}')",
        ) else {
            return;
        };
        let directory = tempfile::tempdir().expect("fixture");
        let path = directory.path().join("response.json");
        let result = process
            .with_response_file(&path)
            .execute::<_, Value>(&json!({"responseFile":path}), &CancellationToken::new())
            .await;
        assert!(matches!(result, Err(EngineError::Json(_))));
    }

    fn test_script_process(python_script: &str, sh_script: &str) -> Option<ToolProcess> {
        if let Some(process) = python(python_script) {
            return Some(process);
        }
        #[cfg(unix)]
        {
            let sh = PathBuf::from("/bin/sh");
            if sh.is_file() {
                return Some(ToolProcess::new(sh).with_arguments(["-c".into(), sh_script.into()]));
            }
        }
        let _ = sh_script;
        None
    }

    #[tokio::test]
    async fn process_budget_excludes_slot_queue_wait_from_timeout_and_elapsed() {
        let Some(process) = test_script_process(
            "import json,sys; json.load(sys.stdin); print('{\"ok\":true}')",
            "cat >/dev/null; printf '{\"ok\":true}'",
        ) else {
            return;
        };
        let total = std::thread::available_parallelism()
            .map(|count| count.get().clamp(2, 6))
            .unwrap_or(4);
        let permits = process_slots()
            .acquire_many(total as u32)
            .await
            .expect("saturate slots");
        let budget = ProcessBudget::new(Duration::from_millis(250));
        let cancellation = CancellationToken::new();
        let deadline = cancellation.child_token();
        let release = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(450)).await;
            drop(permits);
        });
        let payload = json!({});
        let operation = ProcessBudget::scope(
            Arc::clone(&budget),
            process.execute::<_, Value>(&payload, &deadline),
        );
        tokio::pin!(operation);
        let response = tokio::select! {
            result = &mut operation => result.expect("queued execution should not exhaust budget"),
            () = budget.expired() => {
                deadline.cancel();
                let _ = operation.await;
                panic!("slot queueing must not exhaust ProcessBudget");
            }
        };
        release.await.expect("release task");
        assert_eq!(response["ok"], true);
        assert!(
            budget.active_elapsed() < Duration::from_millis(250),
            "active elapsed {:?} should exclude 450ms slot wait",
            budget.active_elapsed()
        );
    }

    #[tokio::test]
    async fn process_budget_still_times_out_active_execution_after_slot_acquired() {
        let Some(process) = test_script_process(
            "import time; time.sleep(2); print('{}')",
            "sleep 2; printf '{}'",
        ) else {
            return;
        };
        let budget = ProcessBudget::new(Duration::from_millis(120));
        let cancellation = CancellationToken::new();
        let deadline = cancellation.child_token();
        let payload = json!({});
        let operation = ProcessBudget::scope(
            Arc::clone(&budget),
            process.execute::<_, Value>(&payload, &deadline),
        );
        tokio::pin!(operation);
        let timed_out = tokio::select! {
            _ = &mut operation => false,
            () = budget.expired() => {
                deadline.cancel();
                let _ = operation.await;
                true
            }
        };
        assert!(
            timed_out,
            "hanging process must still hit ProcessBudget timeout"
        );
    }
}
