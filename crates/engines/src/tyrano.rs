//! Unpacked TyranoScript applications may be nested beside unrelated game resources.

use crate::discovery::{Inventory, resolve_relative};
use std::{collections::BTreeSet, path::Path};

const RUNTIME: [&str; 2] = ["tyrano/tyrano.js", "tyrano/plugins/kag/kag.js"];
const TYRANO_DATA_DIR: &str = "data";
const KS_EXTENSION: &str = "ks";

pub(crate) struct TyranoData {
    pub(crate) directories: Vec<String>,
}

impl TyranoData {
    pub(crate) fn detect<'a>(
        paths: impl Iterator<Item = &'a str>,
        mut exists: impl FnMut(&str) -> bool,
    ) -> Self {
        let mut roots = BTreeSet::new();
        for path in paths.filter(|p| {
            p.rsplit_once('.')
                .is_some_and(|(_, ext)| ext.eq_ignore_ascii_case(KS_EXTENSION))
        }) {
            let mut prefix = 0;
            for part in path.split('/') {
                if part.eq_ignore_ascii_case(TYRANO_DATA_DIR) {
                    roots.insert(&path[..prefix]);
                }
                prefix += part.len() + 1;
            }
        }
        let directories = roots
            .into_iter()
            .filter(|root| {
                RUNTIME
                    .iter()
                    .all(|marker| exists(&format!("{root}{marker}")))
            })
            .map(|root| format!("{root}{TYRANO_DATA_DIR}/"))
            .collect();
        Self { directories }
    }

    pub(crate) fn from_inventory(inventory: &Inventory, scripts: &[&str]) -> Self {
        if scripts.is_empty() {
            return Self {
                directories: Vec::new(),
            };
        }
        let markers: BTreeSet<_> = inventory
            .files
            .iter()
            .filter(|file| {
                let name = file.path.rsplit('/').next().unwrap_or_default();
                RUNTIME
                    .iter()
                    .filter_map(|marker| marker.rsplit('/').next())
                    .any(|marker_name| name.eq_ignore_ascii_case(marker_name))
            })
            .map(|file| file.path.to_ascii_lowercase())
            .collect();
        Self::detect(scripts.iter().copied(), |path| {
            markers.contains(&path.to_ascii_lowercase())
        })
    }

    pub(crate) fn from_resources(game: &Path, paths: &[String]) -> Self {
        Self::detect(paths.iter().map(String::as_str), |file| {
            resolve_relative(game, file).is_ok_and(|path| path.is_file())
        })
    }

    pub(crate) fn contains(&self, path: &str) -> bool {
        self.directories.iter().any(|dir| {
            path.get(..dir.len())
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case(dir))
        })
    }
}
