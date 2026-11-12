use crate::{Context, UiInput, view::App};
use gamel10n_core::{CoreError, launch::LaunchView};
use gamel10n_engines::protocol::LaunchConfiguration;
use slint::{ComponentHandle, ModelRc, VecModel};

pub async fn refresh(ctx: &Context, id: &str, edit: bool) -> Result<(), CoreError> {
    let view = ctx
        .app()
        .await
        .launch_view(id)
        .await
        .unwrap_or_else(|error| LaunchView {
            configuration: None,
            source: "未设置".into(),
            reason: error.to_string(),
            candidates: vec![],
            can_launch: false,
        });
    let id = id.to_owned();
    ctx.update(move |ui| {
        let app = ui.global::<App>();
        if app.get_project_id().as_str() != id {
            return;
        }
        let summary = view
            .configuration
            .as_ref()
            .map(|configuration| format!("{} · {}", view.source, configuration.executable))
            .unwrap_or_else(|| "尚未设置启动程序".into());
        app.set_launch_summary(summary.into());
        app.set_launch_can_run(view.can_launch);
        if edit || app.get_modal() != "launch" {
            let configuration = view.configuration.unwrap_or_default();
            app.set_launch_executable(configuration.executable.into());
            app.set_launch_arguments(configuration.arguments.into());
            app.set_launch_directory(if configuration.working_directory == "." {
                "".into()
            } else {
                configuration.working_directory.into()
            });
            app.set_launch_reason(view.reason.into());
            app.set_launch_candidates(ModelRc::new(VecModel::from(
                view.candidates
                    .into_iter()
                    .map(Into::into)
                    .collect::<Vec<_>>(),
            )));
            app.set_launch_candidate_index(-1);
        }
    });
    Ok(())
}

pub async fn handle(ctx: &Context, action: &str, input: &UiInput) -> Result<bool, CoreError> {
    match action {
        "launch-settings" => {
            refresh(ctx, &input.project_id, true).await?;
            let id = input.project_id.clone();
            ctx.update(move |ui| {
                let app = ui.global::<App>();
                if app.get_project_id().as_str() == id {
                    app.set_modal("launch".into());
                    app.set_error("".into());
                }
            });
        }
        "launch-browse" => {
            let project = ctx.app().await.open_project(&input.project_id).await?;
            let selected = rfd::AsyncFileDialog::new()
                .set_title("选择游戏或启动器")
                .set_directory(&project.game_directory)
                .add_filter("可执行程序", &["exe"])
                .pick_file()
                .await;
            if let Some(selected) = selected {
                let path = selected
                    .path()
                    .strip_prefix(&project.game_directory)
                    .map(|path| path.to_string_lossy().replace('\\', "/"))
                    .unwrap_or_else(|_| selected.path().display().to_string());
                let id = input.project_id.clone();
                ctx.update(move |ui| {
                    let app = ui.global::<App>();
                    if app.get_project_id().as_str() == id && app.get_modal() == "launch" {
                        app.set_launch_executable(path.into());
                        app.set_launch_candidate_index(-1);
                    }
                });
            }
        }
        "launch-save" => {
            let configuration = LaunchConfiguration {
                executable: input.launch_executable.trim().into(),
                arguments: input.launch_arguments.clone(),
                working_directory: if input.launch_directory.trim().is_empty() {
                    ".".into()
                } else {
                    input.launch_directory.trim().into()
                },
            };
            if let Err(error) = ctx
                .app()
                .await
                .save_launch(&input.project_id, Some(configuration))
                .await
            {
                let id = input.project_id.clone();
                let message = error.to_string();
                ctx.update(move |ui| {
                    let app = ui.global::<App>();
                    if app.get_project_id().as_str() == id {
                        app.set_modal("launch".into());
                        app.set_launch_reason(message.into());
                    }
                });
                return Err(error);
            }
            refresh(ctx, &input.project_id, false).await?;
            clear_error(ctx, &input.project_id);
        }
        "launch-detect" => {
            ctx.app().await.detect_launch(&input.project_id).await?;
            refresh(ctx, &input.project_id, true).await?;
            clear_error(ctx, &input.project_id);
        }
        "launch-reset" => {
            ctx.app().await.save_launch(&input.project_id, None).await?;
            refresh(ctx, &input.project_id, true).await?;
            clear_error(ctx, &input.project_id);
        }
        "launch-game" => {
            let pid = ctx.app().await.launch_game(&input.project_id).await?;
            let id = input.project_id.clone();
            ctx.update(move |ui| {
                let app = ui.global::<App>();
                if app.get_project_id().as_str() == id {
                    app.set_error("".into());
                    app.set_activity(format!("已启动游戏进程（PID {pid}）。").into());
                }
            });
        }
        _ => return Ok(false),
    }
    Ok(true)
}

fn clear_error(ctx: &Context, id: &str) {
    let id = id.to_owned();
    ctx.update(move |ui| {
        let app = ui.global::<App>();
        if app.get_project_id().as_str() == id {
            app.set_error("".into());
        }
    });
}
