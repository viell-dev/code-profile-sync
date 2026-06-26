//! Per-user application config and app-home path derivation.

use std::env;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::editor::Editor;
use crate::safety;

const APP_DIR_NAME: &str = "code-profile-manager";

#[derive(Debug, Clone)]
pub struct AppPaths {
    pub config: PathBuf,
    pub editors_dir: PathBuf,
    pub snapshots_dir: PathBuf,
    pub backups_dir: PathBuf,
    pub vendor_dir: PathBuf,
}

impl AppPaths {
    pub fn resolve(override_dir: Option<&Path>) -> Result<Self> {
        let root = match override_dir {
            Some(path) => path.to_path_buf(),
            None => default_app_home()?,
        };
        Ok(Self {
            config: root.join("config.toml"),
            editors_dir: root.join("editors"),
            snapshots_dir: root.join("snapshots"),
            backups_dir: root.join("backups"),
            vendor_dir: root.join("vendor").join("extensions"),
        })
    }

    pub fn editor_config_path(&self, editor: &Editor) -> PathBuf {
        self.editors_dir
            .join(format!("{}.toml", safe_id(editor.id())))
    }

    pub fn snapshot_path(&self, editor: &Editor) -> PathBuf {
        self.snapshots_dir
            .join(format!("{}.snapshot.json", safe_id(editor.id())))
    }

    pub fn backup_dir(&self) -> PathBuf {
        self.backups_dir.join(safety::timestamp())
    }
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AppConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_editor: Option<String>,
    pub editors: Vec<KnownEditor>,
}

impl AppConfig {
    pub fn load(path: &Path) -> Result<Self> {
        if !path.is_file() {
            return Ok(Self::default());
        }
        let raw =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        toml::from_str(&raw).with_context(|| format!("parsing {}", path.display()))
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let body = toml::to_string_pretty(self).context("serializing app config")?;
        safety::atomic_write(
            path,
            &format!("# code-profile-manager app config.\n\n{body}"),
        )
    }

    pub fn upsert_editor(&mut self, editor: &Editor) {
        let known = KnownEditor::from_editor(editor);
        if let Some(existing) = self.editors.iter_mut().find(|e| e.id == known.id) {
            existing.merge(known);
        } else {
            self.editors.push(known);
        }
    }
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct KnownEditor {
    pub id: String,
    pub aliases: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub binary: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_dir: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extensions_dir: Option<PathBuf>,
}

impl KnownEditor {
    pub fn matches(&self, selector: &str) -> bool {
        identifiers_match(&self.id, selector)
            || self
                .aliases
                .iter()
                .any(|alias| identifiers_match(alias, selector))
    }

    fn from_editor(editor: &Editor) -> Self {
        Self {
            id: editor.id().to_owned(),
            aliases: editor.aliases(),
            binary: Some(editor.launcher.clone()),
            user_dir: Some(editor.user_dir.clone()),
            extensions_dir: Some(editor.extensions_dir.clone()),
        }
    }

    fn merge(&mut self, other: Self) {
        self.binary = other.binary;
        self.user_dir = other.user_dir;
        self.extensions_dir = other.extensions_dir;
        for alias in other.aliases {
            if !self
                .aliases
                .iter()
                .any(|existing| identifiers_match(existing, &alias))
            {
                self.aliases.push(alias);
            }
        }
    }
}

#[derive(Debug, Default)]
struct PathEnv {
    home: Option<PathBuf>,
    local_appdata: Option<PathBuf>,
    xdg_config_home: Option<PathBuf>,
}

impl PathEnv {
    fn current() -> Self {
        Self {
            home: env::var_os("HOME").map(PathBuf::from),
            local_appdata: env::var_os("LOCALAPPDATA").map(PathBuf::from),
            xdg_config_home: env::var_os("XDG_CONFIG_HOME").map(PathBuf::from),
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum Platform {
    Linux,
    Macos,
    Windows,
}

fn current_platform() -> Platform {
    if cfg!(windows) {
        Platform::Windows
    } else if cfg!(target_os = "macos") {
        Platform::Macos
    } else {
        Platform::Linux
    }
}

fn default_app_home() -> Result<PathBuf> {
    default_app_home_for(current_platform(), &PathEnv::current())
}

fn default_app_home_for(platform: Platform, vars: &PathEnv) -> Result<PathBuf> {
    match platform {
        Platform::Linux => {
            if let Some(xdg) = vars
                .xdg_config_home
                .as_ref()
                .filter(|v| !v.as_os_str().is_empty())
            {
                Ok(xdg.join(APP_DIR_NAME))
            } else {
                Ok(vars
                    .home
                    .clone()
                    .context("HOME is not set")?
                    .join(".config")
                    .join(APP_DIR_NAME))
            }
        }
        Platform::Macos => Ok(vars
            .home
            .clone()
            .context("HOME is not set")?
            .join("Library")
            .join("Application Support")
            .join(APP_DIR_NAME)),
        Platform::Windows => Ok(vars
            .local_appdata
            .clone()
            .context("LOCALAPPDATA is not set")?
            .join(APP_DIR_NAME)),
    }
}

pub fn identifiers_match(left: &str, right: &str) -> bool {
    left.eq_ignore_ascii_case(right) || normalize_identifier(left) == normalize_identifier(right)
}

pub fn safe_id(value: &str) -> String {
    let normalized = value
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>();
    normalized
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

fn normalize_identifier(value: &str) -> String {
    value
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derives_linux_app_home_from_xdg_config_home() -> Result<()> {
        let vars = PathEnv {
            xdg_config_home: Some(PathBuf::from("/xdg/config")),
            ..PathEnv::default()
        };

        assert_eq!(
            default_app_home_for(Platform::Linux, &vars)?,
            PathBuf::from("/xdg/config").join(APP_DIR_NAME)
        );
        Ok(())
    }

    #[test]
    fn derives_linux_app_home_from_home_fallback() -> Result<()> {
        let vars = PathEnv {
            home: Some(PathBuf::from("/home/alice")),
            xdg_config_home: Some(PathBuf::new()),
            ..PathEnv::default()
        };

        assert_eq!(
            default_app_home_for(Platform::Linux, &vars)?,
            PathBuf::from("/home/alice")
                .join(".config")
                .join(APP_DIR_NAME)
        );
        Ok(())
    }

    #[test]
    fn derives_macos_app_home_from_home() -> Result<()> {
        let vars = PathEnv {
            home: Some(PathBuf::from("/Users/alice")),
            ..PathEnv::default()
        };

        assert_eq!(
            default_app_home_for(Platform::Macos, &vars)?,
            PathBuf::from("/Users/alice")
                .join("Library")
                .join("Application Support")
                .join(APP_DIR_NAME)
        );
        Ok(())
    }

    #[test]
    fn derives_windows_app_home_from_local_appdata() -> Result<()> {
        let vars = PathEnv {
            local_appdata: Some(PathBuf::from(r"C:\Users\alice\AppData\Local")),
            ..PathEnv::default()
        };

        assert_eq!(
            default_app_home_for(Platform::Windows, &vars)?,
            PathBuf::from(r"C:\Users\alice\AppData\Local").join(APP_DIR_NAME)
        );
        Ok(())
    }

    #[test]
    fn matches_aliases_with_normalized_punctuation() {
        assert!(identifiers_match("Code - OSS", "codeoss"));
        assert!(identifiers_match("code-oss", "CODE OSS"));
        assert!(!identifiers_match("codium", "cursor"));
    }
}
