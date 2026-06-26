//! Reading the editor's profile registry (`globalStorage/storage.json`) and
//! resolving where each profile keeps its settings and extension list.

use std::collections::BTreeMap;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::Editor;
use crate::safety;

/// Name of the implicit Default profile (its data lives at the `User/` root).
pub const DEFAULT_PROFILE: &str = "Default";

/// A profile entry exactly as stored in `userDataProfiles`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct StoredProfile {
    pub name: String,
    pub location: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    #[serde(
        rename = "useDefaultFlags",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub use_default_flags: Option<BTreeMap<String, bool>>,
}

/// A profile in the editor, including the implicit Default profile.
#[derive(Debug, Clone)]
pub struct Profile {
    pub name: String,
    /// Location id of the profile directory, or `None` for the Default profile.
    pub location: Option<String>,
    pub icon: Option<String>,
    /// Resource types inherited from the Default profile (`useDefaultFlags`).
    pub use_default: BTreeMap<String, bool>,
}

impl Profile {
    /// The Default profile.
    fn default_profile() -> Self {
        Self {
            name: DEFAULT_PROFILE.to_owned(),
            location: None,
            icon: None,
            use_default: BTreeMap::new(),
        }
    }

    pub fn is_default(&self) -> bool {
        self.location.is_none()
    }

    /// Path to this profile's `settings.json`.
    pub fn settings_path(&self, editor: &Editor) -> PathBuf {
        match &self.location {
            Some(loc) => editor.profile_dir(loc).join("settings.json"),
            None => editor.user_dir.join("settings.json"),
        }
    }

    /// Path to this profile's extension membership list. Named profiles keep it
    /// in the profile directory; the Default profile uses the shared extensions
    /// directory's own `extensions.json`.
    pub fn extensions_path(&self, editor: &Editor) -> PathBuf {
        match &self.location {
            Some(loc) => editor.profile_dir(loc).join("extensions.json"),
            None => editor.extensions_dir.join("extensions.json"),
        }
    }

    /// Whether this profile inherits `resource` (e.g. `"settings"`,
    /// `"extensions"`, `"keybindings"`) from the Default profile.
    pub fn inherits(&self, resource: &str) -> bool {
        self.use_default.get(resource).copied().unwrap_or(false)
    }

    /// The `--profile` argument for the editor CLI, or `None` for the Default
    /// profile (which is targeted by omitting the flag).
    pub fn cli_profile(&self) -> Option<&str> {
        if self.is_default() {
            None
        } else {
            Some(self.name.as_str())
        }
    }
}

/// Read the raw `userDataProfiles` array from the registry.
pub fn read_stored(editor: &Editor) -> Result<Vec<StoredProfile>> {
    let path = editor.storage_json();
    if !path.is_file() {
        return Ok(Vec::new());
    }
    let raw = fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let value: serde_json::Value =
        serde_json::from_str(&raw).with_context(|| format!("parsing {}", path.display()))?;
    let Some(field) = value.get("userDataProfiles") else {
        return Ok(Vec::new());
    };
    // The value is normally a JSON array, but some builds store it as a
    // JSON-encoded string; handle both.
    let profiles = match field {
        serde_json::Value::String(encoded) => {
            serde_json::from_str(encoded).context("parsing stringified userDataProfiles")?
        }
        other => serde_json::from_value(other.clone()).context("parsing userDataProfiles array")?,
    };
    Ok(profiles)
}

/// All profiles in the editor, Default first.
pub fn read_all(editor: &Editor) -> Result<Vec<Profile>> {
    let mut profiles = vec![Profile::default_profile()];
    for stored in read_stored(editor)? {
        profiles.push(Profile {
            name: stored.name,
            location: Some(stored.location),
            icon: stored.icon,
            use_default: stored.use_default_flags.unwrap_or_default(),
        });
    }
    Ok(profiles)
}

/// Create a named profile: register it in `storage.json` and create its data
/// directory. Returns the resulting [`Profile`]. A dry run only constructs the
/// model without touching disk.
pub fn create(
    editor: &Editor,
    name: &str,
    icon: Option<&str>,
    use_default: &BTreeMap<String, bool>,
    dry_run: bool,
    backup_dir: &std::path::Path,
) -> Result<Profile> {
    let mut stored = read_stored(editor)?;
    let location = generate_location(name, &stored);
    let profile = Profile {
        name: name.to_owned(),
        location: Some(location.clone()),
        icon: icon.map(ToOwned::to_owned),
        use_default: use_default.clone(),
    };
    if dry_run {
        return Ok(profile);
    }
    stored.push(StoredProfile {
        name: name.to_owned(),
        location: location.clone(),
        icon: profile.icon.clone(),
        use_default_flags: (!use_default.is_empty()).then(|| use_default.clone()),
    });
    write_stored(editor, &stored, backup_dir)?;
    let dir = editor.profile_dir(&location);
    fs::create_dir_all(&dir).with_context(|| format!("creating profile dir {}", dir.display()))?;
    Ok(profile)
}

/// Replace the `userDataProfiles` array in `storage.json`, preserving all other
/// keys. Creates the file if missing.
fn write_stored(
    editor: &Editor,
    stored: &[StoredProfile],
    backup_dir: &std::path::Path,
) -> Result<()> {
    let path = editor.storage_json();
    let mut root: serde_json::Value = if path.is_file() {
        let raw =
            fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        serde_json::from_str(&raw).with_context(|| format!("parsing {}", path.display()))?
    } else {
        serde_json::Value::Object(serde_json::Map::new())
    };
    let serde_json::Value::Object(map) = &mut root else {
        anyhow::bail!("{} is not a JSON object", path.display());
    };
    map.insert("userDataProfiles".to_owned(), serde_json::to_value(stored)?);

    safety::backup_file(&path, backup_dir)?;
    let mut text = serde_json::to_string(&root).context("serializing storage.json")?;
    text.push('\n');
    safety::atomic_write(&path, &text)
}

/// Generate a profile location id (8 hex chars) not already in use.
fn generate_location(name: &str, existing: &[StoredProfile]) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    for attempt in 0..u32::MAX {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        name.hash(&mut hasher);
        nanos.hash(&mut hasher);
        attempt.hash(&mut hasher);
        let masked = hasher.finish() & 0xFFFF_FFFF;
        let id = format!("{:08x}", u32::try_from(masked).unwrap_or(0));
        if existing.iter().all(|s| s.location != id) {
            return id;
        }
    }
    // Practically unreachable; fall back to a timestamp-derived id.
    format!("{nanos:08x}")
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};

    use anyhow::Result;
    use tempfile::tempdir;

    use super::*;
    use crate::editor::product::Product;

    fn editor(user_dir: &Path, extensions_dir: &Path) -> Editor {
        Editor {
            product: Product {
                name_short: "VSCodium".to_owned(),
                name_long: "VSCodium".to_owned(),
                application_name: "codium".to_owned(),
                data_folder_name: ".vscode-oss".to_owned(),
                quality: Some("stable".to_owned()),
                commit: Some("abc123".to_owned()),
            },
            launcher: PathBuf::from("codium"),
            launcher_aliases: vec!["codium".to_owned()],
            user_dir: user_dir.to_path_buf(),
            extensions_dir: extensions_dir.to_path_buf(),
        }
    }

    #[test]
    fn read_all_includes_default_and_named_profiles() -> Result<()> {
        let dir = tempdir()?;
        let user_dir = dir.path().join("User");
        let storage_dir = user_dir.join("globalStorage");
        fs::create_dir_all(&storage_dir)?;
        fs::write(
            storage_dir.join("storage.json"),
            r#"{
                "unrelated": true,
                "userDataProfiles": [
                    {
                        "location": "-72fdf191",
                        "name": "Rust",
                        "icon": "package",
                        "useDefaultFlags": { "keybindings": true }
                    }
                ]
            }"#,
        )?;
        let editor = editor(&user_dir, &dir.path().join("extensions"));

        let profiles = read_all(&editor)?;

        assert_eq!(profiles.len(), 2);
        assert_eq!(profiles.first().map(|p| p.name.as_str()), Some("Default"));
        assert_eq!(profiles.get(1).map(|p| p.name.as_str()), Some("Rust"));
        assert_eq!(
            profiles
                .get(1)
                .and_then(|p| p.use_default.get("keybindings"))
                .copied(),
            Some(true)
        );
        Ok(())
    }

    #[test]
    fn read_stored_accepts_stringified_profile_array() -> Result<()> {
        let dir = tempdir()?;
        let user_dir = dir.path().join("User");
        let storage_dir = user_dir.join("globalStorage");
        fs::create_dir_all(&storage_dir)?;
        fs::write(
            storage_dir.join("storage.json"),
            r#"{
                "userDataProfiles": "[{\"location\":\"abc123\",\"name\":\"Web\"}]"
            }"#,
        )?;
        let editor = editor(&user_dir, &dir.path().join("extensions"));

        let stored = read_stored(&editor)?;

        assert_eq!(stored.len(), 1);
        assert_eq!(stored.first().map(|p| p.name.as_str()), Some("Web"));
        assert_eq!(stored.first().map(|p| p.location.as_str()), Some("abc123"));
        Ok(())
    }

    #[test]
    fn profile_paths_respect_default_vs_named_storage_locations() -> Result<()> {
        let dir = tempdir()?;
        let user_dir = dir.path().join("User");
        let extensions_dir = dir.path().join("extensions");
        let editor = editor(&user_dir, &extensions_dir);
        let default = Profile::default_profile();
        let named = Profile {
            name: "Rust".to_owned(),
            location: Some("-72fdf191".to_owned()),
            icon: None,
            use_default: BTreeMap::new(),
        };

        assert_eq!(
            default.settings_path(&editor),
            user_dir.join("settings.json")
        );
        assert_eq!(
            default.extensions_path(&editor),
            extensions_dir.join("extensions.json")
        );
        assert_eq!(
            named.settings_path(&editor),
            user_dir
                .join("profiles")
                .join("-72fdf191")
                .join("settings.json")
        );
        assert_eq!(
            named.extensions_path(&editor),
            user_dir
                .join("profiles")
                .join("-72fdf191")
                .join("extensions.json")
        );
        Ok(())
    }
}
