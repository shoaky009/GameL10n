use super::query::{Overlay, Query, Status};
use super::{
    page_file::{Pages, Record, SIZE},
    *,
};
use crate::project_migrations::{self, Stamps, stamps};
use gamel10n_translation::edits::EditStore;
use std::{io, path::Path};

pub(crate) struct CachedProject {
    pub(super) id: String,
    session: Arc<std::sync::Mutex<Option<Session>>>,
}

struct Session {
    policy: TranslationPolicy,
    pages: Pages,
    edits: Arc<EditStore>,
    revision: u64,
    stamps: Stamps,
    overlay: BTreeMap<usize, Overlay>,
    summary: WorkbenchSummary,
    matches: Option<(WorkbenchFilter, Vec<u32>)>,
}
impl Session {
    fn open(
        dir: &Path,
        legacy: &Path,
        project: &crate::GameProject,
        policy: &TranslationPolicy,
        mut observed: Stamps,
        mut repair: bool,
    ) -> Result<Option<Self>, CoreError> {
        loop {
            let pages = match project_migrations::open(dir, legacy, project, policy, repair) {
                Ok(Some(pages)) => pages,
                Ok(None) => return Ok(None),
                Err(CoreError::Io(e))
                    if matches!(
                        e.kind(),
                        io::ErrorKind::PermissionDenied | io::ErrorKind::ReadOnlyFilesystem
                    ) =>
                {
                    return Ok(None);
                }
                Err(error) => return Err(error),
            };
            let latest = stamps(dir, legacy)?;
            if latest == observed {
                return Self::new(policy.clone(), pages, dir, latest).map(Some);
            }
            // The upgrade itself or a concurrent writer committed new files. Reopen that generation.
            observed = latest;
            repair = false;
        }
    }
    fn new(
        policy: TranslationPolicy,
        pages: Pages,
        dir: &Path,
        stamps: Stamps,
    ) -> Result<Self, CoreError> {
        let summary = pages.directory.summary.clone();
        let edits = EditStore::open(dir)?;
        let mut s = Self {
            policy,
            pages,
            edits,
            revision: 0,
            stamps,
            overlay: BTreeMap::new(),
            summary,
            matches: None,
        };
        s.sync_edits(true)?;
        Ok(s)
    }
    fn sync_edits(&mut self, force: bool) -> Result<(), CoreError> {
        let edits = Arc::clone(&self.edits);
        edits.read(|saved| -> Result<(), CoreError> {
            if !force && self.revision == saved.revision {
                return Ok(());
            }
            self.revision = saved.revision;
            self.overlay.clear();
            self.matches = None;
            self.summary = self.pages.directory.summary.clone();
            if saved.entries.is_empty() && saved.candidates.is_empty() {
                return Ok(());
            }
            if saved.entries.len() + saved.candidates.len() < self.pages.directory.blocks.len() {
                let keys = saved.entries.keys().chain(saved.candidates.keys());
                let keys: Vec<_> = keys.cloned().collect();
                let mut block = Vec::new();
                let mut loaded = None;
                for (i, _) in self.positions(&keys, false)? {
                    if loaded != Some(i / SIZE) {
                        block = self.pages.rows(i / SIZE)?;
                        loaded = Some(i / SIZE);
                    }
                    let (unit, state) = &mut block[i % SIZE];
                    self.apply_edit(i, unit, state, saved);
                }
                return Ok(());
            }
            for p in 0..self.pages.directory.blocks.len() {
                let block = self.pages.search_block(p)?;
                let mut bytes = block.as_slice();
                let mut positions = Vec::new();
                let mut i = 0;
                while !bytes.is_empty() {
                    let r = Record::next(&mut bytes)?;
                    if saved.entries.contains_key(r.id) || saved.candidates.contains_key(r.id) {
                        positions.push(i);
                    }
                    i += 1;
                }
                if !positions.is_empty() {
                    let mut rows = self.pages.rows(p)?;
                    for i in positions {
                        let (unit, state) = &mut rows[i];
                        self.apply_edit(p * SIZE + i, unit, state, saved);
                    }
                }
            }
            Ok(())
        })??;
        Ok(())
    }
    fn apply_edit(
        &mut self,
        i: usize,
        unit: &TranslationUnit,
        state: &mut RowState,
        edits: &TranslationEdits,
    ) {
        let base = Status::effective(state);
        let old = self.overlay.remove(&i).map_or(base, |e| e.status);
        state.read_edits(edits, unit, &self.policy.target_language);
        let status = Status::effective(state);
        if state.edit.is_some() || state.previous_edit.is_some() || state.candidate.is_some() {
            self.overlay.insert(
                i,
                Overlay {
                    status,
                    text: state.edit.as_ref().map(|e| e.text.to_lowercase()),
                },
            );
        }
        old.replace(status, &mut self.summary);
    }
    fn positions(
        &mut self,
        keys: &[String],
        strict: bool,
    ) -> Result<Vec<(usize, usize)>, CoreError> {
        // A sparse selection needs a few seeks. A dense selection is cheaper to resolve in a single pass.
        let mut result = Vec::new();
        let mut wanted = HashMap::new();
        for (order, key) in keys.iter().enumerate() {
            wanted.entry(key.as_str()).or_insert(order);
        }
        if !wanted.is_empty() && wanted.len() >= self.pages.directory.blocks.len() {
            for p in 0..self.pages.directory.blocks.len() {
                let block = self.pages.search_block(p)?;
                let mut bytes = block.as_slice();
                let mut i = p * SIZE;
                while !bytes.is_empty() {
                    let r = Record::next(&mut bytes)?;
                    if let Some(order) = wanted.remove(r.id) {
                        result.push((i, order));
                    }
                    i += 1;
                }
            }
        } else {
            for (key, order) in wanted {
                if let Some(i) = self.pages.find(key)? {
                    result.push((i, order));
                }
            }
            result.sort_unstable();
        }
        if strict && result.len() != keys.len() {
            return Err(CoreError::InvalidData(
                "文本选区已失效，请重新选择。".into(),
            ));
        }
        Ok(result)
    }
    fn selected(&mut self, id: &str) -> Result<usize, CoreError> {
        self.pages
            .find(id)?
            .ok_or_else(|| CoreError::InvalidData("找不到所选文本。".into()))
    }
    fn raw(&mut self, i: usize) -> Result<(TranslationUnit, RowState), CoreError> {
        self.pages
            .rows(i / SIZE)?
            .into_iter()
            .nth(i % SIZE)
            .ok_or_else(|| CoreError::InvalidData("工作台分页行号无效。".into()))
    }
    fn row(&mut self, id: &str) -> Result<WorkbenchRow, CoreError> {
        let i = self.selected(id)?;
        let (unit, mut state) = self.raw(i)?;
        if self.overlay.contains_key(&i) {
            self.edits
                .read(|e| state.read_edits(e, &unit, &self.policy.target_language))?;
        }
        Ok(state.row(&unit))
    }
    fn prepare(
        &mut self,
        f: &WorkbenchFilter,
        current: &dyn Fn() -> bool,
    ) -> Result<(), CoreError> {
        if f.query.is_empty()
            && f.source_query.is_empty()
            && f.translation_query.is_empty()
            && matches!(f.status.as_str(), "" | "all")
            && matches!(f.kind.as_str(), "" | "all")
            && f.resource_path.is_none()
            && f.selected_ids.is_none()
        {
            self.matches = None;
            return Ok(());
        }
        if self.matches.as_ref().is_some_and(|(prev, _)| prev == f) {
            return Ok(());
        }
        let query = Query::new(f);
        let mut found = Vec::new();
        for p in 0..self.pages.directory.blocks.len() {
            let block = self.pages.search_block(p)?;
            if !current() {
                return Err(gamel10n_engines::EngineError::Cancelled.into());
            }
            let ordinals = p * SIZE..(p + 1) * SIZE;
            if !query.may_match_block(&block, self.overlay.range(ordinals).next().is_some()) {
                continue;
            }
            let mut bytes = block.as_slice();
            let mut i = p * SIZE;
            while !bytes.is_empty() {
                let r = Record::next(&mut bytes)?;
                if query.matches(&r, self.overlay.get(&i)) {
                    found.push(i as u32);
                }
                i += 1;
            }
        }
        self.matches = Some((f.clone(), found));
        Ok(())
    }
    fn total(&self) -> usize {
        self.matches
            .as_ref()
            .map_or(self.summary.total, |(_, v)| v.len())
    }
    fn ordinal(&self, rank: usize) -> usize {
        self.matches
            .as_ref()
            .map_or(rank, |(_, v)| v[rank] as usize)
    }
    fn page(
        &mut self,
        f: &WorkbenchFilter,
        offset: usize,
        limit: usize,
        current: &dyn Fn() -> bool,
    ) -> Result<WorkbenchPage, CoreError> {
        self.prepare(f, current)?;
        let total = self.total();
        let offset = offset.min(total);
        let mut rows = Vec::new();
        let mut loaded = None;
        let mut detail = Vec::new();
        for rank in offset..offset.saturating_add(limit.clamp(1, 500)).min(total) {
            let i = self.ordinal(rank);
            if loaded != Some(i / SIZE) {
                detail = self.pages.rows(i / SIZE)?;
                loaded = Some(i / SIZE);
            }
            let (unit, state) = &mut detail[i % SIZE];
            if self.overlay.contains_key(&i) {
                self.edits
                    .read(|e| state.read_edits(e, unit, &self.policy.target_language))?;
            }
            rows.push(state.row(unit));
        }
        Ok(WorkbenchPage {
            total,
            offset,
            rows,
            summary: self.summary.clone(),
            resources: self.pages.directory.resources.iter().cloned().collect(),
            kinds: self.pages.directory.kinds.iter().cloned().collect(),
        })
    }
    fn ids(&mut self, f: &WorkbenchFilter) -> Result<Vec<String>, CoreError> {
        self.prepare(f, &|| true)?;
        let mut ids = Vec::with_capacity(self.total());
        let mut rank = 0;
        while rank < self.total() {
            let p = self.ordinal(rank) / SIZE;
            let block = self.pages.search_block(p)?;
            let mut bytes = block.as_slice();
            let mut i = p * SIZE;
            while !bytes.is_empty() && rank < self.total() {
                let r = Record::next(&mut bytes)?;
                if self.ordinal(rank) == i {
                    ids.push(r.id.into());
                    rank += 1;
                }
                i += 1;
            }
        }
        Ok(ids)
    }
    fn adjacent(
        &mut self,
        f: &WorkbenchFilter,
        id: &str,
        direction: i32,
    ) -> Result<Option<(String, usize)>, CoreError> {
        self.prepare(f, &|| true)?;
        let total = self.total();
        if total == 0 {
            return Ok(None);
        }
        let current = self.pages.find(id)?;
        let rank = match (direction < 0, current) {
            (true, None) => 0,
            (false, None) => total - 1,
            (true, Some(i)) => self
                .matches
                .as_ref()
                .map_or(i, |(_, v)| v.partition_point(|&n| (n as usize) < i))
                .saturating_sub(1),
            (false, Some(i)) => self
                .matches
                .as_ref()
                .map_or(i + 1, |(_, v)| v.partition_point(|&n| (n as usize) <= i))
                .min(total - 1),
        };
        let (unit, _) = self.raw(self.ordinal(rank))?;
        Ok(Some((unit.id, rank)))
    }
    fn context(&mut self, id: &str) -> Result<String, CoreError> {
        let i = self.selected(id)?;
        let (unit, _) = self.raw(i)?;
        let before = i
            .checked_sub(1)
            .map(|j| self.raw(j))
            .transpose()?
            .map(|v| v.0);
        let after = (i + 1 < self.summary.total)
            .then(|| self.raw(i + 1))
            .transpose()?
            .map(|v| v.0);
        Ok(super::context_text(&unit, before.as_ref(), after.as_ref()))
    }
}
impl Application {
    pub(crate) fn invalidate_paged_workbench(&self, id: &str) {
        let mut cached = self
            .paged_workbench
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if cached.as_ref().is_some_and(|s| s.id == id) {
            *cached = None;
        }
    }
    async fn paged<T: Send + 'static>(
        &self,
        id: &str,
        project_guard: Arc<tokio::sync::OwnedMutexGuard<()>>,
        mut read: impl FnMut(&mut Session) -> Result<T, CoreError> + Send + 'static,
    ) -> Result<Option<T>, CoreError> {
        if self.live_workbench(id).await {
            return Ok(None);
        }
        let project = self.store.get(id).await?;
        if project.is_removed {
            return Err(CoreError::ProjectMissing(id.into()));
        }
        let policy = validated_policy(&project.settings)?;
        let dir = self.store.directory(id)?;
        let legacy = self.data_directory().join("memory").join(id);
        let cache = {
            let mut slot = self
                .paged_workbench
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if slot.as_ref().is_none_or(|cached| cached.id != id) {
                *slot = Some(CachedProject {
                    id: id.into(),
                    session: Default::default(),
                });
            }
            Arc::clone(&slot.as_ref().expect("selected project").session)
        };
        tokio::task::spawn_blocking(move || {
            let _project = project_guard;
            let mut slot = cache.lock().unwrap_or_else(|e| e.into_inner());
            let mut repair = false;
            loop {
                let observed = stamps(&dir, &legacy)?;
                if slot
                    .as_ref()
                    .is_some_and(|s| s.policy != policy || s.stamps != observed)
                {
                    *slot = None;
                }
                let result = (|| {
                    if slot.is_none() {
                        *slot = Session::open(&dir, &legacy, &project, &policy, observed, repair)?;
                    }
                    let Some(session) = slot.as_mut() else {
                        return Ok(None);
                    };
                    session.sync_edits(false)?;
                    read(session).map(Some)
                })();
                if !repair && matches!(result, Err(CoreError::DamagedWorkbench(_))) {
                    *slot = None;
                    repair = true;
                } else {
                    return result;
                }
            }
        })
        .await?
    }
    pub(super) async fn disk_edit(
        &self,
        id: &str,
        project: Arc<tokio::sync::OwnedMutexGuard<()>>,
        keys: Vec<String>,
        edit: Arc<editing::Edit>,
    ) -> Result<Option<usize>, CoreError> {
        self.paged(id, project, move |s| {
            let mut rows = Vec::with_capacity(keys.len());
            let positions = s.positions(&keys, true)?;
            let mut loaded = None;
            let mut block = Vec::new();
            for (i, _) in positions {
                if loaded != Some(i / SIZE) {
                    block = s.pages.rows(i / SIZE)?;
                    loaded = Some(i / SIZE);
                }
                let (unit, state) = &block[i % SIZE];
                rows.push((i, unit.clone(), state.clone()));
            }
            let store = Arc::clone(&s.edits);
            let count = edit.commit(&store, &s.policy.target_language, &rows)?;
            store.read(|saved| {
                for (i, unit, mut state) in rows {
                    s.apply_edit(i, &unit, &mut state, saved);
                }
                s.matches = None;
                s.revision = saved.revision;
            })?;
            Ok(count)
        })
        .await
    }
    pub(super) async fn disk_selection(
        &self,
        id: &str,
        project: Arc<tokio::sync::OwnedMutexGuard<()>>,
        keys: Vec<String>,
        strict: bool,
    ) -> Result<Option<Vec<String>>, CoreError> {
        self.paged(id, project, move |s| {
            let mut positions = s.positions(&keys, strict)?;
            positions.sort_unstable_by_key(|(_, order)| *order);
            Ok(positions
                .into_iter()
                .map(|(_, order)| keys[order].clone())
                .collect())
        })
        .await
    }
    pub(super) async fn disk_page(
        &self,
        id: &str,
        f: &WorkbenchFilter,
        offset: usize,
        limit: usize,
        current: impl Fn() -> bool + Send + 'static,
    ) -> Result<Option<WorkbenchPage>, CoreError> {
        let f = f.clone();
        self.paged(id, Arc::new(self.lock_project(id).await?), move |s| {
            s.page(&f, offset, limit, &current)
        })
        .await
    }
    pub(super) async fn disk_row(
        &self,
        id: &str,
        unit: &str,
    ) -> Result<Option<WorkbenchRow>, CoreError> {
        let unit = unit.to_owned();
        self.paged(id, Arc::new(self.lock_project(id).await?), move |s| {
            s.row(&unit)
        })
        .await
    }
    pub(super) async fn disk_context(
        &self,
        id: &str,
        unit: &str,
    ) -> Result<Option<String>, CoreError> {
        let unit = unit.to_owned();
        self.paged(id, Arc::new(self.lock_project(id).await?), move |s| {
            s.context(&unit)
        })
        .await
    }
    pub(super) async fn disk_ids(
        &self,
        id: &str,
        f: &WorkbenchFilter,
    ) -> Result<Option<Vec<String>>, CoreError> {
        let f = f.clone();
        self.paged(id, Arc::new(self.lock_project(id).await?), move |s| {
            s.ids(&f)
        })
        .await
    }
    pub(super) async fn disk_adjacent(
        &self,
        id: &str,
        f: &WorkbenchFilter,
        unit: &str,
        direction: i32,
    ) -> Result<Option<Option<(String, usize)>>, CoreError> {
        let f = f.clone();
        let unit = unit.to_owned();
        self.paged(id, Arc::new(self.lock_project(id).await?), move |s| {
            s.adjacent(&f, &unit, direction)
        })
        .await
    }
}
