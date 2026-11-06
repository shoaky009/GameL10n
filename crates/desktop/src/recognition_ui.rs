use crate::view::App;
use gamel10n_core::TaskState;
use gamel10n_engines::EngineProgress;
use serde_json::Value;
use std::collections::BTreeMap;

pub fn concise_text(text: &str) -> String {
    const LIMIT: usize = 100;
    let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = normalized.chars();
    let mut short: String = chars.by_ref().take(LIMIT).collect();
    if chars.next().is_some() {
        short.push('…');
    }
    short
}

pub fn project_status(state: Option<&TaskState>, extra: &BTreeMap<String, Value>) -> String {
    let saved = extra.get("taskSummary").unwrap_or(&Value::Null);
    let count = extra.get("analysis").and_then(|a| a["unitCount"].as_u64());
    if let Some(state) = state {
        return task_status(
            &state.operation,
            state.running,
            state.succeeded,
            saved.get("adaptationStatus").and_then(Value::as_str),
            saved["pendingCoverageCount"].as_u64().unwrap_or(0) > 0,
        );
    }
    if let Some(operation) = saved.get("operation").and_then(Value::as_str) {
        return task_status(
            operation,
            false,
            saved.get("succeeded").and_then(Value::as_bool),
            saved.get("adaptationStatus").and_then(Value::as_str),
            saved["pendingCoverageCount"].as_u64().unwrap_or(0) > 0,
        );
    }
    if saved["running"].as_bool() == Some(true) {
        return "任务已暂停，可继续".into();
    }
    if let Some(stage) = saved.get("stage").and_then(Value::as_str).map(str::trim)
        && !stage.is_empty()
        && !stage.contains(['\r', '\n'])
        && stage.chars().count() <= 48
    {
        return stage.into();
    }
    count
        .map(|count| format!("已提取 {count} 条文本"))
        .unwrap_or_else(|| "等待识别".into())
}

pub fn task_status(
    operation: &str,
    running: bool,
    succeeded: Option<bool>,
    adaptation_status: Option<&str>,
    partial: bool,
) -> String {
    let operation_name = match operation {
        "recognize" => "常规适配",
        "extract" => "提取文本",
        "adapt" => "智能适配",
        "review" => "总结经验",
        "translate" => "翻译",
        "retranslate" => "重新翻译",
        "candidate" => "生成候选译文",
        "build" => "生成补丁",
        "install" => "安装补丁",
        "restore" => "恢复原版",
        "verify" => "验证适配器",
        "cleanup" => "清理项目文件",
        _ => "任务",
    };
    if running {
        return format!("正在{operation_name}");
    }
    match succeeded {
        Some(false) => format!("{operation_name}失败"),
        None => format!("{operation_name}已暂停"),
        Some(true) if operation == "adapt" => adaptation_stage(adaptation_status, partial).into(),
        Some(true) => format!("{operation_name}完成"),
    }
}

pub fn recognition_task(operation: &str) -> bool {
    matches!(operation, "recognize" | "adapt" | "review" | "extract")
}

pub fn task_api<'a>(operation: &str, summary: &'a str) -> &'a str {
    let prefix = match operation {
        "adapt" | "review" => "分析 API：",
        "translate" | "retranslate" | "candidate" => "翻译 API 链：",
        _ => return "",
    };
    summary
        .lines()
        .find_map(|line| line.strip_prefix(prefix))
        .unwrap_or("")
}

fn adaptation_stage(status: Option<&str>, partial: bool) -> &'static str {
    match status {
        Some("Activated" | "Reused") if partial => "部分文字可试译",
        Some("Activated" | "Reused") => "适配完成，可以试译",
        Some("WaitingForUser") => "适配需要补充信息或操作",
        Some("Unsupported") => "当前方案无法完成适配",
        Some("Cancelled" | "BudgetExhausted") => "适配已暂停，可继续",
        Some("Failed" | "Unavailable") => "适配遇到错误",
        Some("Incomplete") => "适配尚未完成，可继续",
        _ => "智能适配完成",
    }
}

fn text<'a>(value: &'a Value, field: &str) -> &'a str {
    value.get(field).and_then(Value::as_str).unwrap_or("")
}

fn lines(value: &Value) -> Vec<String> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter(|line| !line.trim().is_empty())
        .map(str::to_owned)
        .collect()
}

fn note(value: &Value, fallback: &str) -> String {
    let message = text(value, "message");
    if message.is_empty() {
        return String::new();
    }
    let title = match text(value, "kind") {
        "verified" => "主程序验证",
        "exploring" => "仍在探索 · 模型说明",
        "breakthrough" => "关键突破 · 模型说明",
        "implementing" => "方案已确定 · 模型说明",
        "correction" => "进展更正 · 模型说明",
        "update" => "模型说明",
        _ => fallback,
    };
    format!("{title}：{message}")
}

pub fn progress_notes(current: &Value, breakthrough: &Value) -> String {
    let mut notes = vec![note(current, "适配进展")];
    if breakthrough != current {
        let previous = note(breakthrough, "进展");
        if !previous.is_empty() {
            notes.push(format!("最近进展 · {previous}"));
        }
    }
    notes.retain(|note| !note.is_empty());
    notes.join("\n")
}

fn blocker(value: &Value) -> String {
    let mut details = Vec::new();
    for (field, title) in [
        ("reason", "当前卡点"),
        ("evidence", "依据"),
        ("nextAction", "处理方式"),
    ] {
        let detail = text(value, field);
        if !detail.is_empty() {
            details.push(format!("{title}：{detail}"));
        }
    }
    details.join("\n\n")
}

pub fn progress_detail(progress: &EngineProgress) -> String {
    progress_text(
        &progress.stage,
        &progress.message,
        progress.tool_name.as_deref(),
    )
}

fn progress_text(stage: &str, message: &str, tool: Option<&str>) -> String {
    let mut heading = stage.to_owned();
    if let Some(tool) = tool.filter(|tool| !tool.is_empty()) {
        if !heading.is_empty() {
            heading.push_str(" · ");
        }
        heading.push_str(tool);
    }
    if !message.is_empty() && message != heading {
        if !heading.is_empty() {
            heading.push('\n');
        }
        heading.push_str(message);
    }
    heading
}

pub struct RecognitionView {
    pub status: String,
    overview_details: String,
    engine_reference: String,
    adapter_reference: String,
    ready: bool,
    running: bool,
    recognition_running: bool,
    result_title: String,
    summary: String,
    next_action: String,
    blocker: String,
    coverage: String,
    validation: String,
    runtime_checks: String,
    warnings: String,
    failure: RecognitionFailure,
    diagnostics: String,
    notes: String,
    progress: String,
}

#[derive(Default)]
struct RecognitionFailure {
    title: String,
    message: String,
    next_action: String,
    from_result: bool,
}

impl RecognitionFailure {
    fn new(state: Option<&TaskState>, result: &Value) -> Self {
        if state.is_some_and(|state| state.running) {
            return Self::default();
        }
        let current_result =
            state.is_none_or(|state| state.operation == "adapt" && state.succeeded.is_some());
        if current_result && matches!(text(result, "status"), "Failed" | "Unavailable") {
            let failure = &result["failure"];
            let mut message = text(failure, "message").to_owned();
            if message.is_empty() {
                message = lines(&result["diagnostics"]).join("\n");
            }
            if message.is_empty() {
                message = text(result, "summary").to_owned();
            }
            if message.is_empty() {
                message = "本次智能适配未能完成。".into();
            }
            let title = match text(failure, "category") {
                "api" => "API 调用失败",
                "tool" => "工具执行失败",
                "storage" => "文件访问失败",
                "adapter" => "适配器错误",
                "data" => "数据处理失败",
                "research" => "资料查询失败",
                _ => "智能适配失败",
            };
            let next_action = [text(failure, "nextAction"), text(result, "nextAction")]
                .into_iter()
                .find(|action| !action.is_empty())
                .unwrap_or("处理上述问题后，重新运行智能识别与适配。");
            // A new attempt can fail before replacing the previous result file.
            let matches_current_task = state.is_none_or(|state| {
                state.succeeded != Some(false)
                    || state.message == message
                    || state
                        .message
                        .strip_prefix(message.as_str())
                        .is_some_and(|suffix| suffix.starts_with('\n'))
            });
            if matches_current_task {
                return Self {
                    title: title.into(),
                    message,
                    next_action: next_action.into(),
                    from_result: true,
                };
            }
        }
        if let Some(state) = state
            .filter(|state| recognition_task(&state.operation) && state.succeeded == Some(false))
        {
            return Self {
                title: task_status(&state.operation, false, Some(false), None, false),
                message: state.message.clone(),
                next_action: "处理上述问题后，重新运行本次任务。".into(),
                from_result: false,
            };
        }
        Self::default()
    }
}

impl RecognitionView {
    pub fn new(
        extra: &BTreeMap<String, Value>,
        state: Option<&TaskState>,
        progress: &Value,
        result: &Value,
    ) -> Self {
        let analysis = extra.get("analysis").unwrap_or(&Value::Null);
        let count = analysis["unitCount"].as_u64().unwrap_or(0);
        let ready = count > 0;
        let running = state.is_some_and(|s| s.running && s.operation == "adapt");
        let recognition_running =
            state.is_some_and(|s| s.running && recognition_task(&s.operation));
        let current_progress =
            state.is_some_and(|s| s.running && s.message == text(progress, "message"));
        let coverage = lines(&result["pendingCoverage"]);
        let result_status = result.get("status").and_then(Value::as_str);
        let failure = RecognitionFailure::new(state, result);
        let mut status = project_status(state, extra);
        if state.is_none_or(|s| !s.running && s.operation == "adapt" && s.succeeded == Some(true))
            && result_status.is_some()
        {
            status = adaptation_stage(result_status, !coverage.is_empty()).into();
        }
        let mut overview_details = String::new();
        let mut engine_reference = String::new();
        let mut adapter_reference = String::new();
        if !analysis.is_null() {
            overview_details = format!(
                "已提取 {count} 条文本  ·  {} 个脚本 / 资源",
                analysis["resourceCount"].as_u64().unwrap_or(0),
            );
            engine_reference = text(analysis, "engineId").into();
            adapter_reference = format!(
                "{}  ·  版本 {}",
                text(analysis, "adapterId"),
                text(analysis, "adapterVersion"),
            );
        }
        let summary = result
            .get("summary")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| {
                state
                    .filter(|s| !s.running && s.operation == "adapt" && s.succeeded == Some(true))
                    .map(|s| s.message.clone())
            })
            .unwrap_or_default();
        let result_title = if summary.is_empty() {
            String::new()
        } else if state
            .is_some_and(|s| s.operation == "adapt" && (s.running || s.succeeded != Some(true)))
            && !failure.from_result
            && !text(result, "summary").is_empty()
        {
            "上次适配结果".into()
        } else if matches!(result_status, Some("Activated" | "Reused")) {
            "适配结果".into()
        } else {
            "适配未完成".into()
        };
        let warnings = lines(&analysis["warnings"]);
        // Older failed results stored the fatal cause in diagnostics.
        let diagnostics = if matches!(result_status, Some("Failed" | "Unavailable"))
            && result["failure"].is_null()
        {
            String::new()
        } else {
            lines(&result["diagnostics"]).join("\n")
        };
        let mut runtime_checks = Vec::new();
        let mut validation = Vec::new();
        let verification = result
            .get("verification")
            .or_else(|| result.get("validation"));
        if let Some(verification) = verification.filter(|value| value.is_object()) {
            if let Some(passed) = verification["passed"].as_bool() {
                validation.push(format!(
                    "静态资源验收：{}。",
                    if passed { "通过" } else { "未通过" }
                ));
            }
            if let Some(passed) = verification["originalRoundtripPassed"].as_bool() {
                validation.push(format!(
                    "原文字节回填：{}。",
                    if passed { "通过" } else { "未通过" }
                ));
            }
            if let Some(tested) = verification["runtimeTested"].as_bool() {
                runtime_checks.push(format!(
                    "游戏运行验证：{}。",
                    if tested {
                        "已记录执行"
                    } else {
                        "未执行"
                    }
                ));
            }
            validation.extend(lines(&verification["diagnostics"]));
        }
        runtime_checks.extend(lines(&result["pendingRuntimeChecks"]));
        let model_running =
            state.is_some_and(|s| s.running && matches!(s.operation.as_str(), "adapt" | "review"));
        let same_operation = state.is_some_and(|s| {
            progress
                .get("operation")
                .and_then(Value::as_str)
                .map_or(s.operation == "adapt", |operation| operation == s.operation)
        });
        // The current step may advance before its progress snapshot is read.
        // Notes remain valid across steps of the same project's adaptation.
        let note_source = if model_running && same_operation {
            progress
        } else if recognition_running {
            &Value::Null
        } else {
            result
        };
        let notes = progress_notes(
            &note_source["progressNote"],
            &note_source["lastBreakthrough"],
        );
        let progress_details = if recognition_running && current_progress {
            progress_text(
                text(progress, "stage"),
                text(progress, "message"),
                progress["toolName"].as_str(),
            )
        } else if recognition_running && let Some(state) = state {
            state.message.clone()
        } else {
            String::new()
        };
        Self {
            status,
            overview_details,
            engine_reference,
            adapter_reference,
            ready,
            running,
            recognition_running,
            result_title,
            summary,
            next_action: if failure.message.is_empty() {
                text(result, "nextAction").into()
            } else {
                String::new()
            },
            blocker: blocker(&result["blocker"]),
            coverage: coverage.join("\n"),
            validation: validation.join("\n"),
            runtime_checks: runtime_checks.join("\n"),
            warnings: warnings.join("\n"),
            failure,
            diagnostics,
            notes,
            progress: progress_details,
        }
    }

    pub fn has_failure(&self) -> bool {
        !self.failure.message.is_empty()
    }

    pub fn apply(self, app: &App) {
        app.set_status(self.status.into());
        app.set_recognition(self.overview_details.into());
        app.set_recognition_engine(self.engine_reference.into());
        app.set_recognition_adapter(self.adapter_reference.into());
        app.set_recognition_ready(self.ready);
        app.set_recognition_running(self.recognition_running);
        app.set_recognition_warnings(self.warnings.into());
        app.set_recognition_failure_title(self.failure.title.into());
        app.set_recognition_failure_message(self.failure.message.into());
        app.set_recognition_failure_next_action(self.failure.next_action.into());
        app.set_recognition_diagnostics(self.diagnostics.into());
        app.set_adaptation_running(self.running);
        app.set_adaptation_result_title(self.result_title.into());
        app.set_adaptation_summary(self.summary.into());
        app.set_adaptation_next_action(self.next_action.into());
        app.set_adaptation_blocker(self.blocker.into());
        app.set_adaptation_pending_coverage(self.coverage.into());
        app.set_adaptation_validation(self.validation.into());
        app.set_adaptation_runtime_checks(self.runtime_checks.into());
        app.set_adaptation_notes(self.notes.into());
        app.set_adaptation_progress(self.progress.into());
    }
}

pub fn clear_failure(app: &App) {
    app.set_recognition_failure_title("".into());
    app.set_recognition_failure_message("".into());
    app.set_recognition_failure_next_action("".into());
    app.set_recognition_diagnostics("".into());
}

pub fn show_task_failure(app: &App, state: &TaskState) {
    let failure = RecognitionFailure::new(Some(state), &Value::Null);
    app.set_recognition_failure_title(failure.title.into());
    app.set_recognition_failure_message(failure.message.into());
    app.set_recognition_failure_next_action(failure.next_action.into());
}

pub fn clear(app: &App) {
    RecognitionView::new(&BTreeMap::new(), None, &Value::Null, &Value::Null).apply(app);
    app.set_adaptation_usage(Default::default());
    app.set_review_summary("".into());
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn completed_partial_adaptation_keeps_full_result_and_discards_stale_finish_progress() {
        let summary = "完整总结\n支持对白与部分界面。\n仍需验证字体。";
        let state = TaskState {
            project_id: "one".into(),
            operation: "adapt".into(),
            running: false,
            message: summary.into(),
            succeeded: Some(true),
        };
        let result = json!({"status":"Activated","summary":summary,"nextAction":"先试译少量文本", "pendingCoverage":["窗口标题"], "pendingRuntimeChecks":["字体和布局"], "blocker":{"reason":"需要新样本"},"verification":{"passed":true,"runtimeTested":false}});
        let view = RecognitionView::new(
            &BTreeMap::new(),
            Some(&state),
            &json!({"message":"正在执行 finish。"}),
            &result,
        );
        assert_eq!(
            (
                view.status.as_str(),
                view.summary.as_str(),
                view.next_action.as_str(),
                view.coverage.as_str(),
                view.progress.as_str()
            ),
            ("部分文字可试译", summary, "先试译少量文本", "窗口标题", "")
        );
        assert!(
            view.validation.contains("静态资源验收：通过")
                && view.runtime_checks.contains("游戏运行验证：未执行")
        );
    }

    #[test]
    fn reopening_old_project_uses_saved_result_while_empty_project_has_no_residual_details() {
        let mut extra = BTreeMap::new();
        extra.insert(
            "taskSummary".into(),
            json!({"stage":"很长的模型总结\n不能用作项目状态", "running":false}),
        );
        extra.insert("analysis".into(), json!({"unitCount":91184}));
        let saved = RecognitionView::new(
            &extra,
            None,
            &json!({"message":"正在执行 finish。"}),
            &json!({"status":"Activated","summary":"完整结果","pendingCoverage":["设置文字"]}),
        );
        let empty = RecognitionView::new(&BTreeMap::new(), None, &Value::Null, &Value::Null);
        let mut restored = extra.clone();
        restored.insert(
            "taskSummary".into(),
            json!({"stage":"已恢复原版", "running":false}),
        );
        assert_eq!(
            (
                project_status(None, &extra),
                project_status(None, &restored),
                saved.status,
                empty.summary,
                empty.coverage,
                empty.notes,
                empty.ready
            ),
            (
                "已提取 91184 条文本".into(),
                "已恢复原版".into(),
                "部分文字可试译".into(),
                String::new(),
                String::new(),
                String::new(),
                false
            )
        );
    }

    #[test]
    fn running_adaptation_labels_previous_result_and_preserves_structured_progress() {
        let mut state = TaskState {
            project_id: "one".into(),
            operation: "adapt".into(),
            running: true,
            message: "正在检查资源".into(),
            succeeded: None,
        };
        let progress = json!({"stage":"执行适配工具", "message":"正在检查资源", "toolName":"test_adapter", "progressNote":{"kind":"exploring", "message":"调查新格式"}, "lastBreakthrough":{"kind":"verified", "message":"原文回填通过"}});
        let view = RecognitionView::new(
            &BTreeMap::new(),
            Some(&state),
            &progress,
            &json!({"status":"Activated", "summary":"上次完整结果"}),
        );
        assert_eq!(
            (
                view.status.as_str(),
                view.result_title.as_str(),
                view.summary.as_str(),
                view.running
            ),
            ("正在智能适配", "上次适配结果", "上次完整结果", true)
        );
        assert!(
            view.progress.contains("test_adapter")
                && view.notes.contains("模型说明")
                && view.notes.contains("主程序验证")
        );
        state.message = "已进入下一项工具操作".into();
        let switched =
            RecognitionView::new(&BTreeMap::new(), Some(&state), &progress, &Value::Null);
        assert_eq!(switched.notes, view.notes);
        assert_eq!(switched.progress, state.message);
        let mut review = progress;
        review["operation"] = json!("review");
        let other_operation =
            RecognitionView::new(&BTreeMap::new(), Some(&state), &review, &Value::Null);
        assert!(other_operation.notes.is_empty());
    }

    #[test]
    fn failed_or_paused_run_does_not_turn_the_previous_adaptation_into_current_success() {
        for succeeded in [Some(false), None] {
            let state = TaskState {
                project_id: "one".into(),
                operation: "adapt".into(),
                running: false,
                message: "本次任务需要处理".into(),
                succeeded,
            };
            let view = RecognitionView::new(
                &BTreeMap::new(),
                Some(&state),
                &Value::Null,
                &json!({"status":"Activated", "summary":"上次成功结果"}),
            );
            assert_eq!(
                (
                    view.status.as_str(),
                    view.result_title.as_str(),
                    view.summary.as_str()
                ),
                (
                    if succeeded == Some(false) {
                        "智能适配失败"
                    } else {
                        "智能适配已暂停"
                    },
                    "上次适配结果",
                    "上次成功结果"
                )
            );
        }
    }

    #[test]
    fn extracted_project_keeps_api_failure_separate_from_scope_and_resource_checks() {
        let extra = BTreeMap::from([(
            "analysis".into(),
            json!({"unitCount":123, "warnings":["暂不支持菜单图片"]}),
        )]);
        let state = TaskState {
            project_id: "one".into(),
            operation: "adapt".into(),
            running: false,
            message: "分析 API 请求失败：服务商异常（HTTP 502）。\n确认服务恢复后重试".into(),
            succeeded: Some(false),
        };
        let result = json!({
            "status":"Failed", "summary":"本次智能适配失败", "nextAction":"请稍后重试",
            "failure":{"category":"api", "code":"http_error", "httpStatus":502,
                "message":"分析 API 请求失败：服务商异常（HTTP 502）。", "nextAction":"确认服务恢复后重试"},
            "diagnostics":["工具返回了额外输出"],
            "verification":{"passed":false,"diagnostics":["部分资源尚未验证"]}
        });
        let view = RecognitionView::new(&extra, Some(&state), &Value::Null, &result);
        assert_eq!(
            (
                view.ready,
                view.failure.title.as_str(),
                view.failure.message.as_str(),
                view.failure.next_action.as_str(),
                view.warnings.as_str(),
                view.diagnostics.as_str(),
                view.validation.as_str(),
                view.next_action.as_str(),
            ),
            (
                true,
                "API 调用失败",
                "分析 API 请求失败：服务商异常（HTTP 502）。",
                "确认服务恢复后重试",
                "暂不支持菜单图片",
                "工具返回了额外输出",
                "静态资源验收：未通过。\n部分资源尚未验证",
                "",
            )
        );
    }

    #[test]
    fn legacy_failed_result_uses_diagnostics_as_failure_even_when_task_said_success() {
        let state = TaskState {
            project_id: "one".into(),
            operation: "adapt".into(),
            running: false,
            message: "适配结束".into(),
            succeeded: Some(true),
        };
        let result = json!({"status":"Failed", "summary":"智能适配失败",
            "diagnostics":["模型调用失败：API 请求失败（HTTP 502）。"],
            "nextAction":"检查分析 API 后重试"});
        for state in [None, Some(&state)] {
            let view = RecognitionView::new(&BTreeMap::new(), state, &Value::Null, &result);
            assert_eq!(
                (
                    view.failure.message.as_str(),
                    view.failure.next_action.as_str(),
                    view.warnings.as_str(),
                    view.diagnostics.as_str(),
                ),
                (
                    "模型调用失败：API 请求失败（HTTP 502）。",
                    "检查分析 API 后重试",
                    "",
                    "",
                )
            );
        }
    }

    #[test]
    fn starting_or_pausing_a_task_hides_the_previous_failure() {
        let result = json!({"status":"Failed", "summary":"旧失败",
            "failure":{"category":"api", "message":"旧 API 错误", "nextAction":"重试"}});
        for (operation, running, succeeded) in [
            ("adapt", true, None),
            ("recognize", true, None),
            ("review", true, None),
            ("adapt", false, None),
            ("recognize", false, Some(true)),
        ] {
            let state = TaskState {
                project_id: "one".into(),
                operation: operation.into(),
                running,
                message: "当前任务".into(),
                succeeded,
            };
            let view = RecognitionView::new(&BTreeMap::new(), Some(&state), &Value::Null, &result);
            assert!(view.failure.message.is_empty(), "{operation}: {running}");
        }
        let empty = RecognitionView::new(&BTreeMap::new(), None, &Value::Null, &Value::Null);
        assert!(empty.failure.message.is_empty() && empty.diagnostics.is_empty());
    }

    #[test]
    fn newer_task_failure_takes_priority_over_a_previous_failed_result() {
        let state = TaskState {
            project_id: "one".into(),
            operation: "adapt".into(),
            running: false,
            message: "读取项目配置失败：文件被占用。".into(),
            succeeded: Some(false),
        };
        let result = json!({"status":"Failed", "summary":"上次适配失败",
            "failure":{"category":"api", "message":"旧 API 错误", "nextAction":"旧提示"}});
        let view = RecognitionView::new(&BTreeMap::new(), Some(&state), &Value::Null, &result);
        assert_eq!(
            (
                view.failure.title.as_str(),
                view.failure.message.as_str(),
                view.result_title.as_str(),
            ),
            (
                "智能适配失败",
                "读取项目配置失败：文件被占用。",
                "上次适配结果",
            )
        );
    }

    #[test]
    fn cancellation_budget_and_user_wait_are_not_task_failures() {
        for status in ["Cancelled", "BudgetExhausted", "WaitingForUser"] {
            let result = json!({"status":status, "diagnostics":["任务需要稍后继续"],
                "verification":{"passed":false,"diagnostics":["资源待补充"]}});
            let view = RecognitionView::new(&BTreeMap::new(), None, &Value::Null, &result);
            assert!(view.failure.message.is_empty(), "{status}");
        }
    }
}
