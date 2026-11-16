use crate::{
    Context, UiInput, show_dialog,
    view::{App, NamedRow},
};
use gamel10n_core::{
    CoreError,
    settings::{ApiProfile, GlobalSettings},
};
use gamel10n_translation::provider::ConnectionTestKind;
use serde_json::{Value, json};
use slint::{ComponentHandle, ModelRc, VecModel};

pub struct PendingApiTest(Context);

impl PendingApiTest {
    pub fn start(ctx: &Context, ui: &crate::view::MainWindow) -> Option<Self> {
        let mut session = ctx
            .session
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if session.api_test.is_some() {
            return None;
        }
        session.api_test = Some(gamel10n_core::CancellationToken::new());
        let app = ui.global::<App>();
        app.set_api_testing(true);
        app.set_api_test_result("".into());
        app.set_api_test_failed(false);
        Some(Self(ctx.clone()))
    }
}

impl Drop for PendingApiTest {
    fn drop(&mut self) {
        let session = self.0.session.clone();
        let _ = self.0.window.upgrade_in_event_loop(move |ui| {
            session
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .api_test = None;
            ui.global::<App>().set_api_testing(false);
        });
    }
}

pub fn cancel_api_test(ctx: &Context) {
    if let Some(token) = ctx
        .session
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .api_test
        .as_ref()
    {
        token.cancel();
    }
}

const REASONING_PRESETS: [&str; 6] = ["", "low", "medium", "high", "xhigh", "max"];

fn update_api_profile(profile: &mut ApiProfile, input: &UiInput) -> Result<(), CoreError> {
    profile.analysis_max_output_tokens = if input.api_output_tokens.trim().is_empty() {
        None
    } else {
        Some(
            input
                .api_output_tokens
                .trim()
                .parse()
                .map_err(|_| CoreError::InvalidData("分析输出上限应为整数。".into()))?,
        )
    };
    profile.analysis_output_limit()?;
    profile.id = input.api_id.clone();
    profile.name = input.api_name.trim().into();
    profile.endpoint = input.api_endpoint.trim().trim_end_matches('/').into();
    profile.model = input.api_model.trim().into();
    profile.protocol = if input.api_protocol == 1 {
        "Responses"
    } else {
        "ChatCompletions"
    }
    .into();
    profile.translation_format = if input.api_format == 1 {
        "PlainText"
    } else {
        "JsonBatch"
    }
    .into();
    let effort = REASONING_PRESETS
        .get(input.api_reasoning.max(0) as usize)
        .copied()
        .unwrap_or(&input.api_reasoning_custom)
        .trim();
    profile.analysis_reasoning_effort = (!effort.is_empty()).then(|| effort.to_owned());
    Ok(())
}

fn draft_matches(ui: &crate::view::MainWindow, input: &UiInput) -> bool {
    let app = ui.global::<App>();
    app.get_api_id().as_str() == input.api_id
        && app.get_api_name().as_str() == input.api_name
        && app.get_api_endpoint().as_str() == input.api_endpoint
        && app.get_api_model().as_str() == input.api_model
        && app.get_api_key().as_str() == input.api_key
        && app.get_api_protocol() == input.api_protocol
        && app.get_api_format() == input.api_format
        && app.get_api_reasoning() == input.api_reasoning
        && app.get_api_reasoning_custom().as_str() == input.api_reasoning_custom
        && app.get_api_output_tokens().as_str() == input.api_output_tokens
}

pub async fn refresh(ctx: &Context, project_id: Option<&str>) -> Result<(), CoreError> {
    let host = ctx.app().await;
    let settings = host.settings().await?;
    let project = if let Some(id) = project_id {
        Some(host.open_project(id).await?)
    } else {
        None
    };
    let api_summary = if let Some(id) = project_id {
        Some(host.task_api_summary(id).await?)
    } else {
        None
    };
    let ps = project
        .as_ref()
        .map(|p| p.settings.clone())
        .unwrap_or(json!({}));
    let config = &settings.api_configuration;
    let analysis = ps
        .get("analysisProfileId")
        .and_then(Value::as_str)
        .or(config.analysis_profile_id.as_deref());
    let index = config
        .profiles
        .iter()
        .position(|p| Some(p.id.as_str()) == analysis)
        .map(|i| i as i32)
        .unwrap_or(-1);
    let ids = ps
        .get("translationProfileIds")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_else(|| config.chain());
    let chain = chain_rows(&settings, &ids);
    let apis = config
        .profiles
        .iter()
        .map(|p| NamedRow {
            id: p.id.clone().into(),
            name: p.name.clone().into(),
            detail: format!(
                "{} · {}{}",
                p.protocol,
                p.model,
                if p.translation_format == "PlainText" {
                    " · 逐条纯文本"
                } else {
                    ""
                }
            )
            .into(),
            enabled: true,
        })
        .collect::<Vec<_>>();
    let names = config
        .profiles
        .iter()
        .map(|p| p.name.clone().into())
        .collect::<Vec<_>>();
    let project_id = project_id.map(str::to_owned);
    ctx.update(move |ui| {
        let a = ui.global::<App>();
        if project_id
            .as_deref()
            .is_some_and(|id| a.get_project_id().as_str() != id)
        {
            return;
        }
        if project_id.is_none() && a.get_page() == 0 && !a.get_project_id().is_empty() {
            return;
        }
        a.set_apis(ModelRc::new(VecModel::from(apis)));
        a.set_api_names(ModelRc::new(VecModel::from(names)));
        a.set_chain(ModelRc::new(VecModel::from(chain)));
        a.set_analysis_index(index);
        a.set_loaded_analysis_index(index);
        if !a.get_busy()
            && let Some(api_summary) = api_summary
        {
            a.set_task_api_summary(api_summary.into());
        }
        a.set_chain_index(if ids.is_empty() { -1 } else { 0 });
        a.set_candidate_api_index(if settings.api_configuration.profiles.is_empty() {
            -1
        } else {
            0
        });
        a.set_budget(
            ps.get("adaptationTokenBudget")
                .and_then(Value::as_u64)
                .unwrap_or(settings.adaptation_token_budget)
                .to_string()
                .into(),
        );
        a.set_turns(
            ps.get("adaptationMaximumTurns")
                .and_then(Value::as_i64)
                .unwrap_or(settings.adaptation_maximum_turns.into()) as i32,
        );
        a.set_loaded_budget(a.get_budget());
        a.set_loaded_turns(a.get_turns());
        a.set_concurrency(settings.maximum_translations as i32);
        a.set_isolation_supported(cfg!(windows));
        a.set_isolate_llm_execution(cfg!(windows) && settings.isolate_llm_execution);
        a.set_language(
            ps.get("targetLanguage")
                .and_then(Value::as_str)
                .unwrap_or("简体中文")
                .into(),
        );
        a.set_style(
            ps.get("style")
                .and_then(Value::as_str)
                .unwrap_or("自然流畅，保留人物语气、情绪和原文含义。")
                .into(),
        );
        a.set_terminology(
            ps.get("terminology")
                .and_then(Value::as_str)
                .unwrap_or("")
                .into(),
        );
        a.set_batch_size(ps.get("batchSize").and_then(Value::as_i64).unwrap_or(24) as i32);
    });
    Ok(())
}

pub fn adaptation_limit_updates(input: &UiInput) -> Result<Value, CoreError> {
    let budget = input
        .budget
        .trim()
        .parse::<u64>()
        .map_err(|_| CoreError::InvalidData("Token 预算应为整数。".into()))?;
    if !(1_000..=20_000_000).contains(&budget) || !(1..=400).contains(&input.turns) {
        return Err(CoreError::InvalidData(
            "Token 预算应为 1,000–20,000,000，轮数应为 1–400。".into(),
        ));
    }
    // Compare with the displayed values so saving unrelated settings does not pin defaults.
    let mut updates = json!({});
    if input.loaded_budget.trim().parse::<u64>().ok() != Some(budget) {
        updates["adaptationTokenBudget"] = json!(budget);
    }
    if input.turns != input.loaded_turns {
        updates["adaptationMaximumTurns"] = json!(input.turns);
    }
    Ok(updates)
}

fn project_settings_updates(
    action: &str,
    input: &UiInput,
    analysis: Option<String>,
    chain: Vec<String>,
) -> Result<Value, CoreError> {
    let mut updates = json!({});
    match action {
        "analysis-api" if input.analysis_index != input.loaded_analysis_index => {
            updates["analysisProfileId"] = json!(analysis);
        }
        "chain-add" | "chain-remove" | "chain-up" | "chain-down" if chain != input.chain => {
            updates["translationProfileIds"] = json!(chain);
        }
        "save-project-settings" => {
            updates = adaptation_limit_updates(input)?;
            updates["targetLanguage"] = json!(input.language);
            updates["style"] = json!(input.style);
            updates["terminology"] = json!(input.terminology);
            updates["batchSize"] = json!(input.batch_size);
        }
        _ => {}
    }
    Ok(updates)
}

fn chain_rows(settings: &GlobalSettings, ids: &[String]) -> Vec<NamedRow> {
    ids.iter()
        .map(|id| {
            let profile = settings
                .api_configuration
                .profiles
                .iter()
                .find(|p| p.id == *id);
            NamedRow {
                id: id.clone().into(),
                name: profile
                    .map(|p| p.name.as_str())
                    .unwrap_or("已删除的 API，请重新选择")
                    .into(),
                detail: profile.map(|p| p.model.as_str()).unwrap_or("").into(),
                enabled: profile.is_some(),
            }
        })
        .collect()
}

pub async fn handle(ctx: &Context, action: &str, input: &UiInput) -> Result<bool, CoreError> {
    match action {
        "api-test-cancel" => cancel_api_test(ctx),
        "api-test" => {
            let host = ctx.app().await;
            let kind = match input.argument.as_str() {
                "translation" => ConnectionTestKind::Translation,
                "analysis" => ConnectionTestKind::Analysis,
                _ => return Err(CoreError::InvalidData("未知的接口测试类型。".into())),
            };
            let (profile, key) = if input.api_test_draft {
                let mut profile = ApiProfile::default();
                update_api_profile(&mut profile, input)?;
                (profile, Some(input.api_key.trim().to_owned()))
            } else {
                let profile = host
                    .settings()
                    .await?
                    .api_configuration
                    .profiles
                    .into_iter()
                    .find(|profile| profile.id == input.api_id)
                    .ok_or_else(|| CoreError::InvalidData("请先选择模型配置。".into()))?;
                (profile, None)
            };
            let cancellation = ctx
                .session
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .api_test
                .clone()
                .unwrap_or_default();
            let result = host
                .test_api_connection(&profile, key, kind, &cancellation)
                .await;
            let label = format!(
                "{} / {} · {}",
                if profile.name.is_empty() {
                    "未命名配置"
                } else {
                    &profile.name
                },
                profile.model,
                if kind == ConnectionTestKind::Analysis {
                    "分析测试"
                } else {
                    "翻译测试"
                }
            );
            let (failed, message) = match result {
                Ok(report) => {
                    let usage = if report.usage.requests == 0 {
                        "未发起请求".into()
                    } else if report.usage.usage_requests == 0 {
                        format!("{} 次请求 · Token 未返回", report.usage.requests)
                    } else {
                        format!(
                            "{} 次请求 · 输入 {} / 输出 {} Token",
                            report.usage.requests,
                            report.usage.input_tokens,
                            report.usage.output_tokens
                        )
                    };
                    (
                        !report.passed,
                        format!(
                            "{label}\n{}\n{:.1} 秒 · {usage}",
                            report.message,
                            report.elapsed_ms as f64 / 1000.0
                        ),
                    )
                }
                Err(error) => (true, format!("{label}\n{error}")),
            };
            let input = input.clone();
            ctx.update(move |ui| {
                let app = ui.global::<App>();
                if input.api_test_draft {
                    if app.get_modal() != "api" || !draft_matches(&ui, &input) {
                        return;
                    }
                } else if app.get_page() != 1 || app.get_api_id().as_str() != input.api_id {
                    return;
                }
                app.set_api_test_failed(failed);
                app.set_api_test_result(message.into());
            });
        }
        "api-add" => {
            ctx.update(|ui| {
                let a = ui.global::<App>();
                a.set_api_id("".into());
                a.set_api_test_result("".into());
                a.set_api_name("".into());
                a.set_api_endpoint("".into());
                a.set_api_model("".into());
                a.set_api_key("".into());
                a.set_api_protocol(0);
                a.set_api_format(0);
                a.set_api_reasoning(0);
                a.set_api_reasoning_custom("".into());
                a.set_api_output_tokens("".into());
                a.set_api_remember_key(true);
                a.set_error("".into());
                a.set_modal("api".into());
            });
        }
        "api-edit" => {
            let settings = ctx.app().await.settings().await?;
            let p = settings
                .api_configuration
                .profiles
                .into_iter()
                .find(|p| p.id == input.argument)
                .ok_or_else(|| CoreError::InvalidData("API 配置不存在。".into()))?;
            let key = ctx.app().await.decrypt_key(&p).await;
            ctx.update(move |ui| {
                let a = ui.global::<App>();
                a.set_api_id(p.id.into());
                a.set_api_test_result("".into());
                a.set_api_name(p.name.into());
                a.set_api_endpoint(p.endpoint.into());
                a.set_api_model(p.model.into());
                match key {
                    Ok(key) => {
                        a.set_api_key(key.into());
                        a.set_error("".into());
                    }
                    Err(error) => {
                        a.set_api_key("".into());
                        a.set_error(error.to_string().into());
                    }
                }
                a.set_api_remember_key(!p.requires_session_key);
                a.set_api_protocol(i32::from(p.protocol == "Responses"));
                a.set_api_format(i32::from(p.translation_format == "PlainText"));
                let effort = p.analysis_reasoning_effort.as_deref().unwrap_or("");
                a.set_api_reasoning(
                    REASONING_PRESETS
                        .iter()
                        .position(|s| *s == effort)
                        .unwrap_or(REASONING_PRESETS.len()) as i32,
                );
                a.set_api_reasoning_custom(effort.into());
                a.set_api_output_tokens(
                    p.analysis_max_output_tokens
                        .map(|value| value.to_string())
                        .unwrap_or_default()
                        .into(),
                );
                a.set_modal("api".into());
            });
        }
        "api-save" => {
            let host = ctx.app().await;
            let settings = host.settings().await?;
            let old = settings
                .api_configuration
                .profiles
                .into_iter()
                .find(|p| p.id == input.api_id);
            let mut p = old.unwrap_or_else(ApiProfile::default);
            if let Err(error) = update_api_profile(&mut p, input) {
                ctx.update(|ui| ui.global::<App>().set_modal("api".into()));
                return Err(error);
            }
            if let Err(error) = host
                .save_api_profile(p, Some(input.api_key.trim().into()), input.api_remember_key)
                .await
            {
                ctx.update(|ui| ui.global::<App>().set_modal("api".into()));
                return Err(error);
            }
            ctx.update(|ui| {
                ui.global::<App>().set_api_key("".into());
                ui.global::<App>().set_api_test_result("".into());
            });
            refresh(ctx, None).await?;
        }
        "api-delete-dialog" => show_dialog(
            ctx,
            "api-delete",
            "删除模型配置",
            "项目中对此配置的独立引用会显示为不可用，可重新选择其他 API。".into(),
        ),
        "api-delete" => {
            ctx.app().await.delete_api_profile(&input.api_id).await?;
            ctx.update(|ui| {
                let app = ui.global::<App>();
                app.set_api_id("".into());
                app.set_api_test_result("".into());
            });
            refresh(ctx, None).await?;
        }
        "analysis-api"
        | "chain-add"
        | "chain-remove"
        | "chain-up"
        | "chain-down"
        | "save-defaults"
        | "save-project-settings" => {
            let host = ctx.app().await;
            let settings = host.settings().await?;
            let mut chain = input.chain.clone();
            let index = input.chain_index.max(0) as usize;
            match action {
                "chain-add" => {
                    if let Some(p) = settings
                        .api_configuration
                        .profiles
                        .get(input.candidate_api_index.max(0) as usize)
                        && !chain.contains(&p.id)
                    {
                        chain.push(p.id.clone());
                    }
                }
                "chain-remove" if index < chain.len() => {
                    chain.remove(index);
                }
                "chain-up" if index > 0 && index < chain.len() => {
                    chain.swap(index, index - 1);
                }
                "chain-down" if index + 1 < chain.len() => {
                    chain.swap(index, index + 1);
                }
                _ => {}
            }
            let analysis = settings
                .api_configuration
                .profiles
                .get(input.analysis_index as usize)
                .map(|p| p.id.clone());
            let edits_limits = action == "save-defaults" || action == "save-project-settings";
            let budget = if edits_limits && input.page == 1 {
                input
                    .budget
                    .trim()
                    .parse::<u64>()
                    .map_err(|_| CoreError::InvalidData("Token 预算应为整数。".into()))?
            } else {
                settings.adaptation_token_budget
            };
            if input.page == 1 {
                host.save_defaults(
                    analysis,
                    chain,
                    budget,
                    if action == "save-defaults" {
                        input.turns.max(0) as u32
                    } else {
                        settings.adaptation_maximum_turns
                    },
                )
                .await?;
                refresh(ctx, None).await?;
            } else {
                let updates = project_settings_updates(action, input, analysis, chain)?;
                host.save_project_settings(&input.project_id, updates)
                    .await?;
                refresh(ctx, Some(&input.project_id)).await?;
            }
            if action != "save-project-settings" {
                let original = input.clone();
                ctx.update(move |ui| {
                    let a = ui.global::<App>();
                    if a.get_page() != original.page
                        || a.get_project_id().as_str() != original.project_id
                    {
                        return;
                    }
                    a.set_language(original.language.into());
                    a.set_style(original.style.into());
                    a.set_terminology(original.terminology.into());
                    a.set_batch_size(original.batch_size);
                    if !edits_limits {
                        a.set_budget(original.budget.into());
                        a.set_turns(original.turns);
                        a.set_loaded_budget(original.loaded_budget.into());
                        a.set_loaded_turns(original.loaded_turns);
                    }
                });
            }
        }
        "save-application" => {
            let host = ctx.app().await;
            host.save_concurrency(input.concurrency.max(0) as usize)
                .await?;
            host.save_execution_isolation(input.isolate_llm_execution)
                .await?;
            ctx.update(|ui| ui.global::<App>().set_activity("程序设置已保存。".into()));
        }
        _ => return Ok(false),
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn analysis_output_limit_preserves_defaults_and_rejects_invalid_drafts() {
        let mut profile = ApiProfile::default();
        let mut input = UiInput::default();
        update_api_profile(&mut profile, &input).unwrap();
        assert_eq!(profile.analysis_max_output_tokens, None);
        assert_eq!(profile.analysis_output_limit().unwrap(), 64_000);
        input.api_output_tokens = " 128000 ".into();
        update_api_profile(&mut profile, &input).unwrap();
        assert_eq!(profile.analysis_output_limit().unwrap(), 128_000);
        for value in ["not a number", "0", "128001"] {
            input.api_output_tokens = value.into();
            assert!(update_api_profile(&mut profile, &input).is_err());
        }
    }

    fn loaded_input() -> UiInput {
        UiInput {
            analysis_index: 0,
            loaded_analysis_index: 0,
            chain: vec!["translation-default".into()],
            budget: "1000000".into(),
            loaded_budget: "1000000".into(),
            turns: 60,
            loaded_turns: 60,
            language: "简体中文".into(),
            batch_size: 24,
            ..UiInput::default()
        }
    }

    #[test]
    fn saving_translation_options_does_not_pin_global_defaults() {
        let input = UiInput {
            style: "保留原文语气".into(),
            ..loaded_input()
        };
        let updates = project_settings_updates(
            "save-project-settings",
            &input,
            Some("analysis-default".into()),
            input.chain.clone(),
        )
        .unwrap();

        assert_eq!(
            updates,
            json!({
                "targetLanguage": "简体中文", "style": "保留原文语气",
                "terminology": "", "batchSize": 24
            })
        );
    }

    #[test]
    fn editing_one_limit_preserves_the_other_settings_source() {
        for (budget, turns, expected) in [
            ("2000000", 60, json!({"adaptationTokenBudget": 2_000_000})),
            ("1000000", 80, json!({"adaptationMaximumTurns": 80})),
            ("01000000", 60, json!({})),
        ] {
            let input = UiInput {
                budget: budget.into(),
                turns,
                ..loaded_input()
            };
            assert_eq!(adaptation_limit_updates(&input).unwrap(), expected);
        }
    }

    #[test]
    fn api_edits_only_save_the_changed_selection() {
        let mut input = loaded_input();
        assert_eq!(
            project_settings_updates(
                "analysis-api",
                &input,
                Some("analysis-default".into()),
                input.chain.clone()
            )
            .unwrap(),
            json!({})
        );
        assert_eq!(
            project_settings_updates("chain-add", &input, None, input.chain.clone()).unwrap(),
            json!({})
        );
        input.analysis_index = 1;
        assert_eq!(
            project_settings_updates(
                "analysis-api",
                &input,
                Some("analysis-custom".into()),
                input.chain.clone()
            )
            .unwrap(),
            json!({"analysisProfileId": "analysis-custom"})
        );
        assert_eq!(
            project_settings_updates("chain-remove", &input, None, Vec::new()).unwrap(),
            json!({"translationProfileIds": []})
        );
    }
}
