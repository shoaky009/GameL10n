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
    view::{App, ConsumptionRow, MetricGroupRow, MetricTick, TokenPoint, TokenSegment},
};

const PAGE_SIZE: usize = 20;
const MEASURE_CHOICES: [i32; 9] = [0, 4, 5, 6, 7, 8, 9, 10, 11];
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
    "Token 用量",
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
                model: selected(app.get_metrics_model_keys(), app.get_metrics_model_index()),
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
            measure: *MEASURE_CHOICES
                .get(app.get_metrics_measure().max(0) as usize)
                .unwrap_or(&0),
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

#[derive(Default)]
struct TokenChartData {
    points: Vec<TokenPoint>,
    segments: Vec<TokenSegment>,
    ticks: Vec<MetricTick>,
    known: bool,
}

fn token_chart(
    groups: &[ConsumptionGroup],
    average: bool,
    offset: usize,
    dates: bool,
) -> TokenChartData {
    let maximum = groups
        .iter()
        .flat_map(|group| {
            [1, 2, 3]
                .into_iter()
                .filter_map(|measure| value(&group.totals, measure, average))
        })
        .fold(0.0, f64::max);
    let magnitude = if maximum > 0.0 {
        10_f64.powf(maximum.log10().floor())
    } else {
        1.0
    };
    let top = [1.0, 2.0, 5.0, 10.0]
        .into_iter()
        .find(|step| step * magnitude >= maximum)
        .unwrap_or(10.0)
        * magnitude;
    let visible = groups
        .iter()
        .skip(offset)
        .take(PAGE_SIZE)
        .collect::<Vec<_>>();
    let count = visible.len();
    let tick_count = count.min(6);
    let x_ticks = (0..tick_count)
        .map(|index| {
            if tick_count <= 1 {
                0
            } else {
                (index * (count - 1) + (tick_count - 1) / 2) / (tick_count - 1)
            }
        })
        .collect::<Vec<_>>();
    let mut chart = TokenChartData::default();
    for (index, group) in visible.into_iter().enumerate() {
        let values = [1, 2, 3].map(|measure| value(&group.totals, measure, average));
        let normalized = values
            .map(|value| value.map_or(1.0, |value| (1.0 - value / top).clamp(0.0, 1.0) as f32));
        chart.known |= values.iter().any(Option::is_some);
        let label = if dates {
            group.label.get(5..).unwrap_or(&group.label)
        } else {
            &group.label
        };
        let mut axis_label = label.chars().take(12).collect::<String>();
        if label.chars().count() > 12 {
            axis_label.push('…');
        }
        chart.points.push(TokenPoint {
            label: group.label.clone().into(),
            axis_label: if x_ticks.contains(&index) {
                axis_label.into()
            } else {
                "".into()
            },
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
            input: display(&group.totals, 1, average).into(),
            output: display(&group.totals, 2, average).into(),
            cached: display(&group.totals, 3, average).into(),
            x: if count <= 1 {
                0.5
            } else {
                index as f32 / (count - 1) as f32
            },
            input_y: normalized[0],
            output_y: normalized[1],
            cached_y: normalized[2],
            input_known: values[0].is_some(),
            output_known: values[1].is_some(),
            cached_known: values[2].is_some(),
            usage_partial: group.totals.usage_requests < group.totals.requests,
            cache_partial: group.totals.cache_requests < group.totals.requests,
        });
    }
    for pair in chart.points.windows(2) {
        let (left, right) = (&pair[0], &pair[1]);
        for (series, known, y1, y2) in [
            (
                0,
                left.input_known && right.input_known,
                left.input_y,
                right.input_y,
            ),
            (
                1,
                left.output_known && right.output_known,
                left.output_y,
                right.output_y,
            ),
            (
                2,
                left.cached_known && right.cached_known,
                left.cached_y,
                right.cached_y,
            ),
        ] {
            if known {
                chart.segments.push(TokenSegment {
                    x1: left.x,
                    y1,
                    x2: right.x,
                    y2,
                    series,
                });
            }
        }
    }
    if chart.known {
        chart.ticks = (0..=4)
            .map(|index| {
                let fraction = index as f32 / 4.0;
                let amount = top * (1.0 - f64::from(fraction));
                MetricTick {
                    fraction,
                    label: if amount > 0.0 && amount < 10.0 {
                        format!("{amount:.2}").into()
                    } else {
                        compact(amount.round() as u64).into()
                    },
                }
            })
            .collect();
    }
    chart
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
                app.set_token_points(ModelRc::default());
                app.set_token_segments(ModelRc::default());
                app.set_token_ticks(ModelRc::default());
                app.set_token_chart_known(false);
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
        let mut models = BTreeMap::new();
        for record in &history.records {
            versions.insert(record.version_key(), record.version_label());
            projects.insert(record.project_id.clone(), record.project_name.clone());
            models.insert(record.model_key(), record.model_label());
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
        let model_keys = std::iter::once(String::new())
            .chain(models.keys().cloned())
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
        app.set_metrics_model_index(index(&model_keys, &selection.filter.model));
        app.set_metrics_model_keys(strings(model_keys));
        app.set_metrics_models(strings(
            std::iter::once("全部模型配置".into()).chain(models.into_values()),
        ));
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
        let chart = token_chart(&groups, selection.average, offset, selection.dimension == 0);
        app.set_token_chart_known(chart.known);
        app.set_token_points(ModelRc::new(VecModel::from(chart.points)));
        app.set_token_segments(ModelRc::new(VecModel::from(chart.segments)));
        app.set_token_ticks(ModelRc::new(VecModel::from(chart.ticks)));
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

#[cfg(test)]
mod tests {
    use super::*;

    fn group(
        label: &str,
        input: u64,
        output: u64,
        cached: Option<u64>,
        runs: usize,
    ) -> ConsumptionGroup {
        ConsumptionGroup {
            key: label.into(),
            label: label.into(),
            totals: ConsumptionTotals {
                runs,
                requests: runs as u64,
                usage_requests: runs as u64,
                cache_requests: if cached.is_some() { runs as u64 } else { 0 },
                input_tokens: input,
                output_tokens: output,
                cached_tokens: cached.unwrap_or(0),
                cache_input_tokens: if cached.is_some() { input } else { 0 },
                ..Default::default()
            },
        }
    }

    #[test]
    fn token_lines_share_a_scale_and_apply_the_same_run_average_to_each_series() {
        let groups = [
            group("a", 1000, 100, Some(800), 2),
            group("b", 500, 50, Some(400), 1),
        ];
        let chart = token_chart(&groups, true, 0, false);
        assert_eq!(chart.points.len(), 2);
        assert_eq!(chart.segments.len(), 3);
        assert!(
            chart
                .segments
                .iter()
                .all(|segment| segment.y1 == segment.y2)
        );
        assert!(
            chart.points[0].input_y < chart.points[0].cached_y
                && chart.points[0].cached_y < chart.points[0].output_y
        );
        assert!(chart.points[0].input.contains("500.0"));
        assert_eq!(chart.points[0].x, 0.0);
        assert_eq!(chart.points[1].x, 1.0);
    }

    #[test]
    fn unknown_usage_breaks_lines_and_partial_usage_keeps_its_lower_bound() {
        let mut partial = group("partial", 100, 20, Some(80), 2);
        partial.totals.usage_requests = 1;
        partial.totals.cache_requests = 1;
        let mut unknown = group("unknown", 0, 0, None, 1);
        unknown.totals.usage_requests = 0;
        let chart = token_chart(
            &[partial, unknown, group("known", 200, 25, None, 1)],
            false,
            0,
            false,
        );
        assert!(chart.known);
        assert!(chart.segments.is_empty());
        assert!(chart.points[0].usage_partial && chart.points[0].cache_partial);
        assert!(chart.points[0].input.starts_with("≥"));
        assert!(
            !chart.points[1].input_known
                && !chart.points[1].output_known
                && !chart.points[1].cached_known
        );
        assert_eq!(chart.points[1].input.as_str(), "未知");
        assert!(chart.points[2].input_known && !chart.points[2].cached_known);
    }

    #[test]
    fn a_single_point_is_centered_and_empty_charts_do_not_invent_usage() {
        let chart = token_chart(&[group("single", 100, 20, Some(80), 1)], false, 0, false);
        assert_eq!(chart.points[0].x, 0.5);
        assert!(chart.segments.is_empty());
        assert_eq!(chart.ticks.len(), 5);
        let chart = token_chart(&[], false, 0, false);
        assert!(chart.points.is_empty() && chart.ticks.is_empty() && !chart.known);
    }
}
