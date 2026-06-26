//! The declarative TOML config: desired state for an editor's profiles.
//!
//! The config layers `[global]` settings/extensions, reusable `[groups.*]`, and
//! per-`[profiles.*]` overrides into an effective desired state per profile (see
//! [`resolve`]).

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::editor::profiles::DEFAULT_PROFILE;

/// Top-level config document.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub editor: EditorRef,
    pub global: Layer,
    pub groups: BTreeMap<String, Layer>,
    /// The built-in Default profile (always present; cannot be renamed).
    pub default: DefaultProfile,
    /// Named (non-default) profiles.
    pub profiles: BTreeMap<String, ProfileConfig>,
}

/// The built-in Default profile. Unlike named profiles it has no icon and no
/// `use_default` inheritance flags (it is the profile others inherit *from*).
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DefaultProfile {
    /// Names of groups this profile includes.
    pub groups: Vec<String>,
    /// Profile-specific settings (highest precedence).
    pub settings: BTreeMap<String, Value>,
    /// Profile-specific extensions.
    pub extensions: Vec<String>,
    /// Extension IDs to drop even if a group/global adds them.
    pub exclude_extensions: Vec<String>,
}

/// How the config refers to / overrides the target editor.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct EditorRef {
    /// Editor name (`nameShort`/`applicationName`) to match during discovery.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Explicit launcher path, bypassing PATH discovery.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub binary: Option<PathBuf>,
    /// Override for the editor's `User/` directory.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_dir: Option<PathBuf>,
    /// Override for the shared extensions directory.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extensions_dir: Option<PathBuf>,
}

/// A reusable bundle of settings and extensions (`[global]` and `[groups.*]`).
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Layer {
    /// Settings keys applied to profiles using this layer.
    pub settings: BTreeMap<String, Value>,
    /// Extension IDs (`publisher.name`) applied to profiles using this layer.
    pub extensions: Vec<String>,
}

/// Per-profile configuration.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProfileConfig {
    /// Codicon ID used as the profile icon.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// Names of groups this profile includes.
    pub groups: Vec<String>,
    /// Profile-specific settings (highest precedence).
    pub settings: BTreeMap<String, Value>,
    /// Profile-specific extensions.
    pub extensions: Vec<String>,
    /// Extension IDs to drop even if a group/global adds them.
    pub exclude_extensions: Vec<String>,
    /// Resource types this profile inherits from Default (`useDefaultFlags`).
    pub use_default: BTreeMap<String, bool>,
}

/// Effective desired state for one profile after layering.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Resolved {
    pub settings: BTreeMap<String, Value>,
    pub extensions: BTreeSet<String>,
    pub icon: Option<String>,
    pub use_default: BTreeMap<String, bool>,
}

impl Config {
    /// Load a config from a TOML file.
    pub fn load(path: &Path) -> Result<Self> {
        let raw =
            fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let config: Self =
            toml::from_str(&raw).with_context(|| format!("parsing {}", path.display()))?;
        if config.profiles.contains_key(DEFAULT_PROFILE) {
            anyhow::bail!(
                "the Default profile is configured under [default], not [profiles.{DEFAULT_PROFILE}]"
            );
        }
        Ok(config)
    }

    /// Serialize the config to a TOML string with a generated-file header.
    pub fn to_toml(&self) -> Result<String> {
        let body = toml::to_string_pretty(&self.sanitized()).context("serializing config")?;
        Ok(format!(
            "# code-profile-manager editor config. See README.md for the format.\n\n{body}"
        ))
    }

    /// A clone with all settings null values stripped (TOML has no null type).
    fn sanitized(&self) -> Self {
        let mut clone = self.clone();
        clone.global.settings = sanitize_settings(&clone.global.settings);
        for group in clone.groups.values_mut() {
            group.settings = sanitize_settings(&group.settings);
        }
        clone.default.settings = sanitize_settings(&clone.default.settings);
        for profile in clone.profiles.values_mut() {
            profile.settings = sanitize_settings(&profile.settings);
        }
        clone
    }

    /// Effective desired state per profile (keyed by profile name), including the
    /// built-in Default profile.
    pub fn resolve(&self) -> BTreeMap<String, Resolved> {
        let mut out = BTreeMap::new();
        out.insert(DEFAULT_PROFILE.to_owned(), self.resolve_default());
        for (name, profile) in &self.profiles {
            out.insert(name.clone(), self.resolve_profile(profile));
        }
        out
    }

    fn resolve_default(&self) -> Resolved {
        self.resolve_layers(
            &self.default.groups,
            &self.default.settings,
            &self.default.extensions,
            &self.default.exclude_extensions,
            None,
            &BTreeMap::new(),
        )
    }

    fn resolve_profile(&self, profile: &ProfileConfig) -> Resolved {
        self.resolve_layers(
            &profile.groups,
            &profile.settings,
            &profile.extensions,
            &profile.exclude_extensions,
            profile.icon.as_deref(),
            &profile.use_default,
        )
    }

    /// Layer global + the named groups + profile-level fields into the effective
    /// desired state.
    fn resolve_layers(
        &self,
        groups: &[String],
        own_settings: &BTreeMap<String, Value>,
        own_extensions: &[String],
        excludes: &[String],
        icon: Option<&str>,
        use_default: &BTreeMap<String, bool>,
    ) -> Resolved {
        let mut settings = self.global.settings.clone();
        let mut extensions: BTreeSet<String> = self
            .global
            .extensions
            .iter()
            .map(|id| normalize_id(id))
            .collect();

        for group_name in groups {
            if let Some(group) = self.groups.get(group_name) {
                for (key, value) in &group.settings {
                    settings.insert(key.clone(), value.clone());
                }
                extensions.extend(group.extensions.iter().map(|id| normalize_id(id)));
            }
        }

        for (key, value) in own_settings {
            settings.insert(key.clone(), value.clone());
        }
        extensions.extend(own_extensions.iter().map(|id| normalize_id(id)));
        for excluded in excludes {
            extensions.remove(&normalize_id(excluded));
        }

        Resolved {
            settings,
            extensions,
            icon: icon.map(ToOwned::to_owned),
            use_default: use_default.clone(),
        }
    }

    /// Hoist shared settings and extensions into `[global]`, then re-express each
    /// profile as a delta.
    ///
    /// Behavior-preserving (`resolve()` is unchanged):
    /// - a settings key present in *every* profile is hoisted using its most
    ///   common value (when shared by at least two profiles); profiles that
    ///   disagree keep a profile-level override, since profile beats global;
    /// - an extension present in every profile is hoisted (intersection).
    pub fn consolidate(&mut self) -> Consolidation {
        let resolved = self.resolve();
        if resolved.is_empty() {
            return Consolidation::default();
        }
        let common_settings = modal_common_settings(&resolved, &self.default.settings);
        let common_extensions = intersect_extensions(&resolved);

        // Count only what is newly added to global.
        let existing_global: BTreeSet<String> = self
            .global
            .extensions
            .iter()
            .map(|id| normalize_id(id))
            .collect();
        let report = Consolidation {
            settings: common_settings
                .iter()
                .filter(|(key, value)| self.global.settings.get(*key) != Some(*value))
                .count(),
            extensions: common_extensions
                .iter()
                .filter(|id| !existing_global.contains(*id))
                .count(),
        };

        for (key, value) in &common_settings {
            self.global.settings.insert(key.clone(), value.clone());
        }
        let mut global_extensions = existing_global;
        global_extensions.extend(common_extensions.iter().cloned());
        self.global.extensions = global_extensions.into_iter().collect();

        // Re-express each profile as a minimal delta over the new global+groups.
        for (name, effective) in &resolved {
            let groups = self.groups_for(name);
            let base =
                self.resolve_layers(&groups, &BTreeMap::new(), &[], &[], None, &BTreeMap::new());
            let settings = effective
                .settings
                .iter()
                .filter(|(key, value)| base.settings.get(*key) != Some(*value))
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect();
            let extensions = effective
                .extensions
                .difference(&base.extensions)
                .cloned()
                .collect();
            let exclude = base
                .extensions
                .difference(&effective.extensions)
                .cloned()
                .collect();
            if name == DEFAULT_PROFILE {
                self.default.settings = settings;
                self.default.extensions = extensions;
                self.default.exclude_extensions = exclude;
            } else if let Some(profile) = self.profiles.get_mut(name) {
                profile.settings = settings;
                profile.extensions = extensions;
                profile.exclude_extensions = exclude;
            }
        }
        report
    }

    fn groups_for(&self, name: &str) -> Vec<String> {
        if name == DEFAULT_PROFILE {
            self.default.groups.clone()
        } else {
            self.profiles
                .get(name)
                .map(|p| p.groups.clone())
                .unwrap_or_default()
        }
    }
}

/// What `consolidate` moved into `[global]`.
#[derive(Debug, Default, Clone, Copy)]
pub struct Consolidation {
    pub settings: usize,
    pub extensions: usize,
}

/// Normalize an extension identifier for set membership: drop any `@version`
/// pin and lowercase (`publisher.name` is case-insensitive).
pub fn normalize_id(spec: &str) -> String {
    let id = spec.split('@').next().unwrap_or(spec);
    id.trim().to_lowercase()
}

/// Extensions present in every resolved profile (intersection).
fn intersect_extensions(resolved: &BTreeMap<String, Resolved>) -> BTreeSet<String> {
    let mut profiles = resolved.values();
    let Some(first) = profiles.next() else {
        return BTreeSet::new();
    };
    let mut common = first.extensions.clone();
    for profile in profiles {
        common.retain(|id| profile.extensions.contains(id));
    }
    common
}

/// For each settings key present in *every* profile, choose the value held by
/// the most profiles (shared by at least two). Ties prefer the Default profile's
/// value, then the lexicographically smallest, for determinism.
fn modal_common_settings(
    resolved: &BTreeMap<String, Resolved>,
    default_settings: &BTreeMap<String, Value>,
) -> BTreeMap<String, Value> {
    let mut profiles = resolved.values();
    let Some(first) = profiles.next() else {
        return BTreeMap::new();
    };
    let mut keys: BTreeSet<&String> = first.settings.keys().collect();
    for profile in profiles {
        keys.retain(|key| profile.settings.contains_key(*key));
    }

    let mut out = BTreeMap::new();
    for key in keys {
        // Tally each distinct value (keyed by its serialized form).
        let mut tally: BTreeMap<String, (usize, &Value)> = BTreeMap::new();
        for profile in resolved.values() {
            if let Some(value) = profile.settings.get(key) {
                let serialized = serde_json::to_string(value).unwrap_or_default();
                let counter = tally.entry(serialized).or_insert((0, value));
                counter.0 = counter.0.saturating_add(1);
            }
        }
        let default_serialized = default_settings
            .get(key)
            .and_then(|v| serde_json::to_string(v).ok());
        if let Some(value) = pick_mode(&tally, default_serialized.as_deref()) {
            out.insert(key.clone(), value.clone());
        }
    }
    out
}

/// Pick the winning value from a value tally: highest count (min 2), ties broken
/// by matching the Default profile's value, then by smallest serialized form.
fn pick_mode<'v>(
    tally: &BTreeMap<String, (usize, &'v Value)>,
    default_serialized: Option<&str>,
) -> Option<&'v Value> {
    let max_count = tally.values().map(|(count, _)| *count).max()?;
    if max_count < 2 {
        return None;
    }
    // Among values tied at the max count, prefer the Default's value; the BTreeMap
    // iteration order (by serialized form) gives a deterministic fallback.
    let mut winner: Option<(&str, &Value)> = None;
    for (serialized, (count, value)) in tally {
        if *count != max_count {
            continue;
        }
        let prefer = default_serialized == Some(serialized.as_str());
        if prefer {
            return Some(value);
        }
        if winner.is_none() {
            winner = Some((serialized, value));
        }
    }
    winner.map(|(_, value)| value)
}

/// Recursively remove JSON `null` (TOML cannot represent it): `null` itself maps
/// to `None`; nulls inside objects/arrays are dropped.
pub fn strip_nulls(value: &Value) -> Option<Value> {
    match value {
        Value::Null => None,
        Value::Array(items) => Some(Value::Array(items.iter().filter_map(strip_nulls).collect())),
        Value::Object(map) => {
            let cleaned = map
                .iter()
                .filter_map(|(k, v)| strip_nulls(v).map(|s| (k.clone(), s)))
                .collect();
            Some(Value::Object(cleaned))
        }
        other => Some(other.clone()),
    }
}

/// Strip nulls across a settings map, dropping keys whose value was `null`.
pub fn sanitize_settings(settings: &BTreeMap<String, Value>) -> BTreeMap<String, Value> {
    settings
        .iter()
        .filter_map(|(k, v)| strip_nulls(v).map(|s| (k.clone(), s)))
        .collect()
}

#[cfg(test)]
mod tests {
    #![expect(clippy::unwrap_used, reason = "unit tests")]

    use super::*;
    use serde_json::json;

    #[test]
    fn resolve_layers_global_group_and_profile() {
        let mut config = Config::default();
        config.global.settings.insert("a".to_owned(), json!(1));
        config.global.extensions.push("pub.global".to_owned());
        config.groups.insert(
            "g".to_owned(),
            Layer {
                settings: BTreeMap::from([("b".to_owned(), json!(2))]),
                extensions: vec!["pub.group".to_owned()],
            },
        );
        config.profiles.insert(
            "P".to_owned(),
            ProfileConfig {
                groups: vec!["g".to_owned()],
                settings: BTreeMap::from([("a".to_owned(), json!(10))]),
                extensions: vec!["pub.profile".to_owned()],
                exclude_extensions: vec!["pub.global".to_owned()],
                ..ProfileConfig::default()
            },
        );

        let resolved = config.resolve();
        let p = resolved.get("P").unwrap();
        assert_eq!(
            p.settings.get("a"),
            Some(&json!(10)),
            "profile overrides global"
        );
        assert_eq!(
            p.settings.get("b"),
            Some(&json!(2)),
            "group setting present"
        );
        assert!(p.extensions.contains("pub.group"));
        assert!(p.extensions.contains("pub.profile"));
        assert!(
            !p.extensions.contains("pub.global"),
            "exclude removed a global extension"
        );
    }

    #[test]
    fn strip_nulls_removes_nulls_recursively() {
        let value = json!({"a": 1, "b": null, "c": {"d": null, "e": 2}, "f": [1, null, 2]});
        let stripped = strip_nulls(&value).unwrap();
        assert_eq!(stripped, json!({"a": 1, "c": {"e": 2}, "f": [1, 2]}));
    }

    #[test]
    fn normalize_id_drops_version_and_lowercases() {
        assert_eq!(normalize_id("Pub.Name@1.2.3"), "pub.name");
        assert_eq!(normalize_id("  a.b  "), "a.b");
    }

    #[test]
    fn consolidate_hoists_common_items_and_preserves_resolution() {
        let mut config = Config::default();
        // Default and one named profile both share a setting and an extension,
        // and each has something unique.
        config
            .default
            .settings
            .insert("shared".to_owned(), json!(1));
        config
            .default
            .settings
            .insert("only_default".to_owned(), json!("d"));
        config.default.extensions = vec!["pub.shared".to_owned(), "pub.default".to_owned()];
        config.profiles.insert(
            "Rust".to_owned(),
            ProfileConfig {
                settings: BTreeMap::from([
                    ("shared".to_owned(), json!(1)),
                    ("only_rust".to_owned(), json!("r")),
                ]),
                extensions: vec!["pub.shared".to_owned(), "pub.rust".to_owned()],
                ..ProfileConfig::default()
            },
        );

        let before = config.resolve();
        let report = config.consolidate();
        let after = config.resolve();

        assert_eq!(before, after, "consolidation must preserve resolution");
        assert_eq!(report.settings, 1);
        assert_eq!(report.extensions, 1);
        assert_eq!(config.global.settings.get("shared"), Some(&json!(1)));
        assert!(config.global.extensions.contains(&"pub.shared".to_owned()));
        // The shared items are gone from the individual profiles.
        assert!(!config.default.settings.contains_key("shared"));
        let rust = config.profiles.get("Rust").unwrap();
        assert!(!rust.extensions.contains(&"pub.shared".to_owned()));
    }

    #[test]
    fn consolidate_hoists_modal_value_and_keeps_overrides() {
        let mut config = Config::default();
        // "tab" is set in every profile, mostly 2 but 4 in one. "uniq" is set
        // only in one profile, so it must not be hoisted.
        config.default.settings.insert("tab".to_owned(), json!(2));
        config.profiles.insert(
            "A".to_owned(),
            ProfileConfig {
                settings: BTreeMap::from([("tab".to_owned(), json!(2))]),
                ..ProfileConfig::default()
            },
        );
        config.profiles.insert(
            "B".to_owned(),
            ProfileConfig {
                settings: BTreeMap::from([
                    ("tab".to_owned(), json!(4)),
                    ("uniq".to_owned(), json!(true)),
                ]),
                ..ProfileConfig::default()
            },
        );

        let before = config.resolve();
        config.consolidate();
        let after = config.resolve();

        assert_eq!(before, after, "consolidation must preserve resolution");
        // The modal value (2) lands in global; B keeps its override; uniq stays put.
        assert_eq!(config.global.settings.get("tab"), Some(&json!(2)));
        assert_eq!(
            config.profiles.get("B").unwrap().settings.get("tab"),
            Some(&json!(4))
        );
        assert!(!config.global.settings.contains_key("uniq"));
        assert!(!config.default.settings.contains_key("tab"));
    }

    #[test]
    fn consolidate_skips_keys_with_no_shared_value() {
        let mut config = Config::default();
        // "tab" present in all, but every value distinct -> no benefit to hoist.
        config.default.settings.insert("tab".to_owned(), json!(1));
        config.profiles.insert(
            "A".to_owned(),
            ProfileConfig {
                settings: BTreeMap::from([("tab".to_owned(), json!(2))]),
                ..ProfileConfig::default()
            },
        );

        let before = config.resolve();
        config.consolidate();
        assert_eq!(before, config.resolve());
        assert!(!config.global.settings.contains_key("tab"));
    }

    #[test]
    fn toml_roundtrips_dotted_and_nested_settings() {
        let mut config = Config::default();
        config.profiles.insert(
            "P".to_owned(),
            ProfileConfig {
                settings: BTreeMap::from([
                    ("editor.tabSize".to_owned(), json!(2)),
                    ("[rust]".to_owned(), json!({"editor.formatOnSave": true})),
                ]),
                ..ProfileConfig::default()
            },
        );
        let text = config.to_toml().unwrap();
        let parsed: Config = toml::from_str(&text).unwrap();
        let resolved = parsed.resolve();
        let p = resolved.get("P").unwrap();
        assert_eq!(p.settings.get("editor.tabSize"), Some(&json!(2)));
        assert_eq!(
            p.settings.get("[rust]"),
            Some(&json!({"editor.formatOnSave": true}))
        );
    }
}
