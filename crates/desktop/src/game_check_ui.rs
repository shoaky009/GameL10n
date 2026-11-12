use crate::{Context, UiInput, view::App};
use gamel10n_core::{
    CancellationToken, CoreError,
    game_check::{
        CheckMode, CheckResult, CheckSettings, CheckStatus, Observation, TextRegion, format_steps,
        parse_steps,
    },
};
use slint::{ComponentHandle, ModelRc, VecModel};
use std::sync::atomic::Ordering;

pub struct PendingGameCheck(Context);
impl PendingGameCheck {
    pub fn start(ctx: &Context, ui: &crate::view::MainWindow) -> Option<Self> {
        let mut session = ctx.session.lock().unwrap_or_else(|e| e.into_inner());
        if session.game_check.is_some() {
            return None;
        }
        session.game_check = Some(CancellationToken::new());
        ui.global::<App>().set_check_running(true);
        ui.global::<App>().set_check_message("".into());
        Some(Self(ctx.clone()))
    }
}
impl Drop for PendingGameCheck {
    fn drop(&mut self) {
        let session = self.0.session.clone();
        let _ = self.0.window.upgrade_in_event_loop(move |ui| {
            session.lock().unwrap_or_else(|e| e.into_inner()).game_check = None;
            ui.global::<App>().set_check_running(false);
        });
    }
}

pub fn cancel(ctx: &Context) {
    if let Some(token) = &ctx
        .session
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .game_check
    {
        token.cancel();
    }
}

fn label(observation: &Observation) -> String {
    let status = match observation.status {
        CheckStatus::NotChecked => "未检查",
        CheckStatus::Passed => "通过",
        CheckStatus::Failed => "未通过",
        CheckStatus::Inconclusive => "无法确认",
    };
    if observation.message.is_empty() {
        status.into()
    } else {
        format!("{status} · {}", observation.message)
    }
}

fn display(ctx: &Context, id: &str, result: CheckResult) {
    let id = id.to_owned();
    ctx.update(move |ui| {
        let app = ui.global::<App>();
        if app.get_project_id().as_str() != id || app.get_check_project_id().as_str() != id {
            return;
        }
        app.set_check_launch_result(label(&result.launch).into());
        app.set_check_launch_failed(result.launch.status == CheckStatus::Failed);
        app.set_check_text_result(label(&result.text).into());
        app.set_check_text_failed(result.text.status == CheckStatus::Failed);
    });
}

pub async fn refresh(ctx: &Context, id: &str) -> Result<(), CoreError> {
    let host = ctx.app().await;
    let (settings, error) = match host.game_check_settings(id).await {
        Ok(settings) => (settings, String::new()),
        Err(error) => (CheckSettings::default(), error.to_string()),
    };
    let preview = host
        .game_check_expected(id, &settings.unit_ids)
        .await
        .map(|text| text.join("\n"))
        .unwrap_or_else(|e| e.to_string());
    let result = ctx
        .session
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .game_check_results
        .get(id)
        .cloned()
        .unwrap_or_default();
    let id_owned = id.to_owned();
    ctx.update(move |ui| {
        let app = ui.global::<App>();
        if app.get_project_id().as_str() != id_owned {
            return;
        }
        app.set_check_project_id(id_owned.into());
        app.set_check_timeout(settings.timeout_seconds as i32);
        app.set_check_language(
            ["zh-Hans", "zh-Hant", "ja", "en"]
                .iter()
                .position(|l| *l == settings.language)
                .unwrap_or(0) as i32,
        );
        app.set_check_left(settings.region.left as i32);
        app.set_check_top(settings.region.top as i32);
        app.set_check_width(settings.region.width as i32);
        app.set_check_height(settings.region.height as i32);
        app.set_check_steps(format_steps(&settings.steps).into());
        app.set_check_unit_ids(ModelRc::new(VecModel::from(
            settings
                .unit_ids
                .into_iter()
                .map(Into::into)
                .collect::<Vec<_>>(),
        )));
        app.set_check_expected(preview.chars().take(800).collect::<String>().into());
        app.set_check_message(error.into());
    });
    display(ctx, id, result);
    Ok(())
}

fn settings(
    input: &UiInput,
    steps: Vec<gamel10n_core::game_check::SceneStep>,
) -> Result<CheckSettings, CoreError> {
    let value = CheckSettings {
        timeout_seconds: input.check.timeout_seconds as u32,
        language: ["zh-Hans", "zh-Hant", "ja", "en"]
            .get(input.check.language as usize)
            .unwrap_or(&"")
            .to_string(),
        region: TextRegion {
            left: input.check.left as u16,
            top: input.check.top as u16,
            width: input.check.width as u16,
            height: input.check.height as u16,
        },
        steps,
        unit_ids: input.check.unit_ids.clone(),
    };
    value.validate().map_err(CoreError::InvalidData)?;
    Ok(value)
}

#[derive(Clone, Default)]
pub struct CheckInput {
    pub project_id: String,
    pub timeout_seconds: i32,
    pub language: i32,
    pub left: i32,
    pub top: i32,
    pub width: i32,
    pub height: i32,
    pub steps: String,
    pub unit_ids: Vec<String>,
}
impl CheckInput {
    pub fn capture(app: &App<'_>) -> Self {
        use slint::Model;
        Self {
            project_id: app.get_check_project_id().into(),
            timeout_seconds: app.get_check_timeout(),
            language: app.get_check_language(),
            left: app.get_check_left(),
            top: app.get_check_top(),
            width: app.get_check_width(),
            height: app.get_check_height(),
            steps: app.get_check_steps().into(),
            unit_ids: app.get_check_unit_ids().iter().map(Into::into).collect(),
        }
    }
}

pub async fn handle(ctx: &Context, action: &str, input: &UiInput) -> Result<bool, CoreError> {
    if action == "game-check-dialog" {
        crate::workbench_ui::flush_editor(ctx, input).await?;
        crate::launch_ui::refresh(ctx, &input.project_id, false).await?;
        refresh(ctx, &input.project_id).await?;
        let id = input.project_id.clone();
        ctx.update(move |ui| {
            let app = ui.global::<App>();
            if app.get_project_id().as_str() == id {
                app.set_modal("game-check".into());
                app.set_error("".into());
            }
        });
        return Ok(true);
    }
    if action == "game-check-cancel" || action == "game-check-close" {
        cancel(ctx);
        let host = ctx.app().await;
        let _ = tokio::time::timeout(std::time::Duration::from_secs(3), async {
            while host
                .task_states()
                .await
                .values()
                .any(|s| s.running && s.operation == "game-check")
            {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await;
        host.close_game_check_errors().await?;
        return Ok(true);
    }
    if !matches!(
        action,
        "game-check-save" | "game-check-selection" | "game-check-run"
    ) {
        return Ok(false);
    }
    if input.check.project_id != input.project_id {
        return Ok(true);
    }
    let operation = async {
        let host = ctx.app().await;
        let requested_mode = if action == "game-check-run" {
            Some(match input.argument.as_str() {
                "launch" => CheckMode::Launch,
                "text" => CheckMode::Text,
                "both" => CheckMode::LaunchAndText,
                _ => return Err(CoreError::InvalidData("未知的运行检查方式。".into())),
            })
        } else {
            None
        };
        let previous = host
            .game_check_settings(&input.project_id)
            .await
            .unwrap_or_default();
        let mut settings = if requested_mode == Some(CheckMode::Launch) {
            CheckSettings {
                timeout_seconds: input.check.timeout_seconds as u32,
                ..previous
            }
        } else {
            let steps = if requested_mode == Some(CheckMode::Text) {
                previous.steps
            } else {
                parse_steps(&input.check.steps)?
            };
            settings(input, steps)?
        };
        if action == "game-check-selection" {
            settings.unit_ids = ctx.selection(&input.project_id);
            if settings.unit_ids.is_empty() && !input.editor_id.is_empty() {
                settings.unit_ids.push(input.editor_id.clone());
            }
            host.game_check_expected(&input.project_id, &settings.unit_ids)
                .await?;
        }
        host.save_game_check_settings(&input.project_id, settings)
            .await?;
        let Some(mode) = requested_mode else {
            refresh(ctx, &input.project_id).await?;
            return Ok(());
        };
        let token = ctx
            .session
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .game_check
            .clone()
            .ok_or_else(|| CoreError::InvalidData("运行检查未就绪。".into()))?;
        let epoch = ctx.root_generation.load(Ordering::Acquire);
        let id = input.project_id.clone();
        let mut initial = ctx
            .session
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .game_check_results
            .get(&id)
            .cloned()
            .unwrap_or_default();
        if mode.launches() {
            initial = CheckResult::default();
            initial.launch.message = "正在等待游戏窗口…".into();
        } else {
            initial.text = Observation::new(CheckStatus::NotChecked, "正在识别当前画面…");
        }
        ctx.session
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .game_check_results
            .insert(id.clone(), initial.clone());
        display(ctx, &id, initial);
        host.check_game(&id, mode, &token, |update| {
            if ctx.root_generation.load(Ordering::Acquire) != epoch {
                return;
            }
            let combined = {
                let mut session = ctx.session.lock().unwrap_or_else(|e| e.into_inner());
                let value = session.game_check_results.entry(id.clone()).or_default();
                if mode.launches() {
                    value.launch = update.launch;
                }
                if mode.reads_text() {
                    value.text = update.text;
                }
                value.clone()
            };
            display(ctx, &id, combined);
        })
        .await?;
        Ok::<_, CoreError>(())
    }
    .await;
    if let Err(error) = operation {
        let id = input.project_id.clone();
        let message = error.to_string();
        ctx.update(move |ui| {
            let app = ui.global::<App>();
            if app.get_project_id().as_str() == id && app.get_check_project_id().as_str() == id {
                app.set_check_message(message.into());
            }
        });
    }
    Ok(true)
}
