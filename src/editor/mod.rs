//! Editor discovery and the on-disk model of a VS Code OSS–based editor.

pub mod discover;
pub mod paths;
pub mod product;
pub mod profiles;

use std::path::{Path, PathBuf};

use anyhow::Result;

use product::Product;

pub use discover::{discover, from_path};

/// A discovered (or explicitly configured) editor installation.
#[derive(Debug, Clone)]
pub struct Editor {
    /// Identity from the editor's `product.json`.
    pub product: Product,
    /// Launcher to invoke for CLI operations (e.g. installing extensions).
    pub launcher: PathBuf,
    /// The `User/` directory holding settings, profiles, and the registry.
    pub user_dir: PathBuf,
    /// The shared extensions install directory.
    pub extensions_dir: PathBuf,
}

impl Editor {
    /// Build an editor from its product identity and launcher, deriving its
    /// user-data and extensions directories.
    pub fn new(product: Product, launcher: PathBuf) -> Result<Self> {
        let user_dir = paths::user_dir(&product)?;
        let extensions_dir = paths::extensions_dir(&product)?;
        Ok(Self {
            product,
            launcher,
            user_dir,
            extensions_dir,
        })
    }

    /// Stable identifier used in config and on the CLI (`nameShort`).
    pub fn id(&self) -> &str {
        &self.product.name_short
    }

    /// Whether `selector` matches this editor by `nameShort` or `applicationName`
    /// (case-insensitive).
    pub fn matches(&self, selector: &str) -> bool {
        self.product.name_short.eq_ignore_ascii_case(selector)
            || self.product.application_name.eq_ignore_ascii_case(selector)
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

/// Resolve the editor a selector refers to, or `None` if not found.
pub fn find(selector: &str) -> Option<Editor> {
    // An explicit path takes precedence over a name lookup.
    let as_path = Path::new(selector);
    if as_path.exists()
        && let Ok(editor) = from_path(as_path)
    {
        return Some(editor);
    }
    discover().into_iter().find(|e| e.matches(selector))
}
