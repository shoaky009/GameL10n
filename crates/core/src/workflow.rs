//! Configurable project-owned translation and runtime verification workflow.

use crate::{
    Application, CancellationToken, CoreError, GameProject,
    engine_tasks::{RecognitionMode, RecognitionResult},
    game_check::{CheckMode, CheckResult, CheckSettings, CheckStatus, protocol::normalize_text},
};
use gamel10n_translation::{
    TranslationUnit,
    execution::TranslationOptions,
    files::{io_error, read_json, write_json},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

const WORKFLOW_KEY: &str = "automaticWorkflow";
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
    LocateText,
    Translate,
    BuildPatch,
    InstallPatch,
    CheckGame,
    WaitForInput,
    Complete,
}

impl WorkflowAction {
    pub fn title(self) -> &'static str {
        match self {
            Self::Recognize => "识别与适配",
            Self::LocateText => "定位开场文本",
            Self::Translate => "翻译文本",
            Self::BuildPatch => "生成补丁",
            Self::InstallPatch => "安装补丁",
            Self::CheckGame => "检查游戏显示并关闭进程",
            Self::WaitForInput => "等待补充运行场景",
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
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowStep {
    pub id: String,
    pub action: WorkflowAction,
    #[serde(default, skip_serializing_if = "target_scope")]
    pub scope: WorkflowScope,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub unit_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settings: Option<CheckSettings>,
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
            WorkflowFinish::Trial => "check",
            WorkflowFinish::Build => "buildAll",
            WorkflowFinish::Install => "installAll",
        };
        let steps = [
            ("recognize", Recognize),
            ("locate", LocateText),
            ("trialTranslate", Translate),
            ("trialBuild", BuildPatch),
            ("trialInstall", InstallPatch),
            ("check", CheckGame),
            ("translateAll", Translate),
            ("buildAll", BuildPatch),
            ("installAll", InstallPatch),
            ("input", WaitForInput),
            ("done", Complete),
        ]
        .into_iter()
        .map(|(id, action)| WorkflowStep {
            id: id.into(),
            action,
            scope: if matches!(id, "translateAll" | "buildAll") {
                WorkflowScope::All
            } else {
                WorkflowScope::Target
            },
            unit_id: String::new(),
            settings: None,
            instruction: String::new(),
            message: String::new(),
            on_success: (id == last_step || id == "installAll").then(|| "done".into()),
            on_failure: None,
            on_unavailable: (id == "locate").then(|| "input".into()),
        })
        .collect();
        Self {
            schema_version: 1,
            entry: if preset.use_existing_text {
                "locate"
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
        if self.schema_version != 1 || self.steps.is_empty() || self.steps.len() > 64 {
            return Err(invalid("需要 schemaVersion: 1，且步骤数量为 1～64。"));
        }
        let mut ids = BTreeSet::new();
        for step in &self.steps {
            if step.id.is_empty() || step.id.len() > 64 || !ids.insert(step.id.as_str()) {
                return Err(invalid("步骤 ID 需非空、唯一且不超过 64 字节。"));
            }
            if step.instruction.chars().count() > 6000 || step.message.chars().count() > 2000 {
                return Err(invalid("步骤指令或提示过长。"));
            }
            if let Some(settings) = &step.settings {
                settings.validate().map_err(CoreError::InvalidData)?;
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
}

fn saved_workflow_choice(project: &GameProject) -> Result<Option<WorkflowChoice>, CoreError> {
    project
        .extra
        .get(WORKFLOW_KEY)
        .map(WorkflowChoice::deserialize)
        .transpose()
        .map_err(|error| CoreError::InvalidData(format!("自动汉化设置无效：{error}")))
}

fn read_definition(path: &Path) -> Result<WorkflowDefinition, CoreError> {
    if path
        .metadata()
        .map_err(|error| io_error(path, error))?
        .len()
        > 256 * 1024
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

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum WorkflowStatus {
    Running,
    Completed,
    Failed,
    WaitingForInput,
    Interrupted,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowProgress {
    pub step_id: String,
    pub action: WorkflowAction,
    pub status: WorkflowStatus,
    pub target_unit_id: Option<String>,
}

#[derive(Default)]
struct RunContext {
    target: Option<String>,
    target_settings: Option<CheckSettings>,
    built: Option<(PathBuf, WorkflowScope)>,
    installed: Option<PathBuf>,
    verified: Option<(String, String)>,
    installed_all: bool,
}

impl Application {
    /// Returns the project default and whether to offer the custom workflow choice.
    pub async fn workflow_preferences(
        &self,
        id: &str,
    ) -> Result<(WorkflowChoice, bool), CoreError> {
        let project = self.store.get(id).await?;
        let path = self.project_directory(id)?.join("workflow.json");
        let has_custom = path.try_exists()?
            && !read_definition(&path)
                .is_ok_and(|definition| definition == WorkflowDefinition::default());
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
        let path = self.project_directory(id)?.join("workflow.json");
        if !path.try_exists()? {
            write_json(&path, &WorkflowDefinition::default())?;
        }
        Ok(path)
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
        let definition = match choice {
            WorkflowChoice::Preset(preset) => WorkflowDefinition::from_preset(preset),
            WorkflowChoice::Custom => {
                read_definition(&self.project_directory(id)?.join("workflow.json"))?
            }
        };
        let cancellation = self
            .begin(id, "workflow", "正在开始全自动汉化流程。")
            .await?;
        let mut context = RunContext::default();
        let mut position = definition
            .steps
            .iter()
            .position(|s| s.id == definition.entry)
            .unwrap_or(0);
        let mut last_step = String::new();
        let result = async {
            for _ in 0..64 {
                let step = &definition.steps[position];
                last_step.clone_from(&step.id);
                self.set_workflow_progress(
                    id,
                    WorkflowProgress {
                        step_id: step.id.clone(),
                        action: step.action,
                        status: WorkflowStatus::Running,
                        target_unit_id: context.target.clone(),
                    },
                )
                .await?;
                if cancellation.is_cancelled() {
                    return Err(gamel10n_engines::EngineError::Cancelled.into());
                }
                let outcome = self
                    .run_workflow_step(id, step, &instruction, mode, &cancellation, &mut context)
                    .await;
                if cancellation.is_cancelled() {
                    return Err(gamel10n_engines::EngineError::Cancelled.into());
                }
                if step.action == WorkflowAction::Complete && outcome.is_ok() {
                    return Ok(());
                }
                let next = match &outcome {
                    Ok(()) => step.on_success.as_ref(),
                    Err(CoreError::WorkflowInput(_)) => step.on_unavailable.as_ref(),
                    Err(_) => step.on_failure.as_ref(),
                };
                if let Err(error) = &outcome {
                    self.log_task_message(id, &format!("{}：{error}", step.action.title()))
                        .await;
                }
                if let Some(next) = next {
                    position = definition
                        .steps
                        .iter()
                        .position(|s| s.id == *next)
                        .ok_or_else(|| CoreError::InvalidData("自动流程分支不存在。".into()))?;
                } else {
                    outcome?;
                    position += 1;
                    if position == definition.steps.len() {
                        return Ok(());
                    }
                }
            }
            Err(CoreError::InvalidData(
                "自动流程超过 64 次步骤执行，已停止；请检查分支是否循环。".into(),
            ))
        }
        .await;
        if matches!(&result, Err(CoreError::WorkflowInput(_))) {
            self.mark_workflow_waiting(id).await;
        }
        let terminal = result
            .as_ref()
            .map(|()| {
                if context.installed_all {
                    "全自动汉化完成，全量补丁已安装。".into()
                } else if matches!(context.built, Some((_, WorkflowScope::All))) {
                    "全量补丁已生成。".into()
                } else if context.verified.is_some() {
                    "试译补丁已安装，游戏显示检查已通过。".into()
                } else {
                    "配置的自动流程已完成。".into()
                }
            })
            .map_err(|error| format!("自动流程在步骤“{last_step}”停止：{error}"));
        self.finish(id, "workflow", terminal).await?;
        result
    }

    async fn run_workflow_step(
        &self,
        id: &str,
        step: &WorkflowStep,
        instruction: &str,
        mode: RecognitionMode,
        cancellation: &CancellationToken,
        context: &mut RunContext,
    ) -> Result<(), CoreError> {
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
                        format!("识别尚未完成：{} {}", result.summary, result.next_action);
                    match result.status {
                        Activated | Reused => {}
                        WaitingForUser | Incomplete | BudgetExhausted => {
                            return Err(CoreError::WorkflowInput(message));
                        }
                        Cancelled => {
                            cancellation.cancel();
                            return Err(gamel10n_engines::EngineError::Cancelled.into());
                        }
                        Failed | Unavailable | Unsupported => {
                            return Err(CoreError::InvalidData(message));
                        }
                    }
                }
                if self.cached_analysis(id).await?.is_none() {
                    return Err(CoreError::InvalidData("识别没有生成可用文本。".into()));
                }
                context.verified = None;
            }
            WorkflowAction::LocateText => {
                *context = RunContext::default();
                self.locate_workflow_text(id, step, cancellation, context)
                    .await?;
                context.verified = None;
            }
            WorkflowAction::Translate | WorkflowAction::BuildPatch => {
                let ids = match step.scope {
                    WorkflowScope::Target => {
                        context.verified = None;
                        Some(vec![context.target.clone().ok_or_else(|| {
                            CoreError::WorkflowInput("请先定位或选择一条开场文本。".into())
                        })?])
                    }
                    WorkflowScope::All => {
                        self.require_workflow_verification(id, context).await?;
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
                        .translate_in_task(id, options.clone(), "translate", cancellation)
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
            }
            WorkflowAction::InstallPatch => {
                let patch = self
                    .latest_patch(id)
                    .await?
                    .ok_or_else(|| CoreError::InvalidData("尚未生成补丁。".into()))?;
                let Some((built, scope)) = &context.built else {
                    return Err(CoreError::InvalidData("请先在本次流程中生成补丁。".into()));
                };
                if built != &patch.patch_directory {
                    return Err(CoreError::InvalidData(
                        "待安装的补丁已变化，请重新生成。".into(),
                    ));
                }
                if *scope == WorkflowScope::All {
                    self.require_workflow_verification(id, context).await?;
                } else {
                    context.verified = None;
                }
                context.installed = None;
                context.installed_all = false;
                self.install_patch_in_task(id, cancellation).await?;
                context.installed = Some(patch.patch_directory);
                context.installed_all = *scope == WorkflowScope::All;
            }
            WorkflowAction::CheckGame => {
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
                        "请先生成并安装本次目标文本的试译补丁。".into(),
                    ));
                }
                let target = context
                    .target
                    .as_ref()
                    .ok_or_else(|| CoreError::WorkflowInput("尚未确定目标文本。".into()))?;
                let mut settings = match &step.settings {
                    Some(settings) => settings.clone(),
                    None => {
                        let saved = self.game_check_settings(id).await?;
                        match context.target_settings.clone() {
                            Some(mut located) => {
                                located.language = saved.language;
                                located
                            }
                            None => saved,
                        }
                    }
                };
                settings.unit_ids = vec![target.clone()];
                let expected = self.game_check_expected(id, &settings.unit_ids).await?;
                let launch = self.launch_view(id).await?;
                let request = self
                    .game_check_request(
                        id,
                        CheckMode::LaunchAndText,
                        settings,
                        expected.clone(),
                        true,
                    )
                    .await?;
                self.execute_workflow_check(id, &request, cancellation)
                    .await?;
                if self.launch_view(id).await?.revision != launch.revision {
                    return Err(CoreError::InvalidData(
                        "检查期间启动设置已变化，请重新检查。".into(),
                    ));
                }
                context.verified = Some((launch.revision, expected.join("\n")));
            }
            WorkflowAction::WaitForInput => {
                return Err(CoreError::WorkflowInput(if step.message.is_empty() {
                    "未能定位验证文本。请在运行检查中调整进入场景的操作，或选择一条目标文本后重试。"
                        .into()
                } else {
                    step.message.clone()
                }));
            }
            WorkflowAction::Complete => {}
        }
        Ok(())
    }

    // Custom branches can enter full-scope steps directly. Check the trial and its inputs there.
    async fn require_workflow_verification(
        &self,
        id: &str,
        context: &RunContext,
    ) -> Result<(), CoreError> {
        let Some((revision, text)) = &context.verified else {
            return Err(CoreError::InvalidData(
                "试译补丁尚未通过游戏显示检查，不能开始全量处理。".into(),
            ));
        };
        let ids = vec![
            context
                .target
                .clone()
                .ok_or_else(|| CoreError::WorkflowInput("尚未确定目标文本。".into()))?,
        ];
        if self.launch_view(id).await?.revision != *revision
            || self.game_check_expected(id, &ids).await?.join("\n") != *text
        {
            return Err(CoreError::InvalidData(
                "已验证的启动配置或目标译文已变化，请重新检查。".into(),
            ));
        }
        Ok(())
    }

    async fn execute_workflow_check(
        &self,
        id: &str,
        request: &crate::game_check::protocol::CheckRequest,
        cancellation: &CancellationToken,
    ) -> Result<CheckResult, CoreError> {
        let _desktop = crate::game_check::DESKTOP_CHECK
            .try_lock()
            .map_err(|_| CoreError::InvalidData("已有运行检查正在使用桌面，请稍后重试。".into()))?;
        let mut result = CheckResult::default();
        self.run_game_check_tool(request, cancellation, cancellation, &mut result, &|_| {})
            .await?;
        for (name, observation) in [
            ("启动", &result.launch),
            ("关闭游戏进程", &result.closed),
            ("文字显示", &result.text),
        ] {
            if observation.status != CheckStatus::Passed {
                let message = format!("{name}未通过：{}", observation.message);
                return Err(
                    if name == "文字显示" && request.mode == CheckMode::LocateText {
                        CoreError::WorkflowInput(message)
                    } else {
                        CoreError::InvalidData(message)
                    },
                );
            }
        }
        self.log_task_message(id, "游戏启动、目标文字与进程关闭检查已通过。")
            .await;
        Ok(result)
    }

    async fn locate_workflow_text(
        &self,
        id: &str,
        step: &WorkflowStep,
        cancellation: &CancellationToken,
        context: &mut RunContext,
    ) -> Result<(), CoreError> {
        let mut settings = match &step.settings {
            Some(settings) => settings.clone(),
            None => self.game_check_settings(id).await?,
        };
        let analysis = self
            .cached_analysis(id)
            .await?
            .ok_or_else(|| CoreError::InvalidData("请先识别并提取游戏文本。".into()))?;
        let selected = if !step.unit_id.is_empty() {
            Some(step.unit_id.clone())
        } else {
            match settings.unit_ids.as_slice() {
                [id] => Some(id.clone()),
                _ => None,
            }
        };
        let target = if let Some(selected) = selected {
            if !analysis.extraction().units.iter().any(|u| u.id == selected) {
                return Err(CoreError::WorkflowInput(
                    "配置的目标文本已不存在，请重新选择。".into(),
                ));
            }
            selected
        } else {
            // OCR must observe original text, including after a previous interrupted trial.
            let restored = self.restore_game_in_task(id, cancellation).await?;
            if !restored.conflicts.is_empty() {
                return Err(CoreError::InvalidData(format!(
                    "恢复原文时发生冲突：{}",
                    restored.conflicts.join("；")
                )));
            }
            if step.settings.is_none() {
                settings.language = source_ocr_language(&analysis.extraction().units).into();
            }
            let request = self
                .game_check_request(id, CheckMode::LocateText, settings.clone(), vec![], true)
                .await?;
            let result = self
                .execute_workflow_check(id, &request, cancellation)
                .await?;
            match_visible_unit(
                &analysis.extraction().units,
                result.visible_text.as_deref().unwrap_or_default(),
            )?
        };
        self.log_task_message(id, &format!("已确定运行验证目标：{target}"))
            .await;
        context.target = Some(target);
        context.target_settings = Some(settings);
        Ok(())
    }
}

fn source_ocr_language(units: &[TranslationUnit]) -> &'static str {
    let sample = units
        .iter()
        .take(64)
        .flat_map(|u| u.source_text.chars().take(512));
    let mut chinese = false;
    for c in sample {
        if ('\u{3040}'..='\u{30ff}').contains(&c) {
            return "ja";
        }
        chinese |= ('\u{4e00}'..='\u{9fff}').contains(&c);
    }
    if chinese { "zh-Hans" } else { "en" }
}

fn match_visible_unit(units: &[TranslationUnit], visible: &str) -> Result<String, CoreError> {
    let visible = normalize_text(visible);
    let mut best: Option<((usize, std::cmp::Reverse<usize>), &str)> = None;
    let mut ambiguous = false;
    for unit in units
        .iter()
        .filter(|u| matches!(u.kind.as_str(), "dialogue" | "narration"))
    {
        let text = crate::game_check::display_text(unit, &unit.source_text);
        let normalized = normalize_text(&text);
        let length = normalized.chars().count();
        if length < 4 {
            continue;
        }
        let Some(position) = visible.find(&normalized) else {
            continue;
        };
        let key = (position, std::cmp::Reverse(length));
        if best.is_none_or(|(previous, _)| key < previous) {
            best = Some((key, &unit.id));
            ambiguous = false;
        } else if best.is_some_and(|(previous, _)| key == previous) {
            ambiguous = true;
        }
    }
    match best {
        Some((_, id)) if !ambiguous => Ok(id.to_owned()),
        _ => Err(CoreError::WorkflowInput(
            "当前画面无法唯一匹配开场对白，请补充进入场景的操作或指定目标文本 ID。".into(),
        )),
    }
}

#[cfg(test)]
mod tests;
