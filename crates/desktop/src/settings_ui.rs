use crate::{
    Context, UiInput, show_dialog,
    view::{App, NamedRow},
};
use gamel10n_core::{
    CoreError,
    settings::{ApiProfile, GlobalSettings},
};
use serde_json::{Value, json};
use slint::{ComponentHandle, ModelRc, VecModel};

pub async fn refresh(ctx: &Context, project_id: Option<&str>) -> Result<(), CoreError> {
    let host = ctx.app().await;
    let settings = host.settings().await?;
    let project = if let Some(id) = project_id {
        Some(host.open_project(id).await?)
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
        "api-add" => {
            ctx.update(|ui| {
                let a = ui.global::<App>();
                a.set_api_id("".into());
                a.set_api_name("".into());
                a.set_api_endpoint("".into());
                a.set_api_model("".into());
                a.set_api_key("".into());
                a.set_api_protocol(0);
                a.set_api_format(0);
                a.set_api_reasoning(0);
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
                a.set_api_reasoning(
                    ["", "low", "medium", "high", "xhigh"]
                        .iter()
                        .position(|s| *s == p.analysis_reasoning_effort.as_deref().unwrap_or(""))
                        .unwrap_or(0) as i32,
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
            p.id = input.api_id.clone();
            p.name = input.api_name.trim().into();
            p.endpoint = input.api_endpoint.trim().trim_end_matches('/').into();
            p.model = input.api_model.trim().into();
            p.protocol = if input.api_protocol == 1 {
                "Responses"
            } else {
                "ChatCompletions"
            }
            .into();
            p.translation_format = if input.api_format == 1 {
                "PlainText"
            } else {
                "JsonBatch"
            }
            .into();
            p.analysis_reasoning_effort = [
                None,
                Some("low"),
                Some("medium"),
                Some("high"),
                Some("xhigh"),
            ]
            .get(input.api_reasoning.max(0) as usize)
            .copied()
            .flatten()
            .map(str::to_owned);
            if let Err(error) = host
                .save_api_profile(p, Some(input.api_key.trim().into()), input.api_remember_key)
                .await
            {
                ctx.update(|ui| ui.global::<App>().set_modal("api".into()));
                return Err(error);
            }
            ctx.update(|ui| {
                ui.global::<App>().set_api_key("".into());
            });
            refresh(ctx, None).await?;
        }
        "api-delete-dialog" => show_dialog(
            ctx,
            "api-delete",
            "删除 API 配置",
            "项目中对此配置的独立引用会显示为不可用，可重新选择其他 API。".into(),
        ),
        "api-delete" => {
            ctx.app().await.delete_api_profile(&input.api_id).await?;
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
            ctx.app()
                .await
                .save_concurrency(input.concurrency.max(0) as usize)
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
