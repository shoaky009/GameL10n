//! Local game-check wire types shared with the standalone Windows tool.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum CheckStatus {
    #[default]
    NotChecked,
    Passed,
    Failed,
    Inconclusive,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Observation {
    pub status: CheckStatus,
    pub message: String,
}

impl Observation {
    pub fn new(status: CheckStatus, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CheckResult {
    pub launch: Observation,
    pub text: Observation,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum CheckMode {
    Launch,
    Text,
    LaunchAndText,
}

impl CheckMode {
    pub fn launches(self) -> bool {
        self != Self::Text
    }
    pub fn reads_text(self) -> bool {
        self != Self::Launch
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "action", rename_all = "camelCase")]
pub enum SceneStep {
    Wait { milliseconds: u32 },
    Key { key: String },
    Click { x: u16, y: u16 },
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct TextRegion {
    pub left: u16,
    pub top: u16,
    pub width: u16,
    pub height: u16,
}

impl Default for TextRegion {
    fn default() -> Self {
        Self {
            left: 0,
            top: 0,
            width: 100,
            height: 100,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct CheckSettings {
    pub timeout_seconds: u32,
    pub language: String,
    pub region: TextRegion,
    pub steps: Vec<SceneStep>,
    pub unit_ids: Vec<String>,
}

impl Default for CheckSettings {
    fn default() -> Self {
        Self {
            timeout_seconds: 30,
            language: "zh-Hans".into(),
            region: TextRegion::default(),
            steps: vec![],
            unit_ids: vec![],
        }
    }
}

impl CheckSettings {
    pub fn validate(&self) -> Result<(), String> {
        if !(5..=120).contains(&self.timeout_seconds) {
            return Err("启动等待时间应为 5～120 秒。".into());
        }
        if !["zh-Hans", "zh-Hant", "ja", "en"].contains(&self.language.as_str()) {
            return Err("请选择支持的识别语言。".into());
        }
        let r = &self.region;
        if r.width == 0
            || r.height == 0
            || r.left > 100
            || r.top > 100
            || u32::from(r.left) + u32::from(r.width) > 100
            || u32::from(r.top) + u32::from(r.height) > 100
        {
            return Err("文本区域应在游戏客户区内，宽高须大于零。".into());
        }
        if self.steps.len() > 24
            || self.unit_ids.len() > 8
            || self
                .unit_ids
                .iter()
                .any(|id| id.is_empty() || id.len() > 1024)
        {
            return Err("最多保存 24 个操作和 8 条待检查文本。".into());
        }
        let mut waiting = 0;
        for step in &self.steps {
            match step {
                SceneStep::Wait { milliseconds } if (100..=30000).contains(milliseconds) => {
                    waiting += milliseconds
                }
                SceneStep::Key { key }
                    if [
                        "Enter", "Space", "Escape", "Up", "Down", "Left", "Right", "Tab",
                    ]
                    .contains(&key.as_str()) => {}
                SceneStep::Click { x, y } if *x <= 100 && *y <= 100 => {}
                _ => return Err("操作仅支持等待 100～30000 毫秒、常用按键和区域内点击。".into()),
            }
        }
        if waiting > 120000 {
            return Err("操作中的累计等待时间不能超过 120 秒。".into());
        }
        Ok(())
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckRequest {
    pub mode: CheckMode,
    pub executable: PathBuf,
    pub arguments: String,
    pub working_directory: PathBuf,
    pub game_directory: PathBuf,
    #[serde(default)]
    pub game_executable: Option<PathBuf>,
    pub settings: CheckSettings,
    pub expected: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "camelCase")]
pub enum CheckEvent {
    Launch(Observation),
    Text(Observation),
    ErrorWindow(ErrorWindow),
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ErrorWindow {
    pub window: u64,
    pub process_id: u32,
    pub process_started: u64,
    pub executable: PathBuf,
}

/// Whitespace and full-width Latin glyphs do not distinguish displayed text.
pub fn normalize_text(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_whitespace())
        .map(|c| {
            if ('\u{ff01}'..='\u{ff5e}').contains(&c) {
                char::from_u32(c as u32 - 0xfee0).unwrap_or(c)
            } else {
                c
            }
        })
        .collect()
}
