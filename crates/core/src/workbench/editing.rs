//! Durable proofreading operations shared by committed pages and live translation previews.
use super::*;
use gamel10n_translation::edits::EditStore;

pub(super) enum Edit {
    Save(String, bool),
    Review(bool),
    Undo,
    Adopt,
    Discard,
}
pub(super) type EditableRow = (usize, TranslationUnit, RowState);
impl Edit {
    pub(super) fn commit(
        &self,
        store: &EditStore,
        language: &str,
        rows: &[EditableRow],
    ) -> Result<usize, CoreError> {
        store.update(|saved| {
            let mut changes = EditChanges::default();
            for (_, unit, base) in rows {
                let mut row = base.clone();
                row.read_edits(saved, unit, language);
                match self {
                    Self::Save(text, reviewed) => {
                        if *reviewed && !text::validate(unit, text).is_empty() {
                            return Err(CoreError::InvalidData(
                                "译文存在格式问题，修正后才能标记为已校对。".into(),
                            ));
                        }
                        let mut edit =
                            TranslationEdit::new(unit, language, text.clone(), *reviewed);
                        if row.text() == Some(text) {
                            edit.origin = row.origin();
                        }
                        edit.retranslation_id = saved
                            .find(unit, language)
                            .and_then(|e| e.retranslation_id.clone());
                        changes.entries.insert(unit.id.clone(), Some(edit));
                    }
                    Self::Review(reviewed) => {
                        let Some(text) = row.text() else { continue };
                        if (*reviewed && !row.issues.is_empty())
                            || (row.reviewed() == *reviewed && row.edit.is_some())
                        {
                            continue;
                        }
                        let mut edit = saved.find(unit, language).cloned().unwrap_or_else(|| {
                            TranslationEdit::new(unit, language, text.into(), *reviewed)
                        });
                        edit.reviewed = *reviewed;
                        edit.origin = row.origin();
                        edit.is_generated = false;
                        edit.updated_at = gamel10n_translation::storage::now();
                        changes.entries.insert(unit.id.clone(), Some(edit));
                    }
                    Self::Adopt => {
                        let mut candidate =
                            saved.candidate(unit, language).cloned().ok_or_else(|| {
                                CoreError::InvalidData("当前文本没有已保存的候选译文。".into())
                            })?;
                        candidate.origin = TranslationOrigin::Automatic;
                        candidate.is_generated = false;
                        candidate.retranslation_id = saved
                            .find(unit, language)
                            .and_then(|e| e.retranslation_id.clone());
                        changes.entries.insert(unit.id.clone(), Some(candidate));
                        changes.candidates.insert(unit.id.clone(), None);
                    }
                    Self::Undo | Self::Discard => {
                        if matches!(self, Self::Undo) {
                            changes.entries.insert(unit.id.clone(), None);
                        }
                        changes.candidates.insert(unit.id.clone(), None);
                    }
                }
            }
            Ok((changes.entries.len(), changes))
        })
    }
}
impl Application {
    pub(super) async fn edit_rows(
        &self,
        id: &str,
        ids: &[String],
        operation: Edit,
    ) -> Result<usize, CoreError> {
        let guard = Arc::new(self.lock_project(id).await?);
        self.ensure_editable(id).await?;
        if ids.is_empty() {
            return Err(CoreError::InvalidData("文本选区为空。".into()));
        }
        let keys = ids
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .cloned()
            .collect::<Vec<_>>();
        let operation = Arc::new(operation);
        if let Some(count) = self
            .disk_edit(id, guard.clone(), keys.clone(), Arc::clone(&operation))
            .await?
        {
            self.workbenches.lock().await.remove(id);
            return Ok(count);
        }
        let (analysis, workbench) = self.workbench(id).await?;
        tokio::task::spawn_blocking(move || {
            let mut wb = workbench.blocking_lock();
            let units = &analysis.extraction().units;
            let mut rows = Vec::with_capacity(keys.len());
            for id in keys {
                let i = wb.selected_index(units, &id)?;
                rows.push((i, units[i].clone(), wb.state(i).clone()));
            }
            let count = operation.commit(&wb.edits, &wb.policy.target_language, &rows)?;
            for (i, unit, _) in rows {
                wb.refresh_row(i, &unit)?;
            }
            Ok(count)
        })
        .await?
    }
}
