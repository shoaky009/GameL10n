use crate::application::{TaskGuard, TaskKind};
use gamel10n_engines::{EngineProgress, review::EngineReviewResult};
use gamel10n_translation::files::write_json;
use std::sync::Arc;

use crate::{
    AppEvent, Application, CancellationToken, CoreError, catalog::EngineCatalog, logs::LogSource,
};

#[derive(Clone, Debug, Default)]
pub struct ReviewState {
    pub running: bool,
    pub message: String,
    pub succeeded: Option<bool>,
    pub progress: Option<EngineProgress>,
}

pub(crate) struct ReviewTask {
    active: Option<Arc<()>>,
    pub(crate) state: ReviewState,
    pub(crate) cancellation: CancellationToken,
    pub(crate) order: u64,
}

impl Application {
    pub(crate) fn review_tasks(
        &self,
    ) -> std::sync::MutexGuard<'_, std::collections::HashMap<String, ReviewTask>> {
        self.reviews
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn review_state(&self, id: &str) -> Option<ReviewState> {
        self.review_tasks().get(id).map(|task| task.state.clone())
    }

    pub fn review_running(&self, id: &str) -> bool {
        self.review_tasks()
            .get(id)
            .is_some_and(|task| task.state.running)
    }

    pub fn cancel_review(&self, id: &str) {
        if let Some(task) = self.review_tasks().get(id) {
            task.cancellation.cancel();
        }
    }

    pub(crate) async fn begin_review(&self, id: &str) -> Result<TaskGuard<'_>, CoreError> {
        let _project = self.lock_project(id).await?;
        let _lifecycle = self.project_lifecycle.lock().await;
        if self.store.get(id).await?.is_removed {
            return Err(CoreError::ProjectMissing(id.into()));
        }
        self.ensure_writable().await?;
        if self.review_running(id) {
            return Err(CoreError::InvalidData("此项目正在总结经验。".into()));
        }
        if self.writing_review_evidence(id).await {
            return Err(CoreError::InvalidData(
                "项目正在更新适配资料，请在更新结束后总结经验。".into(),
            ));
        }
        let state = ReviewState {
            running: true,
            message: "正在总结可供同类引擎复用的经验。".into(),
            ..ReviewState::default()
        };
        let cancellation = CancellationToken::new();
        let active = Arc::new(());
        let guard = TaskGuard::new(self, id, TaskKind::Review, &active, cancellation.clone());
        self.review_tasks().insert(
            id.into(),
            ReviewTask {
                active: Some(active),
                state: state.clone(),
                cancellation: cancellation.clone(),
                order: self
                    .next_task_order
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            },
        );
        self.start_metrics(id, "review").await;
        self.publish_review(id, &state);
        drop(_lifecycle);
        self.log_task_message(id, &state.message).await;
        Ok(guard)
    }

    fn publish_review(&self, id: &str, state: &ReviewState) {
        let _ = self.events.send(AppEvent::ReviewChanged {
            project_id: id.into(),
            state: state.clone(),
        });
    }

    async fn review_progress(&self, id: &str, progress: EngineProgress) {
        self.checkpoint_metrics(id, "review").await;
        let log = {
            let mut reviews = self.review_tasks();
            let Some(task) = reviews.get_mut(id) else {
                return;
            };
            let log =
                (!progress.status_only && task.state.message != progress.message).then(|| {
                    (
                        progress.message.clone(),
                        LogSource::from_model(progress.message_from_model),
                    )
                });
            task.state.message.clone_from(&progress.message);
            task.state.progress = Some(progress);
            self.publish_review(id, &task.state);
            log
        };
        if let Some((message, source)) = log {
            self.log_task_message_from(id, &message, source).await;
        }
    }

    pub(crate) async fn finish_review(
        &self,
        id: &str,
        result: Result<&EngineReviewResult, String>,
    ) -> Result<(), CoreError> {
        let _project = self.lock_project(id).await?;
        let cancelled = self
            .review_tasks()
            .get(id)
            .is_some_and(|task| task.cancellation.is_cancelled());
        let succeeded = if cancelled && result.is_err() {
            None
        } else {
            Some(result.is_ok())
        };
        let message = match &result {
            Ok(result) => result.summary.clone(),
            Err(_) if cancelled => "经验总结已停止。".into(),
            Err(error) => error.clone(),
        };
        self.finish_metrics(id, "review", succeeded).await;
        if let Some(task) = self.review_tasks().get_mut(id) {
            if let Ok(result) = result {
                task.state.progress = (!result.reused).then(|| EngineProgress {
                    usage: result.usage.clone(),
                    api_requests: result.api_requests,
                    ..EngineProgress::default()
                });
            }
            task.active = None;
            task.state.running = false;
            task.state.succeeded = succeeded;
            task.state.message.clone_from(&message);
            self.publish_review(id, &task.state);
        }
        self.log_task_message_from(id, &message, LogSource::from_model(succeeded == Some(true)))
            .await;
        Ok(())
    }

    pub async fn review_engine(&self, id: &str) -> Result<EngineReviewResult, CoreError> {
        let directory = self.project_directory(id)?;
        if !directory.join("adaptation/current/session.json").is_file() {
            return Err(CoreError::InvalidData(
                "此项目没有可用于总结的适配记录，或相关工作文件已清理。已有经验总结仍可查看。"
                    .into(),
            ));
        }
        let (provider, limits) = self.analysis_provider(id).await?;
        let cancellation = self.begin_review(id).await?;
        self.metrics_provider(id, "review", &provider).await;
        let result = async {
            let service = self.engine_service()?;
            let knowledge = self.data_directory().join("extensions/engine-knowledge");
            let api = self.task_api_summary(id).await?;
            self.log_task_message(id, api.lines().next().unwrap_or("分析 API 已选择。"))
                .await;
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
            let report = move |progress| {
                let _ = tx.send(progress);
            };
            let future = service.review(
                &directory,
                &knowledge,
                &provider,
                &limits,
                &cancellation,
                Some(&report),
            );
            tokio::pin!(future);
            let result = loop {
                tokio::select! {
                    result = &mut future => break result,
                    Some(progress) = rx.recv() => self.review_progress(id, progress).await,
                }
            };
            while let Ok(progress) = rx.try_recv() {
                self.review_progress(id, progress).await;
            }
            let result = result?;
            if result.reused {
                self.metrics_details(id, "review", None, Some("reused"), None)
                    .await;
            }
            write_json(&directory.join("engine-review-result.json"), &result)?;
            if let Some(entry) = &result.entry_id {
                EngineCatalog::open(self.data_directory())?.register_knowledge(entry)?;
                let _ = self.events.send(AppEvent::EngineLibraryChanged);
            }
            Ok::<_, CoreError>(result)
        }
        .await;
        self.finish_review(id, result.as_ref().map_err(ToString::to_string))
            .await?;
        result
    }
}
