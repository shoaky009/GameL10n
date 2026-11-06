use std::{
    collections::BTreeMap,
    sync::atomic::{AtomicU64, Ordering},
};

use gamel10n_core::{
    CoreError,
    catalog::{
        EngineCatalog, EngineLibraryFamily, EngineLibrarySource, EngineLibraryVersion,
        GameCompatibility, LibraryFilter, LibrarySort,
    },
};
use serde_json::Value;
use slint::{ComponentHandle, Model, ModelRc, VecModel};

use crate::{
    Context, UiInput,
    view::{App, EngineRow, MainWindow, NamedRow},
};

static LIST_GENERATION: AtomicU64 = AtomicU64::new(0);

pub enum LibraryTarget<'a> {
    Version(&'a str),
    Knowledge(Option<&'a str>),
}

pub async fn open_family(
    ctx: &Context,
    id: &str,
    target: LibraryTarget<'_>,
) -> Result<(), CoreError> {
    let generation = ctx.generation.clone();
    let revision = generation.load(Ordering::SeqCst);
    let catalog = catalog(ctx, true).await?;
    let id = id.to_owned();
    let lookup = id.clone();
    let found = tokio::task::spawn_blocking(move || {
        catalog
            .family_position(&lookup)?
            .map(|position| catalog.family(&lookup).map(|family| (position, family)))
            .transpose()
    })
    .await??;
    if generation.load(Ordering::SeqCst) != revision {
        return Ok(());
    }
    let Some((position, family)) = found else {
        ctx.show_error("", "对应引擎记录已删除或不在全局引擎库中。".into());
        return Ok(());
    };
    let (tab, action, selection) = match target {
        LibraryTarget::Version(version) => {
            if !family.versions.iter().any(|entry| entry.version == version) {
                ctx.show_error("", "对应引擎版本已删除或不在全局引擎库中。".into());
                return Ok(());
            }
            (1, "version", version.to_owned())
        }
        LibraryTarget::Knowledge(Some(knowledge)) => (3, "knowledge", knowledge.to_owned()),
        LibraryTarget::Knowledge(None) => (3, "", String::new()),
    };
    {
        let mut session = ctx
            .session
            .lock()
            .map_err(|_| CoreError::InvalidData("引擎选择状态不可用。".into()))?;
        session.engine_selection.clear();
        session.engine_selection.insert(id.clone());
        session.engine_selection_anchor = Some(id.clone());
    }
    let selected = id.clone();
    ctx.update(move |ui| {
        if generation.load(Ordering::SeqCst) != revision {
            return;
        }
        let app = ui.global::<App>();
        app.set_page(2);
        app.set_engine_id(selected.into());
        app.set_engine_tab(tab);
        app.set_catalog_query("".into());
        app.set_catalog_filter(0);
        app.set_catalog_enabled_filter(0);
        app.set_catalog_sort(0);
        app.set_compatible_query("".into());
        app.set_error("".into());
        app.set_activity("".into());
        app.set_knowledge_id("".into());
        app.set_knowledge_content("".into());
        app.set_knowledge_origins(ModelRc::default());
        apply_family(&app, &family, "");
    });
    let input = UiInput {
        catalog_offset: (position / 100 * 100) as i32,
        engine_id: id.clone(),
        ..UiInput::default()
    };
    refresh(ctx, &input, false).await?;
    ctx.update(move |ui| {
        let app = ui.global::<App>();
        if app.get_page() == 2 && app.get_engine_id().as_str() == id {
            app.set_catalog_scroll_y(-((position % 100) as f32) * 86.0);
            if !action.is_empty() {
                app.invoke_action(action.into(), selection.into());
            }
        }
    });
    Ok(())
}

pub async fn open_knowledge(ctx: &Context, id: &str) -> Result<(), CoreError> {
    let revision = ctx.generation.load(Ordering::SeqCst);
    let Some(entry) = load_knowledge(ctx, id).await? else {
        return Ok(());
    };
    if ctx.generation.load(Ordering::SeqCst) != revision {
        return Ok(());
    }
    let family = property(&entry, "family").unwrap_or(&Value::Null);
    let kind = property(family, "kind")
        .and_then(Value::as_str)
        .unwrap_or("");
    let key = property(family, "key")
        .and_then(Value::as_str)
        .unwrap_or("");
    open_family(
        ctx,
        &format!("{kind}.{key}"),
        LibraryTarget::Knowledge(Some(id)),
    )
    .await
}

pub async fn handle(ctx: &Context, action: &str, input: &UiInput) -> Result<bool, CoreError> {
    match action {
        "catalog" | "catalog-refresh" => {
            refresh(ctx, input, action == "catalog-refresh").await?;
            if !input.engine_id.is_empty() {
                show_family(ctx, &input.engine_id, &input.compatible_query).await?;
            }
        }
        "engine" | "engine-toggle" | "engine-range" => {
            let id = input.argument.clone();
            let selection = {
                let mut session = ctx
                    .session
                    .lock()
                    .map_err(|_| CoreError::InvalidData("引擎选择状态不可用。".into()))?;
                match action {
                    "engine-toggle" => {
                        if !session.engine_selection.remove(&id) {
                            session.engine_selection.insert(id.clone());
                        }
                        session.engine_selection_anchor = Some(id.clone());
                    }
                    "engine-range" => {
                        let anchor = session.engine_selection_anchor.as_ref().and_then(|anchor| {
                            session
                                .engine_visible_ids
                                .iter()
                                .position(|value| value == anchor)
                        });
                        let end = session
                            .engine_visible_ids
                            .iter()
                            .position(|value| value == &id);
                        if let (Some(start), Some(end)) = (anchor, end) {
                            let range = session.engine_visible_ids[start.min(end)..=start.max(end)]
                                .to_vec();
                            session.engine_selection = range.into_iter().collect();
                        } else {
                            session.engine_selection.insert(id.clone());
                        }
                    }
                    _ => {
                        session.engine_selection.clear();
                        session.engine_selection.insert(id.clone());
                        session.engine_selection_anchor = Some(id.clone());
                    }
                }
                session.engine_selection.clone()
            };
            let selected = id.clone();
            ctx.update(move |ui| {
                let app = ui.global::<App>();
                app.set_engine_id(selected.into());
                app.set_version_id("".into());
                app.set_knowledge_id("".into());
                app.set_knowledge_content("".into());
                app.set_knowledge_origins(ModelRc::default());
                let rows = app
                    .get_engines()
                    .iter()
                    .map(|mut row| {
                        row.selected = selection.contains(row.id.as_str());
                        row
                    })
                    .collect::<Vec<_>>();
                app.set_engines(ModelRc::new(VecModel::from(rows)));
            });
            show_family(ctx, &id, &input.compatible_query).await?;
        }
        "version" => {
            let catalog = catalog(ctx, false).await?;
            let id = input.engine_id.clone();
            let version = input.argument.clone();
            let selected = version.clone();
            let selected_id = id.clone();
            ctx.update(move |ui| {
                let app = ui.global::<App>();
                if app.get_engine_id().as_str() == selected_id {
                    app.set_version_id(selected.into());
                    app.set_version_detail("正在检查此版本的文件…".into());
                }
            });
            let detail = tokio::task::spawn_blocking(move || catalog.version_detail(&id, &version))
                .await??;
            ctx.update(move |ui| {
                let app = ui.global::<App>();
                if app.get_engine_id().as_str() == detail.adapter_id
                    && app.get_version_id().as_str() == detail.version
                {
                    app.set_version_detail(version_text(&detail).into());
                }
            });
        }
        "compatible-games" => {
            let catalog = catalog(ctx, false).await?;
            let id = input.engine_id.clone();
            let family = tokio::task::spawn_blocking(move || catalog.family(&id)).await??;
            let query = input.compatible_query.clone();
            ctx.update(move |ui| {
                let app = ui.global::<App>();
                if app.get_engine_id().as_str() == family.family.adapter_id() {
                    app.set_games(game_rows(&family.games, &query));
                }
            });
        }
        "knowledge" => {
            let id = input.argument.clone();
            let selected = id.clone();
            ctx.update(move |ui| {
                let app = ui.global::<App>();
                app.set_knowledge_id(selected.into());
                app.set_knowledge_content("".into());
                app.set_knowledge_origins(ModelRc::default());
            });
            let Some(entry) = load_knowledge(ctx, &id).await? else {
                return Ok(true);
            };
            let titles = ctx
                .app()
                .await
                .list_projects("")
                .await
                .into_iter()
                .map(|project| (project.id, project.name))
                .collect::<BTreeMap<_, _>>();
            ctx.update(move |ui| {
                let app = ui.global::<App>();
                if app.get_knowledge_id().as_str() == id {
                    app.set_knowledge_enabled(
                        property(&entry, "enabled")
                            .and_then(Value::as_bool)
                            .unwrap_or(true),
                    );
                    app.set_knowledge_content(knowledge_text(&entry).into());
                    app.set_knowledge_origins(knowledge_origins(&entry, &titles));
                }
            });
        }
        "engine-enable" => {
            let catalog = catalog(ctx, false).await?;
            let mut ids: Vec<_> = ctx
                .session
                .lock()
                .map_err(|_| CoreError::InvalidData("引擎选择状态不可用。".into()))?
                .engine_selection
                .iter()
                .cloned()
                .collect();
            if ids.is_empty() && !input.engine_id.is_empty() {
                ids.push(input.engine_id.clone());
            }
            let count = ids.len();
            let enabled = input.argument == "true";
            tokio::task::spawn_blocking(move || catalog.set_family_enabled(&ids, enabled))
                .await??;
            refresh(ctx, input, false).await?;
            show_family(ctx, &input.engine_id, &input.compatible_query).await?;
            let message = format!(
                "已{}{count} 个引擎与模板，对下一次新识别生效。已有项目固定版本仍可使用。",
                if enabled { "启用" } else { "停用" }
            );
            ctx.update(move |ui| ui.global::<App>().set_status(message.into()));
        }
        "knowledge-enable" => {
            let catalog = catalog(ctx, false).await?;
            let id = input.knowledge_id.clone();
            let enabled = input.knowledge_enabled;
            tokio::task::spawn_blocking(move || catalog.set_knowledge_enabled(&id, enabled))
                .await??;
            show_family(ctx, &input.engine_id, &input.compatible_query).await?;
            let id = input.knowledge_id.clone();
            ctx.update(move |ui| {
                let app = ui.global::<App>();
                app.set_knowledge_id(id.into());
                app.set_knowledge_enabled(enabled);
                app.set_status(
                    if enabled {
                        "已启用引擎经验。"
                    } else {
                        "已停用引擎经验。"
                    }
                    .into(),
                );
            });
        }
        "version-delete-dialog" => {
            let catalog = catalog(ctx, false).await?;
            let id = input.engine_id.clone();
            let version = input.version_id.clone();
            let detail = tokio::task::spawn_blocking(move || catalog.version_detail(&id, &version))
                .await??;
            if detail.source == EngineLibrarySource::BuiltIn {
                return Err(CoreError::InvalidData(
                    "内置版本随程序提供，不能删除。".into(),
                ));
            }
            crate::show_dialog(
                ctx,
                "version-delete",
                "删除引擎版本？",
                format!(
                    "{}\n版本：{}\n{}\n\n将删除此版本的扩展文件、兼容记录、激活记录和索引条目。游戏项目的原文、译文及补丁保留；生成新补丁前需要可用的适配器。此操作无法撤销。",
                    detail.adapter_id,
                    detail.version,
                    detail
                        .directory_path
                        .map(|path| path.display().to_string())
                        .unwrap_or_default()
                ),
            );
        }
        "verify-version" => {
            let catalog = catalog(ctx, false).await?;
            let id = input.engine_id.clone();
            let version = input.version_id.clone();
            let detail = tokio::task::spawn_blocking(move || catalog.version_detail(&id, &version))
                .await??;
            if detail.source != EngineLibrarySource::External || !detail.exists {
                return Err(CoreError::InvalidData(
                    "请选择文件完整的扩展引擎版本。".into(),
                ));
            }
            let projects = ctx.app().await.list_projects("").await;
            if projects.is_empty() {
                return Err(CoreError::InvalidData(
                    "请先添加用于验证的游戏项目。".into(),
                ));
            }
            let index = projects
                .iter()
                .position(|project| project.id == input.project_id)
                .unwrap_or(0);
            ctx.update(move |ui| {
                let app = ui.global::<App>();
                app.set_verification_project_names(ModelRc::new(VecModel::from(
                    projects
                        .iter()
                        .map(|project| {
                            format!("{} — {}", project.name, project.game_directory.display())
                                .into()
                        })
                        .collect::<Vec<slint::SharedString>>(),
                )));
                app.set_verification_project_ids(ModelRc::new(VecModel::from(
                    projects
                        .iter()
                        .map(|project| project.id.clone().into())
                        .collect::<Vec<slint::SharedString>>(),
                )));
                app.set_verification_project_index(index as i32);
            });
            crate::show_dialog(
                ctx,
                "verify-version",
                "验证发布包",
                format!(
                    "{} · {}\n\n将使用所选游戏资源验证发布包与原文回填。实机运行状态单独记录。",
                    input.engine_id, input.version_id
                ),
            );
        }
        "verify-version-confirm" => {
            if input.verification_project_id.is_empty() {
                return Err(CoreError::InvalidData("请选择用于验证的游戏项目。".into()));
            }
            let catalog = catalog(ctx, false).await?;
            let path = catalog.version_directory(&input.engine_id, &input.version_id)?;
            let result = ctx
                .app()
                .await
                .verify_engine_version(&input.verification_project_id, &path)
                .await?;
            let message = format!(
                "发布包验证：{}\n原文回填：{}\n文本：{} 条\n实机：{}\n{}",
                if result.passed { "通过" } else { "未通过" },
                if result.original_roundtrip_passed {
                    "通过"
                } else {
                    "未通过"
                },
                result.units,
                if result.runtime_tested {
                    "已验证"
                } else {
                    "未验证"
                },
                result.diagnostics.join("\n")
            );
            ctx.update(move |ui| {
                let app = ui.global::<App>();
                app.set_version_detail(message.clone().into());
                app.set_status(message.into());
            });
        }
        "version-delete" => {
            // Recognition can acquire a version after it starts; protect the complete running operation.
            let app = ctx.app().await;
            for project in app.list_projects("").await {
                if app
                    .task_state(&project.id)
                    .await
                    .is_some_and(|task| task.running)
                {
                    return Err(CoreError::InvalidData("有运行中的项目任务，任务可能正在使用此版本。请先等待任务完成或暂停，再删除扩展版本。".into()));
                }
            }
            let library = catalog(ctx, false).await?;
            let id = input.engine_id.clone();
            let version = input.version_id.clone();
            let result =
                tokio::task::spawn_blocking(move || library.delete_version(&id, &version, &[]))
                    .await??;
            if !result.deleted {
                return Err(CoreError::InvalidData(
                    "运行中的任务正在使用此版本。".into(),
                ));
            }
            let deleted_id = input.engine_id.clone();
            let deleted_version = input.version_id.clone();
            ctx.update(move |ui| {
                let app = ui.global::<App>();
                if app.get_engine_id().as_str() == deleted_id
                    && app.get_version_id().as_str() == deleted_version
                {
                    app.set_version_id("".into());
                    app.set_version_detail("".into());
                }
            });
            refresh(ctx, input, true).await?;
            let catalog = catalog(ctx, false).await?;
            if catalog.family(&input.engine_id).is_ok() {
                show_family(ctx, &input.engine_id, &input.compatible_query).await?;
            } else {
                ctx.update(clear_family);
            }
            let message = if result.diagnostics.is_empty() {
                format!(
                    "已删除版本 {} · {} 及对应记录和索引。项目文本、译文和补丁已保留。",
                    input.engine_id, input.version_id
                )
            } else {
                format!(
                    "记录和索引已删除，但部分版本文件未能删除。\n{} · {}\n{}",
                    input.engine_id,
                    input.version_id,
                    result.diagnostics.join("\n")
                )
            };
            if !result.diagnostics.is_empty() {
                let _ = app.log("", &message).await;
                ctx.show_error("", message.clone());
            } else {
                ctx.update(|ui| ui.global::<App>().set_error("".into()));
            }
            ctx.update(move |ui| ui.global::<App>().set_status(message.into()));
        }
        "open"
            if input.argument == "version"
                || (input.argument == "knowledge"
                    && input.page == 2
                    && !input.knowledge_id.is_empty()) =>
        {
            let catalog = catalog(ctx, false).await?;
            let path = if input.argument == "version" {
                catalog.version_directory(&input.engine_id, &input.version_id)?
            } else {
                catalog.knowledge_path(&input.knowledge_id)?
            };
            crate::open_path(path)?;
        }
        _ => return Ok(false),
    }
    Ok(true)
}

async fn catalog(ctx: &Context, refresh: bool) -> Result<EngineCatalog, CoreError> {
    let root = ctx.app().await.data_directory().join("extensions");
    let cache = ctx.catalog.clone();
    tokio::task::spawn_blocking(move || {
        let mut cached = cache
            .lock()
            .map_err(|_| CoreError::InvalidData("引擎库缓存锁失效。".into()))?;
        if cached.as_ref().is_none_or(|catalog| catalog.root() != root) {
            *cached = Some(EngineCatalog::from_extensions_root(&root)?);
        }
        let catalog = cached
            .as_ref()
            .ok_or_else(|| CoreError::InvalidData("无法打开引擎库。".into()))?
            .clone();
        if refresh {
            catalog.refresh()?;
        }
        Ok(catalog)
    })
    .await?
}

async fn refresh(ctx: &Context, input: &UiInput, force: bool) -> Result<(), CoreError> {
    let generation = LIST_GENERATION.fetch_add(1, Ordering::Relaxed) + 1;
    let epoch = ctx.root_generation.load(Ordering::Relaxed);
    let root_generation = ctx.root_generation.clone();
    let catalog = catalog(ctx, force).await?;
    let query = input.catalog_query.clone();
    let offset = match input.argument.as_str() {
        "next" => (input.catalog_offset.max(0) as usize).saturating_add(100),
        "previous" => (input.catalog_offset.max(0) as usize).saturating_sub(100),
        "0" => 0,
        _ => input.catalog_offset.max(0) as usize,
    };
    let filter = match input.catalog_filter {
        1 => LibraryFilter::BuiltIn,
        2 => LibraryFilter::External,
        3 => LibraryFilter::History,
        _ => LibraryFilter::All,
    };
    let enabled = match input.catalog_enabled_filter {
        1 => Some(true),
        2 => Some(false),
        _ => None,
    };
    let sort = match input.catalog_sort {
        1 => LibrarySort::Updated,
        2 => LibrarySort::CompatibleGames,
        _ => LibrarySort::Name,
    };
    let page = tokio::task::spawn_blocking(move || {
        catalog.snapshot_filtered(&query, filter, enabled, sort, offset, 100)
    })
    .await??;
    let selection = {
        let mut session = ctx
            .session
            .lock()
            .map_err(|_| CoreError::InvalidData("引擎选择状态不可用。".into()))?;
        session.engine_visible_ids = page
            .families
            .iter()
            .map(|family| family.family.adapter_id())
            .collect();
        session.engine_selection.clone()
    };
    ctx.update(move |ui| {
        if LIST_GENERATION.load(Ordering::Relaxed) != generation
            || root_generation.load(Ordering::Relaxed) != epoch
        {
            return;
        }
        let app = ui.global::<App>();
        app.set_engines(ModelRc::new(VecModel::from(
            page.families
                .iter()
                .map(|family| EngineRow {
                    id: family.family.adapter_id().into(),
                    name: family.family.name.clone().into(),
                    enabled: family.enabled,
                    selected: selection.contains(&family.family.adapter_id()),
                    detail: format!(
                        "{} · {} 个版本 · {} 个兼容游戏",
                        if family.enabled {
                            "已启用"
                        } else {
                            "已停用"
                        },
                        family.versions.len(),
                        family.games.len()
                    )
                    .into(),
                })
                .collect::<Vec<_>>(),
        )));
        app.set_catalog_total(page.total.min(i32::MAX as usize) as i32);
        app.set_catalog_offset(page.offset.min(i32::MAX as usize) as i32);
        if !page.diagnostics.is_empty() {
            app.set_status(
                format!(
                    "{} 项记录未能完整读取。\n{}",
                    page.diagnostics.len(),
                    page.diagnostics
                        .into_iter()
                        .take(4)
                        .collect::<Vec<_>>()
                        .join("\n")
                )
                .into(),
            );
        }
    });
    Ok(())
}

async fn show_family(ctx: &Context, id: &str, query: &str) -> Result<(), CoreError> {
    let catalog = catalog(ctx, false).await?;
    let id = id.to_owned();
    let query = query.to_owned();
    let family = tokio::task::spawn_blocking(move || catalog.family(&id)).await??;
    ctx.update(move |ui| {
        let app = ui.global::<App>();
        if !app.get_engine_id().is_empty()
            && app.get_engine_id().as_str() != family.family.adapter_id()
        {
            return;
        }
        apply_family(&app, &family, &query);
    });
    Ok(())
}

fn apply_family(app: &App<'_>, family: &EngineLibraryFamily, query: &str) {
    app.set_engine_id(family.family.adapter_id().into());
    app.set_engine_name(family.family.name.clone().into());
    app.set_engine_enabled(family.enabled);
    app.set_engine_summary(family_text(family).into());
    app.set_versions(ModelRc::new(VecModel::from(
        family
            .versions
            .iter()
            .map(|version| NamedRow {
                id: version.version.clone().into(),
                name: version.version.clone().into(),
                enabled: version.enabled,
                detail: format!(
                    "{} · {} · {}{}",
                    source_name(version.source),
                    version.lifecycle.status_label(),
                    if version.exists {
                        "文件已登记"
                    } else {
                        "仅有历史记录"
                    },
                    version
                        .published_at
                        .as_ref()
                        .map(|date| format!(" · {}", crate::time_ui::local_timestamp(date)))
                        .unwrap_or_default()
                )
                .into(),
            })
            .collect::<Vec<_>>(),
    )));
    app.set_games(game_rows(&family.games, query));
    app.set_knowledge(ModelRc::new(VecModel::from(
        family
            .knowledge
            .iter()
            .map(|entry| NamedRow {
                id: entry.id.clone().into(),
                name: entry.summary.clone().into(),
                detail: format!(
                    "{} · {} 个工具",
                    if entry.enabled {
                        "已启用"
                    } else {
                        "已停用"
                    },
                    entry.tool_count
                )
                .into(),
                enabled: entry.enabled,
            })
            .collect::<Vec<_>>(),
    )));
    app.set_version_id("".into());
    app.set_version_detail("选择版本以检查文件和查看可用操作。".into());
}

fn game_rows(games: &[GameCompatibility], query: &str) -> ModelRc<NamedRow> {
    let query = query.trim().to_lowercase();
    ModelRc::new(VecModel::from(
        games
            .iter()
            .filter(|game| {
                query.is_empty()
                    || format!("{}\n{}\n{}", game.title, game.variant, game.adapter_version)
                        .to_lowercase()
                        .contains(&query)
            })
            .map(|game| NamedRow {
                id: game.game_id.clone().into(),
                name: game.title.clone().into(),
                enabled: true,
                detail: format!(
                    "{} · {}\n结构：{} · 实机：{}",
                    game.variant, game.adapter_version, game.structure_status, game.runtime_status
                )
                .into(),
            })
            .collect::<Vec<_>>(),
    ))
}

fn clear_family(ui: MainWindow) {
    let app = ui.global::<App>();
    app.set_engine_id("".into());
    app.set_engine_name("".into());
    app.set_engine_summary("".into());
    app.set_version_id("".into());
    app.set_version_detail("".into());
    app.set_knowledge_id("".into());
    app.set_knowledge_content("".into());
    app.set_knowledge_origins(ModelRc::default());
    app.set_versions(ModelRc::new(VecModel::default()));
    app.set_games(ModelRc::new(VecModel::default()));
    app.set_knowledge(ModelRc::new(VecModel::default()));
}

fn family_text(family: &EngineLibraryFamily) -> String {
    format!(
        "{}\n分类：{}\n状态：{}\n版本：{}\n兼容游戏：{}\n相关经验：{}\n\n兼容性按游戏资源与适用条件记录。结构验证与实机验证分别显示。",
        family.family.adapter_id(),
        if family.family.kind == "engine" {
            "引擎"
        } else {
            "资源模板"
        },
        if family.enabled {
            "已启用"
        } else {
            "已停用"
        },
        family.versions.len(),
        family.games.len(),
        family.knowledge.len()
    )
}

fn source_name(source: EngineLibrarySource) -> &'static str {
    match source {
        EngineLibrarySource::BuiltIn => "内置",
        EngineLibrarySource::External => "扩展",
        EngineLibrarySource::History => "历史",
    }
}

fn version_text(version: &EngineLibraryVersion) -> String {
    let mut text = format!(
        "{} · {}\n来源：{}\n兼容游戏：{}",
        version.adapter_id,
        version.version,
        source_name(version.source),
        version.games.len()
    );
    if version.source == EngineLibrarySource::External {
        text.push_str(&format!(
            "\n游戏验证：{}\n新项目复用：{}",
            version.lifecycle.status_label(),
            if version.is_candidate {
                "可参与匹配，仍需验证兼容性"
            } else {
                "暂不参与自动匹配"
            }
        ));
        for (game_id, feedback) in &version.lifecycle.feedback {
            let title = version
                .games
                .iter()
                .find(|game| &game.game_id == game_id)
                .map(|game| game.title.as_str())
                .unwrap_or(game_id);
            text.push_str(&format!(
                "\n{}：{}{}",
                title,
                if feedback.status == "passed" {
                    "效果正常"
                } else {
                    "存在问题"
                },
                if feedback.notes.is_empty() {
                    String::new()
                } else {
                    format!("；{}", feedback.notes)
                }
            ));
        }
        for previous in version.lifecycle.replaces.values().flatten() {
            text.push_str(&format!(
                "\n修复自：{} · {}",
                previous.adapter_id, previous.version
            ));
        }
    }
    match version.source {
        EngineLibrarySource::BuiltIn => text.push_str("\n内置版本随程序提供，不能删除。"),
        EngineLibrarySource::History => {
            text.push_str("\n此版本仅有历史记录，可以删除对应的记录和索引。")
        }
        EngineLibrarySource::External => {
            if let Some(path) = &version.directory_path {
                text.push_str(&format!("\n位置：{}", path.display()));
            }
            text.push_str(if version.exists {
                "\n版本文件完整。"
            } else {
                "\n版本文件已删除或不完整。删除此版本会一并移除对应的记录和索引。"
            });
        }
    }
    for diagnostic in &version.diagnostics {
        text.push_str(&format!("\n{diagnostic}"));
    }
    text
}

fn property<'a>(value: &'a Value, name: &str) -> Option<&'a Value> {
    value.get(name).or_else(|| {
        value
            .as_object()?
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value)
    })
}

async fn load_knowledge(ctx: &Context, id: &str) -> Result<Option<Value>, CoreError> {
    let catalog = catalog(ctx, false).await?;
    let id = id.to_owned();
    let entry = tokio::task::spawn_blocking(move || {
        if !catalog.knowledge_path(&id)?.try_exists()? {
            return Ok(None);
        }
        catalog.knowledge_detail(&id).map(Some)
    })
    .await??;
    if entry.is_none() {
        ctx.show_error("", "对应经验已删除或不存在。".into());
    }
    Ok(entry)
}

fn knowledge_origins(entry: &Value, titles: &BTreeMap<String, String>) -> ModelRc<NamedRow> {
    ModelRc::new(VecModel::from(
        property(entry, "origins")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .map(|origin| {
                let id = property(origin, "gameId")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let name = titles
                    .get(id)
                    .map(String::as_str)
                    .or_else(|| property(origin, "title").and_then(Value::as_str))
                    .filter(|name| !name.is_empty())
                    .unwrap_or(id);
                NamedRow {
                    id: id.into(),
                    name: name.into(),
                    enabled: !id.is_empty(),
                    ..Default::default()
                }
            })
            .collect::<Vec<_>>(),
    ))
}

fn knowledge_text(entry: &Value) -> String {
    let mut text = String::new();
    for (key, label) in [
        ("summary", "概要"),
        ("conditions", "适用条件"),
        ("guidance", "使用建议"),
    ] {
        if let Some(value) = property(entry, key).and_then(Value::as_str) {
            text.push_str(&format!("{label}\n{value}\n\n"));
        }
    }
    if let Some(tools) = property(entry, "tools").and_then(Value::as_array) {
        for tool in tools {
            for key in ["name", "description", "usage"] {
                if let Some(value) = property(tool, key).and_then(Value::as_str) {
                    text.push_str(value);
                    text.push('\n');
                }
            }
            if let Some(files) = property(tool, "files").and_then(Value::as_object) {
                text.push_str(&format!(
                    "源文件：{}\n\n",
                    files.keys().cloned().collect::<Vec<_>>().join("、")
                ));
            }
        }
    }
    text
}
