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
                                ISSUE_PROVIDER_REFUSAL
                                    | ISSUE_PROVIDER_REQUEST_FAILED
                                    | ISSUE_REQUEST_OUTCOME_UNKNOWN
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

struct InFlightBatch<'a> {
    groups: Batch<'a>,
    batch: ProviderBatch<'a>,
    provider_index: usize,
    intent: TranslationIntent,
    retries: usize,
    failure: ExecutionError,
}

type ReadyBatch<'a> = (usize, Batch<'a>, ExecutionError);

impl<F: FnMut(TranslationProgress)> TranslationRun<'_, F> {
    fn provider_concurrency(&self, index: usize) -> usize {
        if self.options.candidate_only {
            1
        } else {
            self.providers[index].max_concurrency().max(1)
        }
    }

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
        let mut ready: VecDeque<ReadyBatch<'_>> = VecDeque::new();
        let mut in_flight: HashMap<u64, InFlightBatch<'_>> = HashMap::new();
        let mut in_flight_by_provider = vec![0usize; self.providers.len()];
        let mut join_set = tokio::task::JoinSet::new();
        let mut next_task_id: u64 = 0;
        let run_cancel = self.cancellation.child_token();
        let mut stopping_error: Option<ExecutionError> = None;

        loop {
            // Fill available provider concurrency slots while not stopping.
            while stopping_error.is_none() && !run_cancel.is_cancelled() {
                let next_item = if let Some(pos) = ready.iter().position(|(idx, _, _)| {
                    *idx >= self.providers.len()
                        || in_flight_by_provider[*idx] < self.provider_concurrency(*idx)
                }) {
                    ready.remove(pos)
                } else if in_flight_by_provider[0] < self.provider_concurrency(0) {
                    let batch: Batch = groups.by_ref().take(batch_size).collect();
                    if batch.is_empty() {
                        None
                    } else {
                        Some((0, batch, ExecutionError::Refused))
                    }
                } else {
                    None
                };

                let Some((start_provider, mut batch_groups, failure)) = next_item else {
                    break;
                };
                let mut failure = Some(failure);

                if let Err(err) = self.apply_manual_edits(&mut batch_groups, edit_store) {
                    stopping_error = Some(err);
                    run_cancel.cancel();
                    break;
                }
                if batch_groups.is_empty() {
                    continue;
                }

                let mut index = start_provider;
                let mut pending_batch = Some(batch_groups);
                while index < self.providers.len() {
                    let mut current = pending_batch.take().expect("batch present");
                    let scope = &refusal_scopes[index];
                    if let Some(keys) = self.journal.refused_units.get(scope) {
                        let (refused, remaining): (Batch, Batch) = current
                            .into_iter()
                            .partition(|group| keys.contains(&memory_key(group[0], self.policy)));
                        if !refused.is_empty() {
                            ready.push_front((
                                index + 1,
                                refused,
                                failure.take().unwrap_or(ExecutionError::Refused),
                            ));
                            if !remaining.is_empty() {
                                ready.push_front((index, remaining, ExecutionError::Refused));
                            }
                            break;
                        }
                        current = remaining;
                    }
                    let provider = &self.providers[index];
                    if current.len() > provider.maximum_batch_size() {
                        // Keep split batches in source order.
                        for chunk in current.chunks(provider.maximum_batch_size()).rev() {
                            ready.push_front((index, chunk.to_vec(), ExecutionError::Refused));
                        }
                        break;
                    }
                    if in_flight_by_provider[index] >= self.provider_concurrency(index) {
                        ready.push_front((
                            index,
                            current,
                            failure.take().unwrap_or(ExecutionError::Refused),
                        ));
                        break;
                    }

                    let provider_batch = ProviderBatch::new(&current, &state.issues, self.policy);
                    let key = request_key(
                        provider.identity(),
                        &provider_batch.units,
                        self.policy,
                        self.options,
                    );
                    let legacy_refusal = self
                        .engine
                        .directory
                        .join(LEGACY_RESPONSES_DIR)
                        .join(format!("{key}{LEGACY_REFUSAL_SUFFIX}"));
                    if self.journal.refusals.contains_key(&key) || legacy_refusal.is_file() {
                        pending_batch = Some(current);
                        index += 1;
                        continue;
                    }
                    if self
                        .journal
                        .pending
                        .get(&key)
                        .is_some_and(|receipt| receipt.response.is_some())
                    {
                        match self
                            .commit_batch(current, index, &mut ready, &key, state)
                            .await
                        {
                            Ok(next_state) => state = next_state,
                            Err(err) => {
                                run_cancel.cancel();
                                while join_set.join_next().await.is_some() {}
                                return Err(err);
                            }
                        }
                        break;
                    }

                    let intent = TranslationIntent {
                        request_key: key.clone(),
                        provider: provider.identity().into(),
                        sent_at: now(),
                        units: provider_batch
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
                    if let Err(err) = self.save_journal() {
                        stopping_error = Some(err);
                        run_cancel.cancel();
                        break;
                    }
                    self.emit(
                        STAGE_TRANSLATING,
                        format!(
                            "正在使用 {}（{}/{}）翻译 {} 条新文本。",
                            provider.display_name(),
                            index + 1,
                            self.providers.len(),
                            provider_batch.units.len()
                        ),
                    );

                    let task_id = next_task_id;
                    next_task_id += 1;
                    let owned_units: Vec<TranslationUnit> = provider_batch
                        .units
                        .iter()
                        .map(|unit| (*unit).clone())
                        .collect();
                    let owned_policy = self.policy.clone();
                    let owned_feedback = provider_batch.feedback.clone();
                    let owned_provider = provider.clone();
                    let cancel = run_cancel.clone();
                    in_flight_by_provider[index] += 1;
                    in_flight.insert(
                        task_id,
                        InFlightBatch {
                            groups: current,
                            batch: provider_batch,
                            provider_index: index,
                            intent,
                            retries: 0,
                            failure: failure.take().unwrap_or(ExecutionError::Refused),
                        },
                    );
                    join_set.spawn(async move {
                        let result = owned_provider
                            .translate_batch(&owned_units, &owned_policy, &owned_feedback, &cancel)
                            .await;
                        (task_id, Some(result))
                    });
                    break;
                }

                if let Some(remaining) = pending_batch
                    && let Err(err) = self.fail_batch(
                        remaining,
                        failure.take().unwrap_or(ExecutionError::Refused),
                        &mut state.issues,
                    )
                {
                    stopping_error = Some(err);
                    run_cancel.cancel();
                    break;
                }
            }

            let Some(joined) = join_set.join_next().await else {
                if let Some(err) = stopping_error {
                    return Err(err);
                }
                cancelled(self.cancellation)?;
                break;
            };

            let (task_id, result) =
                joined.map_err(|err| ExecutionError::Invalid(err.to_string()))?;
            let mut task = in_flight.remove(&task_id).expect("in-flight batch exists");
            in_flight_by_provider[task.provider_index] -= 1;
            let provider = &self.providers[task.provider_index];
            let scope = &refusal_scopes[task.provider_index];

            match result {
                None => {
                    if self.cancellation.is_cancelled() {
                        stopping_error.get_or_insert(ExecutionError::Cancelled);
                    }
                }
                Some(Ok(response)) => {
                    let message = format!(
                        "{}（{}）已返回 {} 条译文，{} 条拒译。",
                        provider.display_name(),
                        provider.model(),
                        response.translations.len(),
                        response.refused_ids.len()
                    );
                    match self.record_response(scope, task.intent, response) {
                        Ok(key) => {
                            self.emit(STAGE_TRANSLATING, message);
                            match self
                                .commit_batch(
                                    task.groups,
                                    task.provider_index,
                                    &mut ready,
                                    &key,
                                    state,
                                )
                                .await
                            {
                                Ok(next_state) => state = next_state,
                                Err(err) => {
                                    // Paid response is already persisted in journal; abort remaining tasks.
                                    run_cancel.cancel();
                                    while join_set.join_next().await.is_some() {}
                                    return Err(err);
                                }
                            }
                        }
                        Err(err) => {
                            stopping_error.get_or_insert(err);
                            run_cancel.cancel();
                        }
                    }
                }
                Some(Err(error)) if error.is_cancelled() => {
                    if self.cancellation.is_cancelled() {
                        stopping_error.get_or_insert(ExecutionError::Cancelled);
                    }
                }
                Some(Err(error)) => {
                    self.add_usage(&error.usage);
                    if let Err(err) = self
                        .engine
                        .log_provider_error(&task.intent, task.retries + 1, &error)
                    {
                        stopping_error.get_or_insert(err);
                        run_cancel.cancel();
                        continue;
                    }
                    let reason = error.failure_summary();
                    let key = &task.intent.request_key;
                    if error.is_refusal() || provider.unusable_batch_output(&error) {
                        let next_failure = if error.is_refusal() {
                            self.journal.refusals.insert(
                                key.clone(),
                                json!({
                                    "request": task.intent, "recordedAt": now(), "diagnostic": error.diagnostic
                                }),
                            );
                            task.failure
                        } else {
                            ExecutionError::Invalid(format!(
                                "翻译 API 链未返回有效结果。{reason}。"
                            ))
                        };
                        self.journal.pending.remove(key);
                        if let Err(err) = self.save_journal() {
                            stopping_error.get_or_insert(err);
                            run_cancel.cancel();
                            continue;
                        }
                        self.emit(
                            "切换翻译 API",
                            format!(
                                "第 {} 个翻译 API 未返回译文。{reason}。",
                                task.provider_index + 1
                            ),
                        );
                        if stopping_error.is_none() && !run_cancel.is_cancelled() {
                            ready.push_front((
                                task.provider_index + 1,
                                task.groups,
                                next_failure,
                            ));
                        }
                    } else if error.is_non_retryable_client_error()
                        || matches!(
                            error.diagnostic.code.as_str(),
                            diagnostic_codes::CONFIGURATION | diagnostic_codes::REQUEST_SIZE_LIMIT
                        )
                    {
                        self.journal.pending.remove(key);
                        let _ = self.save_journal();
                        stopping_error.get_or_insert(ExecutionError::Invalid(reason));
                        run_cancel.cancel();
                    } else if task.retries == MAX_BATCH_RETRIES {
                        if !error.outcome_unknown {
                            self.journal.pending.remove(key);
                        }
                        if let Err(err) = self.save_journal() {
                            stopping_error.get_or_insert(err);
                            run_cancel.cancel();
                            continue;
                        }
                        if let Err(err) = self.fail_batch(
                            task.groups,
                            ExecutionError::Retries(reason),
                            &mut state.issues,
                        ) {
                            stopping_error.get_or_insert(err);
                            run_cancel.cancel();
                        }
                    } else if stopping_error.is_none() && !run_cancel.is_cancelled() {
                        task.retries += 1;
                        let immediate = error.is_response_format_error();
                        let (stage, timing) = if immediate {
                            ("重试翻译", "立即".to_owned())
                        } else {
                            ("等待重试", format!("{RETRY_DELAY_SECS} 秒后"))
                        };
                        self.emit(
                            stage,
                            format!(
                                "API 请求失败：{reason}。{timing}进行第 {} / {MAX_BATCH_RETRIES} 次重试。",
                                task.retries
                            ),
                        );
                        let retry_delay =
                            (!immediate).then_some(Duration::from_secs(RETRY_DELAY_SECS));
                        let new_task_id = next_task_id;
                        next_task_id += 1;
                        let owned_units: Vec<TranslationUnit> = task
                            .batch
                            .units
                            .iter()
                            .map(|unit| (*unit).clone())
                            .collect();
                        let owned_policy = self.policy.clone();
                        let owned_feedback = task.batch.feedback.clone();
                        let owned_provider = provider.clone();
                        let cancel = run_cancel.clone();
                        in_flight_by_provider[task.provider_index] += 1;
                        in_flight.insert(new_task_id, task);
                        join_set.spawn(async move {
                            if let Some(delay) = retry_delay {
                                tokio::select! {
                                    biased;
                                    _ = cancel.cancelled() => {
                                        return (new_task_id, None);
                                    }
                                    _ = tokio::time::sleep(delay) => {}
                                }
                            }
                            let result = owned_provider
                                .translate_batch(
                                    &owned_units,
                                    &owned_policy,
                                    &owned_feedback,
                                    &cancel,
                                )
                                .await;
                            (new_task_id, Some(result))
                        });
                    }
                }
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

    fn fail_batch(
        &mut self,
        groups: Batch<'_>,
        error: ExecutionError,
        issues: &mut Vec<RecordedIssue>,
    ) -> Result<(), ExecutionError> {
        let units: Vec<_> = groups.into_iter().flatten().collect();
        let code = if matches!(error, ExecutionError::Refused) {
            ISSUE_PROVIDER_REFUSAL
        } else {
            ISSUE_PROVIDER_REQUEST_FAILED
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
        ready: &mut VecDeque<ReadyBatch<'a>>,
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
            ready.push_front((index + 1, refused, ExecutionError::Refused));
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
