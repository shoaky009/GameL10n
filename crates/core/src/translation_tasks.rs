//! Translation task coordination and provider selection, without desktop dependencies.

use crate::{Application, CancellationToken, CoreError};
use gamel10n_translation::{
    execution::{TranslationEngine, TranslationOptions, TranslationRunResult},
    provider::{ApiProtocol, OpenAiProvider, ProviderSettings, TranslationFormat},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::Notify;

#[derive(Default)]
pub(crate) struct TranslationScheduler {
    active: Mutex<usize>,
    changed: Notify,
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn trial_fixture() -> (tempfile::TempDir, Application, String, TranslationOptions) {
        use gamel10n_translation::files::{hash_text, write_json};
        let root = tempfile::tempdir().unwrap();
        let game = root.path().join("game");
        std::fs::create_dir(&game).unwrap();
        let app = Application::open(
            root.path().join("data"),
            &crate::ToolPaths::new(root.path().join("tools")),
        )
        .await
        .unwrap();
        let project = app.add_project(&game).await.unwrap();
        app.save_api_profile(
            crate::settings::ApiProfile {
                id: "unused".into(),
                name: "Unused provider".into(),
                endpoint: "http://127.0.0.1:9/v1".into(),
                model: "unused".into(),
                protocol: "ChatCompletions".into(),
                ..Default::default()
            },
            None,
            false,
        )
        .await
        .unwrap();
        write_json(
            &app.project_directory(&project.id)
                .unwrap()
                .join("extraction/native.json"),
            &crate::StoredAnalysis {
                schema_version: 1,
                format: "fixture".into(),
                source_directory: game.clone(),
                resource_files: vec!["story.txt".into()],
                source_hashes: std::collections::BTreeMap::from([(
                    "story.txt".into(),
                    hash_text("Hello"),
                )]),
                adapter: None,
                probe: None,
                extraction: gamel10n_engines::protocol::ExtractionResult {
                    units: vec![crate::TranslationUnit {
                        id: "line-1".into(),
                        resource_path: "story.txt".into(),
                        locator: "1".into(),
                        source_text: "Hello".into(),
                        context: "opening".into(),
                        speaker: None,
                        kind: "dialogue".into(),
                        protected_tokens: None,
                    }],
                    warnings: vec![],
                },
            },
        )
        .unwrap();
        app.save_translation_edit(&project.id, "line-1", "你好".into(), true)
            .await
            .unwrap();
        // A changed snapshot must fail the build, after reusing the saved translation.
        std::fs::write(game.join("story.txt"), "changed source").unwrap();
        let options = TranslationOptions {
            unit_ids: Some(vec!["line-1".into()]),
            build_patch_after: true,
            ..Default::default()
        };
        (root, app, project.id, options)
    }

    #[test]
    fn original_translation_scope_restores_selection_and_retranslation_task() {
        let scope:TranslationScope=serde_json::from_str(r#"{"unitIds":["original-id"],"buildPatchAfter":true,"completed":false,"retranslationId":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}"#).unwrap();
        let options = scope.options();
        assert_eq!(options.unit_ids, Some(vec!["original-id".into()]));
        assert!(options.build_patch_after);
        assert_eq!(options.retranslation_id, Some("a".repeat(32)));
        let persisted =
            serde_json::to_value(TranslationScope::from_options(&options, false)).unwrap();
        assert_eq!(persisted["unitIds"][0], "original-id");
        assert_eq!(persisted["buildPatchAfter"], true);
        assert_eq!(persisted["completed"], false);
    }

    #[tokio::test]
    async fn trial_build_failure_keeps_scope_resumable_without_retranslating() {
        let (_root, app, id, options) = trial_fixture().await;
        let mut events = app.subscribe();
        let first =
            tokio::time::timeout(Duration::from_secs(5), app.translate_project(&id, options))
                .await
                .unwrap()
                .unwrap_err();
        assert!(first.to_string().contains("原文快照已变化"), "{first}");
        let mut states = Vec::new();
        while let Ok(event) = events.try_recv() {
            if let crate::AppEvent::TaskChanged(state) = event {
                states.push(state);
            }
        }
        assert!(
            states
                .iter()
                .filter(|state| !state.running)
                .all(|state| state.operation == "build")
        );
        assert_eq!(states.iter().filter(|state| !state.running).count(), 1);
        let scope = app.translation_scope(&id).await.unwrap().unwrap();
        assert!(!scope.completed && scope.build_patch_after);
        assert_eq!(scope.unit_ids, Some(vec!["line-1".into()]));
        let resumed = tokio::time::timeout(Duration::from_secs(5), app.resume_translation(&id))
            .await
            .unwrap()
            .unwrap_err();
        assert!(resumed.to_string().contains("原文快照已变化"), "{resumed}");
        let state = app.task_state(&id).await.unwrap();
        assert_eq!(
            (state.operation.as_str(), state.succeeded),
            ("build", Some(false))
        );
        assert_eq!(
            app.workbench_row(&id, "line-1")
                .await
                .unwrap()
                .text
                .as_deref(),
            Some("你好")
        );
    }

    #[tokio::test]
    async fn trial_cancellation_at_build_transition_preserves_task_and_selection() {
        let (_root, app, id, options) = trial_fixture().await;
        let mut events = app.subscribe();
        let task = app.translate_project(&id, options);
        tokio::pin!(task);
        let mut cancelled_at_build = false;
        let mut states = Vec::new();
        let result = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                tokio::select! {
                    biased;
                    event = events.recv() => {
                        if let crate::AppEvent::TaskChanged(state) = event.unwrap() {
                            if state.running && state.operation == "build" && !cancelled_at_build {
                                assert!(matches!(app.begin(&id, "recognize", "blocked").await, Err(CoreError::ProjectBusy)));
                                app.cancel(&id).await;
                                cancelled_at_build = true;
                            }
                            states.push(state);
                        }
                    }
                    result = &mut task => break result,
                }
            }
        }).await.unwrap();
        assert!(cancelled_at_build);
        assert!(matches!(
            result,
            Err(CoreError::Engine(gamel10n_engines::EngineError::Cancelled))
        ));
        assert!(
            states
                .iter()
                .filter(|state| !state.running)
                .all(|state| state.operation == "build")
        );
        let scope = app.translation_scope(&id).await.unwrap().unwrap();
        assert!(!scope.completed && scope.build_patch_after);
        assert_eq!(scope.unit_ids, Some(vec!["line-1".into()]));
        let state = app.task_state(&id).await.unwrap();
        assert!(!state.running && state.succeeded.is_none());
    }
}
struct Permit(Arc<TranslationScheduler>);

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct TranslationScope {
    pub unit_ids: Option<Vec<String>>,
    pub build_patch_after: bool,
    pub completed: bool,
    pub retranslation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub options: Option<TranslationOptions>,
}
impl TranslationScope {
    pub fn options(&self) -> TranslationOptions {
        let mut options = self.options.clone().unwrap_or_default();
        options.unit_ids = self.unit_ids.clone();
        options.retranslation_id = self.retranslation_id.clone();
        options.build_patch_after = self.build_patch_after;
        options
    }
    fn from_options(options: &TranslationOptions, completed: bool) -> Self {
        Self {
            unit_ids: options.unit_ids.clone(),
            build_patch_after: options.build_patch_after,
            completed,
            retranslation_id: options.retranslation_id.clone(),
            options: Some(options.clone()),
        }
    }
}
impl Drop for Permit {
    fn drop(&mut self) {
        if let Ok(mut count) = self.0.active.lock() {
            *count = count.saturating_sub(1);
        }
        self.0.changed.notify_waiters();
    }
}

impl Application {
    pub async fn translation_providers(&self, id: &str) -> Result<Vec<OpenAiProvider>, CoreError> {
        let project = self.store.get(id).await?;
        let settings = self.settings().await?;
        let configured = project
            .settings
            .get("translationProfileIds")
            .and_then(Value::as_array)
            .map(|ids| {
                ids.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            })
            .or_else(|| {
                project
                    .settings
                    .get("translationProfileId")
                    .and_then(Value::as_str)
                    .map(|id| vec![id.into()])
            })
            .unwrap_or_else(|| settings.api_configuration.chain());
        if configured.is_empty() {
            return Err(CoreError::InvalidData("请先配置翻译 API 链。".into()));
        }
        let mut providers = Vec::new();
        for id in configured {
            let profile = settings
                .api_configuration
                .profiles
                .iter()
                .find(|profile| profile.id == id)
                .ok_or_else(|| {
                    CoreError::InvalidData("所选翻译 API 配置不存在，请重新选择。".into())
                })?;
            let provider = OpenAiProvider::new(ProviderSettings {
                endpoint: profile.endpoint.clone(),
                api_key: self.decrypt_key(profile).await?,
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
            .map_err(|error| CoreError::InvalidData(error.to_string()))?
            .with_display_name(profile.name.clone());
            providers.push(provider);
        }
        Ok(providers)
    }

    async fn translation_permit(
        &self,
        cancellation: &CancellationToken,
    ) -> Result<Permit, CoreError> {
        loop {
            if cancellation.is_cancelled() {
                return Err(gamel10n_engines::EngineError::Cancelled.into());
            }
            let notified = self.translation_scheduler.changed.notified();
            let maximum = self.settings().await?.maximum_translations.clamp(1, 8);
            {
                let mut active = self
                    .translation_scheduler
                    .active
                    .lock()
                    .map_err(|_| CoreError::InvalidData("翻译任务调度器不可用。".into()))?;
                if *active < maximum {
                    *active += 1;
                    return Ok(Permit(Arc::clone(&self.translation_scheduler)));
                }
            }
            tokio::select! {biased;_=cancellation.cancelled()=>return Err(gamel10n_engines::EngineError::Cancelled.into()),_=notified=>{},_=tokio::time::sleep(Duration::from_millis(250))=>{}}
        }
    }

    pub async fn translate_project(
        &self,
        id: &str,
        mut options: TranslationOptions,
    ) -> Result<TranslationRunResult, CoreError> {
        let project = self.store.get(id).await?;
        if options.batch_size == TranslationOptions::default().batch_size {
            options.batch_size = project
                .settings
                .get("batchSize")
                .and_then(Value::as_u64)
                .unwrap_or(24) as usize;
        }
        let analysis = self
            .cached_analysis(id)
            .await?
            .ok_or_else(|| CoreError::InvalidData("请先识别并提取游戏文本。".into()))?;
        let providers = self.translation_providers(id).await?;
        let policy = crate::workbench::validated_policy(&project.settings)?;
        let directory = self.store.directory(id)?;
        let operation = if options.retranslation_id.is_some() {
            "retranslate"
        } else if options.candidate_only {
            "candidate"
        } else {
            "translate"
        };
        let cancellation = self.begin(id, operation, "翻译任务等待执行。").await?;
        let mut final_operation = operation;
        let mut patch = None;
        let result = async {
            self.progress(
                id,
                operation,
                &format!(
                    "文本翻译 API 链：{}",
                    providers
                        .iter()
                        .map(|provider| format!("{}（{}）", provider.display_name(), provider.model()))
                        .collect::<Vec<_>>()
                        .join(" → ")
                ),
            )
            .await;
            if !options.candidate_only {
                self.save_translation_scope(id, &options, false).await?;
            }
            // Release translation concurrency capacity before building, but keep the project task.
            let result = {
                let _permit = self.translation_permit(&cancellation).await?;
                let engine = TranslationEngine::new(
                    directory,
                    Some(self.data_directory().join("memory").join(id)),
                );
                let (progress_tx, mut progress_rx) = tokio::sync::mpsc::unbounded_channel();
                let report = move |progress| {
                    let _ = progress_tx.send(progress);
                };
                let run = engine.run(
                    &analysis.extraction().units,
                    &providers,
                    &policy,
                    &options,
                    &cancellation,
                    report,
                );
                tokio::pin!(run);
                let result = loop {
                    tokio::select! {
                        result = &mut run => break result,
                        Some(progress) = progress_rx.recv() => {
                            self.translation_progress(id, operation, &analysis, &options, progress).await;
                        },
                    }
                };
                while let Ok(progress) = progress_rx.try_recv() {
                    self.translation_progress(id, operation, &analysis, &options, progress).await;
                }
                self.workbenches.lock().await.remove(id);
                result.map_err(|error| CoreError::InvalidData(error.to_string()))?
            };
            if !options.candidate_only && result.failed == 0 {
                if options.build_patch_after {
                    final_operation = "build";
                    self.progress(id, "build", "翻译已保存，正在检查原始资源并生成补丁。")
                        .await;
                    patch = Some(
                        self.generate_patch_in_task(id, options.unit_ids.as_deref(), false, &cancellation)
                            .await?,
                    );
                }
                if cancellation.is_cancelled() {
                    return Err(gamel10n_engines::EngineError::Cancelled.into());
                }
                self.save_translation_scope(id, &options, true).await?;
            }
            Ok::<_, CoreError>(result)
        }
        .await;
        self.finish(
            id,
            final_operation,
            result
                .as_ref()
                .map(|result| {
                    if let Some(path) = &patch {
                        format!("补丁已生成：{}。尚未验证游戏内显示。", path.display())
                    } else {
                        format!(
                            "翻译完成：成功 {} 条，失败 {} 条。",
                            result.completed, result.failed
                        )
                    }
                })
                .map_err(ToString::to_string),
        )
        .await?;
        result
    }

    pub async fn resume_translation(&self, id: &str) -> Result<TranslationRunResult, CoreError> {
        let scope = self
            .translation_scope(id)
            .await?
            .ok_or_else(|| CoreError::InvalidData("找不到可继续的翻译任务。".into()))?;
        self.translate_project(id, scope.options()).await
    }

    async fn translation_progress(
        &self,
        id: &str,
        operation: &str,
        analysis: &crate::application::CachedAnalysis,
        options: &TranslationOptions,
        progress: gamel10n_translation::execution::TranslationProgress,
    ) {
        let workbench = self.workbenches.lock().await.get(id).cloned();
        if let Some(workbench) = workbench {
            workbench
                .lock()
                .await
                .apply_progress(&analysis.extraction().units, &progress, options);
        }
        self.progress(id, operation, &progress.message).await;
        let _ = self.events.send(crate::AppEvent::TranslationChanged {
            project_id: id.into(),
            progress,
        });
    }

    async fn save_translation_scope(
        &self,
        id: &str,
        options: &TranslationOptions,
        completed: bool,
    ) -> Result<(), CoreError> {
        let path = self.store.directory(id)?.join("translation-scope.json");
        let scope = TranslationScope::from_options(options, completed);
        tokio::task::spawn_blocking(move || gamel10n_translation::files::write_json(&path, &scope))
            .await??;
        Ok(())
    }
    pub async fn translation_scope(&self, id: &str) -> Result<Option<TranslationScope>, CoreError> {
        let path = self.store.directory(id)?.join("translation-scope.json");
        if !path.is_file() {
            return Ok(None);
        }
        Ok(Some(gamel10n_translation::files::read_json(&path)?))
    }
    pub async fn task_api_summary(&self, id: &str) -> Result<String, CoreError> {
        let project = self.store.get(id).await?;
        let settings = self.settings().await?;
        let configuration = &settings.api_configuration;
        let analysis = project
            .settings
            .get("analysisProfileId")
            .and_then(Value::as_str)
            .or(configuration.analysis_profile_id.as_deref());
        let label = |id: &str| {
            configuration
                .profiles
                .iter()
                .find(|profile| profile.id == id)
                .map(|profile| {
                    format!(
                        "{}（{} · {}）",
                        profile.name, profile.model, profile.protocol
                    )
                })
                .unwrap_or_else(|| "配置不可用".into())
        };
        let analysis = analysis.map(label).unwrap_or_else(|| "未选择".into());
        let chain = project
            .settings
            .get("translationProfileIds")
            .and_then(Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_else(|| configuration.chain());
        let translation = if chain.is_empty() {
            "未选择".into()
        } else {
            chain
                .iter()
                .map(|id| label(id))
                .collect::<Vec<_>>()
                .join(" → ")
        };
        Ok(format!(
            "分析 API：{analysis}\n用途：智能识别、适配和总结\n翻译 API 链：{translation}\n用途：文本翻译、重译和候选译文"
        ))
    }
}
