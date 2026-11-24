//! Saved project details shared by desktop and command-line callers.
use std::{fmt, fs::File, io::BufReader};

use gamel10n_translation::files::{FileError, io_error, read_json};
use serde::de::{DeserializeSeed, IgnoredAny, MapAccess, Visitor};
use serde_json::Value;

use crate::{Application, CoreError, GameProject, TaskState};

pub struct ProjectSnapshot {
    pub project: GameProject,
    pub task: Option<TaskState>,
    pub translation_progress: Option<crate::LiveTranslationProgress>,
    pub translation_preview: bool,
    pub preview_translation: Option<crate::PreviewTranslationStatus>,
    pub preview_translation_resumable: bool,
    pub progress: Value,
    pub adaptation: Value,
    pub can_resume_adaptation: bool,
    pub workflow_session: Option<crate::workflow::WorkflowSession>,
    pub review: Value,
    pub warnings: Vec<String>,
}

#[derive(Default)]
pub struct PatchTextPage {
    pub texts: serde_json::Map<String, Value>,
    pub total: usize,
}

struct PatchTextPageReader {
    offset: usize,
    legacy: bool,
}

impl<'de> DeserializeSeed<'de> for PatchTextPageReader {
    type Value = PatchTextPage;

    fn deserialize<D: serde::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> Result<Self::Value, D::Error> {
        deserializer.deserialize_map(self)
    }
}

impl<'de> Visitor<'de> for PatchTextPageReader {
    type Value = PatchTextPage;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("an object containing patch translations")
    }

    fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Self::Value, M::Error> {
        let mut page = PatchTextPage::default();
        while let Some(id) = map.next_key::<String>()? {
            if self.legacy && id == "texts" {
                drop(page);
                let page = map.next_value_seed(Self {
                    offset: self.offset,
                    legacy: false,
                })?;
                while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
                return Ok(page);
            }
            if page.total >= self.offset && page.texts.len() < 100 {
                page.texts.insert(id, map.next_value()?);
            } else {
                map.next_value::<IgnoredAny>()?;
            }
            page.total += 1;
        }
        Ok(page)
    }
}

impl Application {
    /// Optional diagnostic details cannot prevent opening valid project data.
    pub async fn project_snapshot(&self, id: &str) -> Result<ProjectSnapshot, CoreError> {
        let project = self.open_project(id).await?;
        let (task, translation_preview, preview_translation) = self.project_task_snapshot(id).await;
        let translation_progress = self.running_translation_progress(id).await;
        let live_progress = self.running_engine_progress(id).await;
        let directory = self.project_directory(id)?;
        tokio::task::spawn_blocking(move || {
            let mut warnings = Vec::new();
            let mut optional = |name: &str, label: &str| {
                let path = directory.join(name);
                match read_json::<Value>(&path) {
                    Ok(value) => value,
                    Err(gamel10n_translation::files::FileError::Io { source, .. })
                        if source.kind() == std::io::ErrorKind::NotFound =>
                    {
                        Value::Null
                    }
                    Err(error) => {
                        warnings.push(format!("{label}暂时无法读取：{error}"));
                        Value::Null
                    }
                }
            };
            let progress =
                live_progress.unwrap_or_else(|| optional("adaptation-progress.json", "适配进度"));
            let adaptation = optional("adaptation-result.json", "适配详情");
            let review = optional("engine-review-result.json", "经验总结");
            let can_resume_adaptation = if task.as_ref().is_some_and(|task| task.running) {
                false
            } else {
                match crate::engine_tasks::resumable_adaptation(&directory, task.as_ref()) {
                    Ok(saved) => saved.is_some(),
                    Err(error) => {
                        warnings.push(format!("无法读取智能识别会话：{error}"));
                        false
                    }
                }
            };
            let workflow_session = match crate::workflow::load_session(&directory) {
                Ok(session) => session,
                Err(error) => {
                    warnings.push(format!("无法读取自动汉化进度：{error}"));
                    None
                }
            };
            let preview_translation_resumable = preview_translation.is_none()
                && crate::translation_tasks::saved_preview_slot(&directory).is_some();
            let task = if task.as_ref().is_some_and(|task| task.running) {
                task
            } else {
                preview_translation
                    .as_ref()
                    .map(|run| run.task.clone())
                    .or(task)
            };
            Ok(ProjectSnapshot {
                translation_progress,
                translation_preview,
                preview_translation,
                preview_translation_resumable,
                project,
                task,
                progress,
                adaptation,
                can_resume_adaptation,
                workflow_session,
                review,
                warnings,
            })
        })
        .await?
    }

    /// Reads up to 100 records from the current or legacy snapshot without loading the full map.
    pub async fn latest_patch_texts(
        &self,
        id: &str,
        offset: usize,
    ) -> Result<PatchTextPage, CoreError> {
        let patch = self
            .latest_patch(id)
            .await?
            .ok_or_else(|| CoreError::InvalidData("尚未生成补丁。".into()))?;
        Self::patch_text_page(patch.patch_directory, offset).await
    }

    pub(crate) async fn patch_text_page(
        directory: std::path::PathBuf,
        offset: usize,
    ) -> Result<PatchTextPage, CoreError> {
        tokio::task::spawn_blocking(move || {
            let name = directory.file_name().unwrap_or_default().to_string_lossy();
            let path = directory.with_file_name(format!("{name}.texts.json"));
            let (path, legacy) = if path.try_exists()? {
                (path, false)
            } else {
                let legacy = directory.join("translation-snapshot.json");
                if !legacy.try_exists()? {
                    return Err(CoreError::InvalidData(
                        "这份旧补丁没有保存可浏览的译文快照。".into(),
                    ));
                }
                (legacy, true)
            };
            let file = File::open(&path).map_err(|error| io_error(&path, error))?;
            let mut input = serde_json::Deserializer::from_reader(BufReader::new(file));
            PatchTextPageReader { offset, legacy }
                .deserialize(&mut input)
                .and_then(|page| {
                    input.end()?;
                    Ok(page)
                })
                .map_err(|source| {
                    FileError::Json {
                        path: path.display().to_string(),
                        source,
                    }
                    .into()
                })
        })
        .await?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn damaged_optional_details_do_not_block_valid_project_metadata() {
        let root = tempfile::tempdir().unwrap();
        let game = root.path().join("game");
        std::fs::create_dir(&game).unwrap();
        let app = Application::open(root.path().join("data"), &crate::ToolPaths::new("missing"))
            .await
            .unwrap();
        let project = app.add_project(&game).await.unwrap();
        std::fs::write(
            app.project_directory(&project.id)
                .unwrap()
                .join("adaptation-result.json"),
            "broken json",
        )
        .unwrap();
        let snapshot = app.project_snapshot(&project.id).await.unwrap();
        assert_eq!(snapshot.project.id, project.id);
        assert_eq!(snapshot.warnings.len(), 1);
        assert!(snapshot.adaptation.is_null());
    }

    #[tokio::test]
    async fn running_task_progress_is_served_from_memory_without_reading_progress_file() {
        let root = tempfile::tempdir().unwrap();
        let game = root.path().join("game");
        std::fs::create_dir(&game).unwrap();
        let app = Application::open(root.path().join("data"), &crate::ToolPaths::new("missing"))
            .await
            .unwrap();
        let project = app.add_project(&game).await.unwrap();
        let progress_path = app
            .project_directory(&project.id)
            .unwrap()
            .join("adaptation-progress.json");
        std::fs::write(&progress_path, "broken json").unwrap();

        let _guard = app.begin(&project.id, "adapt", "正在适配").await.unwrap();
        app.set_task_engine_progress(
            &project.id,
            serde_json::json!({"stage": "验收原文回填", "message": "正在检查原文回填。", "operation": "adapt"}),
        )
        .await;

        let snapshot = app.project_snapshot(&project.id).await.unwrap();
        assert_eq!(snapshot.progress["stage"], "验收原文回填");
        assert!(snapshot.warnings.is_empty());

        app.finish(&project.id, "adapt", Ok("完成".into()))
            .await
            .unwrap();
        std::fs::write(&progress_path, r#"{"stage":"已完成","operation":"adapt"}"#).unwrap();
        let after = app.project_snapshot(&project.id).await.unwrap();
        assert_eq!(after.progress["stage"], "已完成");
    }
}
