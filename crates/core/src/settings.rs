//! Settings shared by the desktop and command line; unknown fields survive updates.
use crate::{AppEvent, Application, CoreError};
use gamel10n_engines::{CancellationToken, ToolProcess};
use gamel10n_translation::files::{hash_text, read_json, write_json};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::BTreeMap, time::Duration};

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ApiProfile {
    pub id: String,
    pub name: String,
    pub endpoint: String,
    pub model: String,
    pub protocol: String,
    pub encrypted_api_key: Option<String>,
    pub requires_session_key: bool,
    pub analysis_reasoning_effort: Option<String>,
    pub translation_format: String,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ApiConfiguration {
    pub profiles: Vec<ApiProfile>,
    pub analysis_profile_id: Option<String>,
    pub translation_profile_id: Option<String>,
    pub translation_profile_ids: Option<Vec<String>>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}
impl ApiConfiguration {
    pub fn chain(&self) -> Vec<String> {
        self.translation_profile_ids
            .clone()
            .unwrap_or_else(|| self.translation_profile_id.clone().into_iter().collect())
    }
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct GlobalSettings {
    pub schema_version: u32,
    pub last_game_id: Option<String>,
    pub maximum_translations: usize,
    pub adaptation_token_budget: u64,
    pub adaptation_maximum_turns: u32,
    pub api_configuration: ApiConfiguration,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}
impl Default for GlobalSettings {
    fn default() -> Self {
        Self {
            schema_version: 2,
            last_game_id: None,
            maximum_translations: 2,
            adaptation_token_budget: 1_000_000,
            adaptation_maximum_turns: 60,
            api_configuration: ApiConfiguration::default(),
            extra: BTreeMap::new(),
        }
    }
}

impl Application {
    pub async fn remember_project(&self, id: &str) -> Result<(), CoreError> {
        if self.settings().await?.last_game_id.as_deref() == Some(id) {
            return Ok(());
        }
        self.update_settings(|settings| {
            settings.last_game_id = Some(id.into());
            Ok(())
        })
        .await?;
        Ok(())
    }
    pub async fn settings(&self) -> Result<GlobalSettings, CoreError> {
        let _guard = self.settings_gate.lock().await;
        self.read_settings()
    }

    fn read_settings(&self) -> Result<GlobalSettings, CoreError> {
        let path = self.data_directory().join("settings.json");
        if !path.is_file() {
            return Ok(GlobalSettings::default());
        }
        let mut value: Value = read_json(&path)?;
        if value.get("apiConfiguration").is_none_or(Value::is_null) {
            let endpoint = value.get("endpoint").and_then(Value::as_str).unwrap_or("");
            let model = value.get("model").and_then(Value::as_str).unwrap_or("");
            value["apiConfiguration"] = if endpoint.is_empty() && model.is_empty() {
                json!({})
            } else {
                json!({"profiles":[{"id":"default","name":"默认 API","endpoint":endpoint,"model":model,"protocol":value.get("protocol").cloned().unwrap_or(json!("ChatCompletions"))}],"analysisProfileId":"default","translationProfileId":"default"})
            };
        }
        serde_json::from_value(value).map_err(|e| CoreError::InvalidData(e.to_string()))
    }

    async fn update_settings(
        &self,
        change: impl FnOnce(&mut GlobalSettings) -> Result<(), CoreError>,
    ) -> Result<GlobalSettings, CoreError> {
        self.store.ensure_writable().await?;
        let _guard = self.settings_gate.lock().await;
        let mut settings = self.read_settings()?;
        change(&mut settings)?;
        write_json(&self.data_directory().join("settings.json"), &settings)?;
        Ok(settings)
    }

    pub async fn save_defaults(
        &self,
        analysis: Option<String>,
        chain: Vec<String>,
        budget: u64,
        turns: u32,
    ) -> Result<(), CoreError> {
        validate_limits(budget, turns)?;
        self.update_settings(|s| {
            validate_profiles(&s.api_configuration, analysis.as_deref(), &chain)?;
            s.api_configuration.analysis_profile_id = analysis;
            s.api_configuration.translation_profile_id = chain.first().cloned();
            s.api_configuration.translation_profile_ids = Some(chain);
            s.adaptation_token_budget = budget;
            s.adaptation_maximum_turns = turns;
            Ok(())
        })
        .await?;
        Ok(())
    }

    pub async fn save_concurrency(&self, count: usize) -> Result<(), CoreError> {
        if !(1..=8).contains(&count) {
            return Err(CoreError::InvalidData("并行项目数应为 1–8。".into()));
        }
        self.update_settings(|s| {
            s.maximum_translations = count;
            Ok(())
        })
        .await?;
        Ok(())
    }

    pub async fn save_api_profile(
        &self,
        mut profile: ApiProfile,
        new_key: Option<String>,
        remember_key: bool,
    ) -> Result<(), CoreError> {
        if profile.name.trim().is_empty()
            || profile.name.chars().count() > 80
            || profile.name.chars().any(char::is_control)
            || profile.model.trim().is_empty()
        {
            return Err(CoreError::InvalidData(
                "请填写有效的 API 名称和模型。".into(),
            ));
        }
        if !profile.endpoint.starts_with("http://") && !profile.endpoint.starts_with("https://") {
            return Err(CoreError::InvalidData(
                "API 地址应以 http:// 或 https:// 开头。".into(),
            ));
        }
        if !matches!(profile.protocol.as_str(), "ChatCompletions" | "Responses") {
            return Err(CoreError::InvalidData("API 协议无效。".into()));
        }
        use gamel10n_translation::provider::{
            ApiProtocol, OpenAiProvider, ProviderSettings, TranslationFormat,
        };
        OpenAiProvider::new(ProviderSettings {
            endpoint: profile.endpoint.clone(),
            api_key: new_key.clone().unwrap_or_default(),
            model: profile.model.clone(),
            protocol: if profile.protocol == "Responses" {
                ApiProtocol::Responses
            } else {
                ApiProtocol::ChatCompletions
            },
            translation_format: if profile.translation_format == "PlainText" {
                TranslationFormat::PlainText
            } else {
                TranslationFormat::JsonBatch
            },
            analysis_reasoning_effort: profile.analysis_reasoning_effort.clone(),
        })
        .map_err(|e| CoreError::InvalidData(e.to_string()))?;
        if profile.id.is_empty() {
            profile.id = hash_text(&format!(
                "{}{}",
                profile.name,
                chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
            ))[..24]
                .to_string();
        }
        let profile_id = profile.id.clone();
        let mut session_key = None;
        if let Some(key) = &new_key {
            profile.encrypted_api_key = if key.is_empty() || !remember_key {
                None
            } else {
                Some(self.transform_key("protect", key).await?)
            };
            profile.requires_session_key = !remember_key && !key.is_empty();
            if profile.requires_session_key {
                session_key = Some(key.clone());
            }
        }
        self.update_settings(|s| {
            let config = &mut s.api_configuration;
            if config
                .profiles
                .iter()
                .any(|p| p.id != profile.id && p.name.eq_ignore_ascii_case(&profile.name))
            {
                return Err(CoreError::InvalidData("已有同名 API 配置。".into()));
            }
            if config.profiles.is_empty() {
                config.analysis_profile_id = Some(profile.id.clone());
                config.translation_profile_ids = Some(vec![profile.id.clone()]);
                config.translation_profile_id = Some(profile.id.clone());
            }
            if let Some(existing) = config.profiles.iter_mut().find(|p| p.id == profile.id) {
                *existing = profile;
            } else {
                config.profiles.push(profile);
            }
            Ok(())
        })
        .await?;
        if new_key.is_some() {
            let mut keys = self.session_keys.lock().await;
            if let Some(key) = session_key {
                keys.insert(profile_id, key);
            } else {
                keys.remove(&profile_id);
            }
        }
        Ok(())
    }

    pub async fn delete_api_profile(&self, id: &str) -> Result<(), CoreError> {
        self.update_settings(|s| {
            let c = &mut s.api_configuration;
            let chain = c
                .chain()
                .into_iter()
                .filter(|p| p != id)
                .collect::<Vec<_>>();
            c.profiles.retain(|p| p.id != id);
            if c.analysis_profile_id.as_deref() == Some(id) {
                c.analysis_profile_id = None;
            }
            c.translation_profile_id = chain.first().cloned();
            c.translation_profile_ids = Some(chain);
            Ok(())
        })
        .await?;
        self.session_keys.lock().await.remove(id);
        Ok(())
    }

    pub async fn decrypt_key(&self, profile: &ApiProfile) -> Result<String, CoreError> {
        if profile.requires_session_key {
            return self
                .session_keys
                .lock()
                .await
                .get(&profile.id)
                .cloned()
                .ok_or_else(|| {
                    CoreError::InvalidData(format!(
                        "“{}”的密钥只保留在上次运行中，请编辑配置重新填写。",
                        profile.name
                    ))
                });
        }
        match profile.encrypted_api_key.as_deref() {
            Some(value) if !value.is_empty() => self.transform_key("unprotect", value).await,
            _ => Ok(String::new()),
        }
    }
    async fn transform_key(&self, operation: &str, value: &str) -> Result<String, CoreError> {
        let script = self.tools.root.join("host/credentials.py");
        let response: Value = ToolProcess::new(self.tools.python())
            .with_arguments([script.into_os_string()])
            .with_timeout(Duration::from_secs(10))
            .execute(
                &json!({"operation":operation,"value":value}),
                &CancellationToken::new(),
            )
            .await?;
        response
            .get("value")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| CoreError::InvalidData("无法读取 API 密钥，请重新填写。".into()))
    }

    pub async fn save_project_settings(&self, id: &str, updates: Value) -> Result<(), CoreError> {
        // Keep the metadata read/modify/write atomic with task-summary recovery and removal.
        let _project = self.lock_project(id).await?;
        let _lifecycle = self.project_lifecycle.lock().await;
        let mut project = self.store.get(id).await?;
        let previous_settings = project.settings.clone();
        if !project.settings.is_object() {
            project.settings = json!({});
        }
        let Some(map) = updates.as_object() else {
            return Err(CoreError::InvalidData("项目设置无效。".into()));
        };
        for (key, value) in map {
            if value.is_null() {
                project.settings.as_object_mut().map(|m| m.remove(key));
            } else {
                project.settings[key] = value.clone();
            }
        }
        let s = &project.settings;
        if s.get("targetLanguage")
            .and_then(Value::as_str)
            .is_some_and(|v| v.trim().is_empty())
        {
            return Err(CoreError::InvalidData("目标语言不能为空。".into()));
        }
        if let Some(batch) = s.get("batchSize").and_then(Value::as_u64)
            && !(1..=80).contains(&batch)
        {
            return Err(CoreError::InvalidData("每批文本数应为 1–80。".into()));
        }
        let defaults = self.settings().await?;
        validate_limits(
            s.get("adaptationTokenBudget")
                .and_then(Value::as_u64)
                .unwrap_or(defaults.adaptation_token_budget),
            s.get("adaptationMaximumTurns")
                .and_then(Value::as_u64)
                .unwrap_or(defaults.adaptation_maximum_turns.into()) as u32,
        )?;
        if project.settings == previous_settings {
            return Ok(());
        }
        project.mark_updated();
        self.store.save(&project).await?;
        let _ = self.events.send(AppEvent::ProjectsChanged);
        Ok(())
    }

    pub async fn remove_projects(
        &self,
        ids: &[String],
        delete_data: bool,
    ) -> Result<(), CoreError> {
        // Stable ordering avoids deadlocks between overlapping multi-project removals.
        let mut ids: Vec<_> = ids.iter().collect();
        ids.sort();
        ids.dedup();
        let mut guards = Vec::with_capacity(ids.len());
        for id in &ids {
            guards.push(self.lock_project(id).await?);
        }
        {
            let _lifecycle = self.project_lifecycle.lock().await;
            for id in &ids {
                if self.task_state(id).await.is_some_and(|s| s.running) {
                    return Err(CoreError::ProjectBusy);
                }
            }
        }
        if self
            .settings()
            .await?
            .last_game_id
            .as_ref()
            .is_some_and(|id| ids.contains(&id))
        {
            self.update_settings(|settings| {
                settings.last_game_id = None;
                Ok(())
            })
            .await?;
        }
        for id in ids {
            self.store.remove(id, delete_data).await?;
            self.invalidate_project_cache(id).await;
            let _ = self.events.send(AppEvent::ProjectsChanged);
            if delete_data {
                let root = self.data_directory().to_path_buf();
                let game_id = id.clone();
                tokio::task::spawn_blocking(move || {
                    crate::catalog::EngineCatalog::open(&root)?.forget_game(&game_id)
                })
                .await?
                .map_err(|error| {
                    CoreError::InvalidData(format!(
                        "项目数据已删除，但引擎版本引用记录未能移除：{error}"
                    ))
                })?;
            }
        }
        Ok(())
    }
}

fn validate_limits(budget: u64, turns: u32) -> Result<(), CoreError> {
    if !(1_000..=20_000_000).contains(&budget) || !(1..=400).contains(&turns) {
        return Err(CoreError::InvalidData(
            "Token 预算应为 1,000–20,000,000，轮数应为 1–400。".into(),
        ));
    }
    Ok(())
}
fn validate_profiles(
    c: &ApiConfiguration,
    analysis: Option<&str>,
    chain: &[String],
) -> Result<(), CoreError> {
    let mut seen = std::collections::HashSet::new();
    if chain
        .iter()
        .any(|id| !seen.insert(id) || !c.profiles.iter().any(|p| p.id == *id))
        || analysis.is_some_and(|id| !c.profiles.iter().any(|p| p.id == id))
    {
        return Err(CoreError::InvalidData(
            "API 配置不存在或翻译链有重复项。".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn queued_removal_only_excludes_the_target_project() {
        let root = tempfile::tempdir().unwrap();
        let app = Application::open(root.path().join("data"), &crate::ToolPaths::new("missing"))
            .await
            .unwrap();
        let game = root.path().join("game");
        let other_game = root.path().join("other");
        std::fs::create_dir(&game).unwrap();
        std::fs::create_dir(&other_game).unwrap();
        let project = app.add_project(&game).await.unwrap();
        let other = app.add_project(&other_game).await.unwrap();
        let guard = app.lock_project(&project.id).await.unwrap();
        let deleting = app.clone();
        let id = project.id.clone();
        let removal = tokio::spawn(async move { deleting.remove_projects(&[id], true).await });
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            assert_eq!(app.list_projects("").await.len(), 2);
            app.open_project(&other.id).await.unwrap();
            app.begin(&other.id, "translate", "test").await.unwrap();
            app.finish(&other.id, "translate", Ok("done".into()))
                .await
                .unwrap();
        })
        .await
        .unwrap();
        drop(guard);
        removal.await.unwrap().unwrap();
        assert_eq!(app.list_projects("").await.len(), 1);
        assert!(!app.project_directory(&project.id).unwrap().exists());
        app.add_project(&game).await.unwrap();
    }
    use crate::{ToolPaths, catalog::EngineCatalog};
    use std::fs;

    #[tokio::test]
    async fn removing_project_keeps_pins_until_its_data_is_explicitly_deleted() {
        let temporary = tempfile::tempdir().unwrap();
        let game = temporary.path().join("game");
        fs::create_dir(&game).unwrap();
        let data = temporary.path().join("data");
        let app = Application::open(data.clone(), &ToolPaths::new("missing"))
            .await
            .unwrap();
        let project = app.add_project(&game).await.unwrap();
        app.remember_project(&project.id).await.unwrap();
        let package = data.join("extensions/packages/engine.fixture/v1");
        fs::create_dir_all(&package).unwrap();
        let pin = data
            .join("extensions/games")
            .join(hash_text(&project.id))
            .join("active.json");
        write_json(
            &pin,
            &json!({"id":"engine.fixture","version":"v1","gameId":project.id}),
        )
        .unwrap();

        app.remove_projects(std::slice::from_ref(&project.id), false)
            .await
            .unwrap();
        assert!(pin.is_file());
        assert!(app.project_directory(&project.id).unwrap().is_dir());
        assert!(app.settings().await.unwrap().last_game_id.is_none());
        assert!(app.list_projects("").await.is_empty());

        app.add_project(&game).await.unwrap();
        app.remove_projects(std::slice::from_ref(&project.id), true)
            .await
            .unwrap();
        assert!(!pin.exists());
        assert!(!app.project_directory(&project.id).unwrap().exists());
        assert_eq!(
            EngineCatalog::open(&data)
                .unwrap()
                .candidate_package_directories()
                .unwrap(),
            [package]
        );
        assert!(game.is_dir());
    }

    #[tokio::test]
    async fn session_api_keys_survive_profile_edits_but_are_never_persisted() {
        let temporary = tempfile::tempdir().unwrap();
        let tools = ToolPaths::new("missing");
        let app = Application::open(temporary.path().into(), &tools)
            .await
            .unwrap();
        let profile = ApiProfile {
            name: "Session profile".into(),
            endpoint: "https://example.invalid/v1".into(),
            model: "fixture".into(),
            protocol: "ChatCompletions".into(),
            translation_format: "JsonBatch".into(),
            ..ApiProfile::default()
        };
        app.save_api_profile(profile, Some("session-secret-fixture".into()), false)
            .await
            .unwrap();
        let mut profile = app
            .settings()
            .await
            .unwrap()
            .api_configuration
            .profiles
            .remove(0);
        profile.name = "Renamed profile".into();
        app.save_api_profile(profile, None, false).await.unwrap();
        let profile = app
            .settings()
            .await
            .unwrap()
            .api_configuration
            .profiles
            .remove(0);
        assert_eq!(
            app.decrypt_key(&profile).await.unwrap(),
            "session-secret-fixture"
        );
        assert!(profile.encrypted_api_key.is_none());
        assert!(
            !fs::read_to_string(temporary.path().join("settings.json"))
                .unwrap()
                .contains("session-secret-fixture")
        );
        drop(app);
        let reopened = Application::open(temporary.path().into(), &tools)
            .await
            .unwrap();
        assert!(reopened.decrypt_key(&profile).await.is_err());
    }
}
