//! Editor discovery and the on-disk model of a VS Code OSS–based editor.

pub mod discover;
pub mod paths;
pub mod product;
pub mod profiles;

use std::path::PathBuf;

use anyhow::Result;

use crate::app_config::identifiers_match;
use product::Product;

pub use discover::{discover, from_path};

/// A discovered (or explicitly configured) editor installation.
#[derive(Debug, Clone)]
pub struct Editor {
    /// Identity from the editor's `product.json`.
    pub product: Product,
    /// Launcher to invoke for CLI operations (e.g. installing extensions).
    pub launcher: PathBuf,
    /// Command names that resolved to this product during discovery.
    pub launcher_aliases: Vec<String>,
    /// The `User/` directory holding settings, profiles, and the registry.
    pub user_dir: PathBuf,
    /// The shared extensions install directory.
    pub extensions_dir: PathBuf,
}

impl Editor {
    /// Build an editor from its product identity and launcher, deriving its
    /// user-data and extensions directories.
    pub fn new(product: Product, launcher: PathBuf, launcher_aliases: Vec<String>) -> Result<Self> {
        let user_dir = paths::user_dir(&product)?;
        let extensions_dir = paths::extensions_dir(&product)?;
        Ok(Self {
            product,
            launcher,
            launcher_aliases,
            user_dir,
            extensions_dir,
        })
    }

    /// Stable identifier used in config and on the CLI (`nameShort`).
    pub fn id(&self) -> &str {
        &self.product.name_short
    }

    /// Whether `selector` matches this editor by `nameShort` or `applicationName`
    /// or generated aliases.
    pub fn matches(&self, selector: &str) -> bool {
        self.aliases()
            .iter()
            .any(|alias| identifiers_match(alias, selector))
    }

    /// Built-in aliases derived from `product.json` and discovery.
    pub fn aliases(&self) -> Vec<String> {
        let mut aliases = vec![
            self.product.name_short.clone(),
            self.product.name_long.clone(),
            self.product.application_name.clone(),
        ];
        aliases.extend(self.launcher_aliases.clone());
        dedupe_aliases(aliases)
    }

    /// Path to the profile registry (`globalStorage/storage.json`).
    pub fn storage_json(&self) -> PathBuf {
        self.user_dir.join("globalStorage").join("storage.json")
    }

    /// Directory holding a named profile's data, given its location id.
    pub fn profile_dir(&self, location: &str) -> PathBuf {
        self.user_dir.join("profiles").join(location)
    }

    /// Whether the user-data directory actually exists on disk.
    pub fn is_present(&self) -> bool {
        self.user_dir.is_dir()
    }
}

fn dedupe_aliases(aliases: Vec<String>) -> Vec<String> {
    let mut out = Vec::new();
    for alias in aliases {
        if alias.is_empty()
            || out
                .iter()
                .any(|existing: &String| identifiers_match(existing, &alias))
        {
            continue;
        }
        out.push(alias);
    }
    out
}
