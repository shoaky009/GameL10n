//! Project-local UI messages. Provider diagnostics are written separately by their owner.
use crate::{AppEvent, Application, CoreError};
use std::{
    fs::{self, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
};
use tokio::sync::mpsc;

#[derive(Clone, Debug)]
pub(crate) struct FileProgress {
    sender: mpsc::Sender<String>,
}

impl FileProgress {
    pub(crate) fn file(&self, action: &str, path: &Path, name: &str) {
        let size = fs::metadata(path)
            .ok()
            .filter(|metadata| metadata.is_file())
            .map(|metadata| {
                let bytes = metadata.len();
                let units = ["B", "KB", "MB", "GB", "TB"];
                let mut value = bytes as f64;
                let mut unit = 0;
                while value >= 1024.0 && unit < units.len() - 1 {
                    value /= 1024.0;
                    unit += 1;
                }
                if unit == 0 {
                    format!("（{bytes} B）")
                } else {
                    format!("（{value:.2} {}）", units[unit])
                }
            })
            .unwrap_or_default();
        let _ = self.sender.blocking_send(format!("{action}：{name}{size}"));
    }
}

impl Application {
    pub(crate) async fn run_file_task<T: Send + 'static>(
        &self,
        id: &str,
        operation: &str,
        task: impl FnOnce(FileProgress) -> Result<T, CoreError> + Send + 'static,
    ) -> Result<T, CoreError> {
        // Bound pending messages so processing many small files cannot accumulate UI work.
        let (sender, mut receiver) = mpsc::channel(32);
        let mut worker = tokio::task::spawn_blocking(move || task(FileProgress { sender }));
        let result = loop {
            tokio::select! {
                result = &mut worker => break result,
                Some(message) = receiver.recv() => self.progress(id, operation, &message).await,
            }
        };
        while let Ok(message) = receiver.try_recv() {
            self.progress(id, operation, &message).await;
        }
        result?
    }

    pub async fn log(&self, id: &str, message: &str) -> Result<(), CoreError> {
        let directory = if id.is_empty() {
            self.data_directory().join("logs")
        } else {
            self.project_directory(id)?.join("logs")
        };
        let now = chrono::Local::now();
        let line = format!("[{}] {}", now.format("%H:%M:%S"), message);
        let path = directory.join(format!("{}.log", now.format("%Y%m%d")));
        let saved = line.clone();
        let _guard = self.log_gate.lock().await;
        tokio::task::spawn_blocking(move || -> Result<(), CoreError> {
            fs::create_dir_all(directory)?;
            let mut file = OpenOptions::new().create(true).append(true).open(path)?;
            writeln!(file, "{saved}")?;
            Ok(())
        })
        .await??;
        let _ = self.events.send(AppEvent::Log {
            project_id: id.into(),
            line,
        });
        Ok(())
    }

    pub async fn log_tail(&self, id: &str) -> Result<String, CoreError> {
        let directory = if id.is_empty() {
            self.data_directory().join("logs")
        } else {
            self.project_directory(id)?.join("logs")
        };
        let _guard = self.log_gate.lock().await;
        tokio::task::spawn_blocking(move || -> Result<String, CoreError> {
            if !directory.is_dir() {
                return Ok(String::new());
            }
            let mut files = fs::read_dir(directory)?
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .filter(|path| {
                    path.extension().is_some_and(|e| e == "log")
                        && path.file_stem().is_some_and(|name| {
                            name.to_string_lossy().chars().all(|c| c.is_ascii_digit())
                        })
                })
                .collect::<Vec<_>>();
            files.sort();
            let Some(path) = files.last() else {
                return Ok(String::new());
            };
            let mut file = fs::File::open(path)?;
            let length = file.metadata()?.len();
            file.seek(SeekFrom::Start(length.saturating_sub(50_000)))?;
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes)?;
            let text = String::from_utf8_lossy(&bytes);
            let mut in_message = false;
            let mut lines = Vec::new();
            for line in text.lines() {
                if line.starts_with('[') && line.as_bytes().get(3) == Some(&b':') {
                    in_message = true;
                } else if serde_json::from_str::<serde_json::Value>(line).is_ok() {
                    in_message = false;
                }
                if in_message {
                    lines.push(line);
                }
            }
            Ok(lines.join("\n"))
        })
        .await?
    }
}
