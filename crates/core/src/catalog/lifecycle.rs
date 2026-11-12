use super::*;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AdapterVersion {
    pub adapter_id: String,
    pub version: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionFeedback {
    pub status: String,
    pub notes: String,
    pub patch_id: String,
    pub updated_at: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, rename_all = "camelCase")]
pub struct VersionLifecycle {
    pub requires_confirmation: bool,
    pub feedback: BTreeMap<String, VersionFeedback>,
    pub replaces: BTreeMap<String, Vec<AdapterVersion>>,
}

impl VersionLifecycle {
    pub fn status_label(&self) -> &'static str {
        let passed = self.feedback.values().any(|item| item.status == "passed");
        let failed = self.feedback.values().any(|item| item.status == "failed");
        match (passed, failed, self.requires_confirmation) {
            (true, true, _) => "部分游戏存在问题，另有验证通过",
            (_, true, _) => "存在问题",
            (true, false, _) => "用户验证通过",
            (false, false, true) => "待游戏验证",
            _ => "尚未记录游戏验证",
        }
    }

    pub fn failed_for(&self, game_id: &str) -> bool {
        self.feedback
            .get(game_id)
            .is_some_and(|item| item.status == "failed")
    }

    pub fn passed_for(&self, game_id: &str) -> bool {
        self.feedback
            .get(game_id)
            .is_some_and(|item| item.status == "passed")
    }

    pub(super) fn can_reuse(&self) -> bool {
        self.feedback.values().any(|item| item.status == "passed")
            || (!self.requires_confirmation
                && !self.feedback.values().any(|item| item.status == "failed"))
    }

    pub fn replaces_for(&self, game_id: &str, previous: &AdapterVersion) -> bool {
        self.replaces
            .get(game_id)
            .is_some_and(|versions| versions.contains(previous))
    }
}

pub(super) fn lifecycle(setting: &Value, version: &str) -> Result<VersionLifecycle, CoreError> {
    setting
        .get("versionLifecycle")
        .and_then(|entries| entries.get(version))
        .map(|value| {
            serde_json::from_value(value.clone())
                .map_err(|error| invalid(format!("版本验证记录无效：{error}")))
        })
        .transpose()
        .map(Option::unwrap_or_default)
}

pub(super) fn save_lifecycle(
    setting: &mut Value,
    version: &str,
    value: &VersionLifecycle,
) -> Result<(), CoreError> {
    let entries = setting
        .as_object_mut()
        .ok_or_else(|| invalid("引擎发布记录无效。"))?
        .entry("versionLifecycle")
        .or_insert_with(|| json!({}));
    entries
        .as_object_mut()
        .ok_or_else(|| invalid("版本验证记录无效。"))?
        .insert(version.into(), json!(value));
    Ok(())
}

impl EngineCatalog {
    pub(crate) fn version_lifecycle(
        &self,
        adapter: &AdapterVersion,
    ) -> Result<Option<VersionLifecycle>, CoreError> {
        let cached = self.lock()?;
        cached
            .index
            .families
            .get(&adapter.adapter_id)
            .and_then(|family| family.versions.get(&adapter.version))
            .map(|version| {
                version.extra.get("lifecycle").map_or_else(
                    || Ok(VersionLifecycle::default()),
                    |value| {
                        serde_json::from_value(value.clone())
                            .map_err(|error| invalid(error.to_string()))
                    },
                )
            })
            .transpose()
    }

    pub fn discovery_candidates_for_game(
        &self,
        game_id: &str,
        pinned: Option<(&str, &str)>,
    ) -> Result<Vec<EngineDiscoveryCandidate>, CoreError> {
        let mut candidates = self.discovery_candidates(pinned)?;
        let cached = self.lock()?;
        let state = load_state(&self.root)?;
        let mut known_updates = BTreeMap::new();
        for (id, family) in &cached.index.families {
            for game in family.games.iter().filter(|game| {
                game.game_id == game_id
                    && game.structure_status == "passed"
                    && game.runtime_status != "failed"
            }) {
                let Some(entry) = family.versions.get(&game.adapter_version) else {
                    continue;
                };
                if entry.source != 1 || !entry.exists {
                    continue;
                }
                let version_lifecycle = entry
                    .extra
                    .get("lifecycle")
                    .map(|value| serde_json::from_value::<VersionLifecycle>(value.clone()))
                    .transpose()
                    .map_err(|error| invalid(format!("版本验证记录无效：{error}")))?
                    .unwrap_or_default();
                if version_lifecycle.failed_for(game_id) {
                    continue;
                }
                let existing = candidates.iter_mut().find(|candidate| {
                    candidate.adapter_id == *id && candidate.version == game.adapter_version
                });
                let excluded = state["families"][id]["excludedVersions"]
                    .as_array()
                    .is_some_and(|versions| {
                        versions
                            .iter()
                            .any(|version| version.as_str() == Some(game.adapter_version.as_str()))
                    });
                if let Some(candidate) = existing {
                    if !excluded || candidate.is_pinned {
                        candidate.is_known_game = true;
                    }
                } else if family.enabled && !excluded {
                    candidates.push(EngineDiscoveryCandidate {
                        adapter_id: id.clone(),
                        version: game.adapter_version.clone(),
                        directory: self
                            .root
                            .join("packages")
                            .join(id)
                            .join(&game.adapter_version),
                        published_at: entry.published_at.clone(),
                        descriptor: entry
                            .extra
                            .get("discovery")
                            .filter(|value| !value.is_null())
                            .and_then(|value| serde_json::from_value(value.clone()).ok()),
                        is_pinned: false,
                        is_known_game: true,
                    });
                }
                known_updates.insert(
                    (id.as_str(), game.adapter_version.as_str()),
                    game.updated_at.as_deref(),
                );
            }
        }
        candidates.retain_mut(|candidate| {
            let family_entry = cached.index.families.get_key_value(&candidate.adapter_id);
            let family = family_entry.map(|(_, family)| family);
            let version =
                family.and_then(|family| family.versions.get_key_value(&candidate.version));
            let lifecycle_value = version.and_then(|(_, version)| version.extra.get("lifecycle"));
            let lifecycle = lifecycle_value
                .and_then(|value| serde_json::from_value::<VersionLifecycle>(value.clone()).ok());
            if !candidate.is_known_game
                && version.is_some_and(|(_, version)| version.exists)
                && lifecycle
                    .as_ref()
                    .is_some_and(|lifecycle| lifecycle.passed_for(game_id))
                && let (Some((id, _)), Some((version, _)), Some(value)) =
                    (family_entry, version, lifecycle_value)
            {
                candidate.is_known_game = true;
                known_updates.insert(
                    (id.as_str(), version.as_str()),
                    value["feedback"][game_id]["updatedAt"].as_str(),
                );
            }
            let failed = lifecycle.is_some_and(|lifecycle| lifecycle.failed_for(game_id));
            !failed
                && (candidate.is_pinned
                    || !state["families"][&candidate.adapter_id]["excludedVersions"]
                        .as_array()
                        .is_some_and(|versions| {
                            versions
                                .iter()
                                .any(|version| version.as_str() == Some(candidate.version.as_str()))
                        }))
                && !family.is_some_and(|family| {
                    family.games.iter().any(|game| {
                        game.game_id == game_id
                            && game.adapter_version == candidate.version
                            && game.runtime_status == "failed"
                    })
                })
        });
        candidates.sort_by(|left, right| {
            right
                .is_pinned
                .cmp(&left.is_pinned)
                .then_with(|| right.is_known_game.cmp(&left.is_known_game))
                .then_with(|| {
                    known_updates
                        .get(&(right.adapter_id.as_str(), right.version.as_str()))
                        .cmp(&known_updates.get(&(left.adapter_id.as_str(), left.version.as_str())))
                })
        });
        Ok(candidates)
    }

    pub fn register_trial_package(
        &self,
        package: &Path,
        game: GameCompatibility,
        replaces: Option<&AdapterVersion>,
    ) -> Result<(), CoreError> {
        self.register_package_internal(package, Some(game), true, replaces)
    }

    /// Feedback applies to the immutable version that produced the tested patch.
    pub fn record_version_feedback(
        &self,
        adapter: &AdapterVersion,
        game_id: &str,
        feedback: VersionFeedback,
    ) -> Result<bool, CoreError> {
        validate_family_id(&adapter.adapter_id)?;
        validate_part(&adapter.version)?;
        if !matches!(feedback.status.as_str(), "passed" | "failed") {
            return Err(invalid("请选择效果正常或存在问题。"));
        }
        let _write = write_gate()?;
        let mut cached = self.lock()?;
        let mut index = load_index(&self.root)?;
        let Some(family) = index.families.get_mut(&adapter.adapter_id) else {
            return Ok(false);
        };
        if !family
            .versions
            .get(&adapter.version)
            .is_some_and(|entry| entry.source == 1)
        {
            return Ok(false);
        }
        let mut state = load_state(&self.root)?;
        let setting = state["families"]
            .as_object_mut()
            .ok_or_else(|| invalid("引擎库状态无效。"))?
            .entry(&adapter.adapter_id)
            .or_insert_with(|| json!({"enabled":true,"releases":[],"excludedVersions":[]}));
        let mut lifecycle = lifecycle(setting, &adapter.version)?;
        lifecycle.feedback.insert(game_id.into(), feedback.clone());
        // Release order is authoritative: version names may be content hashes,
        // and confirming an older patch must never retire a newer release.
        if feedback.status == "passed"
            && let Some(releases) = setting["releases"].as_array()
            && let Some(position) = releases
                .iter()
                .position(|release| release["version"].as_str() == Some(&adapter.version))
        {
            let predecessors = lifecycle.replaces.entry(game_id.into()).or_default();
            for release in &releases[..position] {
                let Some(version) = release["version"].as_str().filter(|version| {
                    *version != adapter.version
                        && family
                            .versions
                            .get(*version)
                            .is_some_and(|entry| entry.source != 0)
                }) else {
                    continue;
                };
                let previous = AdapterVersion {
                    adapter_id: adapter.adapter_id.clone(),
                    version: version.into(),
                };
                if !predecessors.contains(&previous) {
                    predecessors.push(previous);
                }
            }
        }
        save_lifecycle(setting, &adapter.version, &lifecycle)?;
        let entry = family
            .versions
            .get_mut(&adapter.version)
            .ok_or_else(|| invalid("引擎版本记录不存在。"))?;
        entry.extra.insert("lifecycle".into(), json!(lifecycle));
        entry.is_candidate = lifecycle.can_reuse()
            && !setting["excludedVersions"]
                .as_array()
                .is_some_and(|values| {
                    values
                        .iter()
                        .any(|value| value.as_str() == Some(&adapter.version))
                });
        for game in &mut family.games {
            if game.game_id == game_id && game.adapter_version == adapter.version {
                game.runtime_status = feedback.status.clone();
                game.updated_at = Some(feedback.updated_at.clone());
            }
        }
        write_json(&safe_path(&self.root, "library-state.json")?, &state)?;
        let compatibility = safe_path(
            &self.root,
            &format!("catalog/{}/games.json", adapter.adapter_id),
        )?;
        write_json(
            &compatibility,
            &json!({"family":family.family,"games":family.games}),
        )?;
        self.commit_index(&mut cached, index)?;
        Ok(true)
    }
}
