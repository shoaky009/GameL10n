#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use clap::Parser;
use gamel10n_core::{AppEvent, Application, ProjectSummary, ToolPaths, default_data_directory};
use slint::{ComponentHandle, ModelRc, VecModel};

#[expect(
    clippy::todo,
    reason = "Slint generates unused component-embedding stubs."
)]
mod view {
    slint::include_modules!();
}

use view::{MainWindow, ProjectRow, TextRow};

#[derive(Parser)]
struct Arguments {
    #[arg(long)]
    data_directory: Option<PathBuf>,
    #[arg(long)]
    tools_directory: Option<PathBuf>,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args = Arguments::parse();
    let runtime = tokio::runtime::Runtime::new()?;
    let data = args.data_directory.unwrap_or_else(default_data_directory);
    let tools = match args.tools_directory {
        Some(path) => ToolPaths::new(path),
        None => ToolPaths::beside_executable()?,
    };
    let window = MainWindow::new()?;
    window.set_data_directory(data.display().to_string().into());
    window.set_formats(ModelRc::new(VecModel::from(
        gamel10n_core::native_formats()
            .iter()
            .map(|format| (*format).into())
            .collect::<Vec<_>>(),
    )));
    let app = match runtime.block_on(Application::open(data, &tools)) {
        Ok(app) => app,
        Err(error) => {
            window.set_status(format!("无法读取数据目录：{error}").into());
            append_log(&window, &error.to_string());
            window.run()?;
            return Ok(());
        }
    };
    window.set_app_ready(true);
    let query_generation = Arc::new(AtomicU64::new(0));

    {
        let app = app.clone();
        let weak = window.as_weak();
        let handle = runtime.handle().clone();
        window.on_add_project(move |path| {
            let app = app.clone();
            let weak = weak.clone();
            handle.spawn(async move {
                match app
                    .add_project(&PathBuf::from(path.as_str().trim_matches('"')))
                    .await
                {
                    Ok(project) => {
                        let _ = weak.upgrade_in_event_loop(move |ui| {
                            append_log(&ui, &format!("已添加项目：{}", project.name));
                            ui.invoke_select_project(project.id.into());
                        });
                    }
                    Err(error) => show_error(weak, None, error.to_string()),
                }
            });
        });
    }
    {
        let app = app.clone();
        let weak = window.as_weak();
        let handle = runtime.handle().clone();
        window.on_search_projects(move |query| {
            refresh_projects(&handle, app.clone(), weak.clone(), query.to_string());
        });
    }
    {
        let app = app.clone();
        let weak = window.as_weak();
        let handle = runtime.handle().clone();
        window.on_select_project(move |id| {
            let app = app.clone();
            let weak = weak.clone();
            let id = id.to_string();
            if let Some(ui) = weak.upgrade() {
                ui.set_selected_id(id.clone().into());
                ui.set_loading(true);
                ui.set_status("正在读取项目和已保存的文本。".into());
                ui.set_text_query("".into());
                ui.set_texts(ModelRc::default());
                ui.set_total_texts(0);
                ui.set_shown_texts(0);
            }
            handle.spawn(async move {
                match app.open_project(&id).await {
                    Ok(project) => {
                        let running = app.task_state(&id).await;
                        let _ = weak.upgrade_in_event_loop(move |ui| {
                            if ui.get_selected_id().as_str() != id {
                                return;
                            }
                            ui.set_selected_name(project.name.into());
                            ui.set_selected_path(
                                project.game_directory.display().to_string().into(),
                            );
                            ui.set_busy(running.as_ref().is_some_and(|state| state.running));
                            ui.invoke_query_texts(ui.get_selected_id(), "".into(), 0);
                        });
                    }
                    Err(error) => show_error(weak, Some(id), error.to_string()),
                }
            });
        });
    }
    {
        let app = app.clone();
        let weak = window.as_weak();
        let handle = runtime.handle().clone();
        let generation = Arc::clone(&query_generation);
        window.on_query_texts(move |id, query, offset| {
            let app = app.clone();
            let weak = weak.clone();
            let id = id.to_string();
            let query = query.to_string();
            let current = generation.fetch_add(1, Ordering::Relaxed) + 1;
            let generation = Arc::clone(&generation);
            if let Some(ui) = weak.upgrade() {
                ui.set_loading(true);
            }
            handle.spawn(async move {
                let result = app
                    .text_page(&id, &query, offset.max(0) as usize, 100)
                    .await;
                if generation.load(Ordering::Relaxed) != current {
                    return;
                }
                match result {
                    Ok(page) => {
                        let running = app.task_state(&id).await;
                        let rows = page
                            .units
                            .into_iter()
                            .map(|unit| TextRow {
                                id: unit.id.into(),
                                resource: format!("{} · {}", unit.resource_path, unit.locator)
                                    .into(),
                                source: unit.source_text.into(),
                            })
                            .collect::<Vec<_>>();
                        let _ = weak.upgrade_in_event_loop(move |ui| {
                            if ui.get_selected_id().as_str() != id
                                || generation.load(Ordering::Relaxed) != current
                            {
                                return;
                            }
                            ui.set_shown_texts(rows.len() as i32);
                            ui.set_texts(ModelRc::new(VecModel::from(rows)));
                            ui.set_total_texts(page.total as i32);
                            ui.set_page_offset(page.offset as i32);
                            ui.set_loading(false);
                            if let Some(state) = running {
                                ui.set_status(state.message.into());
                            } else {
                                ui.set_status(
                                    if page.total == 0 {
                                        "请选择脚本格式并提取；这里处理已经解包的脚本目录。"
                                    } else {
                                        "已读取保存的提取结果。"
                                    }
                                    .into(),
                                );
                            }
                        });
                    }
                    Err(error) => show_error(weak, Some(id), error.to_string()),
                }
            });
        });
    }
    {
        let app = app.clone();
        let weak = window.as_weak();
        let handle = runtime.handle().clone();
        window.on_extract(move |id, format| {
            let app = app.clone();
            let weak = weak.clone();
            let id = id.to_string();
            handle.spawn(async move {
                match app.extract_native(&id, format.as_str()).await {
                    Ok(_) => {
                        let _ = weak.upgrade_in_event_loop(move |ui| {
                            if ui.get_selected_id().as_str() == id {
                                ui.invoke_query_texts(id.into(), ui.get_text_query(), 0);
                            }
                        });
                    }
                    Err(error) => show_error(weak, Some(id), error.to_string()),
                }
            });
        });
    }
    {
        let app = app.clone();
        let handle = runtime.handle().clone();
        window.on_cancel_task(move |id| {
            let app = app.clone();
            handle.spawn(async move {
                app.cancel(id.as_str()).await;
            });
        });
    }
    {
        let app = app.clone();
        let weak = window.as_weak();
        let handle = runtime.handle().clone();
        window.on_open_project_folder(move |id| {
            let app = app.clone();
            let weak = weak.clone();
            handle.spawn(async move {
                let result = app.project_directory(id.as_str()).map(|path| {
                    #[cfg(windows)]
                    {
                        std::process::Command::new("explorer.exe")
                            .arg(path)
                            .spawn()
                            .map(|_| ())
                    }
                    #[cfg(not(windows))]
                    {
                        std::process::Command::new("xdg-open")
                            .arg(path)
                            .spawn()
                            .map(|_| ())
                    }
                });
                match result {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => show_error(weak, Some(id.to_string()), error.to_string()),
                    Err(error) => show_error(weak, Some(id.to_string()), error.to_string()),
                }
            });
        });
    }
    {
        let app = app.clone();
        let weak = window.as_weak();
        let mut events = app.subscribe();
        runtime.spawn(async move {
            loop {
                match events.recv().await {
                    Ok(AppEvent::TaskChanged(state)) => {
                        let _ = weak.upgrade_in_event_loop(move |ui| {
                            append_log(&ui, &format!("{} · {}", state.project_id, state.message));
                            if ui.get_selected_id().as_str() == state.project_id {
                                ui.set_status(state.message.into());
                                ui.set_busy(state.running);
                            }
                        });
                    }
                    Ok(AppEvent::ProjectsChanged)
                    | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
                let _ = weak.upgrade_in_event_loop(|ui| {
                    ui.invoke_search_projects(ui.get_project_query());
                });
            }
        });
    }
    refresh_projects(runtime.handle(), app, window.as_weak(), String::new());
    window.run()?;
    runtime.shutdown_timeout(std::time::Duration::from_secs(2));
    Ok(())
}

fn refresh_projects(
    handle: &tokio::runtime::Handle,
    app: Application,
    weak: slint::Weak<MainWindow>,
    query: String,
) {
    handle.spawn(async move {
        let mut rows = Vec::new();
        for project in app.list_projects(&query).await {
            let state = app.task_state(&project.id).await;
            rows.push(project_row(project, state));
        }
        let _ = weak.upgrade_in_event_loop(move |ui| {
            if ui.get_project_query().as_str() != query {
                return;
            }
            ui.set_projects(ModelRc::new(VecModel::from(rows)));
            if ui.get_selected_id().is_empty() {
                ui.set_status("请选择游戏项目。".into());
            }
        });
    });
}

fn project_row(project: ProjectSummary, state: Option<gamel10n_core::TaskState>) -> ProjectRow {
    let running = state.as_ref().is_some_and(|state| state.running);
    let status = state.map(|state| state.message).unwrap_or_else(|| {
        project
            .extracted_unit_count()
            .map(|count| format!("已提取 {count} 条文本"))
            .unwrap_or_else(|| "等待操作".into())
    });
    ProjectRow {
        id: project.id.into(),
        name: project.name.into(),
        path: project.game_directory.display().to_string().into(),
        status: status.into(),
        running,
    }
}

fn show_error(weak: slint::Weak<MainWindow>, project_id: Option<String>, error: String) {
    let _ = weak.upgrade_in_event_loop(move |ui| {
        if project_id
            .as_deref()
            .is_none_or(|id| ui.get_selected_id().as_str() == id)
        {
            ui.set_loading(false);
            ui.set_status(error.clone().into());
        }
        append_log(&ui, &error);
    });
}

fn append_log(ui: &MainWindow, message: &str) {
    let previous = ui.get_log_text();
    let mut lines = previous.lines().rev().take(199).collect::<Vec<_>>();
    lines.reverse();
    let mut text = lines.join("\n");
    if !text.is_empty() {
        text.push('\n');
    }
    text.push_str(message);
    ui.set_log_text(text.into());
}
