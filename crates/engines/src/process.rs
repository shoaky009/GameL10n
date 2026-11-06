use std::{ffi::OsString, path::PathBuf, process::Stdio, time::Duration};

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
}

impl ToolProcess {
    pub fn new(executable: impl Into<PathBuf>) -> Self {
        Self {
            executable: executable.into(),
            arguments: Vec::new(),
            timeout: Duration::from_secs(300),
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

    pub async fn execute<Req: Serialize, Res: DeserializeOwned>(
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
        let mut command = Command::new(&self.executable);
        command
            .args(&self.arguments)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(windows)]
        command.creation_flags(0x08000000); // CREATE_NO_WINDOW: CLI helpers must not open consoles.
        let mut child = command.spawn()?;
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
                read_limited(stdout, 256 * 1024 * 1024),
                read_limited(stderr, 2 * 1024 * 1024),
                async { Ok::<_, EngineError>(child.wait().await?) },
            )?;
            if !status.success() {
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
            Ok(serde_json::from_slice::<Res>(&stdout)?)
        };
        tokio::select! {
            biased;
            () = cancellation.cancelled() => Err(EngineError::Cancelled),
            result = tokio::time::timeout(self.timeout, work) => result.map_err(|_| EngineError::Timeout)?,
        }
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
