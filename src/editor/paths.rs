//! Per-OS path derivation for an editor's user-data and extensions directories.

use std::env;
use std::path::PathBuf;

use anyhow::{Context, Result};

use super::product::Product;

#[derive(Debug, Clone, Copy)]
enum Platform {
    Linux,
    Macos,
    Windows,
}

#[derive(Debug, Default)]
struct PathEnv {
    home: Option<PathBuf>,
    user_profile: Option<PathBuf>,
    appdata: Option<PathBuf>,
    xdg_config_home: Option<PathBuf>,
}

impl PathEnv {
    fn current() -> Self {
        Self {
            home: env::var_os("HOME").map(PathBuf::from),
            user_profile: env::var_os("USERPROFILE").map(PathBuf::from),
            appdata: env::var_os("APPDATA").map(PathBuf::from),
            xdg_config_home: env::var_os("XDG_CONFIG_HOME").map(PathBuf::from),
        }
    }
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

/// The user's home directory.
fn home_dir(platform: Platform, vars: &PathEnv) -> Result<PathBuf> {
    match platform {
        Platform::Windows => vars.user_profile.clone().context("USERPROFILE is not set"),
        Platform::Linux | Platform::Macos => vars.home.clone().context("HOME is not set"),
    }
}

/// The base directory under which editors keep their per-product user-data
/// directory (`<base>/<nameShort>/User`).
fn user_data_base(platform: Platform, vars: &PathEnv) -> Result<PathBuf> {
    match platform {
        Platform::Macos => Ok(home_dir(platform, vars)?
            .join("Library")
            .join("Application Support")),
        Platform::Windows => vars.appdata.clone().context("APPDATA is not set"),
        Platform::Linux => {
            if let Some(xdg) = vars
                .xdg_config_home
                .as_ref()
                .filter(|v| !v.as_os_str().is_empty())
            {
                Ok(xdg.clone())
            } else {
                Ok(home_dir(platform, vars)?.join(".config"))
            }
        }
    }
}

fn user_dir_for(product: &Product, platform: Platform, vars: &PathEnv) -> Result<PathBuf> {
    Ok(user_data_base(platform, vars)?
        .join(&product.name_short)
        .join("User"))
}

fn extensions_dir_for(product: &Product, platform: Platform, vars: &PathEnv) -> Result<PathBuf> {
    Ok(home_dir(platform, vars)?
        .join(&product.data_folder_name)
        .join("extensions"))
}

/// The `User/` directory for `product` (settings, keybindings, profiles, ...).
pub fn user_dir(product: &Product) -> Result<PathBuf> {
    user_dir_for(product, current_platform(), &PathEnv::current())
}

/// The default shared extensions directory for `product`
/// (`$HOME/<dataFolderName>/extensions`).
pub fn extensions_dir(product: &Product) -> Result<PathBuf> {
    extensions_dir_for(product, current_platform(), &PathEnv::current())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn product() -> Product {
        Product {
            name_short: "VSCodium".to_owned(),
            name_long: "VSCodium".to_owned(),
            application_name: "codium".to_owned(),
            data_folder_name: ".vscode-oss".to_owned(),
            quality: Some("stable".to_owned()),
            commit: Some("abc123".to_owned()),
        }
    }

    #[test]
    fn derives_linux_paths_from_xdg_config_home() -> Result<()> {
        let vars = PathEnv {
            home: Some(PathBuf::from("/home/alice")),
            xdg_config_home: Some(PathBuf::from("/xdg/config")),
            ..PathEnv::default()
        };

        assert_eq!(
            user_dir_for(&product(), Platform::Linux, &vars)?,
            PathBuf::from("/xdg/config").join("VSCodium").join("User")
        );
        assert_eq!(
            extensions_dir_for(&product(), Platform::Linux, &vars)?,
            PathBuf::from("/home/alice")
                .join(".vscode-oss")
                .join("extensions")
        );
        Ok(())
    }

    #[test]
    fn derives_linux_paths_from_home_when_xdg_is_empty() -> Result<()> {
        let vars = PathEnv {
            home: Some(PathBuf::from("/home/alice")),
            xdg_config_home: Some(PathBuf::new()),
            ..PathEnv::default()
        };

        assert_eq!(
            user_dir_for(&product(), Platform::Linux, &vars)?,
            PathBuf::from("/home/alice")
                .join(".config")
                .join("VSCodium")
                .join("User")
        );
        Ok(())
    }

    #[test]
    fn derives_macos_paths_from_home() -> Result<()> {
        let vars = PathEnv {
            home: Some(PathBuf::from("/Users/alice")),
            ..PathEnv::default()
        };

        assert_eq!(
            user_dir_for(&product(), Platform::Macos, &vars)?,
            PathBuf::from("/Users/alice")
                .join("Library")
                .join("Application Support")
                .join("VSCodium")
                .join("User")
        );
        assert_eq!(
            extensions_dir_for(&product(), Platform::Macos, &vars)?,
            PathBuf::from("/Users/alice")
                .join(".vscode-oss")
                .join("extensions")
        );
        Ok(())
    }

    #[test]
    fn derives_windows_paths_from_appdata_and_userprofile() -> Result<()> {
        let vars = PathEnv {
            user_profile: Some(PathBuf::from(r"C:\Users\alice")),
            appdata: Some(PathBuf::from(r"C:\Users\alice\AppData\Roaming")),
            ..PathEnv::default()
        };

        assert_eq!(
            user_dir_for(&product(), Platform::Windows, &vars)?,
            PathBuf::from(r"C:\Users\alice\AppData\Roaming")
                .join("VSCodium")
                .join("User")
        );
        assert_eq!(
            extensions_dir_for(&product(), Platform::Windows, &vars)?,
            PathBuf::from(r"C:\Users\alice")
                .join(".vscode-oss")
                .join("extensions")
        );
        Ok(())
    }

    #[test]
    fn reports_missing_required_environment() {
        let vars = PathEnv::default();

        assert!(user_dir_for(&product(), Platform::Linux, &vars).is_err());
        assert!(user_dir_for(&product(), Platform::Macos, &vars).is_err());
        assert!(user_dir_for(&product(), Platform::Windows, &vars).is_err());
        assert!(extensions_dir_for(&product(), Platform::Windows, &vars).is_err());
    }
}
