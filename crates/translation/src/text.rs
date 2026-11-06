//! Protects resource syntax before model calls and validates restored translations.

use crate::{TranslationUnit, ValidationIssue};
use regex::Regex;
use std::{
    collections::{BTreeMap, HashMap},
    sync::LazyLock,
};

static MARKERS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"\((?:\*|#?[+-]?xx?\d+(?:%?x\d+)?%?)\)|\{[^{}\r\n]+\}|</?[A-Za-z][A-Za-z0-9_-]*(?:[^<>"'\r\n]|"[^"\r\n]*"|'[^'\r\n]*')*>|%%|%(?:\d+\$)?[-+0 #]*\d*(?:\.\d+)?[sdifuxX]|\\[nrt]"#).expect("constant marker regex")
});
static GROUPS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\[\[[^\[\]\r\n]+\]\]|\[[^\[\]\r\n]+\]").expect("constant group regex")
});
static TAGS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"<[^<>\r\n]*>").expect("constant tag regex"));
static EXPRESSIONS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"[=(){}]|\s[+*/%<>!&|?:-]\s|^(?:not|if)\s|\s(?:if|else)\s|^[\w.]+(?:\s+(?:and|or|in|is)\s+[\w.]+)+$|^[/@#]?[A-Za-z_][\w.-]*(?:\s+(?:[-+]?\d+(?:\.\d+)?|"[^"]*"|'[^']*'))+$"#).expect("constant expression regex")
});
static DELIMITERS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[+|\]+").expect("constant delimiter regex"));
static OPENING: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[\p{Ps}\p{Pi}]$").expect("constant punctuation regex"));
static CLOSING: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[\p{Pe}\p{Pf}]$").expect("constant punctuation regex"));

/// Builds the same synthetic Chinese verification text as the existing adapter acceptance flow.
pub fn verification_text(unit: &TranslationUnit, index: usize, trial: bool) -> String {
    let spans = protected_spans(unit);
    let source = &unit.source_text;
    let mutate = |mut start: usize, mut end: usize| -> Option<String> {
        while start < end {
            let character = source[start..end].chars().next()?;
            if !(character.is_whitespace()
                || matches!(character, '\'' | '"')
                || OPENING.is_match(character.encode_utf8(&mut [0; 4])))
            {
                break;
            }
            start += character.len_utf8();
        }
        while end > start {
            let character = source[start..end].chars().next_back()?;
            if !(character.is_whitespace()
                || matches!(character, '\'' | '"')
                || CLOSING.is_match(character.encode_utf8(&mut [0; 4])))
            {
                break;
            }
            end -= character.len_utf8();
        }
        if start == end {
            return None;
        }
        let is_name = unit.kind.eq_ignore_ascii_case("name");
        if !is_name && spans.is_empty() && start == 0 && end == source.len() {
            return Some(format!(
                "{}{index}{}{}",
                if trial {
                    "AVG_TRIAL_"
                } else {
                    "AVG_TRANSLATION_"
                },
                if trial { "_译文 " } else { "_翻译 " },
                source
            ));
        }
        let mut marker = format!(
            "{}{}",
            if trial { "试" } else { "译" },
            chinese_index(index)
        );
        if !is_name {
            return Some(format!("{}{marker} {}", &source[..start], &source[start..]));
        }
        if source[start..end] == marker {
            marker = format!("测{}", chinese_index(index));
        }
        Some(format!("{}{marker}{}", &source[..start], &source[end..]))
    };
    let mut position = 0;
    for (start, end) in &spans {
        if let Some(text) = mutate(position, *start) {
            return text;
        }
        position = *end;
    }
    mutate(position, source.len()).unwrap_or_else(|| source.clone())
}
fn chinese_index(mut index: usize) -> String {
    let digits = ['甲', '乙', '丙', '丁', '戊', '己', '庚', '辛', '壬', '癸'];
    let mut result = Vec::new();
    loop {
        result.push(digits[index % 10]);
        if index < 10 {
            break;
        }
        index = index / 10 - 1;
    }
    result.into_iter().rev().collect()
}

fn prose(group: &str) -> bool {
    let doubled = group.starts_with("[[") && group.ends_with("]]");
    let width = if doubled { 2 } else { 1 };
    let stripped = TAGS.replace_all(&group[width..group.len() - width], "");
    let content = stripped.trim();
    !EXPRESSIONS.is_match(content)
        && (content.chars().any(char::is_whitespace)
            || doubled && !content.is_empty() && content.chars().all(char::is_alphabetic))
}

fn common_markers(text: &str) -> Vec<(usize, usize)> {
    MARKERS
        .find_iter(text)
        .filter(|value| {
            !(value.as_str().starts_with("% ")
                && (text[..value.start()]
                    .chars()
                    .next_back()
                    .is_some_and(|c| c.is_ascii_digit())
                    || text[value.end()..]
                        .chars()
                        .next()
                        .is_some_and(|c| c.is_ascii_alphabetic())))
        })
        .map(|value| (value.start(), value.end()))
        .collect()
}

fn declared_spans(text: &str, tokens: Option<&[String]>) -> Vec<(usize, usize)> {
    let mut result = Vec::new();
    let mut distinct = std::collections::HashSet::new();
    for token in tokens
        .into_iter()
        .flatten()
        .filter(|token| !token.is_empty())
    {
        if !distinct.insert(token) {
            continue;
        }
        result.extend(
            text.match_indices(token)
                .map(|(start, _)| (start, start + token.len())),
        );
    }
    result
}

fn protected_spans(unit: &TranslationUnit) -> Vec<(usize, usize)> {
    let source = &unit.source_text;
    let mut spans = common_markers(source);
    spans.extend(declared_spans(source, unit.protected_tokens.as_deref()));
    for group in GROUPS.find_iter(source) {
        if !prose(group.as_str()) {
            spans.push((group.start(), group.end()));
        } else {
            let width = if group.as_str().starts_with("[[") {
                2
            } else {
                1
            };
            spans.push((group.start(), group.start() + width));
            spans.push((group.end() - width, group.end()));
        }
    }
    spans.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| b.1.cmp(&a.1)));
    let mut result = Vec::new();
    let mut end = 0;
    for span in spans {
        if span.0 >= end {
            end = span.1;
            result.push(span);
        }
    }
    result
}

pub struct ProtectedText {
    pub text: String,
    pub counts: BTreeMap<String, usize>,
    originals: HashMap<String, String>,
    prefix: String,
}
impl ProtectedText {
    pub fn new(unit: &TranslationUnit, prefix: &str, index: usize) -> Self {
        let mut result = Self {
            text: String::new(),
            counts: BTreeMap::new(),
            originals: HashMap::new(),
            prefix: prefix.into(),
        };
        let mut names = HashMap::<&str, String>::new();
        let mut offset = 0;
        for (start, end) in protected_spans(unit) {
            result.text.push_str(&unit.source_text[offset..start]);
            let original = &unit.source_text[start..end];
            let next = names.len();
            let token = names
                .entry(original)
                .or_insert_with(|| format!("{prefix}{index}_{next}}}"));
            result.originals.insert(token.clone(), original.into());
            *result.counts.entry(token.clone()).or_default() += 1;
            result.text.push_str(token);
            offset = end;
        }
        result.text.push_str(&unit.source_text[offset..]);
        result
    }
    pub fn restore(&self, translated: &str) -> String {
        // Match in the wire text only; restored tokens may themselves resemble placeholders.
        let mut output = String::with_capacity(translated.len());
        let mut offset = 0;
        while let Some(start) = translated[offset..].find(&self.prefix) {
            let start = offset + start;
            output.push_str(&translated[offset..start]);
            let Some(end) = translated[start..].find('}') else {
                output.push_str(&translated[start..]);
                return output;
            };
            let end = start + end + 1;
            let token = &translated[start..end];
            output.push_str(self.originals.get(token).map_or(token, String::as_str));
            offset = end;
        }
        output.push_str(&translated[offset..]);
        output
    }
}

pub fn validate(unit: &TranslationUnit, translated: &str) -> Vec<ValidationIssue> {
    let mut issues = Vec::new();
    let mut issue = |code: &str, message: String| {
        issues.push(ValidationIssue {
            code: code.into(),
            message,
            unit_id: Some(unit.id.clone()),
        })
    };
    if translated.trim().is_empty() {
        issue("empty-translation", "译文为空。".into());
    }
    if translated.contains('\0') {
        issue("nul-character", "译文含有空字符。".into());
    }
    if !unit.source_text.contains('\n') && translated.contains('\n') {
        issue(
            "unexpected-newline",
            "译文增加了脚本中不存在的换行。".into(),
        );
    }
    let groups: Vec<_> = GROUPS
        .find_iter(&unit.source_text)
        .map(|group| group.as_str())
        .collect();
    let protected: Vec<_> = groups
        .iter()
        .copied()
        .filter(|group| !prose(group))
        .collect();
    let has_prose = groups.len() != protected.len();
    let count = |text: &str, is_source: bool| {
        let mut counts = BTreeMap::<String, usize>::new();
        for (start, end) in common_markers(text)
            .into_iter()
            .chain(declared_spans(text, unit.protected_tokens.as_deref()))
        {
            *counts.entry(text[start..end].into()).or_default() += 1;
        }
        for group in GROUPS.find_iter(text) {
            if if is_source {
                !prose(group.as_str())
            } else {
                !has_prose || protected.contains(&group.as_str())
            } {
                *counts.entry(group.as_str().into()).or_default() += 1;
            }
        }
        counts
    };
    let source = count(&unit.source_text, true);
    let target = count(translated, false);
    if source != target {
        let differences: Vec<_> = source
            .keys()
            .chain(target.keys())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .filter(|key| source.get(*key) != target.get(*key))
            .take(3)
            .map(|key| {
                format!(
                    "“{key}”（原文 {} 次，译文 {} 次）",
                    source.get(key).copied().unwrap_or(0),
                    target.get(key).copied().unwrap_or(0)
                )
            })
            .collect();
        issue(
            "placeholder-mismatch",
            format!("占位符或控制标记与原文不一致：{}。", differences.join("；")),
        );
    } else {
        let brackets = |text: &str| {
            let mut values: Vec<_> = DELIMITERS
                .find_iter(text)
                .map(|item| item.as_str().to_owned())
                .collect();
            values.sort();
            values
        };
        if brackets(&unit.source_text) != brackets(translated) {
            issue(
                "placeholder-mismatch",
                "方括号结构与原文不一致，请保留每组括号的单双层数。".into(),
            );
        }
    }
    issues
}

#[cfg(test)]
mod tests {
    use super::*;
    fn unit(text: &str) -> TranslationUnit {
        TranslationUnit {
            id: "id".into(),
            resource_path: "a".into(),
            locator: "1".into(),
            source_text: text.into(),
            context: String::new(),
            speaker: None,
            kind: "dialogue".into(),
            protected_tokens: None,
        }
    }
    #[test]
    fn restores_markers_without_freezing_visible_emphasis() {
        let source = unit("Hi [name] [[also]] 25% increased <b>damage</b> {count}");
        let protected = ProtectedText::new(&source, "{v", 0);
        assert!(protected.text.contains("also"));
        assert!(protected.text.contains("25% increased"));
        let text = protected.restore(&protected.text.replace("Hi", "你好").replace("also", "也"));
        assert!(validate(&source, &text).is_empty());
        assert!(!validate(&source, &text.replace("[name]", "")).is_empty());
    }
    #[test]
    fn verification_text_preserves_wrappers_tags_and_short_names() {
        for (source, kind) in [
            ("【？？？】", "name"),
            ("[font face=serif]【Alice】[resetfont]", "name"),
            ("<color=red>Hello {player}</color>", "dialogue"),
            ("[[Danger ahead]] %s", "dialogue"),
        ] {
            let mut unit = unit(source);
            unit.kind = kind.into();
            unit.protected_tokens = Some(
                ["【", "】", "[font face=serif]", "[resetfont]"]
                    .into_iter()
                    .map(str::to_owned)
                    .collect(),
            );
            let translated = verification_text(&unit, 19415, false);
            assert_ne!(translated, source);
            assert!(validate(&unit, &translated).is_empty());
            assert_ne!(translated, verification_text(&unit, 19416, false));
            assert_ne!(translated, verification_text(&unit, 19415, true));
            if source.starts_with('<') {
                assert!(translated.starts_with("<color=red>"));
            }
        }
        let source = unit("[wait]{player}<br>");
        assert_eq!(verification_text(&source, 0, false), source.source_text);
    }
}
