use std::{ffi::OsString, io::BufReader, path::PathBuf, process::Stdio, time::Duration};

use serde::{Serialize, de::DeserializeOwned};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::Command,
};
use tokio_util::sync::CancellationToken;

use crate::EngineError;

/// JSON subprocess execution. A separate process is not a security sandbox.
#[derive(Clone, Debug)]
pub struct ToolProcess {
    executable: PathBuf,
    arguments: Vec<OsString>,
    timeout: Duration,
    directory: Option<PathBuf>,
    response_file: Option<PathBuf>,
    maximum_output_bytes: Option<u64>,
    preserve_failure_response: bool,
}

impl ToolProcess {
    pub fn new(executable: impl Into<PathBuf>) -> Self {
        Self {
            executable: executable.into(),
            arguments: Vec::new(),
            timeout: Duration::from_secs(300),
            directory: None,
            response_file: None,
            maximum_output_bytes: None,
            preserve_failure_response: false,
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
        let input = serde_json::to_vec(request)?;
        let response_file = self.response_file.clone();
        let maximum_output_bytes = self.maximum_output_bytes;
        let preserve_failure_response = self.preserve_failure_response;
        let mut command = Command::new(&self.executable);
        command
            .args(&self.arguments)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if let Some(directory) = &self.directory {
            command.current_dir(directory);
        }
        #[cfg(windows)]
        command.creation_flags(0x08000000); // CREATE_NO_WINDOW: CLI helpers must not open consoles.
        #[cfg(unix)]
        command.process_group(0);
        let mut child = command.spawn()?;
        let process_id = child.id();
        let mut stdin = child
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
        let work = async move {
            // Read both pipes while writing; fail promptly if a helper floods either pipe.
            let write = async move {
                let result = stdin.write_all(&input).await;
                drop(stdin);
                result
            };
            let (written, stdout, stderr, status) = tokio::try_join!(
                async { Ok::<_, EngineError>(write.await) },
                read_limited(stdout, maximum_output_bytes.unwrap_or(256 * 1024 * 1024)),
                read_limited(stderr, maximum_output_bytes.unwrap_or(2 * 1024 * 1024)),
                async { Ok::<_, EngineError>(child.wait().await?) },
            )?;
            if maximum_output_bytes
                .is_some_and(|maximum| stdout.len() as u64 + stderr.len() as u64 > maximum)
            {
                return Err(EngineError::Protocol("工具输出超过大小限制。".into()));
            }
            if !status.success() {
                if preserve_failure_response {
                    let file = response_file.clone();
                    let diagnostic = tokio::task::spawn_blocking(move || {
                        let Some(path) = file else {
                            return Ok(None);
                        };
                        match std::fs::symlink_metadata(&path) {
                            Ok(metadata) if metadata.is_symlink() || !metadata.is_file() => {
                                Err(EngineError::Protocol("适配器响应必须为普通文件。".into()))
                            }
                            Ok(_) => {
                                let value: serde_json::Value = serde_json::from_reader(
                                    BufReader::new(std::fs::File::open(path)?),
                                )?;
                                Ok(Some(value))
                            }
                            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
                            Err(error) => Err(error.into()),
                        }
                    })
                    .await
                    .map_err(|error| {
                        EngineError::Protocol(format!("响应读取任务失败：{error}"))
                    })??;
                    if let Some(value) = diagnostic
                        && value.get("success").and_then(serde_json::Value::as_bool) == Some(false)
                    {
                        return Ok(serde_json::from_value(value)?);
                    }
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
                            .take(4000)
                            .collect()
                    });
                return Err(EngineError::Rejected(if message.is_empty() {
                    format!("工具退出：{status}")
                } else {
                    message
                }));
            }
            written?;
            if let Some(path) = response_file {
                match std::fs::symlink_metadata(&path) {
                    Ok(metadata) if metadata.is_symlink() || !metadata.is_file() => {
                        return Err(EngineError::Protocol("适配器响应必须为普通文件。".into()));
                    }
                    Ok(_) => {
                        return tokio::task::spawn_blocking(move || {
                            let file = std::fs::File::open(path)?;
                            Ok::<Res, EngineError>(serde_json::from_reader(BufReader::new(file))?)
                        })
                        .await
                        .map_err(|error| {
                            EngineError::Protocol(format!("响应读取任务失败：{error}"))
                        })?;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error.into()),
                }
            }
            Ok(serde_json::from_slice::<Res>(&stdout)?)
        };
        let mut work = Box::pin(work);
        let result = tokio::select! {
            biased;
            () = cancellation.cancelled() => Err(EngineError::Cancelled),
            result = tokio::time::timeout(self.timeout, &mut work) => result.unwrap_or(Err(EngineError::Timeout)),
        };
        if matches!(result, Err(EngineError::Cancelled | EngineError::Timeout))
            && let Some(process_id) = process_id
        {
            terminate_process_tree(process_id).await;
        }
        result
    }
}

async fn terminate_process_tree(process_id: u32) {
    #[cfg(windows)]
    {
        let mut command = Command::new("taskkill.exe");
        command
            .args(["/PID", &process_id.to_string(), "/T", "/F"])
            .creation_flags(0x08000000)
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
}
