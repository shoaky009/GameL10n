use crate::view::TokenDisplay;

const THOUSAND: u64 = 1_000;
const MILLION: u128 = 1_000_000;
const MILLION_PROMOTION_THRESHOLD: u64 = 999_995;
const COMPACT_DECIMAL_SCALE: u128 = 100;
const DIGIT_GROUP_LEN: usize = 3;

pub(crate) fn compact(value: u64) -> String {
    if value < THOUSAND {
        return value.to_string();
    }
    // Promote when rounding would otherwise produce 1000.00K.
    let (unit, suffix) = if value >= MILLION_PROMOTION_THRESHOLD {
        (MILLION, "M")
    } else {
        (u128::from(THOUSAND), "K")
    };
    let hundredths = (u128::from(value) * COMPACT_DECIMAL_SCALE + unit / 2) / unit;
    format!(
        "{}.{:02}{suffix}",
        hundredths / COMPACT_DECIMAL_SCALE,
        hundredths % COMPACT_DECIMAL_SCALE
    )
}

pub(crate) fn exact(value: u64) -> String {
    let digits = value.to_string();
    let mut result = String::with_capacity(digits.len() + digits.len() / DIGIT_GROUP_LEN);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(DIGIT_GROUP_LEN) {
            result.push(',');
        }
        result.push(digit);
    }
    result
}

pub fn budget(input: &str) -> TokenDisplay {
    input
        .trim()
        .parse::<u64>()
        .map(|value| TokenDisplay {
            text: format!("{} Token", compact(value)).into(),
            exact: format!("{} Token", exact(value)).into(),
        })
        .unwrap_or_default()
}

pub fn usage(input: u64, output: u64, cached: u64) -> TokenDisplay {
    TokenDisplay {
        text: format!(
            "输入 {} · 输出 {} · 缓存输入 {} Token",
            compact(input),
            compact(output),
            compact(cached)
        )
        .into(),
        exact: format!(
            "输入：{} Token\n输出：{} Token\n缓存输入：{} Token",
            exact(input),
            exact(output),
            exact(cached)
        )
        .into(),
    }
}

pub fn analysis_usage(
    input: u64,
    output: u64,
    cached: u64,
    requests: Option<u64>,
    estimated: bool,
) -> TokenDisplay {
    let mut display = usage(input, output, cached);
    if let Some(requests) = requests {
        let minimum = if estimated { "≥" } else { "" };
        display.text = format!("第 {minimum}{requests} 轮 · {}", display.text).into();
        display.exact = format!(
            "API 调用：{minimum}{} 次（包含重试{}）\n{}",
            exact(requests),
            if estimated {
                "；旧记录仅能确认成功轮次"
            } else {
                ""
            },
            display.exact
        )
        .into();
    }
    display
}

pub fn saved_analysis(snapshot: &serde_json::Value) -> TokenDisplay {
    let Some(usage) = snapshot.get("usage") else {
        return TokenDisplay::default();
    };
    let requests = snapshot["apiRequests"].as_u64();
    analysis_usage(
        usage["inputTokens"].as_u64().unwrap_or(0),
        usage["outputTokens"].as_u64().unwrap_or(0),
        usage["cachedTokens"].as_u64().unwrap_or(0),
        requests.or_else(|| snapshot["turn"].as_u64().filter(|count| *count > 0)),
        requests.is_none() || snapshot["apiRequestsEstimated"] == true,
    )
}
