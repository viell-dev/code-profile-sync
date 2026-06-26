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

/// Ensure `id` is a member of `profile`. Adds from the shared pool when the
/// extension is already installed; otherwise fetches it via the editor CLI.
pub fn add_member(
    editor: &Editor,
    profile: &Profile,
    id: &str,
    catalog: &Catalog,
    backup_dir: &Path,
) -> Result<AddMethod> {
    if add_from_catalog(editor, profile, id, catalog, backup_dir)? {
        return Ok(AddMethod::Pool);
    }
    run_cli(
        editor,
        profile.cli_profile(),
        &["--install-extension", id, "--force"],
    )
    .with_context(|| format!("installing extension {id}"))?;
    Ok(AddMethod::Cli)
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
