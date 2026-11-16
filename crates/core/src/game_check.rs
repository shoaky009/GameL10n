//! Optional launch and visible-translation checks, independent of patch acceptance.

pub mod protocol;
pub use protocol::{
    CheckMode, CheckResult, CheckSettings, CheckStatus, Observation, SceneStep, TextRegion,
};

use crate::{Application, CoreError, launch::validate_configuration};
use protocol::{CheckEvent, CheckRequest, normalize_text};
use std::{collections::BTreeSet, process::Stdio, time::Duration};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

static DESKTOP_CHECK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

pub fn parse_steps(text: &str) -> Result<Vec<SceneStep>, CoreError> {
    let mut steps = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let fields: Vec<_> = line.split_whitespace().collect();
        if fields.is_empty() {
            continue;
        }
        let step = match fields.as_slice() {
            ["等待", ms] => ms
                .parse()
                .ok()
                .map(|milliseconds| SceneStep::Wait { milliseconds }),
            ["按键", key] => Some(SceneStep::Key { key: (*key).into() }),
            ["点击", x, y] => x
                .parse()
                .ok()
                .zip(y.parse().ok())
                .map(|(x, y)| SceneStep::Click { x, y }),
            _ => None,
        }
        .ok_or_else(|| {
            CoreError::InvalidData(format!(
                "第 {} 行操作无效，请使用“等待 1000”“按键 Enter”或“点击 50 50”。",
                index + 1
            ))
        })?;
        steps.push(step);
    }
    CheckSettings {
        steps: steps.clone(),
        ..Default::default()
    }
    .validate()
    .map_err(CoreError::InvalidData)?;
    Ok(steps)
}

pub fn format_steps(steps: &[SceneStep]) -> String {
    steps
        .iter()
        .map(|step| match step {
            SceneStep::Wait { milliseconds } => format!("等待 {milliseconds}"),
            SceneStep::Key { key } => format!("按键 {key}"),
            SceneStep::Click { x, y } => format!("点击 {x} {y}"),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

impl Application {
    pub async fn game_check_settings(&self, id: &str) -> Result<CheckSettings, CoreError> {
        let project = self.store.get(id).await?;
        let settings: CheckSettings = project
            .extra
            .get("gameCheck")
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .map_err(|_| CoreError::InvalidData("运行检查设置无法读取，请重新保存设置。".into()))?
            .unwrap_or_default();
        settings.validate().map_err(CoreError::InvalidData)?;
        Ok(settings)
    }

    pub async fn save_game_check_settings(
        &self,
        id: &str,
        mut settings: CheckSettings,
    ) -> Result<(), CoreError> {
        settings.validate().map_err(CoreError::InvalidData)?;
        let mut seen = BTreeSet::new();
        settings.unit_ids.retain(|id| seen.insert(id.clone()));
        let _guard = self.lock_project(id).await?;
        let _lifecycle = self.project_lifecycle.lock().await;
        if self
            .task_state(id)
            .await
            .is_some_and(|state| state.running && state.operation == "game-check")
        {
            return Err(CoreError::ProjectBusy);
        }
        self.ensure_writable().await?;
        let mut project = self.store.get(id).await?;
        project.extra.insert(
            "gameCheck".into(),
            serde_json::to_value(settings).map_err(|e| CoreError::InvalidData(e.to_string()))?,
        );
        self.store.save(&project).await?;
        Ok(())
    }

    pub async fn game_check_expected(
        &self,
        id: &str,
        ids: &[String],
    ) -> Result<Vec<String>, CoreError> {
        if ids.is_empty() {
            return Err(CoreError::InvalidData(
                "请先在文本页选择需要检查的译文。".into(),
            ));
        }
        if ids.len() > 8 {
            return Err(CoreError::InvalidData(
                "每次最多选择 8 条待检查文本。".into(),
            ));
        }
        let translations = self
            .saved_translations(id, Some(ids), false)
            .await
            .map_err(|_| {
                CoreError::InvalidData("所选文本尚无有效译文，请先翻译或保存校对结果。".into())
            })?;
        let analysis = self
            .cached_analysis(id)
            .await?
            .ok_or_else(|| CoreError::InvalidData("请先提取游戏文本。".into()))?;
        let expected = analysis
            .extraction()
            .units
            .iter()
            .filter(|u| ids.contains(&u.id))
            .filter_map(|u| {
                let translated = translations.get(&u.id)?;
                let normalized = normalize_text(translated);
                (normalized.chars().filter(|c| c.is_alphanumeric()).count() >= 4
                    && normalized != normalize_text(&u.source_text))
                .then(|| translated.clone())
            })
            .collect::<Vec<_>>();
        if expected.is_empty() {
            return Err(CoreError::InvalidData(
                "所选译文需区别于原文，并至少包含 4 个文字或数字。".into(),
            ));
        }
        Ok(expected)
    }

    /// Emits only current observations; screenshots and check results are not persisted.
    pub async fn check_game(
        &self,
        id: &str,
        mode: CheckMode,
        stop: &crate::CancellationToken,
        on_result: impl Fn(CheckResult) + Send + Sync,
    ) -> Result<CheckResult, CoreError> {
        let _desktop = DESKTOP_CHECK.try_lock().map_err(|_| {
            CoreError::InvalidData("已有运行检查正在使用桌面，请等待检查结束。".into())
        })?;
        let cancellation = self
            .begin(id, "game-check", "正在检查游戏运行情况。")
            .await?;
        let mut result = CheckResult::default();
        let operation = async {
            if stop.is_cancelled() {
                return Err(gamel10n_engines::EngineError::Cancelled.into());
            }
            let settings = self.game_check_settings(id).await?;
            let project = self.store.get(id).await?;
            let launch = self.launch_view(id).await?;
            if !launch.can_launch {
                return Err(CoreError::InvalidData(launch.reason));
            }
            let configuration = launch
                .configuration
                .ok_or_else(|| CoreError::InvalidData("请先设置游戏启动程序。".into()))?;
            let (executable, working_directory) =
                validate_configuration(&project.game_directory, &configuration)?;
            let game_executable = self
                .cached_analysis(id)
                .await
                .ok()
                .flatten()
                .and_then(|analysis| {
                    let original = analysis
                        .native()
                        .ok()?
                        .probe
                        .as_ref()?
                        .original_executable
                        .as_ref()?;
                    Some(project.game_directory.join(original))
                })
                .filter(|path| path.is_file());
            let expected = if mode.reads_text() {
                self.game_check_expected(id, &settings.unit_ids).await?
            } else {
                vec![]
            };
            let request = CheckRequest {
                mode,
                executable,
                working_directory,
                game_directory: project.game_directory,
                game_executable,
                arguments: configuration.arguments,
                settings,
                expected,
            };
            self.run_game_check_tool(&request, &cancellation, stop, &mut result, &on_result)
                .await
        }
        .await;
        if let Err(error) = operation {
            if stop.is_cancelled() {
                self.cancel(id).await;
            }
            let observation = Observation::new(
                CheckStatus::Inconclusive,
                if cancellation.is_cancelled() {
                    "检查已停止，游戏保持打开。".into()
                } else {
                    error.to_string()
                },
            );
            if mode.launches() && result.launch.status == CheckStatus::NotChecked {
                result.launch = observation;
            } else if mode.reads_text() {
                result.text = observation;
            } else {
                result.launch = observation;
            }
            on_result(result.clone());
        }
        // The task records completion, while the observation describes whether a check passed.
        self.finish(
            id,
            "game-check",
            Ok("运行检查已结束，结果见运行检查窗口。".into()),
        )
        .await?;
        Ok(result)
    }

    async fn run_game_check_tool(
        &self,
        request: &CheckRequest,
        cancellation: &crate::CancellationToken,
        stop: &crate::CancellationToken,
        result: &mut CheckResult,
        on_result: &impl Fn(CheckResult),
    ) -> Result<(), CoreError> {
        let executable = self.tools.root.join("game-check/GameL10n.GameCheck.exe");
        if !executable.is_file() {
            return Err(CoreError::InvalidData(
                "缺少运行检查组件，请使用完整发行包；开发环境请运行 scripts/build-game-check.ps1。"
                    .into(),
            ));
        }
        let mut command = tokio::process::Command::new(executable);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        #[cfg(windows)]
        command.creation_flags(0x08000000);
        let mut child = command.spawn()?;
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| CoreError::InvalidData("无法连接运行检查组件。".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| CoreError::InvalidData("无法读取运行检查结果。".into()))?;
        let launch_seconds = if request.mode.launches() {
            u64::from(request.settings.timeout_seconds)
        } else {
            0
        };
        let scene_milliseconds: u64 = if request.mode == CheckMode::LaunchAndText {
            request
                .settings
                .steps
                .iter()
                .map(|step| match step {
                    SceneStep::Wait { milliseconds } => u64::from(*milliseconds) + 100,
                    _ => 250,
                })
                .sum()
        } else {
            0
        };
        let timeout =
            Duration::from_secs(launch_seconds + if request.mode.reads_text() { 35 } else { 5 })
                + Duration::from_millis(scene_milliseconds);
        let execution = async {
            let mut input =
                serde_json::to_vec(request).map_err(|e| CoreError::InvalidData(e.to_string()))?;
            if input.len() > 65536 {
                return Err(CoreError::InvalidData(
                    "待检查文本过长，请减少选区。".into(),
                ));
            }
            input.push(b'\n');
            stdin.write_all(&input).await?;
            drop(stdin);
            let mut output = BufReader::new(stdout);
            let mut total = 0;
            loop {
                let mut line = Vec::new();
                use tokio::io::AsyncReadExt;
                let bytes = (&mut output)
                    .take(8193)
                    .read_until(b'\n', &mut line)
                    .await?;
                if bytes == 0 {
                    break;
                }
                total += bytes;
                if bytes > 8192 || total > 32768 {
                    return Err(CoreError::InvalidData("运行检查组件返回内容过长。".into()));
                }
                let event: CheckEvent = serde_json::from_slice(&line)
                    .map_err(|_| CoreError::InvalidData("运行检查组件返回了无效结果。".into()))?;
                match event {
                    CheckEvent::Launch(value) => result.launch = value,
                    CheckEvent::Text(value) => result.text = value,
                    CheckEvent::ErrorWindow(value) => {
                        let mut windows = self.game_check_errors.lock().await;
                        if windows.len() < 32 && !windows.contains(&value) {
                            windows.push(value);
                        }
                    }
                }
                on_result(result.clone());
            }
            if !child.wait().await?.success() {
                return Err(CoreError::InvalidData("运行检查组件异常退出。".into()));
            }
            if request.mode.launches() && result.launch.status == CheckStatus::NotChecked
                || request.mode.reads_text()
                    && (request.mode == CheckMode::Text
                        || result.launch.status == CheckStatus::Passed)
                    && result.text.status == CheckStatus::NotChecked
            {
                return Err(CoreError::InvalidData("运行检查未返回完整结果。".into()));
            }
            Ok(())
        };
        let completed = tokio::select! {
            biased;
            _ = cancellation.cancelled() => Err(gamel10n_engines::EngineError::Cancelled.into()),
            _ = stop.cancelled() => Err(gamel10n_engines::EngineError::Cancelled.into()),
            completed = tokio::time::timeout(timeout, execution) => completed.map_err(|_| CoreError::InvalidData("运行检查超时，请在游戏中确认当前状态。".into()))?,
        };
        if completed.is_err() {
            let _ = child.kill().await;
            let _ = child.wait().await;
        }
        completed
    }

    /// Closes only error dialogs identified during checks in this application session.
    pub async fn close_game_check_errors(&self) -> Result<(), CoreError> {
        let windows = std::mem::take(&mut *self.game_check_errors.lock().await);
        if windows.is_empty() {
            return Ok(());
        }
        let mut command =
            tokio::process::Command::new(self.tools.root.join("game-check/GameL10n.GameCheck.exe"));
        command
            .arg("--cleanup")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        #[cfg(windows)]
        command.creation_flags(0x08000000);
        let mut child = command.spawn()?;
        let cleanup = async {
            let mut stdin = child
                .stdin
                .take()
                .ok_or_else(|| CoreError::InvalidData("无法连接错误窗口清理组件。".into()))?;
            stdin
                .write_all(
                    &serde_json::to_vec(&windows)
                        .map_err(|e| CoreError::InvalidData(e.to_string()))?,
                )
                .await?;
            drop(stdin);
            child.wait().await?;
            Ok::<_, CoreError>(())
        };
        tokio::time::timeout(Duration::from_secs(4), cleanup)
            .await
            .map_err(|_| CoreError::InvalidData("错误窗口清理超时，请手动关闭游戏提示。".into()))?
    }
}

#[cfg(test)]
mod tests;
