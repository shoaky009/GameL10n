use super::{
    DISCOVERY_INDEX_VERSION, EXTENSIONS_DIR, EngineCatalog, EngineDiscoveryDescriptor,
    LIBRARY_INDEX_FILE, LIBRARY_STATE_FILE, LibraryIndex, SOURCE_EXTERNAL,
};
use gamel10n_engines::discovery::{DiscoveryFilter, Inventory, validate_pattern};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    path::{Path, PathBuf},
    time::SystemTime,
};

#[derive(PartialEq)]
struct CatalogStamp {
    files: [Option<(SystemTime, u64)>; 2],
    revision: u64,
}

#[derive(Default)]
pub(crate) struct DiscoveryCatalogCache {
    saved: Option<(CatalogStamp, EngineCatalog)>,
}

impl DiscoveryCatalogCache {
    pub(crate) fn open(&mut self, root: &Path) -> Result<EngineCatalog, crate::CoreError> {
        let stamp = || CatalogStamp {
            files: [LIBRARY_INDEX_FILE, LIBRARY_STATE_FILE].map(|file| {
                let metadata = root.join(EXTENSIONS_DIR).join(file).metadata().ok()?;
                Some((metadata.modified().ok()?, metadata.len()))
            }),
            revision: super::CATALOG_REVISION.load(std::sync::atomic::Ordering::Acquire),
        };
        let before = stamp();
        if before.files[0].is_some()
            && let Some((saved, catalog)) = &self.saved
            && saved == &before
        {
            return Ok(catalog.clone());
        }
        let catalog = EngineCatalog::open(root)?;
        self.saved =
            (before.files[0].is_some() && before == stamp()).then(|| (before, catalog.clone()));
        Ok(catalog)
    }
}

struct Package {
    family: String,
    version: String,
    descriptor: Option<EngineDiscoveryDescriptor>,
}

/// Each bucket is an alternative for one necessary condition. A package with no
/// indexable condition is still checked against its full declaration.
#[derive(Default)]
pub(super) struct PackageRoutes {
    packages: Vec<Package>,
    buckets: HashMap<String, Vec<usize>>,
    fallback: Vec<usize>,
}

fn bucket(pattern: &str) -> Option<String> {
    validate_pattern(pattern).ok()?;
    let name = pattern.rsplit(['/', '\\']).next()?.to_lowercase();
    if !name.contains(['*', '?']) {
        return Some(name);
    }
    let (_, extension) = name.rsplit_once('.')?;
    (!extension.contains(['*', '?'])).then(|| format!(".{extension}"))
}

fn buckets(descriptor: &EngineDiscoveryDescriptor) -> Option<Vec<String>> {
    let keys = |patterns: &[String]| {
        if patterns.is_empty() {
            return None;
        }
        patterns
            .iter()
            .map(|p| bucket(p))
            .collect::<Option<Vec<_>>>()
    };
    if let Some(recognition) = &descriptor.recognition
        && recognition.validate().is_ok()
        && let Some(keys) = recognition
            .required
            .iter()
            .find_map(|rule| keys(&rule.patterns))
    {
        return Some(keys);
    }
    if descriptor.resource_patterns.is_empty() {
        keys(&descriptor.resource_files)
    } else {
        keys(&descriptor.resource_patterns)
    }
}

impl PackageRoutes {
    pub(super) fn new(index: &LibraryIndex) -> Self {
        let mut routes = Self::default();
        for (id, family) in &index.families {
            for (version, entry) in &family.versions {
                if entry.source != SOURCE_EXTERNAL {
                    continue;
                }
                let descriptor = entry
                    .extra
                    .get("discovery")
                    .filter(|value| {
                        value["discoveryIndexVersion"].as_u64()
                            == Some(u64::from(DISCOVERY_INDEX_VERSION))
                    })
                    .and_then(|value| serde_json::from_value(value.clone()).ok());
                let number = routes.packages.len();
                match descriptor.as_ref().and_then(buckets) {
                    Some(keys) => {
                        for key in keys {
                            routes.buckets.entry(key).or_default().push(number);
                        }
                    }
                    None => routes.fallback.push(number),
                }
                routes.packages.push(Package {
                    family: id.clone(),
                    version: version.clone(),
                    descriptor,
                });
            }
        }
        routes
    }

    pub(super) fn select(
        &self,
        inventory: &Inventory,
        resolve: impl Fn(&str) -> Option<PathBuf>,
    ) -> BTreeMap<String, BTreeSet<String>> {
        let mut candidates: HashSet<usize> = self.fallback.iter().copied().collect();
        if inventory.complete {
            let mut keys = HashSet::new();
            for file in &inventory.files {
                let name = file
                    .path
                    .rsplit(['/', '\\'])
                    .next()
                    .unwrap_or(&file.path)
                    .to_lowercase();
                if let Some((_, ext)) = name.rsplit_once('.') {
                    keys.insert(format!(".{ext}"));
                }
                keys.insert(name);
            }
            for key in keys {
                if let Some(indices) = self.buckets.get(&key) {
                    candidates.extend(indices);
                }
            }
        } else {
            candidates.extend(0..self.packages.len());
        }
        let mut filter = DiscoveryFilter::new(inventory, resolve);
        let mut matches = BTreeMap::<_, BTreeSet<_>>::new();
        let mut candidates: Vec<_> = candidates.into_iter().collect();
        candidates.sort_unstable();
        for index in candidates {
            let package = &self.packages[index];
            if package.descriptor.as_ref().is_none_or(|d| {
                filter.allows(
                    &d.resource_files,
                    &d.resource_patterns,
                    d.recognition.as_ref(),
                )
            }) {
                matches
                    .entry(package.family.clone())
                    .or_default()
                    .insert(package.version.clone());
            }
        }
        matches
    }
}
