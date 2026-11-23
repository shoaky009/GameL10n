//! Indexed engine and shared-knowledge management, independent of the host UI.

use std::{
    collections::{BTreeMap, HashSet},
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard},
};

use gamel10n_translation::files::{hash_text, read_json, write_json};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{CoreError, patching::safe_path};

mod lifecycle;
mod routing;
pub(crate) use routing::DiscoveryCatalogCache;
type MatchingVersions = BTreeMap<String, std::collections::BTreeSet<String>>;
pub use lifecycle::{AdapterVersion, VersionFeedback, VersionLifecycle};
use lifecycle::{lifecycle, save_lifecycle};

// Catalog instances are opened independently by concurrent project tasks.
static WRITE_GATE: Mutex<()> = Mutex::new(());
static CATALOG_REVISION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
fn write_gate() -> Result<MutexGuard<'static, ()>, CoreError> {
    WRITE_GATE
        .lock()
        .map_err(|_| invalid("引擎库写入锁不可用。"))
}

fn yes() -> bool {
    true
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdapterFamily {
    pub kind: String,
    pub key: String,
    pub name: String,
}

impl AdapterFamily {
    pub fn adapter_id(&self) -> String {
        format!("{}.{}", self.kind, self.key)
    }
    fn validate(&self) -> Result<(), CoreError> {
        if !matches!(self.kind.as_str(), "engine" | "template")
            || self.name.trim().is_empty()
            || self.name.len() > 640
            || self.name.chars().any(char::is_control)
        {
            return Err(invalid("无效的引擎分类。"));
        }
        validate_part(&self.key)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GameCompatibility {
    pub game_id: String,
    pub title: String,
    pub variant: String,
    pub adapter_version: String,
    pub structure_status: String,
    #[serde(default)]
    pub coverage_confirmed: bool,
    #[serde(default = "not_tested")]
    pub runtime_status: String,
    #[serde(default)]
    pub updated_at: Option<String>,
}

fn not_tested() -> String {
    "not-tested".into()
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineKnowledgeSummary {
    pub id: String,
    pub family: AdapterFamily,
    #[serde(default)]
    pub aliases: Vec<String>,
    pub conditions: String,
    pub summary: String,
    #[serde(default)]
    pub origins: Vec<Value>,
    #[serde(default)]
    pub revision: String,
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(default)]
    pub tool_count: usize,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

/// Numeric values match the existing library-index.json enum contract.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EngineLibrarySource {
    BuiltIn,
    External,
    History,
}

impl EngineLibrarySource {
    fn from_number(value: u8) -> Result<Self, CoreError> {
        match value {
            0 => Ok(Self::BuiltIn),
            1 => Ok(Self::External),
            2 => Ok(Self::History),
            _ => Err(invalid("无效的引擎版本来源。")),
        }
    }
}

#[derive(Clone, Debug)]
pub struct EngineLibraryReference {
    pub adapter_id: String,
    pub version: String,
    pub game_id: Option<String>,
    pub game_title: Option<String>,
    pub kind: String,
    pub path: Option<PathBuf>,
}

#[derive(Clone, Debug)]
pub struct EngineLibraryVersion {
    pub adapter_id: String,
    pub version: String,
    pub source: EngineLibrarySource,
    pub exists: bool,
    pub directory_path: Option<PathBuf>,
    pub published_at: Option<String>,
    pub enabled: bool,
    pub is_candidate: bool,
    pub games: Vec<GameCompatibility>,
    pub references: Vec<EngineLibraryReference>,
    pub manifest: Option<Value>,
    pub diagnostics: Vec<String>,
    pub lifecycle: VersionLifecycle,
}

#[derive(Clone, Debug)]
pub struct EngineLibraryFamily {
    pub family: AdapterFamily,
    pub enabled: bool,
    pub versions: Vec<EngineLibraryVersion>,
    pub games: Vec<GameCompatibility>,
    pub knowledge: Vec<EngineKnowledgeSummary>,
}

#[derive(Clone, Debug)]
pub struct EngineLibraryPage {
    pub total: usize,
    pub offset: usize,
    pub families: Vec<EngineLibraryFamily>,
    pub diagnostics: Vec<String>,
}

pub struct ProjectAdapterOption {
    pub adapter: AdapterVersion,
    pub name: String,
    pub current: bool,
    pub source: EngineLibrarySource,
}

pub(crate) fn project_adapter(project: &crate::GameProject) -> Option<AdapterVersion> {
    let mut version = crate::adapter_feedback::current_adapter(&project.extra)?;
    // The native worker reports vntextpatch-* while the library groups it under engine.*.
    if version.adapter_id.starts_with("vntextpatch-") {
        version.adapter_id.insert_str(0, "engine.");
    }
    Some(version)
}

#[derive(Clone, Copy, Debug, Default)]
pub enum LibraryFilter {
    #[default]
    All,
    Enabled,
    Disabled,
    BuiltIn,
    External,
    History,
}

#[derive(Clone, Copy, Debug, Default)]
pub enum LibrarySort {
    #[default]
    Name,
    Updated,
    CompatibleGames,
}

#[derive(Clone, Debug, Default)]
pub struct EngineLibraryDeleteResult {
    pub deleted: bool,
    pub references: Vec<EngineLibraryReference>,
    pub diagnostics: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineDiscoveryDescriptor {
    #[serde(default)]
    pub discovery_index_version: u32,
    #[serde(default)]
    pub resource_files: Vec<String>,
    #[serde(default)]
    pub resource_patterns: Vec<String>,
    #[serde(default)]
    pub recognition: Option<gamel10n_engines::discovery::Recognition>,
}

#[derive(Clone, Debug)]
pub struct EngineDiscoveryCandidate {
    pub adapter_id: String,
    pub version: String,
    pub directory: PathBuf,
    pub published_at: Option<String>,
    pub descriptor: Option<EngineDiscoveryDescriptor>,
    pub is_pinned: bool,
    pub is_known_game: bool,
    pub coverage_confirmed: bool,
}

impl EngineDiscoveryCandidate {
    /// Definite name mismatches can be excluded without opening a package or reading game content.
    pub fn matches_inventory(&self, inventory: &gamel10n_engines::discovery::Inventory) -> bool {
        let Some(descriptor) = &self.descriptor else {
            return true;
        };
        if !inventory.complete {
            return true;
        }
        if descriptor.resource_patterns.is_empty() {
            descriptor.resource_files.is_empty()
                || descriptor.resource_files.iter().any(|path| {
                    inventory
                        .files
                        .iter()
                        .any(|file| file.path.eq_ignore_ascii_case(path))
                })
        } else {
            if descriptor
                .resource_patterns
                .iter()
                .any(|pattern| gamel10n_engines::discovery::validate_pattern(pattern).is_err())
            {
                return true;
            }
            descriptor.resource_patterns.iter().any(|pattern| {
                inventory
                    .files
                    .iter()
                    .any(|file| gamel10n_engines::discovery::glob_match(pattern, &file.path))
            })
        }
    }
}

fn package_discovery(package: &Value) -> Value {
    json!({
        "discoveryIndexVersion": property(package,"discoveryIndexVersion").and_then(Value::as_u64).unwrap_or(0),
        "resourceFiles": property(package, "resourceFiles").cloned().unwrap_or_else(|| json!([])),
        "resourcePatterns": property(package, "resourcePatterns").cloned().unwrap_or_else(|| json!([])),
        "recognition": property(package, "recognition")
    })
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct IndexVersion {
    version: String,
    source: u8,
    exists: bool,
    #[serde(default)]
    published_at: Option<String>,
    #[serde(default)]
    is_candidate: bool,
    #[serde(flatten)]
    extra: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct IndexFamily {
    family: AdapterFamily,
    #[serde(default = "yes")]
    enabled: bool,
    #[serde(default)]
    versions: BTreeMap<String, IndexVersion>,
    #[serde(default)]
    games: Vec<GameCompatibility>,
    #[serde(flatten)]
    extra: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct LibraryIndex {
    schema_version: u32,
    #[serde(default)]
    families: BTreeMap<String, IndexFamily>,
    #[serde(default)]
    knowledge: BTreeMap<String, EngineKnowledgeSummary>,
    #[serde(default)]
    diagnostics: Vec<String>,
    #[serde(flatten)]
    extra: BTreeMap<String, Value>,
}

impl Default for LibraryIndex {
    fn default() -> Self {
        Self {
            schema_version: 1,
            families: BTreeMap::new(),
            knowledge: BTreeMap::new(),
            diagnostics: Vec::new(),
            extra: BTreeMap::new(),
        }
    }
}

impl LibraryIndex {
    fn matching_families<'a>(
        &'a self,
        matching: Option<&'a MatchingVersions>,
    ) -> Box<dyn Iterator<Item = (&'a String, &'a IndexFamily)> + 'a> {
        match matching {
            Some(ids) => Box::new(ids.keys().filter_map(|id| self.families.get_key_value(id))),
            None => Box::new(self.families.iter()),
        }
    }
}

struct CachedIndex {
    index: LibraryIndex,
    routing: std::sync::OnceLock<routing::PackageRoutes>,
    /// Search strings are built once per refresh, without opening package files.
    search: BTreeMap<String, String>,
    order: Vec<String>,
}

impl CachedIndex {
    fn new(mut index: LibraryIndex) -> Self {
        add_builtins(&mut index);
        for summary in index.knowledge.values() {
            index
                .families
                .entry(summary.family.adapter_id())
                .or_insert_with(|| IndexFamily {
                    family: summary.family.clone(),
                    enabled: true,
                    versions: BTreeMap::new(),
                    games: Vec::new(),
                    extra: BTreeMap::new(),
                });
        }
        let mut search: BTreeMap<String, String> = index
            .families
            .iter()
            .map(|(id, family)| {
                let mut text = format!("{id}\n{}", family.family.name);
                for game in &family.games {
                    text.push_str(&format!("\n{}\n{}", game.title, game.variant));
                }
                (id.clone(), text.to_lowercase())
            })
            .collect();
        for summary in index.knowledge.values() {
            if let Some(text) = search.get_mut(&summary.family.adapter_id()) {
                text.push_str(
                    &format!(
                        "\n{}\n{}\n{}",
                        summary.summary,
                        summary.conditions,
                        summary.aliases.join("\n")
                    )
                    .to_lowercase(),
                );
            }
        }
        let mut order: Vec<_> = index.families.keys().cloned().collect();
        order.sort_by_cached_key(|id| (index.families[id].family.name.to_lowercase(), id.clone()));
        Self {
            index,
            routing: Default::default(),
            search,
            order,
        }
    }
}

/// Open once and refresh after authoritative package changes. Browsing performs no filesystem checks.
#[derive(Clone)]
pub struct EngineCatalog {
    root: PathBuf,
    cached: Arc<Mutex<CachedIndex>>,
}

impl EngineCatalog {
    pub(crate) fn matching_packages(
        &self,
        inventory: &gamel10n_engines::discovery::Inventory,
        resolve: impl Fn(&str) -> Option<PathBuf>,
    ) -> Result<MatchingVersions, CoreError> {
        let cached = self.lock()?;
        let routes = cached
            .routing
            .get_or_init(|| routing::PackageRoutes::new(&cached.index));
        Ok(routes.select(inventory, resolve))
    }

    pub fn open(data_directory: &Path) -> Result<Self, CoreError> {
        Self::from_extensions_root(&data_directory.join("extensions"))
    }

    pub fn from_extensions_root(root: &Path) -> Result<Self, CoreError> {
        let root = std::path::absolute(root)?;
        let index = load_index(&root)?;
        Ok(Self {
            root,
            cached: Arc::new(Mutex::new(CachedIndex::new(index))),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub(crate) fn adaptation_package_index(
        &self,
        matching: &MatchingVersions,
    ) -> Result<BTreeMap<PathBuf, gamel10n_engines::adaptation::IndexedPackage>, CoreError> {
        let cached = self.lock()?;
        let mut packages = BTreeMap::new();
        let base = self.root.join("packages");
        for (id, family) in &cached.index.families {
            let search_text = format!("{} {}", id, json!(family.family)).to_lowercase();
            for (version, entry) in &family.versions {
                if entry.source == 1
                    && entry
                        .extra
                        .get("discovery")
                        .is_some_and(|d| d["discoveryIndexVersion"].as_u64() == Some(1))
                {
                    packages.insert(
                        base.join(id).join(version),
                        gamel10n_engines::adaptation::IndexedPackage {
                            family: id.clone(),
                            version: version.clone(),
                            search_text: search_text.clone(),
                            matches_game: matching
                                .get(id)
                                .is_some_and(|versions| versions.contains(version)),
                        },
                    );
                }
            }
        }
        Ok(packages)
    }

    pub fn refresh(&self) -> Result<(), CoreError> {
        *self.lock()? = CachedIndex::new(load_index(&self.root)?);
        Ok(())
    }

    pub fn snapshot(
        &self,
        query: &str,
        offset: usize,
        limit: usize,
    ) -> Result<EngineLibraryPage, CoreError> {
        self.snapshot_with_options(query, LibraryFilter::All, LibrarySort::Name, offset, limit)
    }

    pub fn snapshot_with_options(
        &self,
        query: &str,
        filter: LibraryFilter,
        sort: LibrarySort,
        offset: usize,
        limit: usize,
    ) -> Result<EngineLibraryPage, CoreError> {
        self.snapshot_filtered(query, filter, None, sort, offset, limit)
    }

    pub fn snapshot_filtered(
        &self,
        query: &str,
        source: LibraryFilter,
        enabled: Option<bool>,
        sort: LibrarySort,
        offset: usize,
        limit: usize,
    ) -> Result<EngineLibraryPage, CoreError> {
        let cached = self.lock()?;
        let query = query.trim().to_lowercase();
        let mut matches: Vec<_> = cached
            .order
            .iter()
            .filter(|id| query.is_empty() || cached.search[*id].contains(&query))
            .filter(|id| {
                let family = &cached.index.families[*id];
                if enabled
                    .is_some_and(|enabled| family.versions.is_empty() || enabled != family.enabled)
                {
                    return false;
                }
                match source {
                    LibraryFilter::All => true,
                    LibraryFilter::Enabled => family.enabled,
                    LibraryFilter::Disabled => !family.enabled,
                    LibraryFilter::BuiltIn => {
                        family.versions.values().any(|version| version.source == 0)
                    }
                    LibraryFilter::External => {
                        family.versions.values().any(|version| version.source == 1)
                    }
                    LibraryFilter::History => {
                        !family.versions.is_empty()
                            && family.versions.values().all(|version| version.source == 2)
                    }
                }
            })
            .collect();
        match sort {
            LibrarySort::Name => {}
            LibrarySort::Updated => matches.sort_by_cached_key(|id| {
                std::cmp::Reverse(
                    cached.index.families[*id]
                        .versions
                        .values()
                        .filter_map(|version| {
                            version
                                .published_at
                                .as_deref()
                                .and_then(|date| chrono::DateTime::parse_from_rfc3339(date).ok())
                        })
                        .max(),
                )
            }),
            LibrarySort::CompatibleGames => {
                matches.sort_by_key(|id| std::cmp::Reverse(cached.index.families[*id].games.len()))
            }
        }
        let offset = offset.min(matches.len());
        let families = matches
            .iter()
            .skip(offset)
            .take(limit.clamp(1, 500))
            .map(|id| {
                let stored = &cached.index.families[*id];
                EngineLibraryFamily {
                    family: stored.family.clone(),
                    enabled: stored.enabled,
                    versions: self.versions(id, stored),
                    games: stored.games.clone(),
                    knowledge: cached
                        .index
                        .knowledge
                        .values()
                        .filter(|entry| entry.family.adapter_id() == **id)
                        .cloned()
                        .collect(),
                }
            })
            .collect();
        Ok(EngineLibraryPage {
            total: matches.len(),
            offset,
            families,
            diagnostics: cached.index.diagnostics.clone(),
        })
    }

    /// Position in the unfiltered name-sorted index, without reading package files.
    pub fn family_position(&self, adapter_id: &str) -> Result<Option<usize>, CoreError> {
        Ok(self.lock()?.order.iter().position(|id| id == adapter_id))
    }

    pub fn project_adapters(
        &self,
        project: &crate::GameProject,
    ) -> Result<Vec<ProjectAdapterOption>, CoreError> {
        let cached = self.lock()?;
        let current = project_adapter(project);
        let mut options = Vec::new();
        for id in &cached.order {
            let family = &cached.index.families[id];
            for version in family.versions.values() {
                let is_current = current
                    .as_ref()
                    .is_some_and(|p| p.adapter_id == *id && p.version == version.version);
                if !version.exists || (!family.enabled && !is_current) || version.source > 1 {
                    continue;
                }
                options.push(ProjectAdapterOption {
                    adapter: AdapterVersion {
                        adapter_id: id.clone(),
                        version: version.version.clone(),
                    },
                    name: family.family.name.clone(),
                    current: is_current,
                    source: EngineLibrarySource::from_number(version.source)?,
                });
            }
        }
        options.sort_by_key(|option| !option.current);
        Ok(options)
    }

    pub fn family(&self, adapter_id: &str) -> Result<EngineLibraryFamily, CoreError> {
        let cached = self.lock()?;
        let stored = cached
            .index
            .families
            .get(adapter_id)
            .ok_or_else(|| invalid("找不到引擎分类。"))?;
        Ok(EngineLibraryFamily {
            family: stored.family.clone(),
            enabled: stored.enabled,
            versions: self.versions(adapter_id, stored),
            games: stored.games.clone(),
            knowledge: cached
                .index
                .knowledge
                .values()
                .filter(|entry| entry.family.adapter_id() == adapter_id)
                .cloned()
                .collect(),
        })
    }

    fn versions(&self, id: &str, family: &IndexFamily) -> Vec<EngineLibraryVersion> {
        let mut versions: Vec<_> = family
            .versions
            .values()
            .map(|version| EngineLibraryVersion {
                adapter_id: id.into(),
                version: version.version.clone(),
                source: match version.source {
                    0 => EngineLibrarySource::BuiltIn,
                    1 => EngineLibrarySource::External,
                    _ => EngineLibrarySource::History,
                },
                exists: version.exists,
                directory_path: (version.source == 1)
                    .then(|| self.root.join("packages").join(id).join(&version.version)),
                published_at: version.published_at.clone(),
                enabled: family.enabled,
                is_candidate: family.enabled && version.is_candidate,
                games: family
                    .games
                    .iter()
                    .filter(|game| game.adapter_version == version.version)
                    .cloned()
                    .collect(),
                references: Vec::new(),
                manifest: None,
                diagnostics: Vec::new(),
                lifecycle: version
                    .extra
                    .get("lifecycle")
                    .cloned()
                    .and_then(|value| serde_json::from_value(value).ok())
                    .unwrap_or_default(),
            })
            .collect();
        versions.sort_by(|left, right| {
            (left.source as u8)
                .cmp(&(right.source as u8))
                .then_with(|| right.published_at.cmp(&left.published_at))
                .then_with(|| left.version.cmp(&right.version))
        });
        versions
    }

    pub fn version_directory(&self, adapter_id: &str, version: &str) -> Result<PathBuf, CoreError> {
        let path = self.package_path(adapter_id, version)?;
        crate::patching::check_path(&path)?;
        Ok(path)
    }

    fn package_path(&self, adapter_id: &str, version: &str) -> Result<PathBuf, CoreError> {
        validate_family_id(adapter_id)?;
        validate_part(version)?;
        Ok(self.root.join("packages").join(adapter_id).join(version))
    }

    /// Checks only the selected manifest and required entry files; payload hashes are checked for execution.
    pub fn version_detail(
        &self,
        adapter_id: &str,
        version: &str,
    ) -> Result<EngineLibraryVersion, CoreError> {
        let mut detail = self
            .family(adapter_id)?
            .versions
            .into_iter()
            .find(|entry| entry.version == version)
            .ok_or_else(|| invalid("找不到引擎版本。"))?;
        if detail.source != EngineLibrarySource::External {
            return Ok(detail);
        }
        let directory = self.package_path(adapter_id, version)?;
        let manifest = safe_path(&directory, "package.json")
            .and_then(|path| read_bounded_json::<Value>(&path, 16 * 1024 * 1024));
        match manifest {
            Ok(manifest) => {
                let id = property(&manifest, "id").and_then(Value::as_str);
                let actual_version = property(&manifest, "version").and_then(Value::as_str);
                if id != Some(adapter_id) || actual_version != Some(version) {
                    detail.exists = false;
                    detail
                        .diagnostics
                        .push("包清单与所选引擎版本不一致。".into());
                } else {
                    let runtime = property(&manifest, "runtime").and_then(Value::as_str);
                    let python = runtime == Some("python")
                        || property(&manifest, "manifestVersion").and_then(Value::as_u64)
                            == Some(4);
                    let entry = property(&manifest, "entryFile")
                        .and_then(Value::as_str)
                        .unwrap_or(if python {
                            "src/adapter.py"
                        } else {
                            "adapter.dll"
                        });
                    detail.exists = safe_path(&directory, entry).is_ok_and(|path| path.is_file())
                        && (entry.ends_with(".py")
                            || safe_path(&directory, "adapter.runtimeconfig.json")
                                .is_ok_and(|path| path.is_file()));
                }
                detail.manifest = Some(manifest);
            }
            Err(error) => {
                detail.exists = false;
                detail.diagnostics.push(format!(
                    "无法读取版本文件（{}）：{error}",
                    directory.display()
                ));
            }
        }
        detail.directory_path = Some(directory);
        Ok(detail)
    }

    pub fn knowledge_path(&self, id: &str) -> Result<PathBuf, CoreError> {
        validate_part(id)?;
        safe_path(&self.root, &format!("engine-knowledge/{id}.json"))
    }

    pub fn knowledge_detail(&self, id: &str) -> Result<Value, CoreError> {
        read_bounded_json(&self.knowledge_path(id)?, 512 * 1024)
    }

    pub fn set_family_enabled(&self, ids: &[String], enabled: bool) -> Result<(), CoreError> {
        let _write = write_gate()?;
        for id in ids {
            validate_family_id(id)?;
        }
        let mut cached = self.lock()?;
        let mut state = load_state(&self.root)?;
        let families = state
            .get_mut("families")
            .and_then(Value::as_object_mut)
            .ok_or_else(|| invalid("引擎启停状态无效。"))?;
        let mut index = load_index(&self.root)?;
        for id in ids {
            let setting = families
                .entry(id)
                .or_insert_with(|| json!({"enabled":true,"releases":[],"excludedVersions":[]}));
            setting
                .as_object_mut()
                .ok_or_else(|| invalid("引擎启停状态无效。"))?
                .insert("enabled".into(), Value::Bool(enabled));
            if let Some(family) = index.families.get_mut(id) {
                family.enabled = enabled;
            }
        }
        write_json(&safe_path(&self.root, "library-state.json")?, &state)?;
        self.commit_index(&mut cached, index)
    }

    pub fn disabled_family_ids(&self) -> Result<HashSet<String>, CoreError> {
        Ok(self
            .lock()?
            .index
            .families
            .iter()
            .filter(|(_, family)| !family.enabled)
            .map(|(id, _)| id.clone())
            .collect())
    }

    pub fn candidate_package_directories(&self) -> Result<Vec<PathBuf>, CoreError> {
        Ok(self
            .discovery_candidates(None)?
            .into_iter()
            .map(|candidate| candidate.directory)
            .collect())
    }

    /// A project pin is explicitly supplied by its caller and remains usable when the family is disabled.
    pub fn discovery_candidates(
        &self,
        pinned: Option<(&str, &str)>,
    ) -> Result<Vec<EngineDiscoveryCandidate>, CoreError> {
        self.candidates_matching(pinned, None)
    }

    fn candidates_matching(
        &self,
        pinned: Option<(&str, &str)>,
        matching: Option<&MatchingVersions>,
    ) -> Result<Vec<EngineDiscoveryCandidate>, CoreError> {
        if let Some((id, version)) = pinned {
            validate_family_id(id)?;
            validate_part(version)?;
        }
        let cached = self.lock()?;
        let mut candidates = Vec::new();
        let mut pin_known = false;
        let families = cached.index.matching_families(matching);
        for (id, family) in families {
            let versions: Box<dyn Iterator<Item = _>> = match matching {
                Some(matching) => Box::new(
                    matching[id]
                        .iter()
                        .filter_map(|v| family.versions.get_key_value(v)),
                ),
                None => Box::new(family.versions.iter()),
            };
            for (version, entry) in versions {
                let is_pinned = pinned
                    .is_some_and(|(pin_id, pin_version)| pin_id == id && pin_version == version);
                pin_known |= is_pinned;
                if entry.source != 1 || (!is_pinned && (!family.enabled || !entry.is_candidate)) {
                    continue;
                }
                let descriptor = entry
                    .extra
                    .get("discovery")
                    .filter(|value| !value.is_null())
                    .and_then(|value| serde_json::from_value(value.clone()).ok());
                candidates.push(EngineDiscoveryCandidate {
                    adapter_id: id.clone(),
                    version: version.clone(),
                    directory: self.root.join("packages").join(id).join(version),
                    published_at: entry.published_at.clone(),
                    descriptor,
                    is_pinned,
                    is_known_game: false,
                    coverage_confirmed: false,
                });
            }
        }
        if let Some((id, version)) = pinned
            && !pin_known
        {
            candidates.push(EngineDiscoveryCandidate {
                adapter_id: id.into(),
                version: version.into(),
                directory: self.root.join("packages").join(id).join(version),
                published_at: None,
                descriptor: None,
                is_pinned: true,
                is_known_game: false,
                coverage_confirmed: false,
            });
        }
        candidates.sort_by(|left, right| {
            right
                .is_pinned
                .cmp(&left.is_pinned)
                .then_with(|| left.adapter_id.cmp(&right.adapter_id))
                .then_with(|| right.published_at.cmp(&left.published_at))
                .then_with(|| left.version.cmp(&right.version))
        });
        Ok(candidates)
    }

    /// Adds or updates one saved experience without scanning packages or other entries.
    pub fn register_knowledge(&self, id: &str) -> Result<(), CoreError> {
        let _write = write_gate()?;
        let summary = read_knowledge_summary(&self.knowledge_path(id)?)?;
        let mut cached = self.lock()?;
        let mut index = load_index(&self.root)?;
        index.knowledge.insert(summary.id.clone(), summary);
        self.commit_index(&mut cached, index)
    }

    pub fn set_knowledge_enabled(&self, id: &str, enabled: bool) -> Result<(), CoreError> {
        let _write = write_gate()?;
        let mut cached = self.lock()?;
        let path = self.knowledge_path(id)?;
        let mut entry: Value = read_bounded_json(&path, 512 * 1024)?;
        let field = if entry.get("Enabled").is_some() {
            "Enabled"
        } else {
            "enabled"
        };
        entry
            .as_object_mut()
            .ok_or_else(|| invalid("引擎经验格式无效。"))?
            .insert(field.into(), Value::Bool(enabled));
        let mut index = load_index(&self.root)?;
        let summary = index
            .knowledge
            .get_mut(id)
            .ok_or_else(|| invalid("引擎经验已不存在，请刷新列表。"))?;
        summary.enabled = enabled;
        write_json(&path, &entry)?;
        self.commit_index(&mut cached, index)
    }

    /// Registers a committed, verified package after engine publication.
    pub fn register_package(
        &self,
        package_directory: &Path,
        game: Option<GameCompatibility>,
    ) -> Result<(), CoreError> {
        self.register_package_internal(package_directory, game, false, None)
    }

    fn register_package_internal(
        &self,
        package_directory: &Path,
        game: Option<GameCompatibility>,
        trial: bool,
        replaces: Option<&AdapterVersion>,
    ) -> Result<(), CoreError> {
        let _write = write_gate()?;
        let package: Value = read_bounded_json(
            &safe_path(package_directory, "package.json")?,
            16 * 1024 * 1024,
        )?;
        let id = property(&package, "id")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid("发布包缺少标识。"))?;
        let version = property(&package, "version")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid("发布包缺少版本。"))?;
        let expected = self.version_directory(id, version)?;
        if std::path::absolute(package_directory)? != expected {
            return Err(invalid("发布包不在当前引擎库。"));
        }
        let family: AdapterFamily = serde_json::from_value(
            property(&package, "family")
                .cloned()
                .ok_or_else(|| invalid("发布包缺少分类。"))?,
        )
        .map_err(|error| invalid(error.to_string()))?;
        family.validate()?;
        if family.adapter_id() != id {
            return Err(invalid("发布包分类与标识不一致。"));
        }
        if game
            .as_ref()
            .is_some_and(|game| game.adapter_version != version)
        {
            return Err(invalid("兼容记录与发布版本不一致。"));
        }
        let mut cached = self.lock()?;
        let mut state = load_state(&self.root)?;
        let predecessors = if let (Some(game), Some(previous)) = (&game, replaces) {
            let mut versions =
                lifecycle(&state["families"][&previous.adapter_id], &previous.version)?
                    .replaces
                    .remove(&game.game_id)
                    .unwrap_or_default();
            if !versions.contains(previous) {
                versions.push(previous.clone());
            }
            versions.retain(|previous| previous.adapter_id != id || previous.version != version);
            versions
        } else {
            Vec::new()
        };
        let setting = state
            .get_mut("families")
            .and_then(Value::as_object_mut)
            .ok_or_else(|| invalid("引擎库状态无效。"))?
            .entry(id)
            .or_insert_with(|| json!({"enabled":true,"releases":[],"excludedVersions":[]}));
        let releases = setting
            .get_mut("releases")
            .ok_or_else(|| invalid("引擎发布记录无效。"))?;
        if releases.is_null() {
            *releases = json!([]);
        }
        let releases = releases
            .as_array_mut()
            .ok_or_else(|| invalid("引擎发布记录无效。"))?;
        let published_at = chrono::Utc::now().to_rfc3339();
        if !releases
            .iter()
            .any(|release| release.get("version").and_then(Value::as_str) == Some(version))
        {
            releases.push(json!({"id":id,"version":version,"activatedAt":published_at,"gameId":game.as_ref().map(|game| &game.game_id)}));
        }
        if let Some(excluded) = setting
            .get_mut("excludedVersions")
            .and_then(Value::as_array_mut)
        {
            excluded.retain(|excluded| excluded.as_str() != Some(version));
        }
        let enabled = setting
            .get("enabled")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let mut index = load_index(&self.root)?;
        let stored = index
            .families
            .entry(id.into())
            .or_insert_with(|| empty_family(family.clone()));
        stored.family = family.clone();
        stored.enabled = enabled;
        let mut extra = BTreeMap::new();
        let mut lifecycle = lifecycle(setting, version)?;
        if trial {
            lifecycle.requires_confirmation = true;
            if let Some(game) = &game
                && !predecessors.is_empty()
            {
                lifecycle
                    .replaces
                    .insert(game.game_id.clone(), predecessors);
            }
        }
        save_lifecycle(setting, version, &lifecycle)?;
        extra.insert("lifecycle".into(), json!(lifecycle));
        extra.insert("discoveryLoaded".into(), Value::Bool(true));
        extra.insert("discovery".into(), package_discovery(&package));
        stored.versions.insert(
            version.into(),
            IndexVersion {
                version: version.into(),
                source: 1,
                exists: true,
                published_at: Some(published_at),
                is_candidate: lifecycle.can_reuse(),
                extra,
            },
        );
        if let Some(mut game) = game {
            game.updated_at = Some(chrono::Utc::now().to_rfc3339());
            if let Some(feedback) = lifecycle.feedback.get(&game.game_id) {
                game.runtime_status = feedback.status.clone();
            }
            stored.games.retain(|prior| prior.game_id != game.game_id);
            stored.games.push(game);
            let path = safe_path(&self.root, &format!("catalog/{id}/games.json"))?;
            write_json(&path, &json!({"family": family, "games":stored.games}))?;
        }
        write_json(&safe_path(&self.root, "library-state.json")?, &state)?;
        self.commit_index(&mut cached, index)
    }

    /// Releases a deleted project's pins while retaining its published engine capability and compatibility history.
    pub fn forget_game(&self, game_id: &str) -> Result<(), CoreError> {
        let _write = write_gate()?;
        if game_id.trim().is_empty() {
            return Err(invalid("游戏项目标识不能为空。"));
        }
        // The existing registry hashes the raw project identity, independently of its project-directory name.
        let directory = safe_path(&self.root, &format!("games/{}", hash_text(game_id)))?;
        let active = safe_path(&directory, "active.json")?;
        let previous = safe_path(&directory, "previous.json")?;
        let mut cached = self.lock()?;
        if active.is_file() {
            let activation: Value = read_bounded_json(&active, 4096)?;
            let id = property(&activation, "id")
                .and_then(Value::as_str)
                .ok_or_else(|| invalid("引擎激活记录缺少标识。"))?;
            let version = property(&activation, "version")
                .and_then(Value::as_str)
                .ok_or_else(|| invalid("引擎激活记录缺少版本。"))?;
            let package = self.version_directory(id, version)?;
            if package.is_dir() {
                let mut state = load_state(&self.root)?;
                let families = state
                    .get_mut("families")
                    .and_then(Value::as_object_mut)
                    .ok_or_else(|| invalid("引擎库启停状态无效。"))?;
                let setting = families
                    .entry(id)
                    .or_insert_with(|| json!({"enabled":true,"releases":[],"excludedVersions":[]}));
                let setting = setting
                    .as_object_mut()
                    .ok_or_else(|| invalid("引擎发布记录无效。"))?;
                let releases = setting.entry("releases").or_insert_with(|| json!([]));
                if releases.is_null() {
                    *releases = json!([]);
                }
                let releases = releases
                    .as_array_mut()
                    .ok_or_else(|| invalid("引擎发布记录无效。"))?;
                let release = releases
                    .iter()
                    .find(|release| {
                        property(release, "version").and_then(Value::as_str) == Some(version)
                    })
                    .cloned()
                    .unwrap_or_else(|| activation.clone());
                releases.retain(|release| {
                    property(release, "version").and_then(Value::as_str) != Some(version)
                });
                releases.push(release);
                let enabled = setting
                    .get("enabled")
                    .and_then(Value::as_bool)
                    .unwrap_or(true);
                let excluded = setting
                    .get("excludedVersions")
                    .and_then(Value::as_array)
                    .is_some_and(|versions| {
                        versions.iter().any(|item| item.as_str() == Some(version))
                    });
                let mut index = load_index(&self.root)?;
                let (kind, key) = id
                    .split_once('.')
                    .ok_or_else(|| invalid("引擎分类标识无效。"))?;
                let stored = index.families.entry(id.into()).or_insert_with(|| {
                    empty_family(AdapterFamily {
                        kind: kind.into(),
                        key: key.into(),
                        name: key.into(),
                    })
                });
                stored.enabled = enabled;
                let entry = stored
                    .versions
                    .entry(version.into())
                    .or_insert_with(|| IndexVersion {
                        version: version.into(),
                        source: 1,
                        exists: true,
                        published_at: property(&activation, "activatedAt")
                            .and_then(Value::as_str)
                            .map(str::to_owned),
                        is_candidate: false,
                        extra: BTreeMap::new(),
                    });
                if entry.source == 2 {
                    entry.source = 1;
                    entry.exists = true;
                }
                let lifecycle = lifecycle(&Value::Object(setting.clone()), version)?;
                entry.is_candidate = !excluded && lifecycle.can_reuse();
                entry.extra.insert("lifecycle".into(), json!(lifecycle));
                // Commit retention before removing either pin. Any failure leaves the project references available for retry.
                write_json(&safe_path(&self.root, "library-state.json")?, &state)?;
                self.commit_index(&mut cached, index)?;
            }
        }
        if active.is_file() {
            fs::remove_file(active)?;
        }
        if previous.is_file() {
            fs::remove_file(previous)?;
        }
        if directory.is_dir() && fs::read_dir(&directory)?.next().is_none() {
            fs::remove_dir(directory)?;
        }
        Ok(())
    }

    /// Removes registry records and attempts package deletion, retaining project data.
    /// Unavailable payloads are reported without blocking record cleanup. Running versions cannot be removed.
    pub fn delete_version(
        &self,
        adapter_id: &str,
        version: &str,
        runtime_references: &[EngineLibraryReference],
    ) -> Result<EngineLibraryDeleteResult, CoreError> {
        let used: Vec<_> = runtime_references
            .iter()
            .filter(|reference| {
                reference.adapter_id == adapter_id
                    && reference.version == version
                    && reference.kind.eq_ignore_ascii_case("running")
            })
            .cloned()
            .collect();
        if !used.is_empty() {
            return Ok(EngineLibraryDeleteResult {
                deleted: false,
                references: used,
                diagnostics: Vec::new(),
            });
        }
        let _write = write_gate()?;
        self.delete_version_locked(adapter_id, version, load_state(&self.root)?, None)
    }

    pub(crate) fn retire_project_version(
        &self,
        previous: &AdapterVersion,
        game_id: &str,
        in_use: bool,
    ) -> Result<Option<EngineLibraryDeleteResult>, CoreError> {
        let _write = write_gate()?;
        let state = load_state(&self.root)?;
        // Older queues included unrelated releases from the same family. Require
        // positive ownership evidence before removing any files or references.
        let family = &state["families"][&previous.adapter_id];
        let releases = family["releases"].as_array();
        let mut matching = releases
            .into_iter()
            .flatten()
            .filter(|release| release["version"].as_str() == Some(&previous.version))
            .peekable();
        if !lifecycle(family, &previous.version)?.requires_confirmation
            || matching.peek().is_none()
            || !matching.all(|r| r["gameId"].as_str() == Some(game_id))
        {
            return Ok(None);
        }
        if in_use {
            return Ok(Some(EngineLibraryDeleteResult::default()));
        }
        self.delete_version_locked(
            &previous.adapter_id,
            &previous.version,
            state,
            Some(game_id),
        )
        .map(Some)
    }

    fn delete_version_locked(
        &self,
        adapter_id: &str,
        version: &str,
        mut state: Value,
        retiring_project: Option<&str>,
    ) -> Result<EngineLibraryDeleteResult, CoreError> {
        let path = self.package_path(adapter_id, version)?;
        let mut cached = self.lock()?;
        let mut index = load_index(&self.root)?;
        if index
            .families
            .get(adapter_id)
            .and_then(|family| family.versions.get(version))
            .is_some_and(|entry| entry.source == 0)
        {
            return Err(invalid("内置引擎不能删除。"));
        }
        let mut writes: Vec<(PathBuf, Option<Value>)> = Vec::new();
        plan_reference_removal(&self.root, adapter_id, Some(version), &mut writes)?;
        if let Some(game_id) = retiring_project {
            let own_pins = self.root.join("games").join(hash_text(game_id));
            // A runtime migration or another game's legacy pin still needs this package.
            let shared_reference = writes
                .iter()
                .any(|(file, _)| file.parent() != Some(own_pins.as_path()));
            let used_by_other_game = index.families.get(adapter_id).is_some_and(|family| {
                family
                    .games
                    .iter()
                    .any(|g| g.game_id != game_id && g.adapter_version == version)
            }) || lifecycle(&state["families"][adapter_id], version)?
                .feedback
                .iter()
                .any(|(id, feedback)| id != game_id && feedback.status == "passed");
            if shared_reference || used_by_other_game {
                return Ok(EngineLibraryDeleteResult::default());
            }
        }
        let catalog_dir = safe_path(&self.root, &format!("catalog/{adapter_id}"))?;
        for file in json_files(&catalog_dir)? {
            let mut record: Value = read_bounded_json(&file, 16 * 1024 * 1024)?;
            if let Some(games) = record.get_mut("games").and_then(Value::as_array_mut) {
                games.retain(|game| {
                    property(game, "adapterVersion").and_then(Value::as_str) != Some(version)
                });
                let empty = games.is_empty();
                writes.push((file, (!empty).then_some(record)));
            } else if record
                .get("game")
                .and_then(|game| property(game, "adapterVersion"))
                .and_then(Value::as_str)
                == Some(version)
            {
                writes.push((file, None));
            }
        }
        if let Some(family) = state
            .get_mut("families")
            .and_then(|value| value.get_mut(adapter_id))
        {
            if let Some(lifecycles) = family
                .get_mut("versionLifecycle")
                .and_then(Value::as_object_mut)
            {
                lifecycles.remove(version);
            }
            if let Some(releases) = family.get_mut("releases").and_then(Value::as_array_mut) {
                releases.retain(|release| {
                    property(release, "version").and_then(Value::as_str) != Some(version)
                });
            }
            if let Some(excluded) = family
                .get_mut("excludedVersions")
                .and_then(Value::as_array_mut)
            {
                excluded.retain(|item| item.as_str() != Some(version));
            }
        }
        // Deleted or inaccessible payloads must not prevent removal of their registry records.
        let payload_result = (|| -> Result<(), CoreError> {
            validate_tree(&path)?;
            match fs::remove_dir_all(&path) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(error.into()),
            }
        })();
        let mut diagnostics = Vec::new();
        if let Err(error) = payload_result {
            if retiring_project.is_some() {
                return Err(error);
            }
            diagnostics.push(format!(
                "版本文件未能删除，请关闭占用程序或检查目录权限后手动删除（{}）：{error}",
                path.display()
            ));
        }
        // Metadata is prepared before deleting payloads; retry finishes an interrupted cleanup.
        let result = (|| -> Result<(), CoreError> {
            apply_metadata_changes(writes)?;
            write_json(&safe_path(&self.root, "library-state.json")?, &state)?;
            if let Some(family) = index.families.get_mut(adapter_id) {
                family.versions.remove(version);
                family.games.retain(|game| game.adapter_version != version);
                if family.versions.is_empty() && family.games.is_empty() {
                    index.families.remove(adapter_id);
                }
            }
            self.commit_index(&mut cached, index)
        })();
        if let Err(error) = result {
            diagnostics.insert(0, format!("版本关联记录清理未完成，请重试：{error}"));
            return Err(invalid(diagnostics.join("\n")));
        }
        Ok(EngineLibraryDeleteResult {
            deleted: true,
            references: Vec::new(),
            diagnostics,
        })
    }

    /// Removes the published packages and shared knowledge owned by one external family.
    pub fn delete_family(&self, adapter_id: &str) -> Result<(), CoreError> {
        validate_family_id(adapter_id)?;
        let _write = write_gate()?;
        let mut cached = self.lock()?;
        let mut index = load_index(&self.root)?;
        let family = index.families.get(adapter_id);
        if family.is_some_and(|f| f.versions.values().any(|version| version.source == 0)) {
            return Err(invalid("内置引擎随程序提供，不能删除。"));
        }
        let owns_knowledge =
            |entry: &EngineKnowledgeSummary| entry.family.adapter_id() == adapter_id;
        if family.is_none() && !index.knowledge.values().any(owns_knowledge) {
            return Err(invalid("找不到要删除的引擎。"));
        }

        let packages = safe_path(&self.root, &format!("packages/{adapter_id}"))?;
        let records = safe_path(&self.root, &format!("catalog/{adapter_id}"))?;
        validate_tree(&packages)?;
        validate_tree(&records)?;
        let mut writes = Vec::new();
        plan_reference_removal(&self.root, adapter_id, None, &mut writes)?;
        let prefix = format!("{adapter_id}.");
        for file in json_files(&safe_path(&self.root, "engine-knowledge")?)? {
            // Family and knowledge keys can both contain dots. Check ownership in the file.
            if file
                .file_stem()
                .and_then(|name| name.to_str())
                .is_some_and(|id| id.starts_with(&prefix))
                && owns_knowledge(&read_knowledge_summary(&file)?)
            {
                writes.push((file, None));
            }
        }
        let mut state = load_state(&self.root)?;
        if let Some(families) = state["families"].as_object_mut() {
            families.remove(adapter_id);
        }
        writes.push((safe_path(&self.root, "library-state.json")?, Some(state)));
        for directory in [packages, records] {
            if directory.try_exists()? {
                fs::remove_dir_all(&directory).map_err(|error| {
                    invalid(format!("无法删除 {}：{error}", directory.display()))
                })?;
            }
        }
        apply_metadata_changes(writes)?;
        index.families.remove(adapter_id);
        index.knowledge.retain(|_, entry| !owns_knowledge(entry));
        self.commit_index(&mut cached, index)
    }

    fn commit_index(&self, cached: &mut CachedIndex, index: LibraryIndex) -> Result<(), CoreError> {
        let path = safe_path(&self.root, "library-index.json")?;
        let mut persisted = index.clone();
        for family in persisted.families.values_mut() {
            family.versions.retain(|_, version| version.source != 0);
        }
        if let Err(error) = write_json(&path, &persisted) {
            if path.is_file() {
                fs::remove_file(path)?;
            }
            return Err(error.into());
        }
        CATALOG_REVISION.fetch_add(1, std::sync::atomic::Ordering::Release);
        *cached = CachedIndex::new(index);
        Ok(())
    }

    fn lock(&self) -> Result<MutexGuard<'_, CachedIndex>, CoreError> {
        self.cached
            .lock()
            .map_err(|_| invalid("引擎索引锁已失效，请重新打开。"))
    }
}

fn plan_reference_removal(
    root: &Path,
    adapter_id: &str,
    version: Option<&str>,
    writes: &mut Vec<(PathBuf, Option<Value>)>,
) -> Result<(), CoreError> {
    let mapping_path = safe_path(root, "runtime-migrations.json")?;
    if mapping_path.is_file() {
        let mut mappings: Value = read_bounded_json(&mapping_path, 16 * 1024 * 1024)?;
        let matches = |identity: &str| {
            identity
                .split_once('/')
                .is_some_and(|(id, v)| id == adapter_id && version.is_none_or(|wanted| wanted == v))
        };
        let entries = mappings
            .get_mut("entries")
            .and_then(Value::as_object_mut)
            .ok_or_else(|| invalid("适配器运行包映射索引无效。"))?;
        let previous_count = entries.len();
        entries.retain(|source, entry| {
            !matches(source)
                && !entry
                    .get("target")
                    .and_then(Value::as_str)
                    .is_some_and(matches)
        });
        if entries.len() != previous_count {
            writes.push((mapping_path, Some(mappings)));
        }
    }
    for directory in directories(&safe_path(root, "games")?)? {
        for name in ["active.json", "previous.json"] {
            let pin = safe_path(&directory, name)?;
            if pin.is_file() {
                let activation: Value = read_bounded_json(&pin, 4096)?;
                if property(&activation, "id").and_then(Value::as_str) == Some(adapter_id)
                    && version.is_none_or(|wanted| {
                        property(&activation, "version").and_then(Value::as_str) == Some(wanted)
                    })
                {
                    writes.push((pin, None));
                }
            }
        }
    }
    Ok(())
}

fn apply_metadata_changes(writes: Vec<(PathBuf, Option<Value>)>) -> Result<(), CoreError> {
    for (file, value) in writes {
        if let Some(value) = value {
            write_json(&file, &value)?;
        } else if file.exists() {
            fs::remove_file(file)?;
        }
    }
    Ok(())
}

fn load_index(root: &Path) -> Result<LibraryIndex, CoreError> {
    let path = safe_path(root, "library-index.json")?;
    let mut index = if path.is_file() {
        match read_bounded_json::<LibraryIndex>(&path, 64 * 1024 * 1024).and_then(|index| {
            validate_index(&index)?;
            Ok(index)
        }) {
            Ok(index) => index,
            Err(error) => {
                let mut index = rebuild_index(root)?;
                index
                    .diagnostics
                    .push(format!("原索引无法读取，已从注册元数据重建：{error}"));
                index
            }
        }
    } else {
        rebuild_index(root)?
    };
    add_builtins(&mut index);
    let state = load_state(root)?;
    if let Some(families) = state.get("families").and_then(Value::as_object) {
        for (id, setting) in families {
            if let Some(family) = index.families.get_mut(id) {
                family.enabled = setting
                    .get("enabled")
                    .and_then(Value::as_bool)
                    .unwrap_or(true);
                let released: HashSet<_> = setting["releases"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|v| v["version"].as_str())
                    .collect();
                let excluded: HashSet<_> = setting["excludedVersions"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .collect();
                for (version, entry) in &mut family.versions {
                    let lifecycle = lifecycle(setting, version)?;
                    if setting
                        .get("versionLifecycle")
                        .and_then(|values| values.get(version))
                        .is_some()
                    {
                        entry.is_candidate = released.contains(version.as_str())
                            && !excluded.contains(version.as_str())
                            && lifecycle.can_reuse();
                        entry.extra.insert("lifecycle".into(), json!(lifecycle));
                    }
                }
            }
        }
    }
    Ok(index)
}

fn validate_index(index: &LibraryIndex) -> Result<(), CoreError> {
    if index.schema_version != 1 {
        return Err(invalid("引擎索引版本不支持。"));
    }
    for (id, family) in &index.families {
        family.family.validate()?;
        if family.family.adapter_id() != *id {
            return Err(invalid("引擎索引分类与标识不一致。"));
        }
        for (version, entry) in &family.versions {
            validate_part(version)?;
            EngineLibrarySource::from_number(entry.source)?;
            if entry.version != *version {
                return Err(invalid("引擎索引版本与标识不一致。"));
            }
        }
    }
    for (id, summary) in &index.knowledge {
        validate_part(id)?;
        summary.family.validate()?;
        if *id != summary.id {
            return Err(invalid("引擎经验索引标识不一致。"));
        }
    }
    Ok(())
}

fn rebuild_index(root: &Path) -> Result<LibraryIndex, CoreError> {
    let mut index = LibraryIndex::default();
    for directory in directories(&safe_path(root, "catalog")?)? {
        let consolidated = safe_path(&directory, "games.json")?;
        let files = if consolidated.is_file() {
            vec![consolidated]
        } else {
            json_files(&directory)?
        };
        for path in files {
            let result = (|| -> Result<(), CoreError> {
                let record: Value = read_bounded_json(&path, 16 * 1024 * 1024)?;
                let family: AdapterFamily = serde_json::from_value(
                    property(&record, "family")
                        .cloned()
                        .ok_or_else(|| invalid("兼容记录缺少分类。"))?,
                )
                .map_err(|error| invalid(error.to_string()))?;
                family.validate()?;
                if directory.file_name().and_then(|name| name.to_str())
                    != Some(&family.adapter_id())
                {
                    return Err(invalid("兼容记录与目录不一致。"));
                }
                let games = if let Some(games) = record.get("games") {
                    games.clone()
                } else {
                    json!([record
                        .get("game")
                        .cloned()
                        .ok_or_else(|| invalid("兼容记录缺少游戏。"))?])
                };
                let games: Vec<GameCompatibility> =
                    serde_json::from_value(games).map_err(|error| invalid(error.to_string()))?;
                let stored = index
                    .families
                    .entry(family.adapter_id())
                    .or_insert_with(|| empty_family(family));
                for game in games {
                    validate_part(&game.adapter_version)?;
                    stored
                        .versions
                        .entry(game.adapter_version.clone())
                        .or_insert_with(|| IndexVersion {
                            version: game.adapter_version.clone(),
                            source: 2,
                            exists: false,
                            published_at: None,
                            is_candidate: false,
                            extra: BTreeMap::new(),
                        });
                    if let Some(prior) = stored
                        .games
                        .iter_mut()
                        .find(|prior| prior.game_id == game.game_id)
                    {
                        if game.updated_at >= prior.updated_at {
                            *prior = game;
                        }
                    } else {
                        stored.games.push(game);
                    }
                }
                Ok(())
            })();
            if let Err(error) = result {
                index
                    .diagnostics
                    .push(format!("无法读取兼容记录 {}：{error}", path.display()));
            }
        }
    }
    let state = load_state(root)?;
    for family_directory in directories(&safe_path(root, "packages")?)? {
        let id = family_directory
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        if validate_family_id(id).is_err() {
            continue;
        }
        for directory in directories(&family_directory)? {
            let version = directory
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default();
            if version.contains(".pending-") || validate_part(version).is_err() {
                continue;
            }
            let package: Value = match read_bounded_json(
                &safe_path(&directory, "package.json")?,
                16 * 1024 * 1024,
            ) {
                Ok(package) => package,
                Err(error) => {
                    index
                        .diagnostics
                        .push(format!("无法读取外置引擎 {id}/{version}：{error}"));
                    continue;
                }
            };
            let family: AdapterFamily = match property(&package, "family")
                .cloned()
                .and_then(|family| serde_json::from_value(family).ok())
            {
                Some(family) => family,
                None => continue,
            };
            if family.validate().is_err()
                || family.adapter_id() != id
                || property(&package, "version").and_then(Value::as_str) != Some(version)
            {
                continue;
            }
            let setting = state.get("families").and_then(|families| families.get(id));
            let candidate = setting
                .and_then(|setting| setting.get("releases"))
                .and_then(Value::as_array)
                .is_some_and(|releases| {
                    releases.iter().any(|release| {
                        release.get("version").and_then(Value::as_str) == Some(version)
                    })
                })
                && !setting
                    .and_then(|setting| setting.get("excludedVersions"))
                    .and_then(Value::as_array)
                    .is_some_and(|excluded| {
                        excluded.iter().any(|item| item.as_str() == Some(version))
                    });
            let published_at = safe_path(&directory, "verification.json")
                .ok()
                .filter(|path| path.is_file())
                .and_then(|path| read_bounded_json::<Value>(&path, 1024 * 1024).ok())
                .and_then(|value| {
                    value
                        .get("verifiedAt")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                });
            let stored = index
                .families
                .entry(id.into())
                .or_insert_with(|| empty_family(family.clone()));
            stored.family = family;
            stored.versions.insert(
                version.into(),
                IndexVersion {
                    version: version.into(),
                    source: 1,
                    exists: true,
                    published_at,
                    is_candidate: candidate,
                    extra: BTreeMap::from([
                        ("discovery".into(), package_discovery(&package)),
                        ("discoveryLoaded".into(), Value::Bool(true)),
                    ]),
                },
            );
        }
    }
    for file in json_files(&safe_path(root, "engine-knowledge")?)? {
        match read_knowledge_summary(&file) {
            Ok(summary) => {
                index.knowledge.insert(summary.id.clone(), summary);
            }
            Err(error) => index.diagnostics.push(error.to_string()),
        }
    }
    Ok(index)
}

fn read_knowledge_summary(file: &Path) -> Result<EngineKnowledgeSummary, CoreError> {
    let entry: Value = read_bounded_json(file, 512 * 1024)?;
    let family: AdapterFamily = serde_json::from_value(
        property(&entry, "family")
            .cloned()
            .ok_or_else(|| invalid("经验缺少分类。"))?,
    )
    .map_err(|error| invalid(error.to_string()))?;
    family.validate()?;
    let key = property(&entry, "key")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("经验缺少标识。"))?;
    let id = format!("{}.{key}", family.adapter_id());
    validate_part(&id)?;
    if file.file_stem().and_then(|name| name.to_str()) != Some(&id) {
        return Err(invalid("经验标识与文件不一致。"));
    }
    Ok(EngineKnowledgeSummary {
        id,
        family,
        aliases: serde_json::from_value(property(&entry, "aliases").cloned().unwrap_or(json!([])))
            .map_err(|error| invalid(error.to_string()))?,
        conditions: property(&entry, "conditions")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .into(),
        summary: property(&entry, "summary")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .into(),
        origins: property(&entry, "origins")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default(),
        revision: property(&entry, "revision")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .into(),
        enabled: property(&entry, "enabled")
            .and_then(Value::as_bool)
            .unwrap_or(true),
        tool_count: property(&entry, "tools")
            .and_then(Value::as_array)
            .map_or(0, Vec::len),
        extra: BTreeMap::new(),
    })
}

fn empty_family(family: AdapterFamily) -> IndexFamily {
    IndexFamily {
        family,
        enabled: true,
        versions: BTreeMap::new(),
        games: Vec::new(),
        extra: BTreeMap::new(),
    }
}

fn add_builtins(index: &mut LibraryIndex) {
    for format in gamel10n_engines::vntextpatch::FORMATS {
        let name = match *format {
            "AdvHd" => "AdvHD / WS2",
            "Age" => "ArcGameEngine / BIN",
            "ArtemisAsb" => "Artemis / ASB",
            "ArtemisAst" => "Artemis / AST",
            "ArtemisTxt" => "Artemis / text",
            "CatSystem" => "CatSystem2 / CST",
            "CSystem" => "Cyberworks C,system / A0",
            "Ethornell" => "Ethornell / BGI",
            "Kaguya" => "KaGuYa / message.dat",
            "KirikiriKs" => "Kirikiri / KS",
            "KirikiriSoc" => "Kirikiri / SOC",
            "KirikiriTjs" => "Kirikiri / compiled TJS",
            "Majiro" => "Majiro / MJO",
            "Musica" => "Musica / SC",
            "Mware" => "Mware / NUT",
            "Propeller" => "Propeller / Stuff Script Engine / MSC",
            "Qlie" => "QLIE / S",
            "RealLive" => "RealLive / scenario",
            "Renpy" => "Ren'Py / RPY",
            "ShSystem" => "ShSystem / HST",
            "SilkysMap" => "Silky's / AI6WIN / MAP",
            "SilkysMes" => "Silky's / AI6WIN / MES",
            "Softpal" => "Softpal / SRC + DAT",
            "SystemNnnDev" => "SystemNNN / NNN",
            "SystemNnnRelease" => "SystemNNN / SPT",
            "TmrHiroAdvSystemCode" => "TmrHiroAdvSystem / SRP",
            "TmrHiroAdvSystemText" => "TmrHiroAdvSystem / text table",
            "Whale" => "Whale / text",
            "Yuris" => "YU-RIS / YBN",
            _ => format,
        };
        insert_builtin(
            index,
            AdapterFamily {
                kind: "engine".into(),
                key: format!("vntextpatch-{}", format.to_lowercase()),
                name: name.into(),
            },
            "1.0.1",
        );
    }
    insert_builtin(
        index,
        AdapterFamily {
            kind: "engine".into(),
            key: "siglus".into(),
            name: "Siglus / Scene.pck".into(),
        },
        "1.0.0",
    );
    insert_builtin(
        index,
        AdapterFamily {
            kind: "engine".into(),
            key: "sfa16.aos-v2".into(),
            name: "LiLiM SFA16 / AOS v2 / SCR".into(),
        },
        "2.0.4",
    );
    insert_builtin(
        index,
        AdapterFamily {
            kind: "engine".into(),
            key: "kirikiri-smil-smile".into(),
            name: "KiriKiri / SMIL / SmileCrypt / PSB SCN".into(),
        },
        "1",
    );
}

fn insert_builtin(index: &mut LibraryIndex, family: AdapterFamily, version: &str) {
    let stored = index
        .families
        .entry(family.adapter_id())
        .or_insert_with(|| empty_family(family));
    stored.versions.insert(
        version.into(),
        IndexVersion {
            version: version.into(),
            source: 0,
            exists: true,
            published_at: None,
            is_candidate: true,
            extra: BTreeMap::new(),
        },
    );
}

fn load_state(root: &Path) -> Result<Value, CoreError> {
    let path = safe_path(root, "library-state.json")?;
    if !path.is_file() {
        return Ok(json!({"schemaVersion":1,"families":{}}));
    }
    let state: Value = read_bounded_json(&path, 16 * 1024 * 1024)?;
    if state.get("schemaVersion").and_then(Value::as_u64) != Some(1)
        || !state.get("families").is_some_and(Value::is_object)
    {
        return Err(invalid("引擎库启停状态格式无效。"));
    }
    Ok(state)
}

fn property<'a>(value: &'a Value, name: &str) -> Option<&'a Value> {
    value.get(name).or_else(|| {
        value
            .as_object()?
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value)
    })
}

pub(crate) fn validate_part(value: &str) -> Result<(), CoreError> {
    if value.is_empty()
        || value.len() > 180
        || !value
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_alphanumeric())
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
        || value.ends_with('.')
    {
        return Err(invalid("无效的引擎或版本标识。"));
    }
    Ok(())
}

fn validate_family_id(id: &str) -> Result<(), CoreError> {
    validate_part(id)?;
    if !id.starts_with("engine.") && !id.starts_with("template.") {
        return Err(invalid("无效的引擎分类标识。"));
    }
    Ok(())
}

fn read_bounded_json<T: serde::de::DeserializeOwned>(
    path: &Path,
    maximum: u64,
) -> Result<T, CoreError> {
    crate::patching::check_path(path)?;
    if fs::metadata(path)?.len() > maximum {
        return Err(invalid("元数据文件超过大小限制。"));
    }
    Ok(read_json(path)?)
}

fn directories(root: &Path) -> Result<Vec<PathBuf>, CoreError> {
    crate::patching::check_path(root)?;
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut result = Vec::new();
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        crate::patching::check_path(&entry.path())?;
        if entry.file_type()?.is_dir() {
            result.push(entry.path());
        }
    }
    result.sort();
    Ok(result)
}

fn json_files(root: &Path) -> Result<Vec<PathBuf>, CoreError> {
    crate::patching::check_path(root)?;
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut result = Vec::new();
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        crate::patching::check_path(&entry.path())?;
        if entry.file_type()?.is_file()
            && entry
                .path()
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("json"))
        {
            result.push(entry.path());
        }
    }
    result.sort();
    Ok(result)
}

fn validate_tree(root: &Path) -> Result<(), CoreError> {
    crate::patching::check_path(root)?;
    if root.is_dir() {
        for entry in fs::read_dir(root)? {
            validate_tree(&entry?.path())?;
        }
    }
    Ok(())
}

fn invalid(message: impl Into<String>) -> CoreError {
    CoreError::InvalidData(message.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index(root: &Path, count: usize) {
        let mut families = serde_json::Map::new();
        for number in 0..count {
            let key = format!("fixture-{number:04}");
            families.insert(format!("engine.{key}"), json!({"family":{"kind":"engine","key":key,"name":format!("Fixture {number:04}")},"enabled":true,
                "versions":{"v1":{"version":"v1","source":1,"exists":true,"isCandidate":true,"discovery":{"recognition":{"required":["preserve"]}},"discoveryLoaded":true}},
                "games":[{"gameId":format!("game-{number}"),"title":format!("Game {number:04}"),"variant":"sample","adapterVersion":"v1","structureStatus":"passed","runtimeStatus":"not-tested","updatedAt":null}]}));
        }
        write_json(&root.join("library-index.json"), &json!({"schemaVersion":1,"families":families,"knowledge":{},"diagnostics":[],"futureProperty":{"keep":true}})).unwrap();
    }

    #[test]
    fn thousands_of_families_browse_from_index_and_check_missing_paths_only_on_detail() {
        let root = tempfile::tempdir().unwrap();
        index(root.path(), 2000);
        let catalog = EngineCatalog::from_extensions_root(root.path()).unwrap();
        let page = catalog.snapshot("Game 1999", 0, 30).unwrap();
        assert_eq!(page.total, 1);
        assert!(page.families[0].versions[0].exists);
        assert!(
            !catalog
                .version_detail("engine.fixture-1999", "v1")
                .unwrap()
                .exists
        );
        assert!(catalog.snapshot("Game 1999", 0, 30).unwrap().families[0].versions[0].exists);
        assert_eq!(
            catalog
                .snapshot("Fixture", 1500, 50)
                .unwrap()
                .families
                .len(),
            50
        );
        assert_eq!(catalog.snapshot("Fixture", 1500, 50).unwrap().total, 2000);
    }

    #[test]
    fn family_toggle_preserves_index_discovery_and_unknown_contract_fields() {
        let root = tempfile::tempdir().unwrap();
        index(root.path(), 1);
        let catalog = EngineCatalog::from_extensions_root(root.path()).unwrap();
        catalog
            .set_family_enabled(&["engine.fixture-0000".into()], false)
            .unwrap();
        assert!(!catalog.family("engine.fixture-0000").unwrap().enabled);
        let stored: Value = read_json(&root.path().join("library-index.json")).unwrap();
        assert_eq!(stored["futureProperty"]["keep"], true);
        assert_eq!(
            stored["families"]["engine.fixture-0000"]["versions"]["v1"]["discovery"]["recognition"]
                ["required"][0],
            "preserve"
        );
        assert!(
            catalog
                .disabled_family_ids()
                .unwrap()
                .contains("engine.fixture-0000")
        );
    }

    #[test]
    fn delete_version_removes_files_compatibility_pins_and_index_without_deleting_project_data() {
        let root = tempfile::tempdir().unwrap();
        index(root.path(), 1);
        let package = root.path().join("packages/engine.fixture-0000/v1");
        fs::create_dir_all(&package).unwrap();
        fs::write(package.join("payload"), b"fixture").unwrap();
        let compatibility = root.path().join("catalog/engine.fixture-0000/games.json");
        write_json(&compatibility, &json!({"family":{"kind":"engine","key":"fixture-0000","name":"Fixture"},"games":[{"adapterVersion":"v1"}]})).unwrap();
        let pin = root.path().join("games/game-0/active.json");
        write_json(&pin, &json!({"id":"engine.fixture-0000","version":"v1"})).unwrap();
        let migrations = root.path().join("runtime-migrations.json");
        write_json(
            &migrations,
            &json!({"schemaVersion":1,"entries":{
                "engine.fixture-0000/old":{"target":"engine.fixture-0000/v1"},
                "engine.other/old":{"target":"engine.other/python"}
            }}),
        )
        .unwrap();
        fs::write(pin.parent().unwrap().join("unrelated"), b"keep").unwrap();
        let catalog = EngineCatalog::from_extensions_root(root.path()).unwrap();
        let running = EngineLibraryReference {
            adapter_id: "engine.fixture-0000".into(),
            version: "v1".into(),
            game_id: None,
            game_title: None,
            kind: "running".into(),
            path: None,
        };
        assert!(
            !catalog
                .delete_version("engine.fixture-0000", "v1", &[running])
                .unwrap()
                .deleted
        );
        assert!(package.is_dir());
        assert!(
            catalog
                .delete_version("engine.fixture-0000", "v1", &[])
                .unwrap()
                .deleted
        );
        assert!(!package.exists());
        assert!(!compatibility.exists());
        assert!(!pin.exists());
        assert_eq!(
            read_json::<Value>(&migrations).unwrap()["entries"],
            json!({
                "engine.other/old":{"target":"engine.other/python"}
            })
        );
        assert!(pin.parent().unwrap().join("unrelated").exists());
        assert!(
            catalog
                .snapshot("Fixture 0000", 0, 30)
                .unwrap()
                .families
                .is_empty()
        );
    }

    #[test]
    fn delete_missing_version_cleans_metadata_and_does_not_return_after_index_rebuild() {
        let root = tempfile::tempdir().unwrap();
        index(root.path(), 2);
        let id = "engine.fixture-0000";
        let compatibility = root.path().join(format!("catalog/{id}/games.json"));
        write_json(&compatibility, &json!({"family":{"kind":"engine","key":"fixture-0000","name":"Fixture"},"games":[{"gameId":"game-0","title":"Game","adapterVersion":"v1"}]})).unwrap();
        let pin = root.path().join("games/game-0/active.json");
        write_json(&pin, &json!({"id":id,"version":"v1"})).unwrap();
        let state = root.path().join("library-state.json");
        write_json(&state, &json!({"schemaVersion":1,"families":{id:{"enabled":true,"releases":[{"version":"v1"}],"excludedVersions":["v1"]}}})).unwrap();
        let migrations = root.path().join("runtime-migrations.json");
        write_json(
            &migrations,
            &json!({"entries":{
                "engine.fixture-0000/v1":{"target":"engine.fixture-0000/v2"},
                "engine.other/v1":{"target":"engine.other/v2"}
            }}),
        )
        .unwrap();
        let catalog = EngineCatalog::from_extensions_root(root.path()).unwrap();
        assert!(!catalog.version_detail(id, "v1").unwrap().exists);

        let result = catalog.delete_version(id, "v1", &[]).unwrap();

        assert!(result.deleted);
        assert!(result.diagnostics.is_empty());
        assert!(!compatibility.exists());
        assert!(!pin.exists());
        assert!(catalog.family(id).is_err());
        assert!(catalog.family("engine.fixture-0001").is_ok());
        let state: Value = read_json(&state).unwrap();
        assert_eq!(state["families"][id]["releases"], json!([]));
        assert_eq!(state["families"][id]["excludedVersions"], json!([]));
        assert_eq!(
            read_json::<Value>(&migrations).unwrap()["entries"],
            json!({
                "engine.other/v1":{"target":"engine.other/v2"}
            })
        );
        assert!(
            EngineCatalog::from_extensions_root(root.path())
                .unwrap()
                .family(id)
                .is_err()
        );
        fs::remove_file(root.path().join("library-index.json")).unwrap();
        assert!(
            EngineCatalog::from_extensions_root(root.path())
                .unwrap()
                .family(id)
                .is_err()
        );
    }

    #[test]
    #[cfg(windows)]
    fn unreadable_version_can_be_inspected_and_removed_with_a_payload_warning() {
        use std::os::windows::fs::OpenOptionsExt;

        let root = tempfile::tempdir().unwrap();
        index(root.path(), 1);
        let id = "engine.fixture-0000";
        let package = root.path().join("packages").join(id).join("v1");
        fs::create_dir_all(&package).unwrap();
        let manifest = package.join("package.json");
        fs::write(&manifest, b"{}").unwrap();
        let held = fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&manifest)
            .unwrap();
        let catalog = EngineCatalog::from_extensions_root(root.path()).unwrap();
        let detail = catalog.version_detail(id, "v1").unwrap();
        assert!(!detail.exists);
        assert!(!detail.diagnostics.is_empty());

        let result = catalog.delete_version(id, "v1", &[]).unwrap();

        assert!(result.deleted);
        assert!(result.diagnostics[0].contains(&package.display().to_string()));
        assert!(catalog.family(id).is_err());
        assert!(
            EngineCatalog::from_extensions_root(root.path())
                .unwrap()
                .family(id)
                .is_err()
        );
        drop(held);
        assert!(manifest.is_file());
    }

    #[test]
    fn knowledge_toggle_preserves_guidance_and_tools() {
        let root = tempfile::tempdir().unwrap();
        index(root.path(), 0);
        let id = "engine.fixture.knowledge";
        let family = json!({"kind":"engine","key":"fixture","name":"Fixture"});
        let path = root.path().join(format!("engine-knowledge/{id}.json"));
        write_json(&path, &json!({"key":"knowledge","family":family,"aliases":[],"conditions":"Fixture","summary":"Summary","origins":[],"revision":"abc","enabled":true,"guidance":"keep","tools":[{"files":{"x.py":"print(1)"}}]})).unwrap();
        let catalog = EngineCatalog::from_extensions_root(root.path()).unwrap();
        catalog.register_knowledge(id).unwrap();
        catalog.register_knowledge(id).unwrap();
        assert_eq!(catalog.family("engine.fixture").unwrap().knowledge.len(), 1);
        assert_eq!(
            EngineCatalog::from_extensions_root(root.path())
                .unwrap()
                .family("engine.fixture")
                .unwrap()
                .knowledge
                .len(),
            1
        );
        catalog.set_knowledge_enabled(id, false).unwrap();
        let entry = catalog.knowledge_detail(id).unwrap();
        assert_eq!(entry["guidance"], "keep");
        assert_eq!(entry["tools"][0]["files"]["x.py"], "print(1)");
        assert!(!catalog.family("engine.fixture").unwrap().knowledge[0].enabled);
    }

    #[test]
    fn python_package_registration_updates_discovery_state_and_selected_version_detail() {
        let root = tempfile::tempdir().unwrap();
        let package = root.path().join("packages/engine.fixture/v4");
        fs::create_dir_all(package.join("src")).unwrap();
        fs::write(package.join("src/adapter.py"), b"print('fixture')").unwrap();
        write_json(&package.join("package.json"), &json!({"id":"engine.fixture","version":"v4","manifestVersion":4,"family":{"kind":"engine","key":"fixture","name":"Fixture"},"resourceFiles":[],"resourcePatterns":["*.dat"],"recognition":{"schemaVersion":1}})).unwrap();
        let catalog = EngineCatalog::from_extensions_root(root.path()).unwrap();
        catalog
            .register_package(
                &package,
                Some(GameCompatibility {
                    game_id: "fixture-game".into(),
                    title: "Fixture game".into(),
                    variant: "sample".into(),
                    adapter_version: "v4".into(),
                    structure_status: "passed".into(),
                    coverage_confirmed: true,
                    runtime_status: "not-tested".into(),
                    updated_at: None,
                }),
            )
            .unwrap();
        assert!(
            catalog
                .version_detail("engine.fixture", "v4")
                .unwrap()
                .exists
        );
        assert_eq!(catalog.candidate_package_directories().unwrap(), [package]);
        assert_eq!(catalog.family("engine.fixture").unwrap().games.len(), 1);
        assert!(
            root.path()
                .join("catalog/engine.fixture/games.json")
                .is_file()
        );
    }

    #[test]
    fn trial_history_preserves_source_reuse_and_runtime_matching_policy() {
        let root = tempfile::tempdir().unwrap();
        let package = root.path().join("packages/engine.fixture/v1");
        fs::create_dir_all(package.join("src")).unwrap();
        fs::write(package.join("src/adapter.py"), b"print('fixture')").unwrap();
        write_json(&package.join("package.json"), &json!({
            "id":"engine.fixture", "version":"v1", "manifestVersion":4,
            "family":{"kind":"engine","key":"fixture","name":"Fixture"}, "resourcePatterns":["*.dat"]
        })).unwrap();
        let catalog = EngineCatalog::from_extensions_root(root.path()).unwrap();
        catalog
            .register_trial_package(
                &package,
                GameCompatibility {
                    game_id: "game-a".into(),
                    title: "Game A".into(),
                    variant: "fixture".into(),
                    adapter_version: "v1".into(),
                    structure_status: "passed".into(),
                    coverage_confirmed: true,
                    runtime_status: "not-tested".into(),
                    updated_at: None,
                },
                None,
            )
            .unwrap();
        assert!(catalog.discovery_candidates(None).unwrap().is_empty());
        assert_eq!(
            catalog.adaptation_reference_directories().unwrap(),
            std::slice::from_ref(&package)
        );
        assert!(
            catalog
                .discovery_candidates_for_game("game-b", None)
                .unwrap()
                .is_empty()
        );
        let known = catalog
            .discovery_candidates_for_game("game-a", None)
            .unwrap();
        assert_eq!(known.len(), 1);
        assert!(known[0].is_known_game && !known[0].is_pinned);
        catalog.forget_game("game-a").unwrap();
        fs::remove_file(root.path().join("library-index.json")).unwrap();
        let catalog = EngineCatalog::from_extensions_root(root.path()).unwrap();
        assert_eq!(
            catalog.adaptation_reference_directories().unwrap(),
            std::slice::from_ref(&package)
        );
        assert_eq!(
            catalog
                .discovery_candidates_for_game("game-a", None)
                .unwrap()
                .len(),
            1
        );
        assert!(catalog.discovery_candidates(None).unwrap().is_empty());
        catalog
            .set_family_enabled(&["engine.fixture".into()], false)
            .unwrap();
        assert!(
            catalog
                .adaptation_reference_directories()
                .unwrap()
                .is_empty()
        );
        assert!(
            catalog
                .discovery_candidates_for_game("game-a", None)
                .unwrap()
                .is_empty()
        );
        catalog
            .set_family_enabled(&["engine.fixture".into()], true)
            .unwrap();
        let mut state = load_state(root.path()).unwrap();
        state["families"]["engine.fixture"]["excludedVersions"] = json!(["v1"]);
        write_json(&root.path().join("library-state.json"), &state).unwrap();
        assert!(
            catalog
                .adaptation_reference_directories()
                .unwrap()
                .is_empty()
        );
        assert!(
            catalog
                .discovery_candidates_for_game("game-a", None)
                .unwrap()
                .is_empty()
        );
        state["families"]["engine.fixture"]["excludedVersions"] = json!([]);
        write_json(&root.path().join("library-state.json"), &state).unwrap();
        catalog
            .record_version_feedback(
                &AdapterVersion {
                    adapter_id: "engine.fixture".into(),
                    version: "v1".into(),
                },
                "game-a",
                VersionFeedback {
                    status: "failed".into(),
                    notes: "Unsupported layout".into(),
                    patch_id: "fixture-patch".into(),
                    updated_at: "2026-11-12T00:00:00Z".into(),
                },
            )
            .unwrap();
        assert!(
            catalog
                .adaptation_reference_directories()
                .unwrap()
                .is_empty()
        );
        assert!(
            catalog
                .discovery_candidates_for_game("game-a", None)
                .unwrap()
                .is_empty()
        );
        assert!(
            catalog
                .discovery_candidates_for_game("game-a", Some(("engine.fixture", "v1")))
                .unwrap()
                .is_empty()
        );
        catalog
            .record_version_feedback(
                &AdapterVersion {
                    adapter_id: "engine.fixture".into(),
                    version: "v1".into(),
                },
                "game-a",
                VersionFeedback {
                    status: "passed".into(),
                    notes: "Runtime verified".into(),
                    patch_id: "verified-patch".into(),
                    updated_at: "2026-11-12T01:00:00Z".into(),
                },
            )
            .unwrap();
        let newer = root.path().join("packages/engine.fixture/v2");
        fs::create_dir_all(newer.join("src")).unwrap();
        fs::copy(package.join("src/adapter.py"), newer.join("src/adapter.py")).unwrap();
        let mut manifest: Value = read_json(&package.join("package.json")).unwrap();
        manifest["version"] = json!("v2");
        write_json(&newer.join("package.json"), &manifest).unwrap();
        catalog
            .register_trial_package(
                &newer,
                GameCompatibility {
                    game_id: "game-a".into(),
                    title: "Game A".into(),
                    variant: "fixture".into(),
                    adapter_version: "v2".into(),
                    structure_status: "passed".into(),
                    coverage_confirmed: true,
                    runtime_status: "not-tested".into(),
                    updated_at: None,
                },
                None,
            )
            .unwrap();
        catalog
            .record_version_feedback(
                &AdapterVersion {
                    adapter_id: "engine.fixture".into(),
                    version: "v2".into(),
                },
                "game-a",
                VersionFeedback {
                    status: "failed".into(),
                    notes: "Layout regression".into(),
                    patch_id: "new-patch".into(),
                    updated_at: "2026-11-12T02:00:00Z".into(),
                },
            )
            .unwrap();
        let fallback = catalog
            .discovery_candidates_for_game("game-a", None)
            .unwrap();
        assert_eq!(fallback.len(), 1);
        assert_eq!(fallback[0].version, "v1");
        assert!(fallback[0].is_known_game);
        assert!(
            catalog
                .delete_version("engine.fixture", "v1", &[])
                .unwrap()
                .deleted
        );
        assert!(
            catalog
                .discovery_candidates_for_game("game-a", None)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn forgetting_game_promotes_legacy_active_pin_and_preserves_compatibility_history() {
        let root = tempfile::tempdir().unwrap();
        index(root.path(), 1);
        let id = "engine.fixture-0000";
        let package = root.path().join("packages").join(id).join("v1");
        write_json(&package.join("package.json"), &json!({"id":id,"version":"v1","family":{"kind":"engine","key":"fixture-0000","name":"Fixture"}})).unwrap();
        let index_path = root.path().join("library-index.json");
        let mut stored: Value = read_json(&index_path).unwrap();
        stored["families"][id]["versions"]["v1"]["source"] = json!(2);
        stored["families"][id]["versions"]["v1"]["exists"] = json!(false);
        stored["families"][id]["versions"]["v1"]["isCandidate"] = json!(false);
        write_json(&index_path, &stored).unwrap();
        let compatibility_path = root.path().join("catalog").join(id).join("games.json");
        let compatibility = json!({"family":stored["families"][id]["family"],"games":stored["families"][id]["games"]});
        write_json(&compatibility_path, &compatibility).unwrap();
        let game_id = "game-0";
        let pin_directory = root.path().join("games").join(hash_text(game_id));
        let activation =
            json!({"id":id,"version":"v1","gameId":game_id,"activatedAt":"2026-01-01T00:00:00Z"});
        write_json(&pin_directory.join("active.json"), &activation).unwrap();
        write_json(
            &pin_directory.join("previous.json"),
            &json!({"id":id,"version":"v0"}),
        )
        .unwrap();
        let unrelated_pin = root.path().join("games/other/active.json");
        write_json(&unrelated_pin, &activation).unwrap();
        let catalog = EngineCatalog::from_extensions_root(root.path()).unwrap();

        catalog.forget_game(game_id).unwrap();

        assert!(!pin_directory.exists());
        assert!(unrelated_pin.is_file());
        assert_eq!(
            read_json::<Value>(&compatibility_path).unwrap(),
            compatibility
        );
        assert_eq!(catalog.family(id).unwrap().games.len(), 1);
        assert_eq!(
            catalog.candidate_package_directories().unwrap(),
            std::slice::from_ref(&package)
        );
        let state = load_state(root.path()).unwrap();
        assert_eq!(state["families"][id]["releases"], json!([activation]));
        fs::remove_file(index_path).unwrap();
        assert_eq!(
            EngineCatalog::from_extensions_root(root.path())
                .unwrap()
                .candidate_package_directories()
                .unwrap(),
            [package]
        );
    }

    #[test]
    fn forgetting_game_keeps_both_pins_when_retention_cannot_be_committed() {
        let root = tempfile::tempdir().unwrap();
        let package = root.path().join("packages/engine.fixture/v1");
        fs::create_dir_all(&package).unwrap();
        let pin_directory = root.path().join("games").join(hash_text("game-0"));
        let activation = json!({"id":"engine.fixture","version":"v1"});
        write_json(&pin_directory.join("active.json"), &activation).unwrap();
        write_json(&pin_directory.join("previous.json"), &activation).unwrap();
        fs::create_dir(root.path().join("library-index.json")).unwrap();
        let catalog = EngineCatalog::from_extensions_root(root.path()).unwrap();

        assert!(catalog.forget_game("game-0").is_err());
        assert!(pin_directory.join("active.json").is_file());
        assert!(pin_directory.join("previous.json").is_file());
        assert!(package.is_dir());
    }

    #[test]
    fn source_and_enabled_filters_can_be_combined_for_historical_versions() {
        let root = tempfile::tempdir().unwrap();
        index(root.path(), 2);
        let path = root.path().join("library-index.json");
        let mut stored: Value = read_json(&path).unwrap();
        stored["families"]["engine.fixture-0001"]["versions"]["v1"]["source"] = json!(2);
        write_json(&path, &stored).unwrap();
        let catalog = EngineCatalog::from_extensions_root(root.path()).unwrap();
        catalog
            .set_family_enabled(&["engine.fixture-0001".into()], false)
            .unwrap();
        assert_eq!(
            catalog
                .snapshot_filtered(
                    "fixture",
                    LibraryFilter::History,
                    Some(false),
                    LibrarySort::Name,
                    0,
                    100
                )
                .unwrap()
                .total,
            1
        );
        assert_eq!(
            catalog
                .snapshot_filtered(
                    "fixture",
                    LibraryFilter::External,
                    Some(false),
                    LibrarySort::Name,
                    0,
                    100
                )
                .unwrap()
                .total,
            0
        );
        assert_eq!(
            catalog
                .snapshot_filtered(
                    "fixture",
                    LibraryFilter::External,
                    Some(true),
                    LibrarySort::Name,
                    0,
                    100
                )
                .unwrap()
                .total,
            1
        );
    }

    #[test]
    fn discovery_uses_only_candidates_or_explicit_pin_and_filters_from_index_metadata() {
        let root = tempfile::tempdir().unwrap();
        index(root.path(), 4);
        let path = root.path().join("library-index.json");
        let mut stored: Value = read_json(&path).unwrap();
        stored["families"]["engine.fixture-0000"]["versions"]["v1"]["discovery"] =
            json!({"resourceFiles":[],"resourcePatterns":["*.dat"],"recognition":null});
        stored["families"]["engine.fixture-0001"]["versions"]["v1"]["isCandidate"] = json!(false);
        stored["families"]["engine.fixture-0002"]["enabled"] = json!(false);
        stored["families"]["engine.fixture-0003"]["versions"]["v1"]["discovery"] = json!({"discoveryIndexVersion":1,"resourceFiles":[],"resourcePatterns":["*.png"],"recognition":null});
        write_json(&path, &stored).unwrap();
        let game = root.path().join("game");
        fs::create_dir_all(&game).unwrap();
        fs::write(game.join("script.dat"), b"fixture").unwrap();
        let inventory = gamel10n_engines::discovery::Inventory::scan(&game).unwrap();
        let catalog = EngineCatalog::from_extensions_root(root.path()).unwrap();
        let candidates = catalog.discovery_candidates(None).unwrap();
        let matching = catalog
            .matching_packages(&inventory, |path| Some(game.join(path)))
            .unwrap();
        assert!(matching.contains_key("engine.fixture-0000"));
        assert!(!matching.contains_key("engine.fixture-0003"));
        let sources = catalog.adaptation_package_index(&matching).unwrap();
        assert!(!sources.contains_key(&root.path().join("packages/engine.fixture-0000/v1")));
        assert!(!sources[&root.path().join("packages/engine.fixture-0003/v1")].matches_game);
        assert_eq!(candidates.len(), 2);
        assert_eq!(
            candidates
                .iter()
                .filter(|candidate| candidate.matches_inventory(&inventory))
                .count(),
            1
        );
        assert!(
            catalog
                .discovery_candidates(Some(("engine.fixture-0001", "v1")))
                .unwrap()
                .iter()
                .any(|candidate| candidate.is_pinned
                    && candidate.adapter_id == "engine.fixture-0001")
        );
        assert!(
            catalog
                .discovery_candidates(Some(("engine.fixture-0002", "v1")))
                .unwrap()
                .iter()
                .any(|candidate| candidate.is_pinned
                    && candidate.adapter_id == "engine.fixture-0002")
        );
        assert!(!root.path().join("packages").exists());
    }

    #[test]
    fn large_catalog_excludes_unrelated_packages_before_opening_manifests() {
        use gamel10n_engines::discovery::Inventory;
        use std::{cell::Cell, time::Instant};

        for count in [1_000, 10_000] {
            let root = tempfile::tempdir().unwrap();
            let game = root.path().join("game");
            fs::create_dir(&game).unwrap();
            fs::write(game.join("GAME.EXE"), b"MZ").unwrap();
            fs::write(game.join("story.arc"), b"GOOD").unwrap();
            let inventory = Inventory::scan(&game).unwrap();
            let mut versions = serde_json::Map::new();
            for number in 0..count {
                let version = format!("v{number}");
                let rule = match number {
                    0 => {
                        json!({"patterns":["**/*.arc"],"signatures":[{"offset":0,"hex":"474f4f44"}]})
                    }
                    n if n % 2 == 0 => {
                        json!({"patterns":["**/*.arc"],"signatures":[{"offset":0,"hex":"4e4f5045"}]})
                    }
                    _ => json!({"patterns":[format!("**/format{number}.bin")]}),
                };
                versions.insert(version.clone(), json!({"source":1, "version":version, "exists":true, "isCandidate":true,
                    "discovery":{"discoveryIndexVersion":1,"resourcePatterns":["*.exe","*.arc"],"recognition":{"schemaVersion":1,"required":[rule]}}}));
            }
            write_json(&root.path().join("library-index.json"), &json!({"schemaVersion":1,"families":{
                "engine.archive":{"family":{"kind":"engine","key":"archive","name":"Archive"},"versions":versions}
            }})).unwrap();
            let cold = Instant::now();
            let catalog = EngineCatalog::from_extensions_root(root.path()).unwrap();
            for run in 0..2 {
                let start = Instant::now();
                let reads = Cell::new(0);
                let matching = catalog
                    .matching_packages(&inventory, |path| {
                        reads.set(reads.get() + 1);
                        Some(game.join(path))
                    })
                    .unwrap();
                let candidates = catalog
                    .matching_candidates_for_game("new-game", None, Some(&matching))
                    .unwrap();
                assert_eq!(candidates.len(), 1);
                assert_eq!(candidates[0].version, "v0");
                assert_eq!(reads.get(), 2);
                eprintln!(
                    "versions={count} run={run} select={:?} since_open={:?}",
                    start.elapsed(),
                    cold.elapsed()
                );
            }
            assert!(!root.path().join("packages").exists());
        }
    }
}
