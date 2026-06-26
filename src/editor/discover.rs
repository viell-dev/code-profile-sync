//! Discover installed editors by scanning the `PATH` for known launchers and
//! identifying each via its `product.json` (see [`super::product`]).
//!
//! Editor installs lay out their files inconsistently (a launcher may be a
//! symlink into the install tree, or a wrapper script that reveals nothing), so
//! we probe a set of candidate `product.json` locations relative to the resolved
//! launcher and under common system prefixes, then take the first that parses.

use std::env;
use std::path::{Path, PathBuf};

use super::Editor;
use super::product::Product;

/// Launcher command names we look for on `PATH`, in preference order.
const LAUNCHERS: &[&str] = &[
    "code-oss",
    "codium",
    "vscodium",
    "code",
    "code-insiders",
    "cursor",
];

/// System prefixes under which distros install editor app trees.
const SYSTEM_PREFIXES: &[&str] = &[
    "/usr/lib",
    "/usr/share",
    "/opt",
    "/usr/local/lib",
    "/usr/local/share",
];

/// Find all editors discoverable on this machine, de-duplicated by product.
pub fn discover() -> Vec<Editor> {
    let mut editors: Vec<Editor> = Vec::new();
    for name in LAUNCHERS {
        let Some(launcher) = which(name) else {
            continue;
        };
        let Some(product_path) = find_product_json(&launcher) else {
            continue;
        };
        let Ok(product) = Product::from_file(&product_path) else {
            continue;
        };
        let Ok(editor) = Editor::new(product, launcher) else {
            continue;
        };
        if !editors
            .iter()
            .any(|e| e.product.name_short == editor.product.name_short)
        {
            editors.push(editor);
        }
    }
    editors
}

/// Build an [`Editor`] from an explicit user-provided path, which may point at a
/// launcher binary, an install directory, or a `product.json` itself.
pub fn from_path(input: &Path) -> anyhow::Result<Editor> {
    let product_path = if input.is_file() && input.file_name() == Some("product.json".as_ref()) {
        input.to_path_buf()
    } else if input.is_dir() {
        product_in_dir(input)
            .ok_or_else(|| anyhow::anyhow!("no product.json found under {}", input.display()))?
    } else {
        find_product_json(input).ok_or_else(|| {
            anyhow::anyhow!("could not locate product.json for {}", input.display())
        })?
    };
    let product = Product::from_file(&product_path)?;
    // For a launcher file, invoke it directly; otherwise fall back to the
    // application name resolved from PATH.
    let launcher = if input.is_file() && input.file_name() != Some("product.json".as_ref()) {
        input.to_path_buf()
    } else {
        which(&product.application_name).unwrap_or_else(|| PathBuf::from(&product.application_name))
    };
    Editor::new(product, launcher)
}

/// Locate the first existing executable named `name` on `PATH`.
fn which(name: &str) -> Option<PathBuf> {
    let path = env::var_os("PATH")?;
    for dir in env::split_paths(&path) {
        let candidate = dir.join(name);
        if is_executable_file(&candidate) {
            return Some(candidate);
        }
        if cfg!(windows) {
            let exe = dir.join(format!("{name}.exe"));
            if is_executable_file(&exe) {
                return Some(exe);
            }
        }
    }
    None
}

fn is_executable_file(path: &Path) -> bool {
    path.is_file()
}

/// Probe candidate `product.json` locations for a launcher path.
fn find_product_json(launcher: &Path) -> Option<PathBuf> {
    let resolved = launcher
        .canonicalize()
        .unwrap_or_else(|_| launcher.to_path_buf());

    // 1. Walk up from the resolved launcher, checking each ancestor directory.
    for ancestor in resolved.ancestors().skip(1) {
        if let Some(found) = product_in_dir(ancestor) {
            return Some(found);
        }
    }

    // 2. Name-based fallbacks under common system prefixes (for wrapper-script
    //    launchers that don't resolve into their install tree).
    for name in name_variants(launcher) {
        for prefix in SYSTEM_PREFIXES {
            if let Some(found) = product_in_dir(&Path::new(prefix).join(&name)) {
                return Some(found);
            }
        }
    }
    None
}

/// Return `dir/product.json` or `dir/resources/app/product.json` if either exists.
fn product_in_dir(dir: &Path) -> Option<PathBuf> {
    let direct = dir.join("product.json");
    if direct.is_file() {
        return Some(direct);
    }
    let nested = dir.join("resources").join("app").join("product.json");
    if nested.is_file() {
        return Some(nested);
    }
    None
}

/// Directory-name guesses for a launcher, e.g. `code-oss` also implies `code`.
fn name_variants(launcher: &Path) -> Vec<String> {
    let mut names = Vec::new();
    if let Some(base) = launcher.file_name().and_then(|n| n.to_str()) {
        names.push(base.to_owned());
        for suffix in ["-oss", "-insiders"] {
            if let Some(stripped) = base.strip_suffix(suffix) {
                names.push(stripped.to_owned());
            }
        }
    }
    names
}
