//! Per-OS path derivation for an editor's user-data and extensions directories.

use std::env;
use std::path::PathBuf;

use anyhow::{Context, Result};

use super::product::Product;

/// The user's home directory.
fn home_dir() -> Result<PathBuf> {
    if cfg!(windows) {
        env::var_os("USERPROFILE")
            .map(PathBuf::from)
            .context("USERPROFILE is not set")
    } else {
        env::var_os("HOME")
            .map(PathBuf::from)
            .context("HOME is not set")
    }
}

/// The base directory under which editors keep their per-product user-data
/// directory (`<base>/<nameShort>/User`).
fn user_data_base() -> Result<PathBuf> {
    if cfg!(target_os = "macos") {
        Ok(home_dir()?.join("Library").join("Application Support"))
    } else if cfg!(windows) {
        env::var_os("APPDATA")
            .map(PathBuf::from)
            .context("APPDATA is not set")
    } else {
        // Linux / other XDG systems.
        if let Some(xdg) = env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
            Ok(PathBuf::from(xdg))
        } else {
            Ok(home_dir()?.join(".config"))
        }
    }
}

/// The `User/` directory for `product` (settings, keybindings, profiles, ...).
pub fn user_dir(product: &Product) -> Result<PathBuf> {
    Ok(user_data_base()?.join(&product.name_short).join("User"))
}

/// The default shared extensions directory for `product`
/// (`$HOME/<dataFolderName>/extensions`).
pub fn extensions_dir(product: &Product) -> Result<PathBuf> {
    Ok(home_dir()?
        .join(&product.data_folder_name)
        .join("extensions"))
}
