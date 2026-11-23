use super::{
    FileRule, Inventory, RULE_FILE_LIMIT, Recognition, RecognitionStatus, glob_match,
    validate_pattern,
};
use std::{collections::HashMap, path::PathBuf};

/// Static package selection over one immutable view of the game's original resources.
pub struct DiscoveryFilter<'a, F> {
    inventory: &'a Inventory,
    paths: HashMap<String, Vec<usize>>,
    names: HashMap<String, Vec<usize>>,
    extensions: HashMap<String, Vec<usize>>,
    patterns: HashMap<String, Vec<usize>>,
    rules: HashMap<FileRule, bool>,
    resolve: F,
}

impl<'a, F: Fn(&str) -> Option<PathBuf>> DiscoveryFilter<'a, F> {
    pub fn new(inventory: &'a Inventory, resolve: F) -> Self {
        let mut filter = Self {
            inventory,
            paths: HashMap::new(),
            names: HashMap::new(),
            extensions: HashMap::new(),
            patterns: HashMap::new(),
            rules: HashMap::new(),
            resolve,
        };
        for (index, file) in inventory.files.iter().enumerate() {
            let path = file.path.replace('\\', "/").to_lowercase();
            let name = path.rsplit('/').next().unwrap_or(&path);
            if let Some((_, extension)) = name.rsplit_once('.') {
                filter
                    .extensions
                    .entry(extension.into())
                    .or_default()
                    .push(index);
            }
            filter.names.entry(name.into()).or_default().push(index);
            filter.paths.entry(path).or_default().push(index);
        }
        filter
    }

    fn matching_files(&mut self, pattern: &str) -> &[usize] {
        self.patterns.entry(pattern.into()).or_insert_with(|| {
            let normalized = pattern.replace('\\', "/").to_lowercase();
            let name = normalized.rsplit('/').next().unwrap_or(&normalized);
            let extension = name.rsplit_once('.').map(|(_, ext)| ext);
            let bucket = if !normalized.contains(['*', '?']) {
                Some(self.paths.get(&normalized))
            } else if !name.contains(['*', '?']) {
                Some(self.names.get(name))
            } else if let Some(ext) = extension.filter(|ext| !ext.contains(['*', '?'])) {
                Some(self.extensions.get(ext))
            } else {
                None
            };
            let matches = |&index: &usize| glob_match(pattern, &self.inventory.files[index].path);
            match bucket {
                Some(Some(indices)) => indices
                    .iter()
                    .copied()
                    .filter(matches)
                    .take(RULE_FILE_LIMIT + 1)
                    .collect(),
                Some(None) => Vec::new(),
                None => (0..self.inventory.files.len())
                    .filter(matches)
                    .take(RULE_FILE_LIMIT + 1)
                    .collect(),
            }
        })
    }

    fn allows_rule(&mut self, rule: &FileRule) -> bool {
        if let Some(result) = self.rules.get(rule) {
            return *result;
        }
        let mut indices = Vec::new();
        for pattern in &rule.patterns {
            indices.extend_from_slice(self.matching_files(pattern));
        }
        indices.sort_unstable();
        indices.dedup();
        let status = self.inventory.matching_rule_files(
            rule,
            &mut 0,
            &self.resolve,
            indices.into_iter().map(|i| &self.inventory.files[i]),
        );
        let allowed = status != RecognitionStatus::NoMatch;
        self.rules.insert(rule.clone(), allowed);
        allowed
    }

    pub fn allows(
        &mut self,
        files: &[String],
        patterns: &[String],
        recognition: Option<&Recognition>,
    ) -> bool {
        if !self.inventory.complete {
            return true;
        }
        if let Some(recognition) = recognition
            && recognition.validate().is_ok()
            && !recognition
                .required
                .iter()
                .all(|rule| self.allows_rule(rule))
        {
            return false;
        }
        if patterns.is_empty() {
            files.is_empty()
                || files
                    .iter()
                    .any(|path| self.paths.contains_key(&path.to_lowercase()))
        } else {
            patterns.iter().any(|pattern| {
                validate_pattern(pattern).is_err() || !self.matching_files(pattern).is_empty()
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;

    #[test]
    fn indexed_rules_keep_every_possible_match_from_the_full_scan() {
        let temp = tempfile::tempdir().unwrap();
        let game = temp.path();
        fs::create_dir(game.join("Data")).unwrap();
        fs::write(game.join("Data/STORY.ARC"), b"headTAIL").unwrap();
        fs::write(game.join("game.exe"), b"MZ").unwrap();
        let mut inventory = Inventory::scan(game).unwrap();
        let declarations = [
            json!({"required":[{"patterns":["absent","**/Story.Arc"],"minimumSize":8,
                "signatures":[{"offset":0,"hex":"68656164"},{"origin":"end","offset":0,"hex":"5441494c"}]}]}),
            json!({"required":[{"patterns":["**/missing.arc"]}]}),
            json!({"hints":[{"patterns":["**/missing.arc"]}]}),
            json!({"required":[{"patterns":["**/st?ry.*"],"maximumSize":7}]}),
            json!({"required":[{"patterns":["**/*.ARC"],"signatures":[{"offset":0,"hex":"424144"}]}]}),
            json!({"required":[{"patterns":["missing","**/*"]},{"patterns":["*.exe"]}]}),
        ];
        for complete in [true, false] {
            inventory.complete = complete;
            for readable in [true, false] {
                let resolve = |p: &str| readable.then(|| game.join(p));
                let mut filter = DiscoveryFilter::new(&inventory, resolve);
                for declaration in &declarations {
                    let mut json = declaration.clone();
                    json["schemaVersion"] = json!(1);
                    let rule: Recognition = serde_json::from_value(json).unwrap();
                    let reference = inventory.recognize_with(Some(&rule), &resolve).0;
                    assert_eq!(
                        filter.allows(&[], &[], Some(&rule)),
                        !complete || reference != RecognitionStatus::NoMatch,
                        "{declaration}"
                    );
                }
                assert!(filter.allows(&[], &["*.exe".into()], None));
            }
        }
    }
}
