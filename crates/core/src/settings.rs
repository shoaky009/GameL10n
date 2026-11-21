//! Settings shared by the desktop and command line; unknown fields survive updates.
use crate::{AppEvent, Application, CoreError};
use gamel10n_engines::{CancellationToken, ToolProcess};
use gamel10n_translation::files::{hash_text, read_json, write_json};
use gamel10n_translation::provider::{ConnectionTestKind, RequestMetrics};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::BTreeMap, time::Duration};

#[derive(Clone, Debug)]
pub struct ApiConnectionTest {
    pub passed: bool,
    pub message: String,
    pub elapsed_ms: u64,
    pub usage: RequestMetrics,
}

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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub analysis_max_output_tokens: Option<u32>,
    pub translation_format: String,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl ApiProfile {
    /// Maximum output tokens for each intelligent adaptation request.
    pub fn analysis_output_limit(&self) -> Result<u32, CoreError> {
        use gamel10n_engines::adaptation::AdaptationLimits;
        let limit = self
            .analysis_max_output_tokens
            .unwrap_or(AdaptationLimits::DEFAULT_OUTPUT_TOKENS);
        if !(128..=AdaptationLimits::MAX_OUTPUT_TOKENS).contains(&limit) {
            return Err(CoreError::InvalidData(format!(
                "分析输出上限应为 128–{} Token。",
                AdaptationLimits::MAX_OUTPUT_TOKENS
            )));
        }
        Ok(limit)
    }
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

    /// An explicit empty project chain disables translation. Only an absent override inherits the global chain.
    pub fn project_translation_chain(&self, settings: &Value) -> Vec<String> {
        if let Some(ids) = settings["translationProfileIds"].as_array() {
            return ids
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect();
        }
        match settings["translationProfileId"].as_str() {
            Some(id) => vec![id.to_owned()],
            None => self.chain(),
        }
    }
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct GlobalSettings {
    pub schema_version: u32,
    pub last_game_id: Option<String>,
    pub maximum_translations: usize,
    pub adaptation_token_budget: u64,
    pub isolate_llm_execution: bool,
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
            isolate_llm_execution: cfg!(windows),
            api_configuration: ApiConfiguration::default(),
            extra: BTreeMap::new(),
        }
    }
}

impl Application {
    /// Tests an existing profile or unsaved draft without updating settings or projects.
    pub async fn test_api_connection(
        &self,
        profile: &ApiProfile,
        key: Option<String>,
        kind: ConnectionTestKind,
        cancellation: &crate::CancellationToken,
    ) -> Result<ApiConnectionTest, CoreError> {
        use gamel10n_translation::provider::{
            ApiProtocol, OpenAiProvider, ProviderSettings, TranslationFormat,
        };
        let key = match key {
            Some(key) => key,
            None => self.decrypt_key(profile).await?,
        };
        let provider = OpenAiProvider::new(ProviderSettings {
            endpoint: profile.endpoint.clone(),
            api_key: key,
            model: profile.model.clone(),
            protocol: match profile.protocol.as_str() {
                "ChatCompletions" => ApiProtocol::ChatCompletions,
                "Responses" => ApiProtocol::Responses,
                _ => return Err(CoreError::InvalidData("API 协议无效。".into())),
            },
            translation_format: if profile.translation_format == "PlainText" {
                TranslationFormat::PlainText
            } else {
                TranslationFormat::JsonBatch
            },
            analysis_reasoning_effort: profile.analysis_reasoning_effort.clone(),
        })
        .map_err(|error| CoreError::InvalidData(error.to_string()))?;
        let started = std::time::Instant::now();
        let result = tokio::time::timeout(
            Duration::from_secs(45),
            provider.test_connection(kind, cancellation),
        )
        .await;
        let (passed, message) = match result {
            Ok(Ok(())) => (
                true,
                match kind {
                    ConnectionTestKind::Translation => "翻译测试通过：响应格式正常。".into(),
                    ConnectionTestKind::Analysis => "分析测试通过：工具调用返回正常。".into(),
                },
            ),
            Ok(Err(error)) => (
                false,
                if matches!(
                    error.diagnostic.code.as_str(),
                    "connection-tool-call" | "configuration"
                ) {
                    error.to_string()
                } else if error.diagnostic.code == "output-token-limit" {
                    "响应达到测试长度上限，未能确认测试结果。".into()
                } else {
                    error.failure_summary()
                },
            ),
            Err(_) => (
                false,
                if provider.request_metrics().requests == 0 {
                    "等待执行超时（45 秒），可在其他模型任务结束后重试。".into()
                } else {
                    "接口响应超时（45 秒），请检查网络或模型响应速度。".into()
                },
            ),
        };
        Ok(ApiConnectionTest {
            passed,
            message,
            elapsed_ms: started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
            usage: provider.request_metrics(),
        })
    }

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

    pub(crate) fn read_settings(&self) -> Result<GlobalSettings, CoreError> {
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
    ) -> Result<(), CoreError> {
        validate_token_budget(budget)?;
        self.update_settings(|s| {
            validate_profiles(&s.api_configuration, analysis.as_deref(), &chain)?;
            s.api_configuration.analysis_profile_id = analysis;
            s.api_configuration.translation_profile_id = chain.first().cloned();
            s.api_configuration.translation_profile_ids = Some(chain);
            s.adaptation_token_budget = budget;
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

    pub async fn save_execution_isolation(&self, enabled: bool) -> Result<(), CoreError> {
        self.update_settings(|settings| {
            settings.isolate_llm_execution = enabled && cfg!(windows);
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
        profile.analysis_output_limit()?;
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
        validate_token_budget(
            s.get("adaptationTokenBudget")
                .and_then(Value::as_u64)
                .unwrap_or(defaults.adaptation_token_budget),
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
                if self.task_state(id).await.is_some_and(|s| s.running) || self.review_running(id) {
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

fn validate_token_budget(budget: u64) -> Result<(), CoreError> {
    if !(1_000..=20_000_000).contains(&budget) {
        return Err(CoreError::InvalidData(
            "Token 预算应为 1,000–20,000,000。".into(),
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

    #[test]
    fn old_settings_use_the_platform_isolation_default() {
        let settings: GlobalSettings = serde_json::from_value(json!({"schemaVersion":2})).unwrap();
        assert_eq!(settings.isolate_llm_execution, cfg!(windows));
    }

    #[tokio::test]
    async fn execution_isolation_preference_survives_other_settings_updates() {
        let root = tempfile::tempdir().unwrap();
        let app = Application::open(root.path().join("data"), &crate::ToolPaths::new("missing"))
            .await
            .unwrap();
        app.save_execution_isolation(false).await.unwrap();
        app.save_concurrency(3).await.unwrap();
        assert!(!app.settings().await.unwrap().isolate_llm_execution);
        app.save_execution_isolation(true).await.unwrap();
        assert_eq!(
            app.settings().await.unwrap().isolate_llm_execution,
            cfg!(windows)
        );
    }

    #[test]
    fn legacy_profiles_use_the_larger_analysis_limit_and_preserve_custom_fields() {
        let mut profile: ApiProfile =
            serde_json::from_value(json!({"id":"legacy","customOption":true})).unwrap();
        assert_eq!(profile.analysis_output_limit().unwrap(), 64_000);
        assert_eq!(profile.extra["customOption"], true);
        profile.analysis_max_output_tokens = Some(128_000);
        let restored: ApiProfile =
            serde_json::from_value(serde_json::to_value(profile).unwrap()).unwrap();
        assert_eq!(restored.analysis_output_limit().unwrap(), 128_000);
        assert_eq!(restored.extra["customOption"], true);
    }

    #[tokio::test]
    async fn selected_profile_output_limit_reaches_analysis_requests() {
        let root = tempfile::tempdir().unwrap();
        let app = Application::open(root.path().join("data"), &crate::ToolPaths::new("missing"))
            .await
            .unwrap();
        let game = root.path().join("game");
        std::fs::create_dir_all(&game).unwrap();
        let project = app.add_project(&game).await.unwrap();
        for (configured, expected) in [
            (Some(16_000), 16_000),
            (Some(128_000), 128_000),
            (None, 64_000),
        ] {
            app.save_api_profile(
                ApiProfile {
                    id: "analysis".into(),
                    name: "Analysis".into(),
                    endpoint: "http://127.0.0.1:9/v1".into(),
                    model: "fixture".into(),
                    protocol: "ChatCompletions".into(),
                    analysis_max_output_tokens: configured,
                    ..Default::default()
                },
                None,
                false,
            )
            .await
            .unwrap();
            let (_, limits) = app.analysis_provider(&project.id).await.unwrap();
            assert_eq!(limits.maximum_output_tokens, expected);
        }
        let mut invalid = app
            .settings()
            .await
            .unwrap()
            .api_configuration
            .profiles
            .remove(0);
        invalid.analysis_max_output_tokens = Some(128_001);
        assert!(app.save_api_profile(invalid, None, false).await.is_err());
        assert_eq!(
            app.settings().await.unwrap().api_configuration.profiles[0]
                .analysis_output_limit()
                .unwrap(),
            64_000
        );
    }

    #[tokio::test]
    async fn api_tests_keep_drafts_unsaved_and_use_saved_session_keys_without_changing_settings() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            for key in [None, Some("session-key-fixture")] {
                let (mut connection, _) = listener.accept().unwrap();
                connection
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut bytes = Vec::new();
                let mut chunk = [0_u8; 4096];
                loop {
                    let count = connection.read(&mut chunk).unwrap();
                    assert!(count > 0);
                    bytes.extend_from_slice(&chunk[..count]);
                    if let Some(split) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&bytes[..split]);
                        let length = headers
                            .lines()
                            .find_map(|line| {
                                line.to_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|length| length.trim().parse::<usize>().unwrap())
                            })
                            .unwrap();
                        if bytes.len() >= split + 4 + length {
                            break;
                        }
                    }
                }
                let headers = String::from_utf8_lossy(&bytes).to_lowercase();
                if let Some(key) = key {
                    assert!(headers.contains(&format!("authorization: bearer {key}")));
                } else {
                    assert!(!headers.contains("authorization:"));
                }
                let body = json!({"choices":[{"finish_reason":"stop","message":{"role":"assistant","content":"{\"0\":\"你好。\"}"}}],"usage":{"prompt_tokens":16,"completion_tokens":5}}).to_string();
                write!(connection,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
            }
        });
        let root = tempfile::tempdir().unwrap();
        let app = Application::open(root.path().join("data"), &crate::ToolPaths::new("missing"))
            .await
            .unwrap();
        let profile = ApiProfile {
            name: "Unsaved profile".into(),
            model: "fixture".into(),
            endpoint,
            protocol: "ChatCompletions".into(),
            ..Default::default()
        };
        let result = app
            .test_api_connection(
                &profile,
                Some(String::new()),
                ConnectionTestKind::Translation,
                &crate::CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.passed);
        assert_eq!(
            (
                result.usage.requests,
                result.usage.input_tokens,
                result.usage.output_tokens
            ),
            (1, 16, 5)
        );
        assert!(!app.data_directory().join("settings.json").exists());
        assert!(app.list_projects("").await.is_empty());
        assert!(app.consumption_history().await.unwrap().records.is_empty());
        app.save_api_profile(profile, Some("session-key-fixture".into()), false)
            .await
            .unwrap();
        let saved = app
            .settings()
            .await
            .unwrap()
            .api_configuration
            .profiles
            .remove(0);
        let settings_path = app.data_directory().join("settings.json");
        let settings_before = std::fs::read(&settings_path).unwrap();
        let saved_result = app
            .test_api_connection(
                &saved,
                None,
                ConnectionTestKind::Translation,
                &crate::CancellationToken::new(),
            )
            .await
            .unwrap();
        server.join().unwrap();
        assert!(saved_result.passed);
        assert_eq!(saved_result.usage.requests, 1);
        assert_eq!(std::fs::read(&settings_path).unwrap(), settings_before);
    }

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
