use super::{RowState, TranslationOrigin, WorkbenchFilter, WorkbenchSummary, page_file::Record};
use memchr::memmem::Finder;
use std::collections::HashSet;

#[derive(Default, Clone, Copy)]
pub(super) struct Status {
    translated: bool,
    reviewed: bool,
    edited: bool,
    invalid: bool,
    failed: bool,
    candidate: bool,
    origin: TranslationOrigin,
}
impl Status {
    fn base(r: &Record) -> Self {
        Self {
            translated: r.has_model,
            invalid: r.invalid,
            failed: r.failed,
            origin: if r.has_model {
                TranslationOrigin::Automatic
            } else {
                TranslationOrigin::Unknown
            },
            ..Default::default()
        }
    }
    pub(super) fn effective(r: &RowState) -> Self {
        Self {
            translated: r.text().is_some(),
            reviewed: r.reviewed(),
            edited: r.edited(),
            invalid: !r.issues.is_empty(),
            failed: r.failure.is_some(),
            candidate: r.candidate.is_some(),
            origin: r.origin(),
        }
    }
    pub(super) fn matches(self, filter: &str) -> bool {
        match filter {
            "missing" => !self.translated,
            "failed" => self.failed,
            "pending" => self.translated && !self.reviewed,
            "edited" => self.edited,
            "automatic" => self.translated && self.origin == TranslationOrigin::Automatic,
            "manual" => self.translated && self.origin == TranslationOrigin::Manual,
            "unknown" => self.translated && self.origin == TranslationOrigin::Unknown,
            "reviewed" => self.reviewed,
            "invalid" => self.invalid,
            "candidate" => self.candidate,
            _ => true,
        }
    }
    pub(super) fn replace(self, new: Self, s: &mut WorkbenchSummary) {
        for (n, old, new) in [
            (&mut s.translated, self.translated, new.translated),
            (&mut s.edited, self.edited, new.edited),
            (&mut s.reviewed, self.reviewed, new.reviewed),
            (&mut s.invalid, self.invalid, new.invalid),
            (&mut s.failed, self.failed, new.failed),
        ] {
            *n = *n - usize::from(old) + usize::from(new);
        }
    }
}
pub(super) struct Overlay {
    pub(super) status: Status,
    pub(super) text: Option<String>,
}
pub(super) struct Query<'a> {
    filter: &'a WorkbenchFilter,
    query: Option<Finder<'static>>,
    source: Option<Finder<'static>>,
    translation: Option<Finder<'static>>,
    selected: Option<HashSet<&'a str>>,
}
impl<'a> Query<'a> {
    pub(super) fn new(f: &'a WorkbenchFilter) -> Self {
        let finder = |s: &str| {
            (!s.is_empty()).then(|| Finder::new(s.to_lowercase().as_bytes()).into_owned())
        };
        Self {
            filter: f,
            query: finder(&f.query),
            source: finder(&f.source_query),
            translation: finder(&f.translation_query),
            selected: f
                .selected_ids
                .as_ref()
                .map(|v| v.iter().map(String::as_str).collect()),
        }
    }
    pub(super) fn may_match_block(&self, block: &[u8], contains_edits: bool) -> bool {
        // An edit can introduce a match absent from the immutable search block.
        self.source.as_ref().is_none_or(|f| f.find(block).is_some())
            && (contains_edits
                || [self.query.as_ref(), self.translation.as_ref()]
                    .into_iter()
                    .flatten()
                    .all(|f| f.find(block).is_some()))
    }
    pub(super) fn matches(&self, r: &Record, edit: Option<&Overlay>) -> bool {
        let status = edit.map_or_else(|| Status::base(r), |e| e.status);
        let translated = edit
            .and_then(|e| e.text.as_deref())
            .unwrap_or(r.model.lower);
        self.filter
            .resource_path
            .as_deref()
            .is_none_or(|p| p == r.path.raw)
            && (matches!(self.filter.kind.as_str(), "" | "all") || self.filter.kind == r.kind)
            && self.selected.as_ref().is_none_or(|ids| ids.contains(r.id))
            && status.matches(&self.filter.status)
            && self
                .source
                .as_ref()
                .is_none_or(|f| f.find(r.source.lower.as_bytes()).is_some())
            && self
                .translation
                .as_ref()
                .is_none_or(|f| status.translated && f.find(translated.as_bytes()).is_some())
            && self.query.as_ref().is_none_or(|f| {
                [
                    r.source.lower,
                    translated,
                    r.path.lower,
                    r.locator.lower,
                    r.speaker.lower,
                ]
                .iter()
                .any(|s| f.find(s.as_bytes()).is_some())
            })
    }
}
