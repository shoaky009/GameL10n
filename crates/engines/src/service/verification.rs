//! Extraction compatibility and resource roundtrip verification.
use super::*;
use crate::protocol::{OP_BUILD, VALIDATION_CONTEXT_VERSION};
use std::borrow::Cow;

const PHASE_ORIGINAL_RESOURCE_CHECK: &str = "original-resource-check";
const PHASE_PARTIAL_REEXTRACTION: &str = "partial-translation-re-extraction";

#[derive(Clone, Copy, PartialEq, Eq)]
enum RoundtripStage {
    Original,
    Partial,
    Full,
}

impl RoundtripStage {
    const ALL: [Self; 3] = [Self::Original, Self::Partial, Self::Full];

    fn label(self) -> &'static str {
        match self {
            Self::Original => "原文回填",
            Self::Partial => "部分试译",
            Self::Full => "中文修改",
        }
    }

    fn dir_name(self) -> &'static str {
        match self {
            Self::Original => "original",
            Self::Partial => "partial",
            Self::Full => "changed",
        }
    }

    fn build_phase(self) -> &'static str {
        match self {
            Self::Original => "original-byte-roundtrip",
            Self::Partial => "partial-translation-build",
            Self::Full => "full-translation-build",
        }
    }

    fn display_phase(self) -> &'static str {
        match self {
            Self::Partial => "partial-display-readback",
            Self::Original | Self::Full => "full-display-readback",
        }
    }

    fn reextract_phase(self) -> &'static str {
        match self {
            Self::Original => "original-re-extraction",
            Self::Partial => PHASE_PARTIAL_REEXTRACTION,
            Self::Full => "full-translation-re-extraction",
        }
    }

    fn validate_phase(self) -> &'static str {
        match self {
            Self::Original => "original-validation",
            Self::Partial => "partial-translation-validation",
            Self::Full => "full-translation-validation",
        }
    }
}

struct Verification<'a> {
    service: &'a EngineService,
    adapter: &'a AdapterHandle,
    source_directory: &'a Path,
    resources: &'a [String],
    directory: &'a Path,
    before: &'a BTreeMap<String, String>,
    probe: Option<&'a ProbeResult>,
    cancellation: &'a CancellationToken,
    progress: ProgressCallback<'a>,
    display_check: Option<&'a display::QlieDisplay>,
    result: VerificationResult,
    phase: &'static str,
    phase_started: Instant,
    issue: Option<crate::VerificationIssue>,
    cleanup_stage_outputs: bool,
}

impl EngineService {
    #[expect(
        clippy::too_many_arguments,
        reason = "Coverage anchors are independent of resource and baseline verification inputs."
    )]
    pub(crate) async fn verify_with_coverage<'a>(
        &self,
        adapter: &AdapterHandle,
        source: &Path,
        resources: &[String],
        directory: &Path,
        baseline: impl IntoIterator<Item = &'a TranslationUnit>,
        coverage: &Value,
        expected_source_hashes: Option<&BTreeMap<String, String>>,
        probe: Option<&ProbeResult>,
        preview: Option<&(dyn Fn(Arc<ExtractionResult>) + Send + Sync)>,
        cancellation: &CancellationToken,
        progress: ProgressCallback<'_>,
    ) -> Result<VerificationResult, EngineError> {
        self.verify_with_coverage_options(
            adapter,
            source,
            resources,
            directory,
            baseline,
            coverage,
            expected_source_hashes,
            probe,
            preview,
            false,
            cancellation,
            progress,
        )
        .await
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "Coverage anchors are independent of resource and baseline verification inputs."
    )]
    pub(crate) async fn verify_with_coverage_cleanup<'a>(
        &self,
        adapter: &AdapterHandle,
        source: &Path,
        resources: &[String],
        directory: &Path,
        baseline: impl IntoIterator<Item = &'a TranslationUnit>,
        coverage: &Value,
        expected_source_hashes: Option<&BTreeMap<String, String>>,
        probe: Option<&ProbeResult>,
        preview: Option<&(dyn Fn(Arc<ExtractionResult>) + Send + Sync)>,
        cancellation: &CancellationToken,
        progress: ProgressCallback<'_>,
    ) -> Result<VerificationResult, EngineError> {
        self.verify_with_coverage_options(
            adapter,
            source,
            resources,
            directory,
            baseline,
            coverage,
            expected_source_hashes,
            probe,
            preview,
            true,
            cancellation,
            progress,
        )
        .await
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "Coverage anchors are independent of resource and baseline verification inputs."
    )]
    async fn verify_with_coverage_options<'a>(
        &self,
        adapter: &AdapterHandle,
        source: &Path,
        resources: &[String],
        directory: &Path,
        baseline: impl IntoIterator<Item = &'a TranslationUnit>,
        coverage: &Value,
        expected_source_hashes: Option<&BTreeMap<String, String>>,
        probe: Option<&ProbeResult>,
        preview: Option<&(dyn Fn(Arc<ExtractionResult>) + Send + Sync)>,
        cleanup_stage_outputs: bool,
        cancellation: &CancellationToken,
        progress: ProgressCallback<'_>,
    ) -> Result<VerificationResult, EngineError> {
        let preparation_started = Instant::now();
        Self::report(progress, "准备验收资源", "正在校验原始资源。");
        fs::create_dir_all(directory)?;
        for resource in resources
            .iter()
            .filter(|path| path.to_lowercase().ends_with(EXE_EXTENSION))
        {
            let profile = format!("{resource}{LE_CONFIG_SUFFIX}");
            if resolve_relative(source, &profile)?.is_file()
                && !resources
                    .iter()
                    .any(|path| path.eq_ignore_ascii_case(&profile))
            {
                return Err(EngineError::Rejected(format!(
                    "原程序的转区配置未纳入必需资源：{profile}"
                )));
            }
        }
        // Only reuse hashes calculated by this invocation's input inspection, not
        // hashes from a previous verification or a metadata-only file check.
        let original_hashes = match expected_source_hashes {
            Some(expected) => selected_hashes(expected, resources)?,
            None => hashes(source, resources)?,
        };
        let source_input = crate::ReadOnlySource::open(
            source,
            &resources
                .iter()
                .map(|relative| Ok((relative.clone(), resolve_relative(source, relative)?)))
                .collect::<Result<_, EngineError>>()?,
            cancellation,
        )?;
        let source_directory = source_input.directory();
        let before = hashes(source_directory, resources)?;
        if before != original_hashes {
            return Err(EngineError::Rejected(
                "验收输入发生变化，请重新选择资源。".into(),
            ));
        }
        let mut result = VerificationResult {
            resource_hashes: original_hashes,
            cacheable: true,
            ..VerificationResult::default()
        };
        result.stage_timings_ms.insert(
            "resource-preparation".into(),
            preparation_started.elapsed().as_millis() as u64,
        );
        let phase_started = Instant::now();
        let adapter_fingerprint = verified_adapter_fingerprint(self, adapter)?;
        let display_check = display::QlieDisplay::detect(
            adapter,
            source_directory,
            resources,
            &Default::default(),
        )?;
        let mut run = Verification {
            service: self,
            adapter,
            source_directory,
            resources,
            directory,
            before: &before,
            probe,
            cancellation,
            progress,
            display_check: display_check.as_ref(),
            result,
            phase: "initial-extraction",
            phase_started,
            issue: None,
            cleanup_stage_outputs,
        };
        let verified = run.verify(baseline, coverage, preview).await;
        let phase = run.phase;
        run.enter(PHASE_ORIGINAL_RESOURCE_CHECK);
        let mut result = run.result;
        let mut verified_extraction = None;
        match verified {
            Ok(extraction) => {
                verified_extraction = Some(extraction);
                result.passed = true;
                result.diagnostics.push("文本提取、原文逐字节回填、中文修改及部分翻译验收通过；游戏启动与显示尚未测试。".into());
            }
            Err(error @ (EngineError::Cancelled | EngineError::Isolation(_))) => return Err(error),
            Err(error) => {
                result.cacheable = !matches!(
                    &error,
                    EngineError::Io(_)
                        | EngineError::Timeout
                        | EngineError::ToolMissing(_)
                        | EngineError::Research(_)
                );
                result.issues.push(
                    run.issue
                        .unwrap_or_else(|| crate::VerificationIssue::from_error(phase, &error)),
                );
                result.diagnostics.push(format!("[{phase}] {error}"));
            }
        }
        Self::report(progress, "复核验收资源", "正在复核原始资源的完整性。");
        check_original_resources(source_directory, resources, &before, &mut result);
        record_verification_stage(
            &mut result.stage_timings_ms,
            run.phase,
            &mut run.phase_started,
        );
        if result.passed
            && result.original_roundtrip_passed
            && let (Some(adapter_fingerprint), Some(extraction)) =
                (adapter_fingerprint, verified_extraction)
        {
            if run.display_check.is_some()
                && let Some(probe) = probe
            {
                let key = discover::reuse_key(&adapter_fingerprint, &result.resource_hashes, probe);
                // The verification report remains valid when the optional cache cannot be written.
                let _ = write_json(&self.display_cache(&key), &key);
            }
            result.verified_extraction = Some(Arc::new(VerifiedExtraction {
                adapter_fingerprint,
                resource_hashes: result.resource_hashes.clone(),
                extraction,
            }));
        }
        write_json(&directory.join("verification.json"), &result)
            .map_err(|error| EngineError::Protocol(error.to_string()))?;
        Ok(result)
    }
}

fn check_original_resources(
    source_directory: &Path,
    resources: &[String],
    before: &BTreeMap<String, String>,
    result: &mut VerificationResult,
) {
    match hashes(source_directory, resources) {
        Ok(actual) if actual == *before => {}
        Ok(_) => {
            result.passed = false;
            result.cacheable = false;
            result.issues.push(
                crate::VerificationIssue::new(
                    crate::VerificationIssueCode::InputChanged,
                    PHASE_ORIGINAL_RESOURCE_CHECK,
                )
                .difference("原始资源", "content changed"),
            );
            result.diagnostics.push("验收期间原始资源发生变化。".into());
        }
        Err(error) => {
            result.passed = false;
            result.cacheable = false;
            result.issues.push(
                crate::VerificationIssue::new(
                    crate::VerificationIssueCode::Storage,
                    PHASE_ORIGINAL_RESOURCE_CHECK,
                )
                .difference("原始资源", &error.to_string()),
            );
            result
                .diagnostics
                .push(format!("原始资源完整性复核失败：{error}"));
        }
    }
}

impl Verification<'_> {
    fn enter(&mut self, phase: &'static str) {
        record_verification_stage(
            &mut self.result.stage_timings_ms,
            self.phase,
            &mut self.phase_started,
        );
        self.phase = phase;
    }

    async fn verify<'a>(
        &mut self,
        baseline: impl IntoIterator<Item = &'a TranslationUnit>,
        coverage: &Value,
        preview: Option<&(dyn Fn(Arc<ExtractionResult>) + Send + Sync)>,
    ) -> Result<Arc<ExtractionResult>, EngineError> {
        EngineService::report(
            self.progress,
            "验收文本提取",
            "正在检查文本身份与已有译文可复用性。",
        );
        let mut request = AdapterRequest::new(OP_EXTRACT, self.source_directory);
        request.resource_files = Some(self.resources.to_vec());
        if let Some(probe) = self.probe {
            request.set_probe_context(probe);
        }
        let response = self
            .service
            .execute_with_hashes(
                self.adapter,
                &request,
                Some(self.before),
                None,
                self.cancellation,
            )
            .await?;
        let accepts_validation_context = matches!(
            self.adapter,
            AdapterHandle::Python { .. } | AdapterHandle::BuiltinKirikiriSmil
        ) && response.validation_context_version == Some(VALIDATION_CONTEXT_VERSION);
        let mut extracted = response
            .extraction
            .ok_or_else(|| EngineError::Protocol("缺少提取结果。".into()))?;
        extracted
            .units
            .retain(|unit| !unit.source_text.trim().is_empty());
        let extracted = Arc::new(extracted);
        let units = extracted.units.as_slice();
        self.check_extraction(units, coverage, baseline)?;
        self.enter("repeated-extraction");
        self.check_repeat(units, &request).await?;
        if let Some(publish) = preview {
            publish(Arc::clone(&extracted));
        }
        let mut changed = BTreeMap::new();
        for stage in RoundtripStage::ALL {
            self.enter(stage.build_phase());
            let translations = roundtrip_translations(stage, units, &mut changed)?;
            self.roundtrip(
                stage,
                units,
                Arc::new(translations),
                accepts_validation_context,
            )
            .await?;
        }
        Ok(extracted)
    }

    fn check_extraction<'a>(
        &mut self,
        units: &[TranslationUnit],
        coverage: &Value,
        baseline: impl IntoIterator<Item = &'a TranslationUnit>,
    ) -> Result<(), EngineError> {
        gamel10n_translation::validate_units(units)
            .map_err(|error| EngineError::Rejected(error.to_string()))?;
        let mut sources = BTreeMap::new();
        for unit in units {
            *sources.entry(unit.resource_path.as_str()).or_default() += 1;
        }
        self.result.extracted_sources = sources
            .into_iter()
            .map(|(path, count)| (path.to_owned(), count))
            .collect();
        for sample in coverage["samples"].as_array().into_iter().flatten() {
            if !units.iter().any(|unit| {
                Some(unit.resource_path.as_str()) == sample["resourcePath"].as_str()
                    && Some(unit.locator.as_str()) == sample["locator"].as_str()
                    && Some(unit.source_text.as_str()) == sample["sourceText"].as_str()
            }) {
                return Err(EngineError::Rejected(format!(
                    "独立覆盖锚点不匹配：{} {}",
                    sample["resourcePath"], sample["locator"]
                )));
            }
        }
        let by_id: BTreeMap<_, _> = units.iter().map(|unit| (unit.id.as_str(), unit)).collect();
        self.enter("existing-text-compatibility");
        for previous in baseline {
            if by_id.get(previous.id.as_str()).is_none_or(|unit| {
                unit.resource_path != previous.resource_path
                    || unit.locator != previous.locator
                    || unit.source_text != previous.source_text
                    || unit.context != previous.context
                    || unit.speaker != previous.speaker
                    || unit.kind != previous.kind
                    || token_set(unit) != token_set(previous)
            }) {
                let mismatch = crate::VerificationIssue::new(
                    crate::VerificationIssueCode::IdentityMismatch,
                    self.phase,
                )
                .at(&previous.resource_path, Some(&previous.id))
                .unit_difference(
                    previous,
                    by_id.get(previous.id.as_str()).copied(),
                    &previous.source_text,
                    true,
                    true,
                );
                let fields = mismatch.field_names();
                self.issue = Some(mismatch);
                return Err(EngineError::Rejected(format!(
                    "已有文本身份或上下文发生变化：{}（变化字段：{fields}）",
                    previous.id
                )));
            }
        }
        self.result.units = units.len();
        Ok(())
    }

    async fn check_repeat(
        &mut self,
        units: &[TranslationUnit],
        request: &AdapterRequest,
    ) -> Result<(), EngineError> {
        let repeated = self
            .service
            .execute_with_hashes(
                self.adapter,
                request,
                Some(self.before),
                None,
                self.cancellation,
            )
            .await?
            .extraction
            .ok_or_else(|| EngineError::Protocol("重复提取缺少结果。".into()))?;
        let repeated: BTreeMap<_, _> = repeated
            .units
            .iter()
            .filter(|unit| !unit.source_text.trim().is_empty())
            .map(|unit| (unit.id.as_str(), unit))
            .collect();
        if repeated.len() != units.len() {
            self.issue = Some(
                crate::VerificationIssue::new(crate::VerificationIssueCode::TextCount, self.phase)
                    .difference(&units.len().to_string(), &repeated.len().to_string()),
            );
            return Err(EngineError::Rejected(
                "重复提取改变了文本内容或身份。".into(),
            ));
        }
        if let Some(unit) = units.iter().find(|unit| {
            repeated.get(unit.id.as_str()).is_none_or(|again| {
                again.source_text != unit.source_text
                    || again.context != unit.context
                    || again.resource_path != unit.resource_path
                    || again.locator != unit.locator
                    || again.kind != unit.kind
                    || again.speaker != unit.speaker
                    || token_set(again) != token_set(unit)
            })
        }) {
            let mismatch = crate::VerificationIssue::new(
                crate::VerificationIssueCode::IdentityMismatch,
                self.phase,
            )
            .at(&unit.resource_path, Some(&unit.id))
            .unit_difference(
                unit,
                repeated.get(unit.id.as_str()).copied(),
                &unit.source_text,
                true,
                true,
            );
            let fields = mismatch.field_names();
            self.issue = Some(mismatch);
            return Err(EngineError::Rejected(format!(
                "重复提取改变了文本内容或身份：{}（变化字段：{fields}）。",
                unit.id
            )));
        }
        Ok(())
    }

    async fn roundtrip(
        &mut self,
        stage: RoundtripStage,
        units: &[TranslationUnit],
        translations: Arc<BTreeMap<String, String>>,
        accepts_validation_context: bool,
    ) -> Result<(), EngineError> {
        EngineService::report(
            self.progress,
            "验收资源回填",
            format!("正在检查{}和未改动文本。", stage.label()),
        );
        let output = self.directory.join(stage.dir_name());
        if output.exists() {
            return Err(EngineError::Rejected(
                "验收输出目录已存在，请使用新的验收目录。".into(),
            ));
        }
        fs::create_dir_all(&output)?;
        let mut build = AdapterRequest::new(OP_BUILD, self.source_directory);
        build.resource_files = Some(self.resources.to_vec());
        build.output_directory = Some(output.clone());
        build.translations = Some(translations.clone());
        if let Some(probe) = self.probe {
            build.set_probe_context(probe);
        }
        let built = self
            .service
            .execute_with_hashes(
                self.adapter,
                &build,
                Some(self.before),
                None,
                self.cancellation,
            )
            .await?
            .build
            .ok_or_else(|| EngineError::Protocol("缺少回填结果。".into()))?;
        let overlay = built.mode == BuildMode::Overlay;
        self.check_output_files(stage, &output, overlay)?;
        if stage == RoundtripStage::Original && (!overlay || built.files.is_empty()) {
            self.result.original_roundtrip_passed = true;
            if self.cleanup_stage_outputs {
                let _ = fs::remove_dir_all(&output);
            }
            return Ok(());
        }
        if let Some(check) = self.display_check
            && stage != RoundtripStage::Original
        {
            self.enter(stage.display_phase());
            let warnings = check
                .verify(self.service, &built, &build, units, self.cancellation)
                .await?;
            for warning in warnings {
                if !self.result.diagnostics.contains(&warning) {
                    self.result.diagnostics.push(warning);
                }
            }
        }
        let mut extract = built.extraction_request(self.source_directory, &output, self.resources);
        if let Some(probe) = self.probe {
            extract.set_probe_context(probe);
        }
        self.enter(stage.reextract_phase());
        // Bind reused text to actual output bytes, including companion files.
        let output_hashes = (accepts_validation_context || overlay)
            .then(|| directory_hashes(&output))
            .transpose()?;
        let reextracted = self
            .service
            .execute_with_hashes(
                self.adapter,
                &extract,
                if overlay {
                    Some(self.before)
                } else {
                    output_hashes.as_ref()
                },
                if overlay {
                    output_hashes.as_ref()
                } else {
                    None
                },
                self.cancellation,
            )
            .await?
            .extraction
            .ok_or_else(|| EngineError::Protocol("缺少回填后的提取结果。".into()))?;
        self.check_reextraction(stage, units, &translations, &reextracted, overlay)?;
        self.enter(stage.validate_phase());
        let mut validate = build;
        validate.operation = OP_VALIDATE.into();
        built.set_output_context(&mut validate);
        let validation_context = if accepts_validation_context {
            let temporary = tempfile::Builder::new()
                .prefix("validation-context-")
                .tempdir_in(self.directory)?;
            let path = temporary.path().join("context.json");
            write_json(
                &path,
                &ValidationContext {
                    schema_version: VALIDATION_CONTEXT_VERSION,
                    source_directory: self.source_directory,
                    output_directory: &output,
                    resource_files: self.resources,
                    output_files: &built.files,
                    original_units: units,
                    output_units: &reextracted.units,
                },
            )
            .map_err(|error| EngineError::Protocol(error.to_string()))?;
            validate
                .extra
                .insert("validationContextFile".into(), json!(path.canonicalize()?));
            Some(temporary)
        } else {
            None
        };
        let validation = self
            .service
            .execute_with_hashes(
                self.adapter,
                &validate,
                Some(self.before),
                output_hashes.as_ref(),
                self.cancellation,
            )
            .await?
            .validation
            .ok_or_else(|| EngineError::Protocol("缺少适配器校验结果。".into()))?;
        drop(validation_context);
        if !validation.passed || !validation.issues.is_empty() {
            let summary = validation.issue_summary();
            self.result.adapter_issues = validation.issues;
            return Err(EngineError::Rejected(summary));
        }
        if stage == RoundtripStage::Original {
            self.result.original_roundtrip_passed = true;
        }
        if self.cleanup_stage_outputs {
            let _ = fs::remove_dir_all(&output);
        }
        Ok(())
    }

    fn check_output_files(
        &mut self,
        stage: RoundtripStage,
        output: &Path,
        overlay: bool,
    ) -> Result<(), EngineError> {
        for relative in self.resources.iter().filter(|_| !overlay) {
            let target = resolve_relative(output, relative)?;
            if !target.is_file() {
                return Err(EngineError::Rejected(format!("回填遗漏原资源：{relative}")));
            }
            if stage == RoundtripStage::Original
                && hash_file(&target).map_err(|error| EngineError::Protocol(error.to_string()))?
                    != self.before[relative]
            {
                self.issue = Some(
                    crate::VerificationIssue::new(
                        crate::VerificationIssueCode::ByteRoundtrip,
                        self.phase,
                    )
                    .at(relative, None),
                );
                return Err(EngineError::Rejected(format!(
                    "原文回填改变了资源字节：{relative}"
                )));
            }
            if relative.to_lowercase().ends_with(LE_CONFIG_SUFFIX)
                && hash_file(&target).map_err(|error| EngineError::Protocol(error.to_string()))?
                    != self.before[relative]
            {
                return Err(EngineError::Rejected(format!(
                    "回填改变了原启动转区配置：{relative}"
                )));
            }
        }
        Ok(())
    }

    fn check_reextraction(
        &mut self,
        stage: RoundtripStage,
        units: &[TranslationUnit],
        translations: &BTreeMap<String, String>,
        reextracted: &ExtractionResult,
        overlay: bool,
    ) -> Result<(), EngineError> {
        let stage_label = stage.label();
        let after: BTreeMap<_, _> = reextracted
            .units
            .iter()
            .filter(|unit| !unit.source_text.trim().is_empty())
            .map(|unit| (verification_key(self.adapter, unit), unit))
            .collect();
        if after.len() != units.len() {
            self.issue = Some(
                crate::VerificationIssue::new(crate::VerificationIssueCode::TextCount, self.phase)
                    .difference(&units.len().to_string(), &after.len().to_string()),
            );
            return Err(EngineError::Rejected(format!(
                "{stage_label}改变了文本数量。"
            )));
        }
        for unit in units {
            let actual = after
                .get(&verification_key(self.adapter, unit))
                .ok_or_else(|| {
                    self.issue = Some(
                        crate::VerificationIssue::new(
                            crate::VerificationIssueCode::MissingUnit,
                            self.phase,
                        )
                        .at(&unit.resource_path, Some(&unit.id)),
                    );
                    EngineError::Rejected(format!("回填丢失文本 ID：{}", unit.id))
                })?;
            let expected = translations.get(&unit.id).unwrap_or(&unit.source_text);
            let builtin_sfa = matches!(self.adapter, AdapterHandle::BuiltinSfa16);
            let expected = if builtin_sfa && stage != RoundtripStage::Original {
                Cow::Owned(expected.replace('・', "·").replace('♪', "～"))
            } else {
                Cow::Borrowed(expected.as_str())
            };
            if actual.source_text != expected
                || actual.resource_path != unit.resource_path
                || actual.locator != unit.locator
                || (overlay && actual.context != unit.context)
                || (!builtin_sfa && actual.speaker != unit.speaker)
                || actual.kind != unit.kind
                || token_set(actual) != token_set(unit)
            {
                let code = if actual.source_text != expected {
                    crate::VerificationIssueCode::TextMismatch
                } else if token_set(actual) != token_set(unit) {
                    crate::VerificationIssueCode::ProtectedTokens
                } else {
                    crate::VerificationIssueCode::IdentityMismatch
                };
                let mismatch = crate::VerificationIssue::new(code, self.phase)
                    .at(&unit.resource_path, Some(&unit.id))
                    .unit_difference(unit, Some(actual), &expected, overlay, !builtin_sfa);
                if self.phase == PHASE_PARTIAL_REEXTRACTION && !overlay {
                    self.result.archive_member_retry = ArchiveMemberRetry::for_speaker_difference(
                        &mismatch,
                        unit,
                        units,
                        translations,
                    );
                }
                let fields = mismatch.field_names();
                self.issue = Some(mismatch);
                return Err(EngineError::Rejected(format!(
                    "{stage_label}未保留文本、身份或上下文：{}（变化字段：{fields}）",
                    unit.id
                )));
            }
        }
        Ok(())
    }
}

fn roundtrip_translations(
    stage: RoundtripStage,
    units: &[TranslationUnit],
    changed: &mut BTreeMap<String, String>,
) -> Result<BTreeMap<String, String>, EngineError> {
    Ok(match stage {
        RoundtripStage::Original => units
            .iter()
            .map(|unit| (unit.id.clone(), unit.source_text.clone()))
            .collect(),
        RoundtripStage::Partial => {
            *changed = units
                .iter()
                .enumerate()
                .map(|(index, unit)| {
                    (
                        unit.id.clone(),
                        gamel10n_translation::text::verification_text(unit, index, false),
                    )
                })
                .collect();
            let mutable: Vec<_> = units
                .iter()
                .enumerate()
                .filter(|(_, unit)| changed[&unit.id] != unit.source_text)
                .collect();
            if mutable.is_empty() {
                return Err(EngineError::Rejected(
                    "所有提取文本均为保护标记或空白，无法生成有效测试译文；请提取可见文字。".into(),
                ));
            }
            let mut partial = BTreeMap::new();
            if let Some(&(index, unit)) = mutable.first() {
                partial.insert(
                    unit.id.clone(),
                    gamel10n_translation::text::verification_text(unit, index, true),
                );
            }
            if mutable.len() > 3 {
                let (index, unit) = mutable[mutable.len() / 2];
                partial.insert(
                    unit.id.clone(),
                    gamel10n_translation::text::verification_text(unit, index, true),
                );
            }
            partial
        }
        RoundtripStage::Full => std::mem::take(changed),
    })
}
