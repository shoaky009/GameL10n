//! Saved project details shared by desktop and command-line callers.
use gamel10n_translation::files::read_json;
use serde_json::Value;

use crate::{Application, CoreError, GameProject, TaskState};

pub struct ProjectSnapshot {
    pub project: GameProject,
    pub task: Option<TaskState>,
    pub progress: Value,
    pub adaptation: Value,
    pub review: Value,
    pub warnings: Vec<String>,
}

impl Application {
    /// Optional diagnostic details cannot prevent opening valid project data.
    pub async fn project_snapshot(&self, id: &str) -> Result<ProjectSnapshot, CoreError> {
        let project = self.open_project(id).await?;
        let task = self.task_state(id).await;
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
            let progress = optional("adaptation-progress.json", "适配进度");
            let adaptation = optional("adaptation-result.json", "适配详情");
            let review = optional("engine-review-result.json", "经验总结");
            Ok(ProjectSnapshot {
                project,
                task,
                progress,
                adaptation,
                review,
                warnings,
            })
        })
        .await?
    }

    /// Resolves the current and legacy patch snapshot layouts in one business API.
    pub async fn latest_patch_texts(&self, id: &str) -> Result<Value, CoreError> {
        let patch = self
            .latest_patch(id)
            .await?
            .ok_or_else(|| CoreError::InvalidData("尚未生成补丁。".into()))?;
        tokio::task::spawn_blocking(move || {
            let path = patch
                .patch_directory
                .parent()
                .unwrap_or(&patch.patch_directory)
                .join(format!(
                    "{}.texts.json",
                    patch
                        .patch_directory
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                ));
            if path.try_exists()? {
                return Ok(read_json(&path)?);
            }
            let legacy = patch.patch_directory.join("translation-snapshot.json");
            if legacy.try_exists()? {
                let saved: Value = read_json(&legacy)?;
                return Ok(saved.get("texts").cloned().unwrap_or(saved));
            }
            Err(CoreError::InvalidData(
                "这份旧补丁没有保存可浏览的译文快照。".into(),
            ))
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
}
