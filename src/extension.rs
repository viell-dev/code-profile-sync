//! Reading per-profile extension membership and changing it.
//!
//! Membership is read directly from the relevant `extensions.json`. To add an
//! extension we prefer the shared pool: if it is already installed in the
//! editor's extensions directory, we copy its catalog entry straight into the
//! profile's membership list (no marketplace needed). Only when it is absent do
//! we shell out to the editor CLI to fetch it. Removal edits the membership list
//! directly and never deletes shared files on disk.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::Value;

use crate::config::normalize_id;
use crate::editor::Editor;
use crate::editor::profiles::Profile;
use crate::safety;

/// Full pool catalog entries, keyed by normalized extension id.
pub type Catalog = BTreeMap<String, Value>;

/// How an extension was added to a profile.
pub enum AddMethod {
    /// Copied from the shared pool catalog (already installed on disk).
    Pool,
    /// Restored from a vendored copy in the repo.
    Vendor,
    /// Fetched and installed via the editor CLI.
    Cli,
}

#[derive(Debug, Deserialize)]
struct RawEntry {
    identifier: RawIdentifier,
}

#[derive(Debug, Deserialize)]
struct RawIdentifier {
    id: String,
}

/// Read the set of installed extension IDs (normalized) for a profile.
pub fn read_membership(editor: &Editor, profile: &Profile) -> Result<BTreeSet<String>> {
    read_membership_file(&profile.extensions_path(editor))
}

/// Read normalized extension IDs from an `extensions.json` file.
pub fn read_membership_file(path: &Path) -> Result<BTreeSet<String>> {
    Ok(read_entries(path)?.iter().filter_map(entry_id).collect())
}

/// The shared extensions pool catalog (full entries with metadata/location).
pub fn pool_catalog(editor: &Editor) -> Result<Catalog> {
    let path = editor.extensions_dir.join("extensions.json");
    let mut catalog = Catalog::new();
    for entry in read_entries(&path)? {
        if let Some(id) = entry_id(&entry) {
            catalog.entry(id).or_insert(entry);
        }
    }
    Ok(catalog)
}

/// Ensure `id` is a member of `profile`. Tries, in order: the shared pool (if
/// already installed), a vendored copy in the repo, then the editor CLI.
pub fn add_member(
    editor: &Editor,
    profile: &Profile,
    id: &str,
    catalog: &Catalog,
    vendor_dir: &Path,
    backup_dir: &Path,
) -> Result<AddMethod> {
    if add_from_catalog(editor, profile, id, catalog, backup_dir)? {
        return Ok(AddMethod::Pool);
    }
    if add_from_vendor(editor, profile, id, vendor_dir, backup_dir)? {
        return Ok(AddMethod::Vendor);
    }
    run_cli(
        editor,
        profile.cli_profile(),
        &["--install-extension", id, "--force"],
    )
    .with_context(|| format!("installing extension {id}"))?;
    Ok(AddMethod::Cli)
}

/// Copy local (VSIX-source) extensions referenced by `ids` from the pool into
/// `vendor_dir` so the config is portable to machines without them installed.
/// Returns the number of extensions vendored.
pub fn vendor_local(
    editor: &Editor,
    catalog: &Catalog,
    ids: &BTreeSet<String>,
    vendor_dir: &Path,
    dry_run: bool,
) -> Result<usize> {
    let mut count = 0_usize;
    for id in ids {
        let Some(entry) = catalog.get(id) else {
            continue;
        };
        if entry_source(entry).as_deref() != Some("vsix") {
            continue;
        }
        let Some(rel) = relative_location(entry) else {
            continue;
        };
        let source = editor.extensions_dir.join(&rel);
        if !source.is_dir() {
            continue;
        }
        count = count.saturating_add(1);
        if dry_run {
            continue;
        }
        let dest = vendor_dir.join(&rel);
        if !dest.is_dir() {
            copy_dir(&source, &dest)?;
        }
        let sidecar = vendor_dir.join(format!("{rel}.entry.json"));
        let text = serde_json::to_string_pretty(entry).context("serializing vendored entry")?;
        safety::atomic_write(&sidecar, &text)?;
    }
    Ok(count)
}

/// Remove `id` from a profile's membership list (never deletes shared files).
/// Returns whether an entry was removed.
pub fn remove_member(
    editor: &Editor,
    profile: &Profile,
    id: &str,
    backup_dir: &Path,
) -> Result<bool> {
    let path = profile.extensions_path(editor);
    let mut entries = read_entries(&path)?;
    let before = entries.len();
    entries.retain(|e| entry_id(e).as_deref() != Some(id));
    if entries.len() == before {
        return Ok(false);
    }
    write_entries(&path, &entries, backup_dir)?;
    Ok(true)
}

/// Add the pool's catalog entry for `id` to a profile's membership list. Returns
/// `false` when the extension is not in the pool (caller falls back to the CLI).
fn add_from_catalog(
    editor: &Editor,
    profile: &Profile,
    id: &str,
    catalog: &Catalog,
    backup_dir: &Path,
) -> Result<bool> {
    let Some(entry) = catalog.get(id) else {
        return Ok(false);
    };
    let path = profile.extensions_path(editor);
    let mut entries = read_entries(&path)?;
    if entries.iter().any(|e| entry_id(e).as_deref() == Some(id)) {
        return Ok(true);
    }
    entries.push(entry.clone());
    write_entries(&path, &entries, backup_dir)?;
    Ok(true)
}

/// Restore a vendored extension: copy its folder into the pool (if missing),
/// fix its on-disk location, and add it to the profile's membership list.
/// Returns `false` when no vendored copy of `id` exists.
fn add_from_vendor(
    editor: &Editor,
    profile: &Profile,
    id: &str,
    vendor_dir: &Path,
    backup_dir: &Path,
) -> Result<bool> {
    let Some((mut entry, rel)) = find_vendored(vendor_dir, id)? else {
        return Ok(false);
    };
    let vendored = vendor_dir.join(&rel);
    if !vendored.is_dir() {
        return Ok(false);
    }
    let pool_folder = editor.extensions_dir.join(&rel);
    if !pool_folder.is_dir() {
        copy_dir(&vendored, &pool_folder)?;
    }
    // Point the entry at this machine's pool location.
    if let Value::Object(map) = &mut entry {
        map.insert(
            "location".to_owned(),
            serde_json::json!({
                "$mid": 1,
                "path": pool_folder.to_string_lossy(),
                "scheme": "file",
            }),
        );
    }

    let path = profile.extensions_path(editor);
    let mut entries = read_entries(&path)?;
    if entries.iter().any(|e| entry_id(e).as_deref() == Some(id)) {
        return Ok(true);
    }
    entries.push(entry);
    write_entries(&path, &entries, backup_dir)?;
    Ok(true)
}

/// Find a vendored extension by id, returning its catalog entry and relative
/// location.
fn find_vendored(vendor_dir: &Path, id: &str) -> Result<Option<(Value, String)>> {
    if !vendor_dir.is_dir() {
        return Ok(None);
    }
    for dir_entry in
        fs::read_dir(vendor_dir).with_context(|| format!("reading {}", vendor_dir.display()))?
    {
        let path = dir_entry?.path();
        let is_sidecar = path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.ends_with(".entry.json"));
        if !is_sidecar {
            continue;
        }
        let raw =
            fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        let entry: Value =
            serde_json::from_str(&raw).with_context(|| format!("parsing {}", path.display()))?;
        if entry_id(&entry).as_deref() == Some(id)
            && let Some(rel) = relative_location(&entry)
        {
            return Ok(Some((entry, rel)));
        }
    }
    Ok(None)
}

/// Recursively copy a directory tree.
fn copy_dir(source: &Path, dest: &Path) -> Result<()> {
    fs::create_dir_all(dest).with_context(|| format!("creating {}", dest.display()))?;
    for child in fs::read_dir(source).with_context(|| format!("reading {}", source.display()))? {
        let child = child?;
        let from = child.path();
        let to = dest.join(child.file_name());
        if from.is_dir() {
            copy_dir(&from, &to)?;
        } else {
            fs::copy(&from, &to).with_context(|| format!("copying {}", from.display()))?;
        }
    }
    Ok(())
}

fn entry_source(entry: &Value) -> Option<String> {
    entry
        .get("metadata")?
        .get("source")?
        .as_str()
        .map(str::to_owned)
}

fn relative_location(entry: &Value) -> Option<String> {
    entry.get("relativeLocation")?.as_str().map(str::to_owned)
}

/// Read the raw entry list from an `extensions.json` file (empty if missing).
fn read_entries(path: &Path) -> Result<Vec<Value>> {
    if !path.is_file() {
        return Ok(Vec::new());
    }
    let raw = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_str(&raw).with_context(|| format!("parsing {}", path.display()))
}

/// The normalized id of an `extensions.json` entry.
fn entry_id(entry: &Value) -> Option<String> {
    let raw: RawEntry = serde_json::from_value(entry.clone()).ok()?;
    Some(normalize_id(&raw.identifier.id))
}

fn write_entries(path: &Path, entries: &[Value], backup_dir: &Path) -> Result<()> {
    safety::backup_file(path, backup_dir)?;
    let mut text = serde_json::to_string_pretty(entries).context("serializing extensions.json")?;
    text.push('\n');
    safety::atomic_write(path, &text)
}

fn run_cli(editor: &Editor, profile_name: Option<&str>, args: &[&str]) -> Result<()> {
    let mut command = Command::new(&editor.launcher);
    if let Some(name) = profile_name {
        command.arg("--profile").arg(name);
    }
    command.args(args);
    let output = command
        .output()
        .with_context(|| format!("running {}", editor.launcher.display()))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        bail!("editor CLI failed: {}", best_error_line(&stderr, &stdout));
    }
    Ok(())
}

/// Pick the most informative line of CLI output, skipping Electron/Chromium and
/// Node noise and progress chatter, preferring a line that names the failure.
fn best_error_line(stderr: &str, stdout: &str) -> String {
    let is_noise = |line: &&str| {
        line.is_empty()
            || line.starts_with("Warning:")
            || line.contains("DeprecationWarning")
            || line.contains("trace-deprecation")
            || line.starts_with("Installing extensions")
    };
    let lines: Vec<&str> = stderr
        .lines()
        .chain(stdout.lines())
        .map(str::trim)
        .filter(|l| !is_noise(l))
        .collect();
    let chosen = lines
        .iter()
        .rev()
        .find(|line| {
            line.contains("not found")
                || line.starts_with("Failed")
                || line.to_ascii_lowercase().contains("error")
        })
        .or_else(|| lines.last());
    chosen.map_or_else(|| "unknown error".to_owned(), |line| (*line).to_owned())
}

#[cfg(test)]
mod tests {
    use super::best_error_line;

    #[test]
    fn best_error_line_prefers_the_failure_over_noise_and_progress() {
        let stderr = "Warning: 'enable-features' is not in the list of known options\n\
                      (node:36363) [DEP0169] DeprecationWarning: url.parse() ...\n\
                      Extension 'x.y' not found.\n\
                      Make sure you use the full extension ID, including the publisher\n\
                      Failed Installing Extensions: x.y";
        let stdout = "Installing extensions...";
        assert_eq!(
            best_error_line(stderr, stdout),
            "Failed Installing Extensions: x.y"
        );
    }

    #[test]
    fn best_error_line_falls_back_to_last_line() {
        assert_eq!(
            best_error_line("", "something odd happened"),
            "something odd happened"
        );
        assert_eq!(best_error_line("", ""), "unknown error");
    }
}
