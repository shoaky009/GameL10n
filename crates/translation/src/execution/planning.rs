use super::*;

pub(super) fn select_units<'units, 'options>(
    all_units: &'units [TranslationUnit],
    providers: &[OpenAiProvider],
    policy: &TranslationPolicy,
    options: &'options TranslationOptions,
) -> Result<Selection<'units, 'options>, ExecutionError> {
    if providers.is_empty()
        || !(1..=TranslationOptions::MAX_BATCH_SIZE).contains(&options.batch_size)
        || policy.target_language.trim().is_empty()
    {
        return Err(ExecutionError::Invalid(format!(
            "请配置翻译 API 链、目标语言和每批文本数（1–{}）。",
            TranslationOptions::MAX_BATCH_SIZE
        )));
    }
    if options.candidate_only && options.retranslation_id.is_some()
        || options.retranslation_id.as_deref().is_some_and(|id| {
            id.len() != RETRANSLATION_ID_HEX_LEN
                || !id.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
        || options.retranslation_id.is_some() && options.unit_ids.is_none()
    {
        return Err(ExecutionError::Invalid(
            "重新翻译必须指定选区和有效任务标识。".into(),
        ));
    }
    let selected: Option<HashSet<&str>> = options
        .unit_ids
        .as_ref()
        .map(|ids| ids.iter().map(String::as_str).collect());
    let units: Vec<_> = all_units
        .iter()
        .filter(|unit| {
            selected
                .as_ref()
                .is_none_or(|ids| ids.contains(unit.id.as_str()))
        })
        .collect();
    if selected
        .as_ref()
        .is_some_and(|ids| ids.is_empty() || ids.len() != units.len())
    {
        return Err(ExecutionError::Invalid(
            "文本选区为空或已失效，请重新选择。".into(),
        ));
    }
    Ok((selected, units))
}

type Selection<'units, 'options> = (Option<HashSet<&'options str>>, Vec<&'units TranslationUnit>);

impl<F: FnMut(TranslationProgress)> TranslationRun<'_, F> {
    pub(super) async fn recover_receipts(
        &mut self,
        all_units: &[TranslationUnit],
        selected: Option<&HashSet<&str>>,
        mut state: CommitState,
    ) -> Result<CommitState, ExecutionError> {
        self.engine.import_legacy_receipts(&mut self.journal)?;
        let options = self.options;
        let policy = self.policy;
        // Recover every matching paid response before choosing a new batch size or scope.
        let mut recovered_receipts = Vec::new();
        for (receipt_key, receipt) in &self.journal.pending {
            if receipt.request.candidate_only != options.candidate_only
                || receipt.request.retranslation_id != options.retranslation_id
            {
                continue;
            }
            let Some(response) = &receipt.response else {
                continue;
            };
            let requested: HashSet<_> = receipt
                .request
                .units
                .iter()
                .map(|unit| unit.id.as_str())
                .collect();
            let keys: HashSet<_> = receipt
                .request
                .units
                .iter()
                .map(|unit| unit.memory_key.as_str())
                .collect();
            let recovery_units: Vec<_> = all_units
                .iter()
                .filter(|unit| {
                    // Receipts store representatives; recover identical texts in the selection too.
                    (!options.candidate_only && options.retranslation_id.is_none()
                        || requested.contains(unit.id.as_str())
                        || selected.is_none_or(|ids| ids.contains(unit.id.as_str())))
                        && keys.contains(memory_key(unit, policy).as_str())
                })
                .collect();
            let (committed, updates) = self
                .engine
                .commit_response(
                    &recovery_units,
                    policy,
                    options,
                    receipt.request.clone(),
                    response.clone(),
                    state,
                )
                .await?;
            state = committed;
            for (id, text) in updates {
                if selected.is_none_or(|ids| ids.contains(id.as_str())) {
                    self.completed.insert(&id, &text)?;
                }
            }
            let compatible: HashSet<_> = recovery_units
                .iter()
                .map(|unit| memory_key(unit, policy))
                .collect();
            if receipt
                .request
                .units
                .iter()
                .all(|unit| compatible.contains(&unit.memory_key))
            {
                recovered_receipts.push(receipt_key.clone());
            }
        }
        for key in recovered_receipts {
            self.journal.pending.remove(&key);
            if self
                .engine
                .directory
                .join(LEGACY_RESPONSES_DIR)
                .join(format!("{key}{LEGACY_REQUEST_SUFFIX}"))
                .is_file()
            {
                self.journal.legacy_receipts.insert(key);
            }
        }
        self.save_journal()?;
        Ok(state)
    }

    pub(super) fn load_history(
        &mut self,
        units: &[&TranslationUnit],
        edit_store: &EditStore,
        state: &mut CommitState,
    ) -> Result<(), ExecutionError> {
        let options = self.options;
        let policy = self.policy;
        let history_units = edit_store.read(|edits| -> Result<Vec<_>, ExecutionError> {
            let mut history_units = Vec::new();
            for unit in units {
                cancelled(self.cancellation)?;
                if options.candidate_only {
                    if let Some(candidate) = edits.candidate(unit, &policy.target_language) {
                        self.completed.insert(&unit.id, &candidate.text)?;
                        self.cache_hits += 1;
                    }
                    continue;
                }
                let edit = edits.find(unit, &policy.target_language);
                if options.retranslation_id.is_some()
                    && edit.is_none_or(|edit| edit.retranslation_id != options.retranslation_id)
                {
                    continue;
                }
                if let Some(edit) = edit {
                    self.completed.insert(&unit.id, &edit.text)?;
                    self.cache_hits += 1;
                } else {
                    history_units.push(*unit);
                }
            }
            Ok(history_units)
        })??;
        // Visit each prefix together so a bounded cache does not reread it for every source row.
        let mut keyed_units: Vec<_> = history_units
            .into_iter()
            .map(|unit| (memory_key(unit, policy), unit))
            .collect();
        keyed_units.sort_unstable_by(|(left, _), (right, _)| left.cmp(right));
        for (key, unit) in keyed_units {
            cancelled(self.cancellation)?;
            if let Some(text) = state.memory.find_with_key(unit, policy, key)?
                && text::validate(unit, text).is_empty()
            {
                self.completed.insert(&unit.id, text)?;
                self.cache_hits += 1;
            }
        }
        state.memory.clear_cache();
        state.issues.retain(|issue| {
            issue
                .unit_id
                .as_ref()
                .is_none_or(|id| !self.completed.contains_key(id))
        });
        self.save_issues(&state.issues)
    }

    pub(super) fn group_pending<'a>(
        &mut self,
        units: &[&'a TranslationUnit],
        issues: &mut Vec<RecordedIssue>,
    ) -> Result<Batch<'a>, ExecutionError> {
        let policy = self.policy;
        let options = self.options;
        let mut groups = Vec::<Vec<&TranslationUnit>>::new();
        let mut by_key = HashMap::<String, usize>::new();
        for unit in units
            .iter()
            .filter(|unit| !self.completed.contains_key(&unit.id))
        {
            let key = memory_key(unit, policy);
            let index = *by_key.entry(key).or_insert_with(|| {
                let index = groups.len();
                groups.push(Vec::new());
                index
            });
            groups[index].push(unit);
        }
        drop(by_key);
        if options.retry_uncertain_requests {
            return Ok(groups);
        }
        let uncertain: HashSet<_> = self
            .journal
            .pending
            .values()
            .filter(|receipt| {
                receipt.response.is_none()
                    && receipt.request.candidate_only == options.candidate_only
                    && receipt.request.retranslation_id == options.retranslation_id
            })
            .flat_map(|receipt| {
                receipt
                    .request
                    .units
                    .iter()
                    .map(|unit| unit.memory_key.as_str())
            })
            .collect();
        let mut blocked = Vec::new();
        groups.retain(|group| {
            if uncertain.contains(memory_key(group[0], policy).as_str()) {
                blocked.extend(group.iter().copied());
                false
            } else {
                true
            }
        });
        if !blocked.is_empty() {
            mark_failed(
                &blocked,
                policy,
                ISSUE_REQUEST_OUTCOME_UNKNOWN,
                "上次请求结果未知，未授权重新发送；请确认后重试。",
                issues,
                &mut self.failed,
            );
            self.save_issues(issues)?;
            if options.candidate_only {
                return Err(ExecutionError::Uncertain);
            }
        }
        Ok(groups)
    }
}
