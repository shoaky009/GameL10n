//! Configurable project-owned translation workflow with manual trial review gates.

use crate::{
    Application, CancellationToken, CoreError, GameProject,
    engine_tasks::{RecognitionMode, RecognitionResult},
    logs::LogSource,
    patching::TEST_STATUS_PASSED,
    task_op,
};
use gamel10n_translation::{
    execution::TranslationOptions,
    files::{io_error, read_json, write_json},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeSet, HashSet},
    path::{Path, PathBuf},
};

const WORKFLOW_KEY: &str = "automaticWorkflow";
const WORKFLOW_CONFIG_FILE: &str = "workflow.json";
const WORKFLOW_STATE_FILE: &str = "workflow-state.json";
const WORKFLOW_SCHEMA_VERSION: u32 = 1;
const MAX_WORKFLOW_STEPS: usize = 64;
const MAX_STEP_ID_BYTES: usize = 64;
const MAX_WORKFLOW_ITERATIONS: usize = 64;
const MAX_WORKFLOW_FILE_BYTES: u64 = 256 * 1024;
const MAX_STEP_INSTRUCTION_CHARS: usize = 6_000;
const MAX_STEP_MESSAGE_CHARS: usize = 2_000;
const LEGACY_WAIT_MESSAGE: &str = "未能确定开场文本。请在运行检查中选择一条目标文本并设置进入场景的操作，或编辑 workflow.json 的定位步骤，然后重新开始。";

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum WorkflowFinish {
    Trial,
    Build,
    #[default]
    Install,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowPreset {
    pub use_existing_text: bool,
    pub finish: WorkflowFinish,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum WorkflowChoice {
    Preset(WorkflowPreset),
    Custom,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum WorkflowAction {
    Recognize,
    #[serde(alias = "locateText")]
    SelectTrial,
    Translate,
    BuildPatch,
    InstallPatch,
    #[serde(alias = "checkGame")]
    ReviewTrial,
    WaitForInput,
    Complete,
}

impl WorkflowAction {
    pub fn title(self) -> &'static str {
        match self {
            Self::Recognize => "识别与适配",
            Self::SelectTrial => "确定试译文本",
            Self::Translate => "翻译文本",
            Self::BuildPatch => "生成补丁",
            Self::InstallPatch => "安装补丁",
            Self::ReviewTrial => "等待实机把关",
            Self::WaitForInput => "等待用户操作",
            Self::Complete => "完成流程",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum WorkflowScope {
    #[default]
    Target,
    All,
}

fn target_scope(scope: &WorkflowScope) -> bool {
    *scope == WorkflowScope::Target
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowStep {
    pub id: String,
    pub action: WorkflowAction,
    #[serde(default, skip_serializing_if = "target_scope")]
    pub scope: WorkflowScope,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub unit_id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unit_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub instruction: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_success: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_failure: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_unavailable: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowDefinition {
    pub schema_version: u32,
    pub entry: String,
    pub steps: Vec<WorkflowStep>,
}

impl Default for WorkflowDefinition {
    fn default() -> Self {
        Self::from_preset(WorkflowPreset::default())
    }
}

impl WorkflowDefinition {
    fn from_preset(preset: WorkflowPreset) -> Self {
        use WorkflowAction::*;
        let last_step = match preset.finish {
            WorkflowFinish::Trial => "review",
            WorkflowFinish::Build => "buildAll",
            WorkflowFinish::Install => "installAll",
        };
        let steps = [
            ("recognize", Recognize),
            ("select", SelectTrial),
            ("trialTranslate", Translate),
            ("trialBuild", BuildPatch),
            ("trialInstall", InstallPatch),
            ("review", ReviewTrial),
            ("translateAll", Translate),
            ("buildAll", BuildPatch),
            ("installAll", InstallPatch),
            ("done", Complete),
        ]
        .into_iter()
        .map(|(id, action)| WorkflowStep {
            id: id.into(),
            action,
            scope: if matches!(id, "translateAll" | "buildAll" | "installAll") {
                WorkflowScope::All
            } else {
                WorkflowScope::Target
            },
            unit_id: String::new(),
            unit_ids: Vec::new(),
            instruction: String::new(),
            message: String::new(),
            on_success: (id == last_step || id == "installAll").then(|| "done".into()),
            on_failure: None,
            on_unavailable: None,
        })
        .collect();
        Self {
            schema_version: WORKFLOW_SCHEMA_VERSION,
            entry: if preset.use_existing_text {
                "select"
            } else {
                "recognize"
            }
            .into(),
            steps,
        }
    }

    pub fn validate(&self) -> Result<(), CoreError> {
        let invalid =
            |message: &str| CoreError::InvalidData(format!("自动流程配置无效：{message}"));
        if self.schema_version != WORKFLOW_SCHEMA_VERSION
            || self.steps.is_empty()
            || self.steps.len() > MAX_WORKFLOW_STEPS
        {
            return Err(invalid(&format!(
                "需要 schemaVersion: {WORKFLOW_SCHEMA_VERSION}，且步骤数量为 1～{MAX_WORKFLOW_STEPS}。"
            )));
        }
        let mut ids = BTreeSet::new();
        for step in &self.steps {
            if step.id.is_empty()
                || step.id.len() > MAX_STEP_ID_BYTES
                || !ids.insert(step.id.as_str())
            {
                return Err(invalid(&format!(
                    "步骤 ID 需非空、唯一且不超过 {MAX_STEP_ID_BYTES} 字节。"
                )));
            }
            if step.instruction.chars().count() > MAX_STEP_INSTRUCTION_CHARS
                || step.message.chars().count() > MAX_STEP_MESSAGE_CHARS
            {
                return Err(invalid("步骤指令或提示过长。"));
            }
        }
        if !ids.contains(self.entry.as_str()) {
            return Err(invalid("入口步骤不存在。"));
        }
        for step in &self.steps {
            for next in [&step.on_success, &step.on_failure, &step.on_unavailable]
                .into_iter()
                .flatten()
            {
                if !ids.contains(next.as_str()) {
                    return Err(invalid("分支指向不存在的步骤。"));
                }
            }
        }
        Ok(())
    }

    fn step_index(&self, id: &str) -> Option<usize> {
        self.steps.iter().position(|step| step.id == id)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum WorkflowStatus {
    Running,
    Completed,
    Failed,
    WaitingForInput,
    Interrupted,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum WorkflowPauseKind {
    SelectTrial,
    ReviewTrial,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowProgress {
    pub step_id: String,
    pub action: WorkflowAction,
    pub status: WorkflowStatus,
    pub target_unit_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pause_kind: Option<WorkflowPauseKind>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowSession {
    pub schema_version: u32,
    pub definition: WorkflowDefinition,
    pub next_step_id: String,
    pub pause_kind: WorkflowPauseKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finish: Option<WorkflowFinish>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub target_unit_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trial_patch_directory: Option<PathBuf>,
    #[serde(default)]
    pub trial_verified: bool,
    pub message: String,
}

impl WorkflowSession {
    pub fn confirm_label(&self) -> &'static str {
        let ends_after_review = self
            .definition
            .steps
            .iter()
            .find(|step| step.id == self.next_step_id)
            .is_some_and(|step| step.action == WorkflowAction::Complete);
        if ends_after_review || self.finish == Some(WorkflowFinish::Trial) {
            return "确认正常，完成试译";
        }
        if self.trial_verified {
            return "重试后续流程";
        }
        match self.finish {
            Some(WorkflowFinish::Build) => "确认正常，生成全量补丁",
            Some(WorkflowFinish::Install) => "确认正常，继续全量安装",
            _ => "确认正常，继续后续流程",
        }
    }
}

pub(crate) fn load_session(directory: &Path) -> Result<Option<WorkflowSession>, CoreError> {
    let path = directory.join(WORKFLOW_STATE_FILE);
    if !path.try_exists()? {
        return Ok(None);
    }
    let session: WorkflowSession = read_json(&path)?;
    if session.schema_version != WORKFLOW_SCHEMA_VERSION {
        return Err(CoreError::InvalidData("自动流程进度版本不受支持。".into()));
    }
    session.definition.validate()?;
    if session
        .definition
        .step_index(&session.next_step_id)
        .is_none()
    {
        return Err(CoreError::InvalidData("自动流程继续步骤不存在。".into()));
    }
    Ok(Some(session))
}

pub(crate) fn save_session(directory: &Path, session: &WorkflowSession) -> Result<(), CoreError> {
    write_json(&directory.join(WORKFLOW_STATE_FILE), session)?;
    Ok(())
}

pub(crate) fn clear_session(directory: &Path) -> Result<(), CoreError> {
    let path = directory.join(WORKFLOW_STATE_FILE);
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(io_error(&path, error).into()),
    }
}

fn saved_workflow_choice(project: &GameProject) -> Result<Option<WorkflowChoice>, CoreError> {
    project
        .extra
        .get(WORKFLOW_KEY)
        .map(WorkflowChoice::deserialize)
        .transpose()
        .map_err(|error| CoreError::InvalidData(format!("自动汉化设置无效：{error}")))
}

fn is_legacy_default_definition(definition: &WorkflowDefinition) -> bool {
    let ids: Vec<_> = definition
        .steps
        .iter()
        .map(|step| (step.id.as_str(), step.action))
        .collect();
    use WorkflowAction::*;
    definition.schema_version == WORKFLOW_SCHEMA_VERSION
        && definition.entry == "recognize"
        && ids
            == [
                ("recognize", Recognize),
                ("locate", SelectTrial),
                ("trialTranslate", Translate),
                ("trialBuild", BuildPatch),
                ("trialInstall", InstallPatch),
                ("check", ReviewTrial),
                ("translateAll", Translate),
                ("buildAll", BuildPatch),
                ("installAll", InstallPatch),
                ("input", WaitForInput),
                ("done", Complete),
            ]
}

fn read_definition(path: &Path) -> Result<WorkflowDefinition, CoreError> {
    if path
        .metadata()
        .map_err(|error| io_error(path, error))?
        .len()
        > MAX_WORKFLOW_FILE_BYTES
    {
        return Err(CoreError::InvalidData(
            "自动流程配置不能超过 256 KiB。".into(),
        ));
    }
    let mut definition: WorkflowDefinition = read_json(path)?;
    for step in &mut definition.steps {
        if step.action == WorkflowAction::WaitForInput && step.message == LEGACY_WAIT_MESSAGE {
            step.message.clear();
        }
    }
    definition.validate()?;
    Ok(definition)
}

#[derive(Default)]
struct RunContext {
    targets: Vec<String>,
    built: Option<(PathBuf, WorkflowScope)>,
    installed: Option<PathBuf>,
    verified: Option<PathBuf>,
    installed_all: bool,
}

enum StepOutcome {
    Continue,
    Paused(WorkflowSession),
    Complete,
}

fn needs_workflow_input(error: &CoreError) -> bool {
    let error = match error {
        CoreError::WithModelOutput(error) => error,
        error => error,
    };
    matches!(error, CoreError::WorkflowInput(_))
}

impl Application {
    /// Returns the project default and whether to offer the custom workflow choice.
    pub async fn workflow_preferences(
        &self,
        id: &str,
    ) -> Result<(WorkflowChoice, bool), CoreError> {
        let project = self.store.get(id).await?;
        let path = self.project_directory(id)?.join(WORKFLOW_CONFIG_FILE);
        let has_custom = path.try_exists()?
            && !read_definition(&path).is_ok_and(|definition| {
                definition == WorkflowDefinition::default()
                    || is_legacy_default_definition(&definition)
            });
        let choice = saved_workflow_choice(&project)?.unwrap_or_else(|| {
            if has_custom {
                WorkflowChoice::Custom
            } else {
                WorkflowChoice::Preset(WorkflowPreset::default())
            }
        });
        Ok((choice, has_custom || choice == WorkflowChoice::Custom))
    }

    /// Saves the project default independently of its custom workflow definition.
    pub async fn save_workflow_preferences(
        &self,
        id: &str,
        choice: WorkflowChoice,
    ) -> Result<(), CoreError> {
        let _project = self.lock_project(id).await?;
        self.ensure_writable().await?;
        let mut project = self.store.get(id).await?;
        let value = serde_json::to_value(choice)
            .map_err(|error| CoreError::InvalidData(error.to_string()))?;
        project.extra.insert(WORKFLOW_KEY.into(), value);
        self.store.save(&project).await?;
        Ok(())
    }

    /// Creates a project-local configuration once; subsequent runs read this file afresh.
    pub async fn workflow_configuration(&self, id: &str) -> Result<PathBuf, CoreError> {
        let _project = self.lock_project(id).await?;
        self.ensure_writable().await?;
        self.store.get(id).await?;
        let path = self.project_directory(id)?.join(WORKFLOW_CONFIG_FILE);
        if !path.try_exists()? {
            write_json(&path, &WorkflowDefinition::default())?;
        }
        Ok(path)
    }

    pub async fn workflow_session(&self, id: &str) -> Result<Option<WorkflowSession>, CoreError> {
        let directory = self.project_directory(id)?;
        tokio::task::spawn_blocking(move || load_session(&directory)).await?
    }

    pub async fn run_workflow(
        &self,
        id: &str,
        instruction: String,
        mode: RecognitionMode,
    ) -> Result<(), CoreError> {
        let choice = match saved_workflow_choice(&self.store.get(id).await?)? {
            Some(choice) => choice,
            None => {
                self.workflow_configuration(id).await?;
                WorkflowChoice::Custom
            }
        };
        self.run_workflow_with_choice(id, instruction, mode, choice)
            .await
    }

    /// Runs a one-time workflow selection without updating the project default.
    pub async fn run_workflow_with_choice(
        &self,
        id: &str,
        instruction: String,
        mode: RecognitionMode,
        choice: WorkflowChoice,
    ) -> Result<(), CoreError> {
        let (definition, finish) = match choice {
            WorkflowChoice::Preset(preset) => {
                (WorkflowDefinition::from_preset(preset), Some(preset.finish))
            }
            WorkflowChoice::Custom => (
                read_definition(&self.project_directory(id)?.join(WORKFLOW_CONFIG_FILE))?,
                None,
            ),
        };
        let entry = definition.entry.clone();
        self.execute_workflow(
            id,
            definition,
            entry,
            finish,
            instruction,
            mode,
            RunContext::default(),
            false,
        )
        .await
    }

    /// Resumes a workflow paused after recognition once the user selected trial texts.
    pub async fn resume_workflow_selection(&self, id: &str) -> Result<(), CoreError> {
        let session = self
            .workflow_session(id)
            .await?
            .filter(|session| session.pause_kind == WorkflowPauseKind::SelectTrial)
            .ok_or_else(|| CoreError::InvalidData("当前没有等待选择试译文本的自动流程。".into()))?;
        let selected = self.load_text_selection(id).await?;
        if selected.is_empty() {
            return Err(CoreError::InvalidData(
                "请先在「翻译与校对」勾选需要试译的文本，再点击「继续试译」。".into(),
            ));
        }
        self.execute_workflow(
            id,
            session.definition,
            session.next_step_id,
            session.finish,
            String::new(),
            RecognitionMode::Automatic,
            RunContext::default(),
            true,
        )
        .await
    }

    /// Records the user's real-machine confirmation and continues the remaining workflow steps.
    pub async fn confirm_workflow_trial(&self, id: &str) -> Result<(), CoreError> {
        let mut session = self
            .workflow_session(id)
            .await?
            .filter(|session| session.pause_kind == WorkflowPauseKind::ReviewTrial)
            .ok_or_else(|| CoreError::InvalidData("当前没有等待实机把关的自动流程。".into()))?;
        let trial_patch = session.trial_patch_directory.clone().ok_or_else(|| {
            CoreError::InvalidData("缺少试译补丁记录，请重新运行自动流程。".into())
        })?;
        if !session.trial_verified {
            let latest = self
                .latest_patch(id)
                .await?
                .ok_or_else(|| CoreError::InvalidData("找不到已生成的试译补丁。".into()))?;
            if latest.patch_directory != trial_patch {
                return Err(CoreError::InvalidData(
                    "当前补丁已变化，请重新运行试译或结束当前自动流程。".into(),
                ));
            }
            self.record_patch_feedback(
                id,
                &trial_patch,
                TEST_STATUS_PASSED,
                "自动汉化试译实机确认正常",
            )
            .await?;
            session.trial_verified = true;
            let directory = self.project_directory(id)?;
            let updated = session.clone();
            tokio::task::spawn_blocking(move || save_session(&directory, &updated)).await??;
        }
        self.execute_workflow(
            id,
            session.definition,
            session.next_step_id,
            session.finish,
            String::new(),
            RecognitionMode::Automatic,
            RunContext {
                targets: session.target_unit_ids,
                verified: Some(trial_patch),
                ..Default::default()
            },
            true,
        )
        .await
    }

    /// Cancels a paused workflow gate without removing translations or patches.
    pub async fn dismiss_workflow(&self, id: &str) -> Result<(), CoreError> {
        let _project = self.lock_project(id).await?;
        let _lifecycle = self.project_lifecycle.lock().await;
        if self.task_state(id).await.is_some_and(|state| state.running) {
            return Err(CoreError::ProjectBusy);
        }
        self.ensure_writable().await?;
        let directory = self.project_directory(id)?;
        tokio::task::spawn_blocking(move || clear_session(&directory)).await??;
        self.finish_dismissed_workflow(id).await?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn execute_workflow(
        &self,
        id: &str,
        definition: WorkflowDefinition,
        start_step_id: String,
        finish: Option<WorkflowFinish>,
        instruction: String,
        mode: RecognitionMode,
        mut context: RunContext,
        preserve_existing_session_on_error: bool,
    ) -> Result<(), CoreError> {
        let start_position = definition
            .step_index(&start_step_id)
            .ok_or_else(|| CoreError::InvalidData("自动流程入口步骤不存在。".into()))?;
        let writes_review_evidence = definition.steps[start_position..]
            .iter()
            .any(|step| step.action == WorkflowAction::Recognize);
        let start_message = if preserve_existing_session_on_error {
            "正在继续自动汉化流程。"
        } else {
            "正在开始全自动汉化流程。"
        };
        let cancellation = self
            .begin_with_review_evidence(
                id,
                task_op::WORKFLOW,
                start_message,
                writes_review_evidence,
            )
            .await?;
        let directory = self.project_directory(id)?;
        if !preserve_existing_session_on_error {
            let dir = directory.clone();
            tokio::task::spawn_blocking(move || clear_session(&dir)).await??;
        }
        let mut position = start_position;
        let mut last_step = String::new();
        let mut paused_session: Option<WorkflowSession> = None;
        let result = async {
            for _ in 0..MAX_WORKFLOW_ITERATIONS {
                let step = &definition.steps[position];
                last_step.clone_from(&step.id);
                self.set_workflow_progress(
                    id,
                    WorkflowProgress {
                        step_id: step.id.clone(),
                        action: step.action,
                        status: WorkflowStatus::Running,
                        target_unit_id: context.targets.first().cloned(),
                        pause_kind: None,
                    },
                )
                .await?;
                if cancellation.is_cancelled() {
                    return Err(gamel10n_engines::EngineError::Cancelled.into());
                }
                let outcome = self
                    .run_workflow_step(
                        id,
                        &definition,
                        position,
                        finish,
                        &instruction,
                        mode,
                        &cancellation,
                        &mut context,
                    )
                    .await;
                if cancellation.is_cancelled() {
                    return Err(gamel10n_engines::EngineError::Cancelled.into());
                }
                match outcome {
                    Ok(StepOutcome::Complete) => return Ok(()),
                    Ok(StepOutcome::Paused(session)) => {
                        paused_session = Some(session);
                        return Ok(());
                    }
                    Ok(StepOutcome::Continue) => {
                        if let Some(next) = &step.on_success {
                            position = definition.step_index(next).ok_or_else(|| {
                                CoreError::InvalidData("自动流程分支不存在。".into())
                            })?;
                        } else {
                            position += 1;
                            if position == definition.steps.len() {
                                return Ok(());
                            }
                        }
                    }
                    Err(error) => {
                        let next = if needs_workflow_input(&error) {
                            step.on_unavailable.as_ref()
                        } else {
                            step.on_failure.as_ref()
                        };
                        self.log_task_message_from(
                            id,
                            &format!("{}：{error}", step.action.title()),
                            LogSource::for_error(&error),
                        )
                        .await;
                        if let Some(next) = next {
                            position = definition.step_index(next).ok_or_else(|| {
                                CoreError::InvalidData("自动流程分支不存在。".into())
                            })?;
                        } else {
                            return Err(error);
                        }
                    }
                }
            }
            Err(CoreError::InvalidData(format!(
                "自动流程超过 {MAX_WORKFLOW_ITERATIONS} 次步骤执行，已停止；请检查分支是否循环。"
            )))
        }
        .await;

        if let Some(session) = &paused_session {
            let dir = directory.clone();
            let saved = session.clone();
            tokio::task::spawn_blocking(move || save_session(&dir, &saved)).await??;
            self.mark_workflow_waiting(id, Some(session.pause_kind))
                .await;
        } else if result.is_ok() {
            let dir = directory.clone();
            tokio::task::spawn_blocking(move || clear_session(&dir)).await??;
        } else if result.as_ref().is_err_and(needs_workflow_input) {
            self.mark_workflow_waiting(id, None).await;
        }

        let terminal = if let Some(session) = &paused_session {
            Ok(session.message.clone())
        } else {
            result
                .as_ref()
                .map(|()| {
                    if context.installed_all {
                        "自动汉化完成，全量补丁已安装。".into()
                    } else if matches!(context.built, Some((_, WorkflowScope::All))) {
                        "全量补丁已生成。".into()
                    } else if context.verified.is_some() {
                        "试译补丁已确认正常，自动汉化流程完成。".into()
                    } else {
                        "配置的自动流程已完成。".into()
                    }
                })
                .map_err(|error| format!("自动流程在步骤“{last_step}”停止：{error}"))
        };
        let source = result
            .as_ref()
            .err()
            .map_or(LogSource::Application, LogSource::for_error);
        self.finish_with_log_source(id, task_op::WORKFLOW, terminal, source)
            .await?;
        result
    }

    #[allow(clippy::too_many_arguments)]
    async fn run_workflow_step(
        &self,
        id: &str,
        definition: &WorkflowDefinition,
        position: usize,
        finish: Option<WorkflowFinish>,
        instruction: &str,
        mode: RecognitionMode,
        cancellation: &CancellationToken,
        context: &mut RunContext,
    ) -> Result<StepOutcome, CoreError> {
        let step = &definition.steps[position];
        match step.action {
            WorkflowAction::Recognize => {
                *context = RunContext::default();
                let instruction = format!("{instruction}\n{}", step.instruction);
                let result = self
                    .recognize_in_task(id, instruction, mode, Some(cancellation))
                    .await?;
                if let RecognitionResult::Adapted(result) = result {
                    use gamel10n_engines::AdaptationStatus::*;
                    let message =
                        || format!("识别尚未完成：{} {}", result.summary, result.next_action);
                    let error = match result.status {
                        Activated | Reused => None,
                        WaitingForUser | Incomplete | BudgetExhausted => {
                            Some(CoreError::WorkflowInput(message()))
                        }
                        Cancelled => {
                            cancellation.cancel();
                            return Err(gamel10n_engines::EngineError::Cancelled.into());
                        }
                        Failed | Unavailable | Unsupported => {
                            Some(CoreError::InvalidData(message()))
                        }
                    };
                    if let Some(error) = error {
                        return Err(if result.next_action_from_model {
                            CoreError::WithModelOutput(Box::new(error))
                        } else {
                            error
                        });
                    }
                }
                if self.cached_analysis(id).await?.is_none() {
                    return Err(CoreError::InvalidData("识别没有生成可用文本。".into()));
                }
                context.verified = None;
                Ok(StepOutcome::Continue)
            }
            WorkflowAction::SelectTrial => {
                context.targets.clear();
                context.verified = None;
                self.select_workflow_trial(id, definition, position, finish, context)
                    .await
            }
            WorkflowAction::Translate | WorkflowAction::BuildPatch => {
                let ids = match step.scope {
                    WorkflowScope::Target => {
                        context.verified = None;
                        if context.targets.is_empty() {
                            return Err(CoreError::WorkflowInput(
                                "请先在「翻译与校对」勾选需要试译的文本。".into(),
                            ));
                        }
                        Some(context.targets.clone())
                    }
                    WorkflowScope::All => {
                        self.require_workflow_verification(context)?;
                        None
                    }
                };
                if step.action == WorkflowAction::Translate {
                    let options = TranslationOptions {
                        unit_ids: ids,
                        collect_translations: false,
                        ..Default::default()
                    };
                    let result = self
                        .translate_in_task(
                            id,
                            options.clone(),
                            task_op::TRANSLATE,
                            crate::translation_tasks::TranslationInput::Committed,
                            cancellation,
                        )
                        .await?;
                    if result.failed != 0 || result.completed != result.total {
                        return Err(CoreError::InvalidData(format!(
                            "翻译未完整完成：成功 {} 条，失败 {} 条，共 {} 条。",
                            result.completed, result.failed, result.total
                        )));
                    }
                    self.save_translation_scope(id, &options, true).await?;
                } else {
                    if step.scope == WorkflowScope::All {
                        let summary = self
                            .workbench_page(id, &crate::workbench::WorkbenchFilter::default(), 0, 1)
                            .await?
                            .summary;
                        if summary.total == 0
                            || summary.translated != summary.total
                            || summary.invalid != 0
                        {
                            return Err(CoreError::InvalidData(
                                "全量译文尚未完成，不能生成全量补丁。".into(),
                            ));
                        }
                    }
                    context.built = None;
                    context.installed = None;
                    context.installed_all = false;
                    let patch = self
                        .generate_patch_in_task(id, ids.as_deref(), false, cancellation)
                        .await?;
                    context.built = Some((patch, step.scope));
                }
                Ok(StepOutcome::Continue)
            }
            WorkflowAction::InstallPatch => {
                let Some((built, scope)) = &context.built else {
                    return Err(CoreError::InvalidData("请先在本次流程中生成补丁。".into()));
                };
                if *scope == WorkflowScope::All {
                    self.require_workflow_verification(context)?;
                } else {
                    context.verified = None;
                }
                context.installed = None;
                context.installed_all = false;
                self.install_patch_in_task(id, Some(built), cancellation)
                    .await?;
                context.installed = Some(built.clone());
                context.installed_all = *scope == WorkflowScope::All;
                Ok(StepOutcome::Continue)
            }
            WorkflowAction::ReviewTrial => {
                context.verified = None;
                let patch = self
                    .latest_patch(id)
                    .await?
                    .ok_or_else(|| CoreError::InvalidData("尚未生成试译补丁。".into()))?;
                if context.installed.as_ref() != Some(&patch.patch_directory)
                    || !context.built.as_ref().is_some_and(|(path, scope)| {
                        path == &patch.patch_directory && *scope == WorkflowScope::Target
                    })
                {
                    return Err(CoreError::InvalidData(
                        "请先生成并安装本次试译补丁。".into(),
                    ));
                }
                let next_step_id = step
                    .on_success
                    .clone()
                    .or_else(|| definition.steps.get(position + 1).map(|s| s.id.clone()))
                    .unwrap_or_else(|| step.id.clone());
                let count = context.targets.len();
                let message = match finish {
                    Some(WorkflowFinish::Trial) => format!(
                        "试译补丁已安装（共 {count} 条文本）。请启动游戏检查中文显示、字体和换行，确认无误后点击「确认正常，完成试译」。"
                    ),
                    Some(WorkflowFinish::Build) => format!(
                        "试译补丁已安装（共 {count} 条文本）。请启动游戏检查中文显示、字体和换行，确认无误后点击「确认正常，生成全量补丁」。"
                    ),
                    _ => format!(
                        "试译补丁已安装（共 {count} 条文本）。请启动游戏检查中文显示、字体和换行，确认无误并关闭游戏后点击继续。"
                    ),
                };
                Ok(StepOutcome::Paused(WorkflowSession {
                    schema_version: WORKFLOW_SCHEMA_VERSION,
                    definition: definition.clone(),
                    next_step_id,
                    pause_kind: WorkflowPauseKind::ReviewTrial,
                    finish,
                    target_unit_ids: context.targets.clone(),
                    trial_patch_directory: Some(patch.patch_directory),
                    trial_verified: false,
                    message,
                }))
            }
            WorkflowAction::WaitForInput => {
                Err(CoreError::WorkflowInput(if step.message.is_empty() {
                    "当前流程需要你完成操作后继续。".into()
                } else {
                    step.message.clone()
                }))
            }
            WorkflowAction::Complete => Ok(StepOutcome::Complete),
        }
    }

    // Custom branches can enter full-scope steps directly. Require confirmed trial verification.
    fn require_workflow_verification(&self, context: &RunContext) -> Result<(), CoreError> {
        if context.verified.is_none() {
            return Err(CoreError::InvalidData(
                "试译补丁尚未通过实机确认，不能开始全量处理。".into(),
            ));
        }
        Ok(())
    }

    async fn select_workflow_trial(
        &self,
        id: &str,
        definition: &WorkflowDefinition,
        position: usize,
        finish: Option<WorkflowFinish>,
        context: &mut RunContext,
    ) -> Result<StepOutcome, CoreError> {
        let step = &definition.steps[position];
        let analysis = self
            .cached_analysis(id)
            .await?
            .ok_or_else(|| CoreError::InvalidData("请先识别并提取游戏文本。".into()))?;
        let configured: Vec<String> = if !step.unit_ids.is_empty() {
            step.unit_ids.clone()
        } else if !step.unit_id.is_empty() {
            vec![step.unit_id.clone()]
        } else {
            Vec::new()
        };
        let targets = if !configured.is_empty() {
            let available: HashSet<_> = analysis
                .extraction()
                .units
                .iter()
                .map(|unit| unit.id.as_str())
                .collect();
            let mut seen = HashSet::new();
            let valid: Vec<String> = configured
                .into_iter()
                .filter(|unit_id| {
                    available.contains(unit_id.as_str()) && seen.insert(unit_id.clone())
                })
                .collect();
            if valid.is_empty() {
                return Err(CoreError::WorkflowInput(
                    "配置的试译文本已不存在，请重新选择。".into(),
                ));
            }
            valid
        } else {
            self.load_text_selection(id).await?
        };
        if targets.is_empty() {
            if step.on_unavailable.is_some() {
                return Err(CoreError::WorkflowInput(
                    "尚未勾选试译文本，请前往「翻译与校对」勾选后继续。".into(),
                ));
            }
            return Ok(StepOutcome::Paused(WorkflowSession {
                schema_version: WORKFLOW_SCHEMA_VERSION,
                definition: definition.clone(),
                next_step_id: step.id.clone(),
                pause_kind: WorkflowPauseKind::SelectTrial,
                finish,
                target_unit_ids: Vec::new(),
                trial_patch_directory: None,
                trial_verified: false,
                message: "识别已完成，但尚未勾选试译文本。请前往「翻译与校对」勾选需要试译的条目，然后点击「继续试译」。".into(),
            }));
        }
        self.log_task_message(
            id,
            &format!("已确定试译目标，共 {} 条文本。", targets.len()),
        )
        .await;
        context.targets = targets;
        Ok(StepOutcome::Continue)
    }
}

#[cfg(test)]
mod tests;
