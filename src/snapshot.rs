//! The last-synced state snapshot, used as the common ancestor for 3-way sync.
//!
//! The snapshot records the fully-resolved per-profile settings and extension set
//! as of the last successful sync, so we can tell config-side from editor-side
//! changes instead of guessing.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::safety;

/// Snapshot of all tracked profiles for one editor.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Snapshot {
    pub profiles: BTreeMap<String, ProfileSnapshot>,
}

/// Snapshot of a single profile's tracked state.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ProfileSnapshot {
    pub settings: BTreeMap<String, Value>,
    pub extensions: BTreeSet<String>,
}

impl Snapshot {
    /// Load a snapshot, returning an empty one if the file does not exist.
    pub fn load(path: &Path) -> Result<Self> {
        if !path.is_file() {
            return Ok(Self::default());
        }
        let raw =
            fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let snapshot =
            serde_json::from_str(&raw).with_context(|| format!("parsing {}", path.display()))?;
        Ok(snapshot)
    }

    /// Atomically write the snapshot to `path`.
    pub fn save(&self, path: &Path) -> Result<()> {
        let mut text = serde_json::to_string_pretty(self).context("serializing snapshot")?;
        text.push('\n');
        safety::atomic_write(path, &text)
    }

    pub fn profile(&self, name: &str) -> Option<&ProfileSnapshot> {
        self.profiles.get(name)
    }
}
