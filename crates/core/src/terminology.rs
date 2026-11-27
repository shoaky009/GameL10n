//! Project terminology extraction, merging, and translated-unit impact lookup.

use std::collections::{BTreeMap, BTreeSet};

use gamel10n_translation::{
    TokenUsage, TranslationUnit,
    provider::{SpeakerSample, TranslationFormat, diagnostic_codes},
    storage::DEFAULT_TARGET_LANGUAGE,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    Application, CancellationToken, CoreError,
    metrics::{STATUS_CANCELLED, STATUS_COMPLETED, STATUS_FAILED},
    settings::{DEFAULT_TRANSLATION_STYLE, FORMAT_PLAIN_TEXT, project_settings_keys},
    task_op,
    workbench::parse_terminology,
};

const MAX_SPEAKERS: usize = 60;
const MAX_SPEAKER_SAMPLES: usize = 2;
const MAX_SAMPLE_UNITS: usize = 160;
const MAX_SAMPLE_CHARS: usize = 9_000;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TerminologyCandidate {
    pub source: String,
    pub target: String,
    pub category: String,
    pub count: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub existing_target: Option<String>,
    pub selected: bool,
}

#[derive(Clone, Debug, Default)]
pub struct TerminologyExtraction {
    pub candidates: Vec<TerminologyCandidate>,
    pub usage: TokenUsage,
    pub provider_name: String,
    pub model: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TerminologySaveResult {
    pub terminology: String,
    pub term_count: usize,
    pub changed_terms: Vec<String>,
    pub affected_unit_ids: Vec<String>,
}

pub fn changed_terminology_keys(
    before: &BTreeMap<String, String>,
    after: &BTreeMap<String, String>,
) -> Vec<String> {
    let mut keys = BTreeSet::new();
    for (source, target) in before {
        if after.get(source) != Some(target) {
            keys.insert(source.clone());
        }
    }
    for (source, target) in after {
        if before.get(source) != Some(target) {
            keys.insert(source.clone());
        }
    }
    keys.into_iter().collect()
}

pub fn validate_term_entry(source: &str, target: &str) -> Result<(String, String), CoreError> {
    let source = source.trim();
    let target = target.trim();
    if source.is_empty() || target.is_empty() {
        return Err(CoreError::InvalidData("术语原文和译文不能为空。".into()));
    }
    if source.contains(['\r', '\n', '=']) {
        return Err(CoreError::InvalidData(
            "术语原文不能包含等号或换行。".into(),
        ));
    }
    if target.contains(['\r', '\n']) {
        return Err(CoreError::InvalidData("术语译文不能包含换行。".into()));
    }
    Ok((source.to_owned(), target.to_owned()))
}

pub fn merge_terminology_text(
    existing_raw: &str,
    entries: &[(String, String)],
) -> Result<String, CoreError> {
    let mut pending = BTreeMap::new();
    let mut order = Vec::new();
    for (source, target) in entries {
        let (source, target) = validate_term_entry(source, target)?;
        if !pending.contains_key(&source) {
            order.push(source.clone());
        }
        pending.insert(source, target);
    }
    let mut replaced = BTreeSet::new();
    let mut lines: Vec<String> = existing_raw
        .lines()
        .map(|line| {
            if let Some((left, _)) = line.split_once('=') {
                let key = left.trim();
                if let Some(target) = pending.get(key) {
                    replaced.insert(key.to_owned());
                    return format!("{key}={target}");
                }
            }
            line.to_owned()
        })
        .collect();
    for key in order {
        if !replaced.contains(&key)
            && let Some(target) = pending.get(&key)
        {
            lines.push(format!("{key}={target}"));
        }
    }
    Ok(lines.join("\n"))
}

fn clean_snippet(unit: &TranslationUnit, limit: usize) -> String {
    let mut text = unit.source_text.clone();
    for token in unit.protected_tokens.iter().flatten() {
        text = text.replace(token, "");
    }
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = collapsed.chars();
    let mut short: String = chars.by_ref().take(limit).collect();
    if chars.next().is_some() {
        short.push('…');
    }
    short
}

pub(crate) fn build_extraction_inputs(
    units: &[TranslationUnit],
) -> (Vec<SpeakerSample>, Vec<String>) {
    let mut speaker_map: BTreeMap<String, (usize, Vec<String>)> = BTreeMap::new();
    for unit in units {
        let Some(speaker) = unit.speaker.as_deref().map(str::trim) else {
            continue;
        };
        if speaker.is_empty()
            || speaker.contains(['\r', '\n', '='])
            || speaker.chars().count() > 32
            || !speaker.chars().any(char::is_alphanumeric)
        {
            continue;
        }
        let entry = speaker_map.entry(speaker.to_owned()).or_default();
        entry.0 += 1;
        if entry.1.len() < MAX_SPEAKER_SAMPLES {
            let snippet = clean_snippet(unit, 80);
            if !snippet.is_empty() && !entry.1.contains(&snippet) {
                entry.1.push(snippet);
            }
        }
    }
    let mut speakers: Vec<SpeakerSample> = speaker_map
        .into_iter()
        .map(|(name, (count, samples))| SpeakerSample {
            name,
            count,
            samples,
        })
        .collect();
    speakers.sort_by(|left, right| {
        right
            .count
            .cmp(&left.count)
            .then_with(|| left.name.cmp(&right.name))
    });
    speakers.truncate(MAX_SPEAKERS);

    let mut samples = Vec::new();
    let mut total_chars = 0;
    if !units.is_empty() {
        let step = (units.len() / MAX_SAMPLE_UNITS).max(1);
        for unit in units.iter().step_by(step) {
            if samples.len() >= MAX_SAMPLE_UNITS || total_chars >= MAX_SAMPLE_CHARS {
                break;
            }
            let snippet = clean_snippet(unit, 120);
            if snippet.chars().count() < 2 {
                continue;
            }
            let line = match unit.speaker.as_deref().map(str::trim) {
                Some(speaker) if !speaker.is_empty() => format!("[{speaker}] {snippet}"),
                _ => snippet,
            };
            total_chars += line.chars().count();
            samples.push(line);
        }
    }
    (speakers, samples)
}

fn count_term_occurrences(units: &[TranslationUnit], term: &str) -> usize {
    units
        .iter()
        .filter(|unit| {
            unit.source_text.contains(term)
                || unit.context.contains(term)
                || unit
                    .speaker
                    .as_deref()
                    .is_some_and(|speaker| speaker.contains(term))
        })
        .count()
}

impl Application {
    /// Reports whether the project's translation chain includes at least one JsonBatch profile.
    pub async fn has_batch_translation_api(&self, id: &str) -> Result<bool, CoreError> {
        let project = self.store.get(id).await?;
        let settings = self.settings().await?;
        let chain = settings
            .api_configuration
            .project_translation_chain(&project.settings);
        Ok(chain.iter().any(|profile_id| {
            settings
                .api_configuration
                .profiles
                .iter()
                .find(|profile| profile.id == *profile_id)
                .is_some_and(|profile| profile.translation_format != FORMAT_PLAIN_TEXT)
        }))
    }

    /// Uses the first JsonBatch translation API in the project's chain to suggest glossary terms.
    pub async fn extract_project_terminology(
        &self,
        id: &str,
        cancellation: &CancellationToken,
    ) -> Result<TerminologyExtraction, CoreError> {
        let analysis = self
            .workbench_analysis(id)
            .await?
            .ok_or_else(|| CoreError::InvalidData("请先识别并提取游戏文本。".into()))?;
        let providers = self.translation_providers(id).await?;
        let provider = providers
            .into_iter()
            .find(|provider| provider.translation_format() == TranslationFormat::JsonBatch)
            .ok_or_else(|| {
                CoreError::InvalidData(
                    "当前项目未配置批量 JSON 格式的翻译 API，无法自动提取术语；可手动添加术语。"
                        .into(),
                )
            })?;
        let project = self.store.get(id).await?;
        let target_language = project
            .settings
            .get(project_settings_keys::TARGET_LANGUAGE)
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .unwrap_or(DEFAULT_TARGET_LANGUAGE)
            .to_owned();
        let style = project
            .settings
            .get(project_settings_keys::STYLE)
            .and_then(Value::as_str)
            .unwrap_or(DEFAULT_TRANSLATION_STYLE)
            .to_owned();
        let existing = parse_terminology(
            project
                .settings
                .get(project_settings_keys::TERMINOLOGY)
                .unwrap_or(&Value::Null),
        );
        let units = &analysis.extraction().units;
        let (speakers, samples) = build_extraction_inputs(units);
        if speakers.is_empty() && samples.is_empty() {
            return Err(CoreError::InvalidData("没有可用于提取术语的文本。".into()));
        }
        let started_at = chrono::Utc::now();
        let started = std::time::Instant::now();
        let raw_result = provider
            .extract_terminology(
                &target_language,
                &style,
                &existing,
                &speakers,
                &samples,
                cancellation,
            )
            .await;
        let metrics_status = match &raw_result {
            Ok(_) => STATUS_COMPLETED,
            Err(error) if error.is_cancelled() => STATUS_CANCELLED,
            Err(_) => STATUS_FAILED,
        };
        self.record_single_provider_metrics(
            id,
            task_op::TERMINOLOGY,
            &provider,
            false,
            started_at,
            started.elapsed(),
            metrics_status,
        )
        .await;
        let result = raw_result.map_err(|error| {
            if error.is_cancelled() {
                CoreError::Engine(gamel10n_engines::EngineError::Cancelled)
            } else if error.diagnostic.code == diagnostic_codes::OUTPUT_TOKEN_LIMIT {
                CoreError::InvalidData(
                    "术语提取失败：模型输出达到长度上限，结果被截断（常见于推理模型思考过长），请重试或在项目翻译链中优先配置非推理模型。"
                        .into(),
                )
            } else {
                CoreError::InvalidData(format!("术语提取失败：{}", error.failure_summary()))
            }
        })?;
        let mut candidates: Vec<TerminologyCandidate> = result
            .entries
            .into_iter()
            .map(|entry| {
                let count = count_term_occurrences(units, &entry.source);
                let existing_target = existing.get(&entry.source).cloned();
                let selected = existing_target.is_none();
                TerminologyCandidate {
                    source: entry.source,
                    target: entry.target,
                    category: entry.category,
                    count,
                    existing_target,
                    selected,
                }
            })
            .collect();
        candidates.sort_by(|left, right| {
            right
                .selected
                .cmp(&left.selected)
                .then_with(|| right.count.cmp(&left.count))
                .then_with(|| left.source.cmp(&right.source))
        });
        Ok(TerminologyExtraction {
            candidates,
            usage: result.usage,
            provider_name: provider.display_name().to_owned(),
            model: provider.model().to_owned(),
        })
    }

    /// Merges new or updated glossary entries into project settings and reports impacted translations.
    pub async fn merge_project_terminology(
        &self,
        id: &str,
        entries: &[(String, String)],
    ) -> Result<TerminologySaveResult, CoreError> {
        if entries.is_empty() {
            return Err(CoreError::InvalidData("请至少选择一条术语。".into()));
        }
        let project = self.store.get(id).await?;
        let before_raw = project
            .settings
            .get(project_settings_keys::TERMINOLOGY)
            .and_then(Value::as_str)
            .unwrap_or("");
        let merged = merge_terminology_text(before_raw, entries)?;
        self.save_project_terminology_with_impact(id, merged).await
    }

    /// Saves raw terminology text and computes translated units affected before cache invalidation.
    pub async fn save_project_terminology_with_impact(
        &self,
        id: &str,
        terminology: String,
    ) -> Result<TerminologySaveResult, CoreError> {
        let project = self.store.get(id).await?;
        let before = parse_terminology(
            project
                .settings
                .get(project_settings_keys::TERMINOLOGY)
                .unwrap_or(&Value::Null),
        );
        let after = parse_terminology(&Value::String(terminology.clone()));
        let changed_terms = changed_terminology_keys(&before, &after);
        // Compute impacted translated rows before saving invalidates TranslationMemory keys.
        let affected_unit_ids = self
            .translated_unit_ids_matching_terms(id, &changed_terms)
            .await?;
        self.save_project_settings(id, json!({ project_settings_keys::TERMINOLOGY: terminology }))
            .await?;
        Ok(TerminologySaveResult {
            terminology,
            term_count: after.len(),
            changed_terms,
            affected_unit_ids,
        })
    }

    /// Computes affected translated units for an arbitrary settings save before persisting it.
    pub async fn save_project_settings_with_terminology_impact(
        &self,
        id: &str,
        updates: Value,
    ) -> Result<Option<TerminologySaveResult>, CoreError> {
        let Some(new_raw) = updates
            .get(project_settings_keys::TERMINOLOGY)
            .and_then(Value::as_str)
            .map(str::to_owned)
        else {
            self.save_project_settings(id, updates).await?;
            return Ok(None);
        };
        let project = self.store.get(id).await?;
        let before = parse_terminology(
            project
                .settings
                .get(project_settings_keys::TERMINOLOGY)
                .unwrap_or(&Value::Null),
        );
        let after = parse_terminology(&Value::String(new_raw.clone()));
        let changed_terms = changed_terminology_keys(&before, &after);
        let affected_unit_ids = self
            .translated_unit_ids_matching_terms(id, &changed_terms)
            .await?;
        self.save_project_settings(id, updates).await?;
        Ok(Some(TerminologySaveResult {
            terminology: new_raw,
            term_count: after.len(),
            changed_terms,
            affected_unit_ids,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{StoredAnalysis, ToolPaths, settings::ApiProfile};
    use gamel10n_engines::protocol::ExtractionResult;
    use gamel10n_translation::files::write_json;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    async fn mock_terminology_server(body: String) -> (String, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 65536];
            let _ = socket.read(&mut buf).await;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = socket.write_all(response.as_bytes()).await;
        });
        (format!("http://{addr}/v1"), handle)
    }

    async fn fixture() -> (tempfile::TempDir, Application, String) {
        let root = tempfile::tempdir().unwrap();
        let game = root.path().join("game");
        std::fs::create_dir(&game).unwrap();
        let app = Application::open(
            root.path().join("data"),
            &ToolPaths::new(root.path().join("tools")),
        )
        .await
        .unwrap();
        let id = app.add_project(&game).await.unwrap().id;
        let units = vec![
            TranslationUnit {
                id: "u0".into(),
                source_text: "アリスは王都へ旅立った。".into(),
                resource_path: "ch01.txt".into(),
                locator: "0".into(),
                context: String::new(),
                speaker: Some("アリス".into()),
                kind: "dialogue".into(),
                protected_tokens: None,
            },
            TranslationUnit {
                id: "u1".into(),
                source_text: "ボブは静かに微笑んだ。".into(),
                resource_path: "ch01.txt".into(),
                locator: "1".into(),
                context: String::new(),
                speaker: Some("ボブ".into()),
                kind: "dialogue".into(),
                protected_tokens: None,
            },
            TranslationUnit {
                id: "u2".into(),
                source_text: "王都の鐘が鳴る。".into(),
                resource_path: "ch02.txt".into(),
                locator: "0".into(),
                context: String::new(),
                speaker: None,
                kind: "narration".into(),
                protected_tokens: None,
            },
        ];
        write_json(
            &app.project_directory(&id)
                .unwrap()
                .join("extraction/native.json"),
            &StoredAnalysis {
                schema_version: 2,
                coverage_pending: false,
                format: "fixture".into(),
                source_directory: game,
                resource_files: vec!["ch01.txt".into(), "ch02.txt".into()],
                source_hashes: BTreeMap::new(),
                adapter: None,
                probe: None,
                extraction: ExtractionResult {
                    units,
                    warnings: vec![],
                },
            },
        )
        .unwrap();
        (root, app, id)
    }

    #[test]
    fn merge_terminology_updates_in_place_and_appends_new_entries() {
        let merged = merge_terminology_text(
            "Alice=艾丽丝\nBob=鲍勃",
            &[
                ("Alice".into(), "爱丽丝".into()),
                ("Guild".into(), "公会".into()),
            ],
        )
        .unwrap();
        assert_eq!(merged, "Alice=爱丽丝\nBob=鲍勃\nGuild=公会");
        assert!(merge_terminology_text("", &[("a=b".into(), "c".into())]).is_err());
    }

    #[tokio::test]
    async fn extraction_requires_json_batch_and_merge_tracks_affected_translated_units() {
        let (_root, app, id) = fixture().await;

        // PlainText-only translation chain cannot auto-extract terminology.
        app.save_api_profile(
            ApiProfile {
                id: "plain".into(),
                name: "Local Plain".into(),
                endpoint: "http://127.0.0.1:9/v1".into(),
                model: "local".into(),
                protocol: "ChatCompletions".into(),
                translation_format: "PlainText".into(),
                ..Default::default()
            },
            None,
            false,
        )
        .await
        .unwrap();
        assert!(!app.has_batch_translation_api(&id).await.unwrap());
        let error = app
            .extract_project_terminology(&id, &CancellationToken::new())
            .await
            .unwrap_err();
        assert!(error.to_string().contains("批量 JSON"));

        // Configure JsonBatch translation API and an existing manual term.
        let body = json!({
            "choices": [{
                "finish_reason": "stop",
                "message": {
                    "content": r#"{"terms":[
                        {"source":"アリス","target":"爱丽丝","category":"character"},
                        {"source":"王都","target":"王都城","category":"place"}
                    ]}"#
                }
            }],
            "usage": {"prompt_tokens": 120, "completion_tokens": 40}
        })
        .to_string();
        let (endpoint, server) = mock_terminology_server(body).await;
        app.save_api_profile(
            ApiProfile {
                id: "batch".into(),
                name: "Batch API".into(),
                endpoint,
                model: "fast-model".into(),
                protocol: "ChatCompletions".into(),
                translation_format: "JsonBatch".into(),
                ..Default::default()
            },
            None,
            false,
        )
        .await
        .unwrap();
        app.save_project_settings(
            &id,
            json!({
                "translationProfileIds": ["batch"],
                "terminology": "アリス=艾丽丝"
            }),
        )
        .await
        .unwrap();
        assert!(app.has_batch_translation_api(&id).await.unwrap());

        // Translate u0 and u1 (leave u2 untranslated).
        app.save_translation_edit(&id, "u0", "艾丽丝前往王都。".into(), true)
            .await
            .unwrap();
        app.save_translation_edit(&id, "u1", "鲍勃静静微笑。".into(), true)
            .await
            .unwrap();

        let extracted = app
            .extract_project_terminology(&id, &CancellationToken::new())
            .await
            .unwrap();
        server.await.unwrap();
        assert_eq!(extracted.candidates.len(), 2);
        // New term "王都" is sorted first and selected by default; existing "アリス" is unselected.
        assert_eq!(extracted.candidates[0].source, "王都");
        assert!(extracted.candidates[0].selected);
        assert_eq!(extracted.candidates[0].count, 2);
        assert_eq!(extracted.candidates[1].source, "アリス");
        assert!(!extracted.candidates[1].selected);
        assert_eq!(
            extracted.candidates[1].existing_target.as_deref(),
            Some("艾丽丝")
        );

        // Merging "王都=王都城" affects only translated unit u0 (u2 is untranslated, u1 doesn't contain 王都).
        let saved = app
            .merge_project_terminology(&id, &[("王都".into(), "王都城".into())])
            .await
            .unwrap();
        assert_eq!(saved.term_count, 2);
        assert_eq!(saved.affected_unit_ids, vec!["u0"]);
    }

    #[tokio::test]
    async fn extraction_uses_live_translation_preview_during_adaptation() {
        let root = tempfile::tempdir().unwrap();
        let game = root.path().join("game");
        std::fs::create_dir(&game).unwrap();
        let app = Application::open(
            root.path().join("data"),
            &ToolPaths::new(root.path().join("tools")),
        )
        .await
        .unwrap();
        let id = app.add_project(&game).await.unwrap().id;
        let body = json!({
            "choices": [{
                "finish_reason": "stop",
                "message": {
                    "content": r#"{"terms":[{"source":"アリス","target":"爱丽丝","category":"character"}]}"#
                }
            }],
            "usage": {"prompt_tokens": 80, "completion_tokens": 20}
        })
        .to_string();
        let (endpoint, server) = mock_terminology_server(body).await;
        app.save_api_profile(
            ApiProfile {
                id: "batch".into(),
                name: "Batch API".into(),
                endpoint,
                model: "fast-model".into(),
                protocol: "ChatCompletions".into(),
                translation_format: "JsonBatch".into(),
                ..Default::default()
            },
            None,
            false,
        )
        .await
        .unwrap();
        app.save_project_settings(&id, json!({ "translationProfileIds": ["batch"] }))
            .await
            .unwrap();

        let _task = app.begin(&id, "adapt", "正在智能识别。").await.unwrap();
        // Before any extracted text is published, terminology extraction is rejected.
        let error = app
            .extract_project_terminology(&id, &CancellationToken::new())
            .await
            .unwrap_err();
        assert!(error.to_string().contains("识别并提取游戏文本"));

        // Once adaptation publishes a non-empty translation preview, extraction uses it.
        app.publish_translation_preview(
            &id,
            std::sync::Arc::new(ExtractionResult {
                units: vec![TranslationUnit {
                    id: "preview-0".into(),
                    source_text: "アリスは微笑んだ。".into(),
                    resource_path: "script.rpy".into(),
                    locator: "0".into(),
                    context: String::new(),
                    speaker: Some("アリス".into()),
                    kind: "dialogue".into(),
                    protected_tokens: None,
                }],
                warnings: vec![],
            }),
        )
        .await;
        let extracted = app
            .extract_project_terminology(&id, &CancellationToken::new())
            .await
            .unwrap();
        server.await.unwrap();
        assert_eq!(extracted.candidates.len(), 1);
        assert_eq!(extracted.candidates[0].source, "アリス");
        assert_eq!(extracted.candidates[0].target, "爱丽丝");
        assert_eq!(extracted.candidates[0].count, 1);

        // Merging extracted terms during adaptation also succeeds without blocking.
        let saved = app
            .merge_project_terminology(&id, &[("アリス".into(), "爱丽丝".into())])
            .await
            .unwrap();
        assert_eq!(saved.term_count, 1);
    }
}
