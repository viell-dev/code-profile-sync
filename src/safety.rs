//! Write safety: detecting a running editor, atomic file writes, and backups.

use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use std::{fs, process};

use anyhow::{Context, Result};
use sysinfo::System;

use crate::editor::Editor;

/// Whether a process for `editor` appears to be running.
pub fn editor_running(editor: &Editor) -> bool {
    let system = System::new_all();
    let needle = editor.product.application_name.to_ascii_lowercase();
    let self_pid = sysinfo::get_current_pid().ok();
    system.processes().iter().any(|(pid, proc)| {
        if Some(*pid) == self_pid {
            return false;
        }
        let name = proc.name().to_string_lossy().to_ascii_lowercase();
        process_matches(&name, &needle)
    })
}

/// Match a process name to an application name: exact, or the app name followed
/// by a non-alphanumeric separator (e.g. `codium`, `code-oss`).
fn process_matches(name: &str, needle: &str) -> bool {
    if name == needle {
        return true;
    }
    match name.strip_prefix(needle) {
        Some(rest) => rest
            .chars()
            .next()
            .is_some_and(|c| !c.is_ascii_alphanumeric()),
        None => false,
    }
}

/// Atomically write `contents` to `path` (temp file in the same directory, then
/// rename), creating parent directories as needed.
pub fn atomic_write(path: &Path, contents: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating directory {}", parent.display()))?;
    }
    let temp = temp_sibling(path);
    fs::write(&temp, contents).with_context(|| format!("writing {}", temp.display()))?;
    fs::rename(&temp, path)
        .with_context(|| format!("replacing {} with {}", path.display(), temp.display()))?;
    Ok(())
}

/// Copy `path` into `backup_dir`, prefixing with a short hash of its full path so
/// same-named files from different profiles don't collide. No-op if absent.
pub fn backup_file(path: &Path, backup_dir: &Path) -> Result<()> {
    if !path.is_file() {
        return Ok(());
    }
    fs::create_dir_all(backup_dir)
        .with_context(|| format!("creating backup directory {}", backup_dir.display()))?;
    let name = path
        .file_name()
        .map_or_else(|| "file".into(), ToOwned::to_owned);
    let dest = backup_dir.join(format!("{}-{}", short_hash(path), name.to_string_lossy()));
    fs::copy(path, &dest)
        .with_context(|| format!("backing up {} to {}", path.display(), dest.display()))?;
    Ok(())
}

/// A timestamp string (seconds since the Unix epoch) for naming a backup run.
pub fn timestamp() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or_else(|_| "0".to_owned(), |d| d.as_secs().to_string())
}

fn short_hash(path: &Path) -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    path.hash(&mut hasher);
    format!("{:08x}", hasher.finish())
}

fn temp_sibling(path: &Path) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    let base = path
        .file_name()
        .map_or_else(|| "tmp".into(), ToOwned::to_owned);
    let suffix = format!(".{}.{nanos}.tmp", process::id());
    let mut name = base.to_string_lossy().into_owned();
    name.push_str(&suffix);
    path.with_file_name(name)
}
