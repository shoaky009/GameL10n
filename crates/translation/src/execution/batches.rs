use super::*;

struct ProviderBatch<'a> {
    units: Vec<&'a TranslationUnit>,
    feedback: BTreeMap<String, Vec<String>>,
}

impl<'a> ProviderBatch<'a> {
    fn new(groups: &Batch<'a>, issues: &[RecordedIssue], policy: &TranslationPolicy) -> Self {
        let units: Vec<_> = groups.iter().map(|group| group[0]).collect();
        let feedback = units
            .iter()
            .map(|unit| {
                let messages = issues
                    .iter()
                    .filter(|issue| {
                        issue.unit_id.as_deref() == Some(&unit.id)
                            && !matches!(
                                issue.code.as_str(),
                                "provider-refusal"
                                    | "provider-request-failed"
                                    | "request-outcome-unknown"
                            )
                            && issue
                                .memory_key
                                .as_ref()
                                .is_none_or(|key| *key == memory_key(unit, policy))
                    })
                    .map(|issue| issue.message.clone())
                    .collect();
                (unit.id.clone(), messages)
            })
            .collect();
        Self { units, feedback }
    }
}

enum ProviderOutcome {
    Recorded(String),
    Refused,
    InvalidOutput(String),
    RetriesExhausted(String),
}

impl<F: FnMut(TranslationProgress)> TranslationRun<'_, F> {
    pub(super) async fn execute_batches(
        &mut self,
        groups: Batch<'_>,
        edit_store: &EditStore,
        mut state: CommitState,
    ) -> Result<CommitState, ExecutionError> {
        let batch_size = self.providers[0]
            .maximum_batch_size()
            .min(self.options.batch_size);
        // Row refusals survive regrouping on resume and belong to one provider and operation.
        let refusal_scopes: Vec<_> = self
            .providers
            .iter()
            .map(|provider| request_key(provider.identity(), &[], self.policy, self.options))
            .collect();
        let mut groups = groups.into_iter();
        let mut fallback = VecDeque::new();
        loop {
            let (start_provider, mut batch) = match fallback.pop_front() {
                Some(next) => next,
                None => {
                    let batch: Batch = groups.by_ref().take(batch_size).collect();
                    if batch.is_empty() {
                        break;
                    }
                    (0, batch)
                }
            };
            cancelled(self.cancellation)?;
            self.apply_manual_edits(&mut batch, edit_store)?;
            if !batch.is_empty() {
                state = self
                    .translate_batch(batch, start_provider, &refusal_scopes, &mut fallback, state)
                    .await?;
            }
        }
        Ok(state)
    }

    fn apply_manual_edits(
        &mut self,
        batch: &mut Batch<'_>,
        edit_store: &EditStore,
    ) -> Result<(), ExecutionError> {
        if self.options.candidate_only || self.options.retranslation_id.is_some() {
            return Ok(());
        }
        edit_store.read(|edits| -> Result<(), ExecutionError> {
            for group in &mut *batch {
                let mut remaining = Vec::new();
                for &unit in group.iter() {
                    if let Some(edit) = edits.find(unit, &self.policy.target_language) {
                        self.completed.insert(&unit.id, &edit.text)?;
                        self.cache_hits += 1;
                    } else {
                        remaining.push(unit);
                    }
                }
                *group = remaining;
            }
            Ok(())
        })??;
        batch.retain(|group| !group.is_empty());
        Ok(())
    }

    async fn translate_batch<'a>(
        &mut self,
        mut groups: Batch<'a>,
        start_provider: usize,
        refusal_scopes: &[String],
        fallback: &mut VecDeque<(usize, Batch<'a>)>,
        mut state: CommitState,
    ) -> Result<CommitState, ExecutionError> {
        let batch = ProviderBatch::new(&groups, &state.issues, self.policy);
        let mut failure = ExecutionError::Refused;
        for (index, provider) in self.providers.iter().enumerate().skip(start_provider) {
            cancelled(self.cancellation)?;
            let scope = &refusal_scopes[index];
            if let Some(keys) = self.journal.refused_units.get(scope) {
                let (refused, remaining): (Batch, Batch) = groups
                    .into_iter()
                    .partition(|group| keys.contains(&memory_key(group[0], self.policy)));
                if !refused.is_empty() {
                    fallback.push_front((index + 1, refused));
                    if !remaining.is_empty() {
                        fallback.push_front((index, remaining));
                    }
                    return Ok(state);
                }
                groups = remaining;
            }
            if groups.len() > provider.maximum_batch_size() {
                // Keep split batches in source order and commit each one before sending the next.
                for chunk in groups.chunks(provider.maximum_batch_size()).rev() {
                    fallback.push_front((index, chunk.to_vec()));
                }
                return Ok(state);
            }
            match self.try_provider(index, scope, &batch).await? {
                ProviderOutcome::Recorded(key) => {
                    return self
                        .commit_batch(groups, index, fallback, &key, state)
                        .await;
                }
                ProviderOutcome::Refused => {}
                ProviderOutcome::InvalidOutput(reason) => {
                    failure = ExecutionError::Invalid(reason);
                }
                ProviderOutcome::RetriesExhausted(reason) => {
                    failure = ExecutionError::Retries(reason);
                    break;
                }
            }
        }
        self.fail_batch(groups, failure, &mut state.issues)?;
        Ok(state)
    }

    fn fail_batch(
        &mut self,
        groups: Batch<'_>,
        error: ExecutionError,
        issues: &mut Vec<RecordedIssue>,
    ) -> Result<(), ExecutionError> {
        let units: Vec<_> = groups.into_iter().flatten().collect();
        let code = if matches!(error, ExecutionError::Refused) {
            "provider-refusal"
        } else {
            "provider-request-failed"
        };
        mark_failed(
            &units,
            self.policy,
            code,
            &error.to_string(),
            issues,
            &mut self.failed,
        );
        self.save_issues(issues)?;
        let action = if self.options.candidate_only {
            "已保存失败记录。"
        } else {
            "继续后续文本。"
        };
        self.emit_changes(
            format!("本批 {} 条文本翻译失败，{action}", units.len()),
            &units,
            BTreeMap::new(),
            issues,
        );
        if self.options.candidate_only {
            return Err(error);
        }
        Ok(())
    }

    async fn commit_batch<'a>(
        &mut self,
        mut groups: Batch<'a>,
        index: usize,
        fallback: &mut VecDeque<(usize, Batch<'a>)>,
        key: &str,
        state: CommitState,
    ) -> Result<CommitState, ExecutionError> {
        // The on-disk receipt is cleared only after the translations have been committed.
        let Some(Receipt {
            request,
            response: Some(response),
        }) = self.journal.pending.remove(key)
        else {
            return Err(ExecutionError::Invalid("翻译回执缺少响应。".into()));
        };
        if !response.refused_ids.is_empty() {
            let (refused, remaining) = groups
                .into_iter()
                .partition(|group| response.refused_ids.contains(&group[0].id));
            fallback.push_front((index + 1, refused));
            groups = remaining;
        }
        let units: Vec<_> = groups.iter().flatten().copied().collect();
        let (state, updates) = self
            .engine
            .commit_response(&units, self.policy, self.options, request, response, state)
            .await?;
        self.completed.extend(&updates)?;
        for group in &groups {
            if self.completed.contains_key(&group[0].id) {
                self.cache_hits += group.len() - 1;
            }
            for unit in group {
                if self.completed.contains_key(&unit.id) {
                    self.failed.remove(&unit.id);
                } else {
                    self.failed.insert(unit.id.clone());
                }
            }
        }
        self.save_journal()?;
        self.emit_changes(
            format!(
                "已完成 {} / {} 条文本，失败 {} 条。",
                self.completed.len(),
                self.total,
                self.failed.len()
            ),
            &units,
            updates,
            &state.issues,
        );
        if self.options.candidate_only && units.iter().any(|unit| self.failed.contains(&unit.id)) {
            return Err(ExecutionError::Invalid(
                "部分译文未通过格式检查，有效译文已保存。继续时只重试失败文本。".into(),
            ));
        }
        Ok(state)
    }

    async fn try_provider(
        &mut self,
        index: usize,
        scope: &str,
        batch: &ProviderBatch<'_>,
    ) -> Result<ProviderOutcome, ExecutionError> {
        let provider = &self.providers[index];
        let key = request_key(provider.identity(), &batch.units, self.policy, self.options);
        let legacy_refusal = self
            .engine
            .directory
            .join("responses")
            .join(format!("{key}.refusal.json"));
        if self.journal.refusals.contains_key(&key) || legacy_refusal.is_file() {
            return Ok(ProviderOutcome::Refused);
        }
        if self
            .journal
            .pending
            .get(&key)
            .is_some_and(|receipt| receipt.response.is_some())
        {
            return Ok(ProviderOutcome::Recorded(key));
        }
        let intent = TranslationIntent {
            request_key: key.clone(),
            provider: provider.identity().into(),
            sent_at: now(),
            units: batch
                .units
                .iter()
                .map(|unit| PendingTranslation {
                    id: unit.id.clone(),
                    memory_key: memory_key(unit, self.policy),
                })
                .collect(),
            candidate_only: self.options.candidate_only,
            retranslation_id: self.options.retranslation_id.clone(),
        };
        self.journal.pending.insert(
            key,
            Receipt {
                request: intent.clone(),
                response: None,
            },
        );
        self.save_journal()?;
        self.emit(
            "翻译",
            format!(
                "正在使用 {}（{}/{}）翻译 {} 条新文本。",
                provider.display_name(),
                index + 1,
                self.providers.len(),
                batch.units.len()
            ),
        );
        self.request_with_retry(index, scope, batch, intent).await
    }

    async fn request_with_retry(
        &mut self,
        index: usize,
        scope: &str,
        batch: &ProviderBatch<'_>,
        intent: TranslationIntent,
    ) -> Result<ProviderOutcome, ExecutionError> {
        const MAX_RETRIES: usize = 5;
        let provider = &self.providers[index];
        let key = &intent.request_key;
        let mut retries = 0;
        loop {
            let error = match provider
                .translate_batch(
                    &batch.units,
                    self.policy,
                    &batch.feedback,
                    self.cancellation,
                )
                .await
            {
                Ok(response) => {
                    let message = format!(
                        "{}（{}）已返回 {} 条译文，{} 条拒译。",
                        provider.display_name(),
                        provider.model(),
                        response.translations.len(),
                        response.refused_ids.len()
                    );
                    let key = self.record_response(scope, intent, response)?;
                    self.emit("翻译", message);
                    return Ok(ProviderOutcome::Recorded(key));
                }
                Err(error) if error.is_cancelled() => return Err(ExecutionError::Cancelled),
                Err(error) => error,
            };
            self.add_usage(&error.usage);
            self.engine
                .log_provider_error(&intent, retries + 1, &error)?;
            let reason = error.failure_summary();
            if error.is_refusal() || provider.unusable_batch_output(&error) {
                let outcome = if error.is_refusal() {
                    self.journal.refusals.insert(
                        key.clone(),
                        json!({
                            "request": intent, "recordedAt": now(), "diagnostic": error.diagnostic
                        }),
                    );
                    ProviderOutcome::Refused
                } else {
                    ProviderOutcome::InvalidOutput(format!("翻译 API 链未返回有效结果。{reason}。"))
                };
                self.journal.pending.remove(key);
                self.save_journal()?;
                self.emit(
                    "切换翻译 API",
                    format!("第 {} 个翻译 API 未返回译文。{reason}。", index + 1),
                );
                return Ok(outcome);
            }
            if matches!(error.status_code, Some(400..=499) if !matches!(error.status_code, Some(408 | 409 | 425 | 429)))
                || matches!(
                    error.diagnostic.code.as_str(),
                    "configuration" | "request-size-limit"
                )
            {
                self.journal.pending.remove(key);
                self.save_journal()?;
                return Err(ExecutionError::Invalid(reason));
            }
            if retries == MAX_RETRIES {
                if !error.outcome_unknown {
                    self.journal.pending.remove(key);
                }
                self.save_journal()?;
                return Ok(ProviderOutcome::RetriesExhausted(reason));
            }
            retries += 1;
            let immediate = error.is_response_format_error();
            let (stage, timing) = if immediate {
                ("重试翻译", "立即")
            } else {
                ("等待重试", "10 秒后")
            };
            self.emit(
                stage,
                format!(
                    "API 请求失败：{reason}。{timing}进行第 {retries} / {MAX_RETRIES} 次重试。"
                ),
            );
            if !immediate {
                tokio::select! {
                    biased;
                    _ = self.cancellation.cancelled() => return Err(ExecutionError::Cancelled),
                    _ = tokio::time::sleep(Duration::from_secs(10)) => {}
                }
            }
        }
    }

    fn record_response(
        &mut self,
        scope: &str,
        intent: TranslationIntent,
        response: TranslationResponse,
    ) -> Result<String, ExecutionError> {
        self.add_usage(&response.usage);
        if !response.refused_ids.is_empty() {
            let refused = self
                .journal
                .refused_units
                .entry(scope.to_owned())
                .or_default();
            refused.extend(
                intent
                    .units
                    .iter()
                    .filter(|unit| response.refused_ids.contains(&unit.id))
                    .map(|unit| unit.memory_key.clone()),
            );
        }
        // Persist the paid response before touching edits or memory shards.
        let key = intent.request_key.clone();
        self.journal.pending.insert(
            key.clone(),
            Receipt {
                request: intent,
                response: Some(response),
            },
        );
        self.save_journal()?;
        Ok(key)
    }

    fn add_usage(&mut self, usage: &TokenUsage) {
        add_usage(&mut self.usage, usage);
        add_usage(&mut self.journal.usage, usage);
    }
}

fn request_key(
    provider: &str,
    units: &[&TranslationUnit],
    policy: &TranslationPolicy,
    options: &TranslationOptions,
) -> String {
    let units = units
        .iter()
        .map(|unit| json!({"Id": unit.id, "Key": memory_key(unit, policy)}))
        .collect::<Vec<_>>();
    let mut key = legacy_json(&json!({"Provider": provider, "Units": units}));
    if options.candidate_only {
        key.push_str("\ncandidate");
    }
    if let Some(id) = &options.retranslation_id {
        key.push_str("\nretranslate:");
        key.push_str(id);
    }
    hash_text(&key)
}
