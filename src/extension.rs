//! Reading per-profile extension membership and changing it via the editor CLI.
//!
//! We never query a marketplace ourselves; installs/uninstalls shell out to the
//! editor's own CLI (which performs the fetch). Membership is read directly from
//! the relevant `extensions.json`.

use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::config::normalize_id;
use crate::editor::Editor;
use crate::editor::profiles::Profile;

/// Minimal view of an `extensions.json` entry.
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
    let path = profile.extensions_path(editor);
    read_membership_file(&path)
}

/// Read normalized extension IDs from an `extensions.json` file.
pub fn read_membership_file(path: &Path) -> Result<BTreeSet<String>> {
    if !path.is_file() {
        return Ok(BTreeSet::new());
    }
    let raw =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let entries: Vec<RawEntry> =
        serde_json::from_str(&raw).with_context(|| format!("parsing {}", path.display()))?;
    Ok(entries
        .iter()
        .map(|e| normalize_id(&e.identifier.id))
        .collect())
}

/// Install an extension into a profile via the editor CLI.
pub fn install(editor: &Editor, profile_name: Option<&str>, id: &str) -> Result<()> {
    run_cli(
        editor,
        profile_name,
        &["--install-extension", id, "--force"],
    )
    .with_context(|| format!("installing extension {id}"))
}

/// Uninstall an extension from a profile via the editor CLI.
pub fn uninstall(editor: &Editor, profile_name: Option<&str>, id: &str) -> Result<()> {
    run_cli(editor, profile_name, &["--uninstall-extension", id])
        .with_context(|| format!("uninstalling extension {id}"))
}

/// Invoke the editor CLI, optionally scoped to a named profile. The Default
/// profile is targeted by omitting `--profile`.
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
        bail!("editor CLI failed ({}): {}", output.status, stderr.trim());
    }
    Ok(())
}
