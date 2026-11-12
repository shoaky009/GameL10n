use std::{collections::BTreeMap, sync::atomic::Ordering};

use chrono::{Duration, Local, NaiveDate};
use gamel10n_core::{
    CoreError,
    metrics::{
        ConsumptionDimension, ConsumptionFilter, ConsumptionGroup, ConsumptionRecord,
        ConsumptionTotals, consumption_groups, mode_label, status_label, task_label,
    },
};
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};

use crate::{
    Context, UiInput,
    token_ui::compact,
    view::{App, ConsumptionRow, MetricGroupRow},
};

const PAGE_SIZE: usize = 20;
const TASKS: [&str; 6] = ["", "recognize", "adapt", "repair", "expand", "review"];
const MODES: [&str; 4] = ["", "new", "continued", "reused"];
const STATUSES: [&str; 8] = [
    "",
    "running",
    "completed",
    "failed",
    "cancelled",
    "paused",
    "interrupted",
    "unsupported",
];
const MEASURES: [&str; 12] = [
    "总 Token",
    "输入 Token",
    "输出 Token",
    "缓存输入 Token",
    "未缓存输入 Token",
    "缓存命中率",
    "总耗时",
    "API 耗时",
    "等待耗时",
    "本地及其他耗时",
    "API 请求次数",
    "失败请求次数",
];

#[derive(Clone, Default)]
pub struct MetricsInput {
    pub filter: ConsumptionFilter,
    date_range: i32,
    from: String,
    through: String,
    dimension: i32,
    measure: i32,
    average: bool,
    tab: i32,
    offset: usize,
}

fn selected(model: ModelRc<SharedString>, index: i32) -> String {
    if index <= 0 {
        String::new()
    } else {
        model
            .row_data(index as usize)
            .unwrap_or_default()
            .to_string()
    }
}

impl MetricsInput {
    pub fn capture(app: &App<'_>) -> Self {
        Self {
            filter: ConsumptionFilter {
                version: selected(
                    app.get_metrics_version_keys(),
                    app.get_metrics_version_index(),
                ),
                model: selected(app.get_metrics_models(), app.get_metrics_model_index()),
                project: selected(
                    app.get_metrics_project_keys(),
                    app.get_metrics_project_index(),
                ),
                task: TASKS
                    .get(app.get_metrics_task_index().max(0) as usize)
                    .unwrap_or(&"")
                    .to_string(),
                mode: MODES
                    .get(app.get_metrics_mode_index().max(0) as usize)
                    .unwrap_or(&"")
                    .to_string(),
                status: STATUSES
                    .get(app.get_metrics_status_index().max(0) as usize)
                    .unwrap_or(&"")
                    .to_string(),
                ..Default::default()
            },
            date_range: app.get_metrics_date_range(),
            from: app.get_metrics_from().into(),
            through: app.get_metrics_through().into(),
            dimension: app.get_metrics_dimension(),
            measure: app.get_metrics_measure(),
            average: app.get_metrics_aggregation() == 1,
            tab: app.get_metrics_tab(),
            offset: app.get_metrics_offset().max(0) as usize,
        }
    }

    fn filter(&self) -> Result<ConsumptionFilter, CoreError> {
        let mut filter = self.filter.clone();
        let today = Local::now().date_naive();
        match self.date_range {
            1 => {
                filter.from = Some(today);
                filter.through = Some(today);
            }
            2 | 3 => {
                filter.from =
                    Some(today - Duration::days(if self.date_range == 2 { 6 } else { 29 }));
                filter.through = Some(today);
            }
            4 => {
                let parse = |value: &str| -> Result<Option<NaiveDate>, CoreError> {
                    if value.trim().is_empty() {
                        return Ok(None);
                    }
                    NaiveDate::parse_from_str(value.trim(), "%Y-%m-%d")
                        .map(Some)
                        .map_err(|_| CoreError::InvalidData("日期请填写为 YYYY-MM-DD。".into()))
                };
                filter.from = parse(&self.from)?;
                filter.through = parse(&self.through)?;
                if filter
                    .from
                    .zip(filter.through)
                    .is_some_and(|(from, through)| from > through)
                {
                    return Err(CoreError::InvalidData("开始日期不能晚于结束日期。".into()));
                }
            }
            _ => {}
        }
        Ok(filter)
    }
}

fn duration(ms: f64) -> String {
    let seconds = (ms / 1000.0).round() as u64;
    if seconds >= 3600 {
        format!(
            "{}时{:02}分{:02}秒",
            seconds / 3600,
            seconds / 60 % 60,
            seconds % 60
        )
    } else if seconds >= 60 {
        format!("{}分{:02}秒", seconds / 60, seconds % 60)
    } else if ms > 0.0 && ms < 1000.0 {
        format!("{ms:.0}毫秒")
    } else {
        format!("{seconds}秒")
    }
}

fn value(totals: &ConsumptionTotals, measure: i32, average: bool) -> Option<f64> {
    let token_known = totals.requests == 0 || totals.usage_requests > 0;
    let cache_known = totals.requests == 0 || totals.cache_requests > 0;
    let raw = match measure {
        0 => token_known.then_some(totals.input_tokens as f64 + totals.output_tokens as f64),
        1 => token_known.then_some(totals.input_tokens as f64),
        2 => token_known.then_some(totals.output_tokens as f64),
        3 => cache_known.then_some(totals.cached_tokens as f64),
        4 => cache_known.then_some(totals.uncached_tokens() as f64),
        5 => return totals.cache_hit_rate(),
        6 => Some(totals.elapsed_ms as f64),
        7 => Some(totals.api_ms as f64),
        8 => Some(totals.waiting_ms as f64),
        9 => Some(
            totals
                .elapsed_ms
                .saturating_sub(totals.api_ms.saturating_add(totals.waiting_ms)) as f64,
        ),
        10 => Some(totals.requests as f64),
        11 => Some(totals.failed_requests as f64),
        _ => None,
    }?;
    Some(if average && totals.runs > 0 {
        raw / totals.runs as f64
    } else {
        raw
    })
}

fn display(totals: &ConsumptionTotals, measure: i32, average: bool) -> String {
    let Some(value) = value(totals, measure, average) else {
        return "未知".into();
    };
    let partial = match measure {
        0..=2 => totals.usage_requests < totals.requests,
        3..=4 => totals.cache_requests < totals.requests,
        _ => false,
    };
    let text = match measure {
        0..=4 => {
            let precise = if average {
                format!("{value:.1}")
            } else {
                format!("{value:.0}")
            };
            format!("{} Token（{precise}）", compact(value.round() as u64))
        }
        5 => format!("{:.1}%", value * 100.0),
        6..=9 => duration(value),
        _ if average => format!("{value:.1} 次"),
        _ => format!("{value:.0} 次"),
    };
    if partial { format!("≥ {text}") } else { text }
}

fn bars(
    groups: &[ConsumptionGroup],
    measure: i32,
    average: bool,
    offset: usize,
) -> Vec<MetricGroupRow> {
    let maximum = groups
        .iter()
        .filter_map(|group| value(&group.totals, measure, average))
        .fold(0.0, f64::max);
    groups
        .iter()
        .skip(offset)
        .take(PAGE_SIZE)
        .map(|group| MetricGroupRow {
            label: group.label.clone().into(),
            detail: format!(
                "{} 次运行 · {} 次 API · 用量 {}/{} · 缓存 {}/{}",
                group.totals.runs,
                group.totals.requests,
                group.totals.usage_requests,
                group.totals.requests,
                group.totals.cache_requests,
                group.totals.requests
            )
            .into(),
            value: display(&group.totals, measure, average).into(),
            fraction: value(&group.totals, measure, average)
                .filter(|_| maximum > 0.0)
                .map_or(0.0, |value| (value / maximum).clamp(0.0, 1.0) as f32),
        })
        .collect()
}

fn rows(
    records: &[ConsumptionRecord],
    filter: &ConsumptionFilter,
    offset: usize,
) -> Vec<ConsumptionRow> {
    records
        .iter()
        .filter(|record| filter.matches(record))
        .skip(offset)
        .take(PAGE_SIZE)
        .map(|record| {
            let (totals, _) = consumption_groups(
                std::slice::from_ref(record),
                &ConsumptionFilter::default(),
                ConsumptionDimension::Date,
            );
            ConsumptionRow {
                identity: format!(
                    "{}  ·  {}  ·  {}",
                    record
                        .started_at
                        .with_timezone(&Local)
                        .format("%Y-%m-%d %H:%M:%S"),
                    record.project_name,
                    task_label(&record.task)
                )
                .into(),
                configuration: format!(
                    "{}  ·  {}  ·  {}  ·  {}",
                    record.version_label(),
                    record.model_label(),
                    mode_label(&record.mode),
                    status_label(&record.status)
                )
                .into(),
                counters: format!(
                    "输入 {}  |  输出 {}  |  缓存 {}  |  {} 次 API  |  耗时 {}",
                    display(&totals, 1, false),
                    display(&totals, 2, false),
                    display(&totals, 3, false),
                    record.api.requests,
                    duration(record.elapsed_ms as f64)
                )
                .into(),
            }
        })
        .collect()
}

fn strings(values: impl IntoIterator<Item = String>) -> ModelRc<SharedString> {
    ModelRc::new(VecModel::from(
        values
            .into_iter()
            .map(SharedString::from)
            .collect::<Vec<_>>(),
    ))
}

pub async fn handle(ctx: &Context, action: &str, input: &UiInput) -> Result<bool, CoreError> {
    if action != "metrics" {
        return Ok(false);
    }
    let revision = input.view_revision;
    let generation = ctx.generation.clone();
    ctx.update(move |ui| {
        if generation.load(Ordering::SeqCst) == revision {
            ui.global::<App>().set_metrics_loading(true);
            ui.global::<App>().set_metrics_error("".into());
        }
    });
    let host = ctx.app().await;
    let result = match input.metrics.filter() {
        Ok(filter) => host
            .consumption_history()
            .await
            .map(|history| (filter, history)),
        Err(error) => Err(error),
    };
    let selection = input.metrics.clone();
    let generation = ctx.generation.clone();
    ctx.update(move |ui| {
        let app = ui.global::<App>();
        if app.get_page() != 5 || generation.load(Ordering::SeqCst) != revision {
            return;
        }
        app.set_metrics_loading(false);
        let (filter, history) = match result {
            Ok(result) => result,
            Err(error) => {
                app.set_metrics_error(error.to_string().into());
                app.set_metric_groups(ModelRc::default());
                app.set_consumption_rows(ModelRc::default());
                app.set_metrics_overview("".into());
                app.set_metrics_tokens("".into());
                app.set_metrics_timing("".into());
                app.set_metrics_coverage("".into());
                app.set_metrics_total(0);
                return;
            }
        };
        let mut versions = BTreeMap::new();
        let mut projects = BTreeMap::new();
        let mut models = std::collections::BTreeSet::new();
        for record in &history.records {
            versions.insert(record.version_key(), record.version_label());
            projects.insert(record.project_id.clone(), record.project_name.clone());
            models.insert(record.model_label().to_owned());
        }
        let index = |keys: &[String], selected: &str| {
            keys.iter().position(|key| key == selected).unwrap_or(0) as i32
        };
        let version_keys = std::iter::once(String::new())
            .chain(versions.keys().cloned())
            .collect::<Vec<_>>();
        let project_keys = std::iter::once(String::new())
            .chain(projects.keys().cloned())
            .collect::<Vec<_>>();
        let model_names = std::iter::once("全部模型".to_owned())
            .chain(models)
            .collect::<Vec<_>>();
        app.set_metrics_versions(strings(
            std::iter::once("全部版本".into()).chain(versions.into_values()),
        ));
        app.set_metrics_version_index(index(&version_keys, &selection.filter.version));
        app.set_metrics_version_keys(strings(version_keys));
        app.set_metrics_projects(strings(
            std::iter::once("全部游戏".into()).chain(projects.into_values()),
        ));
        app.set_metrics_project_index(index(&project_keys, &selection.filter.project));
        app.set_metrics_project_keys(strings(project_keys));
        app.set_metrics_model_index(index(&model_names, &selection.filter.model));
        app.set_metrics_models(strings(model_names));
        let dimension = match selection.dimension {
            1 => ConsumptionDimension::Version,
            2 => ConsumptionDimension::Model,
            3 => ConsumptionDimension::Task,
            4 => ConsumptionDimension::Project,
            5 => ConsumptionDimension::Mode,
            6 => ConsumptionDimension::Status,
            _ => ConsumptionDimension::Date,
        };
        let (totals, groups) = consumption_groups(&history.records, &filter, dimension);
        let count = if selection.tab == 0 {
            groups.len()
        } else {
            totals.runs
        };
        let offset = selection
            .offset
            .min(count.saturating_sub(1) / PAGE_SIZE * PAGE_SIZE);
        app.set_metrics_offset(offset as i32);
        app.set_metrics_total(count.min(i32::MAX as usize) as i32);
        app.set_metric_groups(ModelRc::new(VecModel::from(bars(
            &groups,
            selection.measure,
            selection.average,
            offset,
        ))));
        app.set_consumption_rows(ModelRc::new(VecModel::from(rows(
            &history.records,
            &filter,
            offset,
        ))));
        app.set_metrics_overview(
            format!(
                "{} 次运行  ·  {} 次 API 请求  ·  {} 次请求失败",
                totals.runs, totals.requests, totals.failed_requests
            )
            .into(),
        );
        app.set_metrics_tokens(
            format!(
                "输入 {}    输出 {}    缓存输入 {}",
                display(&totals, 1, false),
                display(&totals, 2, false),
                display(&totals, 3, false)
            )
            .into(),
        );
        app.set_metrics_timing(
            format!(
                "累计耗时 {}    API {}    等待 {}",
                duration(totals.elapsed_ms as f64),
                duration(totals.api_ms as f64),
                duration(totals.waiting_ms as f64)
            )
            .into(),
        );
        app.set_metrics_coverage(
            format!(
                "用量覆盖 {}/{} 次请求 · 缓存计数 {}/{} 次请求{}",
                totals.usage_requests,
                totals.requests,
                totals.cache_requests,
                totals.requests,
                if history.unreadable_lines > 0 {
                    format!(" · {} 行记录无法读取", history.unreadable_lines)
                } else {
                    String::new()
                }
            )
            .into(),
        );
        app.set_metrics_chart_title(
            format!(
                "{} · {}",
                MEASURES
                    .get(selection.measure.max(0) as usize)
                    .unwrap_or(&"总 Token"),
                if selection.measure == 5 {
                    "按有效输入加权"
                } else if selection.average {
                    "单次运行平均"
                } else {
                    "合计"
                }
            )
            .into(),
        );
        app.set_metrics_empty(
            if history.records.is_empty() {
                "暂无消耗记录。运行识别、适配或经验总结后，这里会显示指标。"
            } else {
                "没有符合筛选条件的记录。"
            }
            .into(),
        );
    });
    Ok(true)
}
