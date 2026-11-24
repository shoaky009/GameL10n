//! Project-local UI messages. Provider diagnostics are written separately by their owner.
use crate::{AppEvent, Application, CoreError};
use std::{
    fs::{self, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
};
use tokio::sync::mpsc;

#[derive(Clone, Copy)]
pub(crate) enum LogSource {
    Application,
    Model,
}

impl LogSource {
    pub(crate) fn from_model(from_model: bool) -> Self {
        if from_model {
            Self::Model
        } else {
            Self::Application
        }
    }

    pub(crate) fn for_error(error: &CoreError) -> Self {
        Self::from_model(matches!(error, CoreError::WithModelOutput(_)))
    }
}

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
        self.log_from(id, message, LogSource::Application).await
    }

    pub async fn log_error(&self, id: &str, error: &CoreError) -> Result<(), CoreError> {
        self.log_from(id, &error.to_string(), LogSource::for_error(error))
            .await
    }

    pub(crate) async fn lock_log(&self, id: &str) -> tokio::sync::OwnedMutexGuard<()> {
        let mutex = {
            let mut gates = self.log_gates.lock().await;
            gates.retain(|_, lock| lock.strong_count() > 0);
            if let Some(lock) = gates.get(id).and_then(std::sync::Weak::upgrade) {
                lock
            } else {
                let lock = std::sync::Arc::new(tokio::sync::Mutex::new(()));
                gates.insert(id.to_owned(), std::sync::Arc::downgrade(&lock));
                lock
            }
        };
        mutex.lock_owned().await
    }

    pub(crate) async fn log_from(
        &self,
        id: &str,
        message: &str,
        source: LogSource,
    ) -> Result<(), CoreError> {
        let message = match source {
            LogSource::Application => message.trim_end_matches(['.', '。']),
            LogSource::Model => message,
        };
        let directory = if id.is_empty() {
            self.data_directory().join("logs")
        } else {
            self.project_directory(id)?.join("logs")
        };
        let now = chrono::Local::now();
        let line = format!("[{}] {}", now.format("%H:%M:%S"), message);
        let path = directory.join(format!("{}.log", now.format("%Y%m%d")));
        let saved = line.clone();
        let _guard = self.lock_log(id).await;
        tokio::task::spawn_blocking(move || -> Result<(), CoreError> {
            fs::create_dir_all(&directory)
                .map_err(|error| gamel10n_translation::files::io_error(&directory, error))?;
            let mut file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .map_err(|error| gamel10n_translation::files::io_error(&path, error))?;
            writeln!(file, "{saved}")
                .map_err(|error| gamel10n_translation::files::io_error(&path, error))?;
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
        let _guard = self.lock_log(id).await;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn application_log_omits_terminal_periods_in_the_file_and_event() {
        let root = tempfile::tempdir().unwrap();
        let app = Application::open(root.path().join("data"), &crate::ToolPaths::new("tools"))
            .await
            .unwrap();
        let mut events = app.subscribe();
        for (message, expected) in [
            ("已加载资源。", "已加载资源"),
            ("Loaded version 1.2.", "Loaded version 1.2"),
            ("已加载资源。\n等待确认。", "已加载资源。\n等待确认"),
        ] {
            app.log("", message).await.unwrap();
            let AppEvent::Log { line, .. } = events.try_recv().unwrap() else {
                panic!("expected a log event");
            };
            assert_eq!(line.split_once("] ").unwrap().1, expected);
            assert!(app.log_tail("").await.unwrap().ends_with(&line));
        }
    }
}
