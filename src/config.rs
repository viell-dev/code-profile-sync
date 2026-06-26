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

/// Top-level config document.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub editor: EditorRef,
    pub global: Layer,
    pub groups: BTreeMap<String, Layer>,
    pub profiles: BTreeMap<String, ProfileConfig>,
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
#[derive(Debug, Default, Clone)]
pub struct Resolved {
    pub settings: BTreeMap<String, Value>,
    pub extensions: BTreeSet<String>,
    pub icon: Option<String>,
    pub use_default: BTreeMap<String, bool>,
}

impl Config {
    /// Load a config from a TOML file.
    pub fn load(path: &Path) -> Result<Self> {
        let raw = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let config: Self =
            toml::from_str(&raw).with_context(|| format!("parsing {}", path.display()))?;
        Ok(config)
    }

    /// Serialize the config to a TOML string with a generated-file header.
    pub fn to_toml(&self) -> Result<String> {
        let body = toml::to_string_pretty(&self.sanitized()).context("serializing config")?;
        Ok(format!("# code-profile-sync config. See README.md for the format.\n\n{body}"))
    }

    /// A clone with all settings null values stripped (TOML has no null type).
    fn sanitized(&self) -> Self {
        let mut clone = self.clone();
        clone.global.settings = sanitize_settings(&clone.global.settings);
        for group in clone.groups.values_mut() {
            group.settings = sanitize_settings(&group.settings);
        }
        for profile in clone.profiles.values_mut() {
            profile.settings = sanitize_settings(&profile.settings);
        }
        clone
    }

    /// Effective desired state per profile (keyed by profile name).
    pub fn resolve(&self) -> BTreeMap<String, Resolved> {
        let mut out = BTreeMap::new();
        for (name, profile) in &self.profiles {
            out.insert(name.clone(), self.resolve_profile(profile));
        }
        out
    }

    fn resolve_profile(&self, profile: &ProfileConfig) -> Resolved {
        let mut settings = self.global.settings.clone();
        let mut extensions: BTreeSet<String> =
            self.global.extensions.iter().map(|id| normalize_id(id)).collect();

        for group_name in &profile.groups {
            if let Some(group) = self.groups.get(group_name) {
                for (key, value) in &group.settings {
                    settings.insert(key.clone(), value.clone());
                }
                extensions.extend(group.extensions.iter().map(|id| normalize_id(id)));
            }
        }

        for (key, value) in &profile.settings {
            settings.insert(key.clone(), value.clone());
        }
        extensions.extend(profile.extensions.iter().map(|id| normalize_id(id)));
        for excluded in &profile.exclude_extensions {
            extensions.remove(&normalize_id(excluded));
        }

        Resolved {
            settings,
            extensions,
            icon: profile.icon.clone(),
            use_default: profile.use_default.clone(),
        }
    }
}

/// Normalize an extension identifier for set membership: drop any `@version`
/// pin and lowercase (`publisher.name` is case-insensitive).
pub fn normalize_id(spec: &str) -> String {
    let id = spec.split('@').next().unwrap_or(spec);
    id.trim().to_lowercase()
}

/// Recursively remove JSON `null` (TOML cannot represent it): `null` itself maps
/// to `None`; nulls inside objects/arrays are dropped.
pub fn strip_nulls(value: &Value) -> Option<Value> {
    match value {
        Value::Null => None,
        Value::Array(items) => Some(Value::Array(items.iter().filter_map(strip_nulls).collect())),
        Value::Object(map) => {
            let cleaned =
                map.iter().filter_map(|(k, v)| strip_nulls(v).map(|s| (k.clone(), s))).collect();
            Some(Value::Object(cleaned))
        }
        other => Some(other.clone()),
    }
}

/// Strip nulls across a settings map, dropping keys whose value was `null`.
pub fn sanitize_settings(settings: &BTreeMap<String, Value>) -> BTreeMap<String, Value> {
    settings.iter().filter_map(|(k, v)| strip_nulls(v).map(|s| (k.clone(), s))).collect()
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
        assert_eq!(p.settings.get("a"), Some(&json!(10)), "profile overrides global");
        assert_eq!(p.settings.get("b"), Some(&json!(2)), "group setting present");
        assert!(p.extensions.contains("pub.group"));
        assert!(p.extensions.contains("pub.profile"));
        assert!(!p.extensions.contains("pub.global"), "exclude removed a global extension");
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
        assert_eq!(p.settings.get("[rust]"), Some(&json!({"editor.formatOnSave": true})));
    }
}
