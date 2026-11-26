use std::{
    cmp::Ordering as CmpOrdering,
    collections::{BTreeMap, HashSet},
    sync::atomic::Ordering,
};

use chrono::{Duration, Local, NaiveDate};
use gamel10n_core::{
    CoreError,
    metrics::{
        ConsumptionDimension, ConsumptionFilter, ConsumptionGroup, ConsumptionRecord,
        ConsumptionTotals, consumption_groups, status_label, task_label,
    },
};
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};

use crate::{
    Context, UiInput,
    token_ui::{compact, exact},
    view::{App, ConsumptionRow, MetricGroupRow, MetricTick, TokenPoint, TokenSegment},
};

const LIST_PAGE_SIZE: usize = 20;
const TREND_PAGE_SIZE: usize = 30;
const TASKS: [&str; 5] = ["", "translation", "adaptation", "terminology", "review"];

#[derive(Clone, Default)]
pub struct MetricsInput {
    pub filter: ConsumptionFilter,
    date_range: i32,
    from: String,
    through: String,
    tab: i32,
    dimension: i32,
    measure: i32,
    chart_mode: i32,
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
                model: selected(app.get_metrics_model_keys(), app.get_metrics_model_index()),
                project: selected(
                    app.get_metrics_project_keys(),
                    app.get_metrics_project_index(),
                ),
                task: TASKS
                    .get(app.get_metrics_task_index().max(0) as usize)
                    .unwrap_or(&"")
                    .to_string(),
                only_with_api: true,
                ..Default::default()
            },
            date_range: app.get_metrics_date_range(),
            from: app.get_metrics_from().into(),
            through: app.get_metrics_through().into(),
            tab: app.get_metrics_tab(),
            dimension: app.get_metrics_dimension(),
            measure: app.get_metrics_measure(),
            chart_mode: app.get_token_chart_mode(),
            offset: app.get_metrics_offset().max(0) as usize,
        }
    }

    fn filter(&self) -> Result<ConsumptionFilter, CoreError> {
        let mut filter = self.filter.clone();
        filter.only_with_api = true;
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

fn usage_known(totals: &ConsumptionTotals) -> bool {
    totals.requests == 0 || totals.usage_requests > 0
}

fn usage_partial(totals: &ConsumptionTotals) -> bool {
    totals.usage_requests < totals.requests
}

fn has_cache_stats(totals: &ConsumptionTotals) -> bool {
    totals.cache_requests > 0 && totals.cache_input_tokens > 0
}

fn cache_partial(totals: &ConsumptionTotals) -> bool {
    totals.cache_requests < totals.requests
}

fn format_tokens(amount: u64, partial: bool) -> String {
    if partial {
        format!("≥ {}", compact(amount))
    } else {
        compact(amount)
    }
}

fn format_exact_tokens(amount: u64, partial: bool) -> String {
    if partial {
        format!("≥ {} Token", exact(amount))
    } else {
        format!("{} Token", exact(amount))
    }
}

fn token_breakdown_line(totals: &ConsumptionTotals) -> String {
    if !usage_known(totals) {
        return "服务商未返回 Token 用量".into();
    }
    let partial = usage_partial(totals);
    if has_cache_stats(totals) {
        let rate = totals
            .cache_hit_rate()
            .map(|r| format!("（命中率 {:.1}%）", r * 100.0))
            .unwrap_or_default();
        format!(
            "未缓存 {} · 缓存 {}{} · 输出 {}",
            format_tokens(totals.non_cached_input_tokens(), partial),
            format_tokens(totals.cached_tokens, cache_partial(totals)),
            rate,
            format_tokens(totals.output_tokens, partial),
        )
    } else {
        format!(
            "输入 {} · 输出 {}",
            format_tokens(totals.input_tokens, partial),
            format_tokens(totals.output_tokens, partial),
        )
    }
}

fn exact_breakdown_tooltip(title: &str, totals: &ConsumptionTotals) -> String {
    let mut lines = Vec::new();
    if !title.is_empty() {
        lines.push(title.to_owned());
    }
    if usage_known(totals) {
        let partial = usage_partial(totals);
        lines.push(format!(
            "总消耗：{}",
            format_exact_tokens(totals.total_tokens(), partial)
        ));
        lines.push(format!(
            "总输入：{}",
            format_exact_tokens(totals.input_tokens, partial)
        ));
        if has_cache_stats(totals) {
            lines.push(format!(
                "未缓存输入：{}",
                format_exact_tokens(totals.non_cached_input_tokens(), partial)
            ));
            let hit = totals
                .cache_hit_rate()
                .map(|r| format!("（命中率 {:.1}%）", r * 100.0))
                .unwrap_or_default();
            lines.push(format!(
                "缓存输入：{}{}",
                format_exact_tokens(totals.cached_tokens, cache_partial(totals)),
                hit
            ));
        }
        lines.push(format!(
            "输出：{}",
            format_exact_tokens(totals.output_tokens, partial)
        ));
    } else {
        lines.push("Token 用量：未知".into());
    }
    let failed = if totals.failed_requests > 0 {
        format!(" · 失败 {} 次", totals.failed_requests)
    } else {
        String::new()
    };
    lines.push(format!(
        "{} 次任务 · {} 次请求{} · 耗时 {}",
        totals.runs,
        totals.requests,
        failed,
        duration(totals.elapsed_ms as f64)
    ));
    lines.join("\n")
}

struct SummaryDisplay {
    total_value: String,
    total_sub: String,
    total_exact: String,
    uncached_value: String,
    uncached_sub: String,
    uncached_exact: String,
    cached_value: String,
    cached_sub: String,
    cached_exact: String,
    output_value: String,
    output_sub: String,
    output_exact: String,
}

fn build_summary(totals: &ConsumptionTotals) -> SummaryDisplay {
    if totals.runs == 0 {
        return SummaryDisplay {
            total_value: "—".into(),
            total_sub: "暂无调用记录".into(),
            total_exact: String::new(),
            uncached_value: "—".into(),
            uncached_sub: String::new(),
            uncached_exact: String::new(),
            cached_value: "—".into(),
            cached_sub: String::new(),
            cached_exact: String::new(),
            output_value: "—".into(),
            output_sub: String::new(),
            output_exact: String::new(),
        };
    }

    let known = usage_known(totals);
    let partial = usage_partial(totals);
    let total_value = if known {
        format_tokens(totals.total_tokens(), partial)
    } else {
        "未知".into()
    };
    let total_sub = if known {
        format!(
            "输入 {} · 输出 {}",
            format_tokens(totals.input_tokens, partial),
            format_tokens(totals.output_tokens, partial)
        )
    } else {
        "服务商未返回用量".into()
    };
    let total_exact = exact_breakdown_tooltip("", totals);

    let uncached = totals.non_cached_input_tokens();
    let uncached_value = if known {
        format_tokens(uncached, partial)
    } else {
        "未知".into()
    };
    let uncached_sub = if !known {
        String::new()
    } else if totals.cache_requests == 0 {
        "服务商未返回缓存计数".into()
    } else if totals.input_tokens > 0 {
        let ratio = uncached as f64 * 100.0 / totals.input_tokens as f64;
        if cache_partial(totals) {
            format!("占总输入 {ratio:.1}% · 部分无缓存计数")
        } else {
            format!("占总输入 {ratio:.1}%")
        }
    } else {
        "占总输入 0.0%".into()
    };
    let uncached_exact = if known {
        let mut lines = vec![format!(
            "未缓存输入：{}",
            format_exact_tokens(uncached, partial)
        )];
        if totals.cache_requests > 0 && cache_partial(totals) {
            lines.push(format!(
                "已确认未缓存（有缓存计数请求）：{} Token",
                exact(totals.uncached_tokens())
            ));
        }
        lines.join("\n")
    } else {
        String::new()
    };

    let (cached_value, cached_sub, cached_exact) = if has_cache_stats(totals) {
        let rate = totals
            .cache_hit_rate()
            .map(|r| format!("命中率 {:.1}%", r * 100.0))
            .unwrap_or_else(|| "命中率 0.0%".into());
        let sub = if cache_partial(totals) {
            format!(
                "{rate}（{}/{} 次请求）",
                totals.cache_requests, totals.requests
            )
        } else {
            rate.clone()
        };
        let exact_tip = format!(
            "缓存输入：{}\n{}\n统计覆盖：{}/{} 次请求",
            format_exact_tokens(totals.cached_tokens, cache_partial(totals)),
            rate,
            totals.cache_requests,
            totals.requests
        );
        (
            format_tokens(totals.cached_tokens, cache_partial(totals)),
            sub,
            exact_tip,
        )
    } else {
        (
            "—".into(),
            if totals.cache_requests == 0 {
                "服务商未返回缓存计数".into()
            } else {
                "命中率 0.0%".into()
            },
            String::new(),
        )
    };

    let output_value = if known {
        format_tokens(totals.output_tokens, partial)
    } else {
        "未知".into()
    };
    let failed = if totals.failed_requests > 0 {
        format!(" · {} 失败", totals.failed_requests)
    } else {
        String::new()
    };
    let output_sub = format!(
        "{} 次任务 · {} 次请求{} · {}",
        totals.runs,
        totals.requests,
        failed,
        duration(totals.elapsed_ms as f64)
    );
    let output_exact = if known {
        format!(
            "输出：{}\n累计耗时：{}（API {} · 等待 {}）",
            format_exact_tokens(totals.output_tokens, partial),
            duration(totals.elapsed_ms as f64),
            duration(totals.api_ms as f64),
            duration(totals.waiting_ms as f64),
        )
    } else {
        format!(
            "累计耗时：{}（API {} · 等待 {}）",
            duration(totals.elapsed_ms as f64),
            duration(totals.api_ms as f64),
            duration(totals.waiting_ms as f64),
        )
    };

    SummaryDisplay {
        total_value,
        total_sub,
        total_exact,
        uncached_value,
        uncached_sub,
        uncached_exact,
        cached_value,
        cached_sub,
        cached_exact,
        output_value,
        output_sub,
        output_exact,
    }
}

fn measure_value(totals: &ConsumptionTotals, measure: i32) -> Option<f64> {
    match measure {
        0 => usage_known(totals).then_some(totals.total_tokens() as f64),
        1 => usage_known(totals).then_some(totals.non_cached_input_tokens() as f64),
        2 => usage_known(totals).then_some(totals.output_tokens as f64),
        3 => totals.cache_hit_rate(),
        4 => Some(totals.elapsed_ms as f64),
        _ => None,
    }
}

fn measure_display(totals: &ConsumptionTotals, measure: i32) -> String {
    let Some(value) = measure_value(totals, measure) else {
        return "未知".into();
    };
    match measure {
        0..=2 => format!(
            "{} Token",
            format_tokens(value.round() as u64, usage_partial(totals))
        ),
        3 => format!("{:.1}%", value * 100.0),
        4 => duration(value),
        _ => String::new(),
    }
}

fn rank_groups(groups: &mut [ConsumptionGroup], measure: i32) {
    groups.sort_by(|left, right| {
        let lv = measure_value(&left.totals, measure);
        let rv = measure_value(&right.totals, measure);
        match (rv, lv) {
            (Some(r), Some(l)) => r.partial_cmp(&l).unwrap_or(CmpOrdering::Equal),
            (Some(_), None) => CmpOrdering::Greater,
            (None, Some(_)) => CmpOrdering::Less,
            (None, None) => CmpOrdering::Equal,
        }
        .then_with(|| right.totals.total_tokens().cmp(&left.totals.total_tokens()))
        .then_with(|| left.label.cmp(&right.label))
    });
}

fn stacked_fractions(totals: &ConsumptionTotals) -> (f32, f32, f32) {
    let total = totals.total_tokens() as f64;
    if total <= 0.0 || !usage_known(totals) {
        return (0.0, 0.0, 0.0);
    }
    let uncached = (totals.non_cached_input_tokens() as f64 / total).clamp(0.0, 1.0);
    let cached = (totals.cached_tokens as f64 / total).clamp(0.0, 1.0 - uncached);
    let output = (1.0 - uncached - cached).max(0.0);
    (uncached as f32, cached as f32, output as f32)
}

fn breakdown_rows(
    groups: &[ConsumptionGroup],
    grand_totals: &ConsumptionTotals,
    measure: i32,
    dimension: ConsumptionDimension,
    openable_projects: &HashSet<String>,
    offset: usize,
) -> Vec<MetricGroupRow> {
    let maximum = groups
        .iter()
        .filter_map(|group| measure_value(&group.totals, measure))
        .fold(0.0, f64::max);
    let grand = if measure == 3 {
        None
    } else {
        measure_value(grand_totals, measure).filter(|&v| v > 0.0)
    };
    let is_project = matches!(dimension, ConsumptionDimension::Project);

    groups
        .iter()
        .skip(offset)
        .take(LIST_PAGE_SIZE)
        .map(|group| {
            let current = measure_value(&group.totals, measure);
            let fraction = current
                .filter(|_| maximum > 0.0)
                .map_or(0.0, |v| (v / maximum).clamp(0.0, 1.0) as f32);
            let share = current
                .zip(grand)
                .map(|(v, g)| format!("{:.1}%", (v * 100.0 / g).clamp(0.0, 100.0)))
                .unwrap_or_default();
            let (uncached_fraction, cached_fraction, output_fraction) =
                stacked_fractions(&group.totals);
            let detail = format!(
                "{} · {} 次任务 · {}",
                token_breakdown_line(&group.totals),
                group.totals.runs,
                duration(group.totals.elapsed_ms as f64),
            );
            let can_open = is_project && openable_projects.contains(&group.key);
            MetricGroupRow {
                label: group.label.clone().into(),
                detail: detail.into(),
                value: measure_display(&group.totals, measure).into(),
                share: share.into(),
                exact: exact_breakdown_tooltip(&group.label, &group.totals).into(),
                fraction,
                uncached_fraction,
                cached_fraction,
                output_fraction,
                project_id: if can_open {
                    group.key.clone().into()
                } else {
                    "".into()
                },
                can_open,
            }
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

fn daily_trend_chart(groups: &[ConsumptionGroup], mode: i32, offset: usize) -> TokenChartData {
    // groups is chronologically ordered (oldest -> newest). offset=0 shows the latest window.
    let end = groups.len().saturating_sub(offset);
    let start = end.saturating_sub(TREND_PAGE_SIZE);
    let visible = &groups[start..end];
    if visible.is_empty() {
        return TokenChartData::default();
    }

    let series_values = |totals: &ConsumptionTotals| -> [Option<f64>; 3] {
        let known = usage_known(totals);
        match mode {
            1 => [known.then_some(totals.total_tokens() as f64), None, None],
            2 => [known.then_some(totals.output_tokens as f64), None, None],
            _ => [
                known.then_some(totals.input_tokens as f64),
                known.then_some(totals.non_cached_input_tokens() as f64),
                has_cache_stats(totals).then_some(totals.cached_tokens as f64),
            ],
        }
    };

    let maximum = visible
        .iter()
        .flat_map(|group| series_values(&group.totals))
        .flatten()
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

    let count = visible.len();
    let tick_count = count.min(6);
    let x_ticks: Vec<usize> = (0..tick_count)
        .map(|index| {
            if tick_count <= 1 {
                0
            } else {
                (index * (count - 1) + (tick_count - 1) / 2) / (tick_count - 1)
            }
        })
        .collect();

    let mut chart = TokenChartData::default();
    for (index, group) in visible.iter().enumerate() {
        let values = series_values(&group.totals);
        let normalized =
            values.map(|val| val.map_or(1.0, |v| (1.0 - v / top).clamp(0.0, 1.0) as f32));
        chart.known |= values.iter().any(Option::is_some);

        let axis_label = if x_ticks.contains(&index) {
            group.label.get(5..).unwrap_or(&group.label).to_owned()
        } else {
            String::new()
        };
        let known = usage_known(&group.totals);
        let partial = usage_partial(&group.totals);
        let total = if known {
            format!(
                "{} Token",
                format_tokens(group.totals.total_tokens(), partial)
            )
        } else {
            "未知".into()
        };
        let uncached = if known {
            format_tokens(group.totals.non_cached_input_tokens(), partial)
        } else {
            "未知".into()
        };
        let cached = if has_cache_stats(&group.totals) {
            let hit = group
                .totals
                .cache_hit_rate()
                .map(|r| format!("（{:.1}%）", r * 100.0))
                .unwrap_or_default();
            format!(
                "{}{}",
                format_tokens(group.totals.cached_tokens, cache_partial(&group.totals)),
                hit
            )
        } else {
            "—".into()
        };
        let output = if known {
            format_tokens(group.totals.output_tokens, partial)
        } else {
            "未知".into()
        };
        let detail = format!(
            "{} 次任务 · {} 次请求 · {}",
            group.totals.runs,
            group.totals.requests,
            duration(group.totals.elapsed_ms as f64)
        );
        chart.points.push(TokenPoint {
            label: group.label.clone().into(),
            axis_label: axis_label.into(),
            detail: detail.into(),
            total: total.into(),
            uncached: uncached.into(),
            cached: cached.into(),
            output: output.into(),
            x: if count <= 1 {
                0.5
            } else {
                index as f32 / (count - 1) as f32
            },
            primary_y: normalized[0],
            secondary_y: normalized[1],
            tertiary_y: normalized[2],
            primary_known: values[0].is_some(),
            secondary_known: values[1].is_some(),
            tertiary_known: values[2].is_some(),
            primary_partial: partial,
            secondary_partial: partial,
            tertiary_partial: cache_partial(&group.totals),
        });
    }

    for pair in chart.points.windows(2) {
        let (left, right) = (&pair[0], &pair[1]);
        for (series, known, y1, y2) in [
            (
                0,
                left.primary_known && right.primary_known,
                left.primary_y,
                right.primary_y,
            ),
            (
                1,
                left.secondary_known && right.secondary_known,
                left.secondary_y,
                right.secondary_y,
            ),
            (
                2,
                left.tertiary_known && right.tertiary_known,
                left.tertiary_y,
                right.tertiary_y,
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
                        format!("{amount:.1}").into()
                    } else {
                        compact(amount.round() as u64).into()
                    },
                }
            })
            .collect();
    }
    chart
}

fn task_tone(task: &str) -> i32 {
    match task {
        "translate" | "preview-translate" | "retranslate" | "candidate" => 1,
        "adapt" | "repair" | "expand" | "recognize" => 2,
        "terminology" => 3,
        _ => 0,
    }
}

fn status_badge(status: &str) -> (&str, i32) {
    match status {
        "completed" => ("", 0),
        "running" => (status_label(status), 1),
        "failed" => (status_label(status), 5),
        "paused" | "interrupted" | "unsupported" => (status_label(status), 4),
        _ => (status_label(status), 0),
    }
}

fn record_rows(
    records: &[ConsumptionRecord],
    filter: &ConsumptionFilter,
    openable_projects: &HashSet<String>,
    offset: usize,
) -> Vec<ConsumptionRow> {
    records
        .iter()
        .filter(|record| filter.matches(record))
        .skip(offset)
        .take(LIST_PAGE_SIZE)
        .map(|record| {
            let (totals, _) = consumption_groups(
                std::slice::from_ref(record),
                &ConsumptionFilter::default(),
                ConsumptionDimension::Date,
            );
            let (status_text, status_tone) = status_badge(&record.status);
            let failed = if record.api.failed_requests > 0 {
                format!(" · {} 失败", record.api.failed_requests)
            } else {
                String::new()
            };
            let configuration = format!(
                "{}  ·  {} 次请求{}  ·  {}",
                record.model_label(),
                record.api.requests,
                failed,
                duration(record.elapsed_ms as f64)
            );
            let can_open = openable_projects.contains(&record.project_id);
            ConsumptionRow {
                timestamp: record
                    .started_at
                    .with_timezone(&Local)
                    .format("%Y-%m-%d %H:%M")
                    .to_string()
                    .into(),
                project_name: record.project_name.clone().into(),
                project_id: if can_open {
                    record.project_id.clone().into()
                } else {
                    "".into()
                },
                can_open,
                task_label: task_label(&record.task).into(),
                task_tone: task_tone(&record.task),
                status_label: status_text.into(),
                status_tone,
                configuration: configuration.into(),
                tokens_summary: measure_display(&totals, 0).into(),
                tokens_detail: token_breakdown_line(&totals).into(),
                tokens_exact: exact_breakdown_tooltip(&record.project_name, &totals).into(),
            }
        })
        .collect()
}

fn build_notice(totals: &ConsumptionTotals, unreadable_lines: usize) -> String {
    let mut parts = Vec::new();
    if totals.usage_requests < totals.requests {
        parts.push(format!(
            "部分请求未返回 Token 用量（已统计 {}/{} 次），带 ≥ 的数值为已知下限",
            totals.usage_requests, totals.requests
        ));
    }
    if unreadable_lines > 0 {
        parts.push(format!("有 {unreadable_lines} 行历史记录无法读取"));
    }
    parts.join("；")
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
    let openable_projects: HashSet<String> = host
        .list_projects("")
        .await
        .into_iter()
        .map(|project| project.id)
        .collect();
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
                app.set_metrics_notice("".into());
                app.set_metrics_total(0);
                return;
            }
        };

        let api_filter = ConsumptionFilter {
            only_with_api: true,
            ..Default::default()
        };
        let mut projects: BTreeMap<String, String> = BTreeMap::new();
        let mut models: BTreeMap<String, String> = BTreeMap::new();
        let mut any_api_record = false;
        for record in history.records.iter().filter(|r| api_filter.matches(r)) {
            any_api_record = true;
            projects.insert(record.project_id.clone(), record.project_name.clone());
            if record.model.is_some() {
                models.insert(record.model_key(), record.model_label());
            }
        }
        let mut sorted_projects: Vec<(String, String)> = projects.into_iter().collect();
        sorted_projects.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
        let mut sorted_models: Vec<(String, String)> = models.into_iter().collect();
        sorted_models.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));

        let index = |keys: &[String], selected: &str| {
            keys.iter().position(|key| key == selected).unwrap_or(0) as i32
        };
        let project_keys: Vec<String> = std::iter::once(String::new())
            .chain(sorted_projects.iter().map(|(id, _)| id.clone()))
            .collect();
        let model_keys: Vec<String> = std::iter::once(String::new())
            .chain(sorted_models.iter().map(|(key, _)| key.clone()))
            .collect();

        app.set_metrics_projects(strings(
            std::iter::once("全部游戏".into()).chain(sorted_projects.into_iter().map(|(_, name)| name)),
        ));
        app.set_metrics_project_index(index(&project_keys, &selection.filter.project));
        app.set_metrics_project_keys(strings(project_keys));

        app.set_metrics_models(strings(
            std::iter::once("全部模型".into()).chain(sorted_models.into_iter().map(|(_, label)| label)),
        ));
        app.set_metrics_model_index(index(&model_keys, &selection.filter.model));
        app.set_metrics_model_keys(strings(model_keys));

        let breakdown_dimension = match selection.dimension {
            1 => ConsumptionDimension::Model,
            2 => ConsumptionDimension::Task,
            _ => ConsumptionDimension::Project,
        };
        let (totals, mut breakdown_groups) =
            consumption_groups(&history.records, &filter, breakdown_dimension);
        rank_groups(&mut breakdown_groups, selection.measure);
        let (_, date_groups) =
            consumption_groups(&history.records, &filter, ConsumptionDimension::Date);

        let page_size = if selection.tab == 1 {
            TREND_PAGE_SIZE
        } else {
            LIST_PAGE_SIZE
        };
        let count = match selection.tab {
            0 => breakdown_groups.len(),
            1 => date_groups.len(),
            _ => totals.runs,
        };
        let offset = selection
            .offset
            .min(count.saturating_sub(1) / page_size * page_size);
        app.set_metrics_page_size(page_size as i32);
        app.set_metrics_offset(offset as i32);
        app.set_metrics_total(count.min(i32::MAX as usize) as i32);

        app.set_metric_groups(ModelRc::new(VecModel::from(breakdown_rows(
            &breakdown_groups,
            &totals,
            selection.measure,
            breakdown_dimension,
            &openable_projects,
            offset,
        ))));
        let chart = daily_trend_chart(&date_groups, selection.chart_mode, offset);
        app.set_token_chart_known(chart.known);
        app.set_token_points(ModelRc::new(VecModel::from(chart.points)));
        app.set_token_segments(ModelRc::new(VecModel::from(chart.segments)));
        app.set_token_ticks(ModelRc::new(VecModel::from(chart.ticks)));
        app.set_consumption_rows(ModelRc::new(VecModel::from(record_rows(
            &history.records,
            &filter,
            &openable_projects,
            offset,
        ))));

        let summary = build_summary(&totals);
        app.set_metrics_total_value(summary.total_value.into());
        app.set_metrics_total_sub(summary.total_sub.into());
        app.set_metrics_total_exact(summary.total_exact.into());
        app.set_metrics_uncached_value(summary.uncached_value.into());
        app.set_metrics_uncached_sub(summary.uncached_sub.into());
        app.set_metrics_uncached_exact(summary.uncached_exact.into());
        app.set_metrics_cached_value(summary.cached_value.into());
        app.set_metrics_cached_sub(summary.cached_sub.into());
        app.set_metrics_cached_exact(summary.cached_exact.into());
        app.set_metrics_output_value(summary.output_value.into());
        app.set_metrics_output_sub(summary.output_sub.into());
        app.set_metrics_output_exact(summary.output_exact.into());
        app.set_metrics_notice(build_notice(&totals, history.unreadable_lines).into());
        app.set_metrics_empty(
            if !any_api_record {
                "暂无模型消耗记录。运行文本翻译、术语提取、智能适配或经验总结后，这里会汇总 Token 用量。"
            } else {
                "没有符合当前筛选条件的消耗记录。"
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
                elapsed_ms: 10_000,
                ..Default::default()
            },
        }
    }

    #[test]
    fn ranking_sorts_descending_by_selected_measure_and_computes_stacked_fractions() {
        let mut groups = vec![
            group("small", 100, 50, Some(20), 1),
            group("big", 1000, 100, Some(700), 2),
        ];
        rank_groups(&mut groups, 0);
        assert_eq!(groups[0].label, "big");
        assert_eq!(groups[1].label, "small");

        let (uncached, cached, output) = stacked_fractions(&groups[0].totals);
        assert!((uncached - 300.0 / 1100.0).abs() < 1e-5);
        assert!((cached - 700.0 / 1100.0).abs() < 1e-5);
        assert!((output - 100.0 / 1100.0).abs() < 1e-5);
        assert!((uncached + cached + output - 1.0).abs() < 1e-5);
    }

    #[test]
    fn daily_trend_output_mode_uses_dedicated_y_scale_for_output_tokens() {
        let groups = [
            group("2026-11-10", 2_000_000, 10_000, Some(1_600_000), 2),
            group("2026-11-11", 1_000_000, 20_000, Some(800_000), 1),
        ];
        let output_chart = daily_trend_chart(&groups, 2, 0);
        assert!(output_chart.known);
        assert_eq!(output_chart.points.len(), 2);
        assert_eq!(output_chart.segments.len(), 1);
        // Peak output (20K) should sit at top of the dedicated scale (y == 0.0).
        assert_eq!(output_chart.points[1].primary_y, 0.0);
        assert!((output_chart.points[0].primary_y - 0.5).abs() < 1e-5);
        assert!(!output_chart.points[0].secondary_known);
        assert!(!output_chart.points[0].tertiary_known);
    }

    #[test]
    fn daily_trend_input_cache_mode_breaks_unknown_points_and_keeps_partial_lower_bounds() {
        let mut partial = group("2026-11-10", 1000, 20, Some(800), 2);
        partial.totals.usage_requests = 1;
        partial.totals.cache_requests = 1;
        let mut unknown = group("2026-11-11", 0, 0, None, 1);
        unknown.totals.usage_requests = 0;
        let chart = daily_trend_chart(
            &[partial, unknown, group("2026-11-12", 2000, 25, None, 1)],
            0,
            0,
        );
        assert!(chart.known);
        assert!(chart.segments.is_empty());
        assert!(chart.points[0].primary_partial && chart.points[0].tertiary_partial);
        assert!(chart.points[0].total.starts_with("≥"));
        assert!(!chart.points[1].primary_known && !chart.points[1].secondary_known);
        assert_eq!(chart.points[1].total.as_str(), "未知");
        assert!(chart.points[2].primary_known && !chart.points[2].tertiary_known);
    }

    #[test]
    fn summary_hides_notice_when_complete_and_surfaces_uncached_and_hit_rate() {
        let totals = group("g", 1000, 200, Some(750), 2).totals;
        let summary = build_summary(&totals);
        assert_eq!(summary.total_value, "1.20K");
        assert_eq!(summary.uncached_value, "250");
        assert_eq!(summary.cached_value, "750");
        assert!(summary.cached_sub.contains("75.0%"));
        assert_eq!(summary.output_value, "200");
        assert!(build_notice(&totals, 0).is_empty());
    }
}
