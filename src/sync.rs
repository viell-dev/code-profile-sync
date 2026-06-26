//! The sync engine: read editor state, compare against the config and snapshot,
//! and apply changes in either or both directions.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde_json::Value;

use crate::cli::Prefer;
use crate::config::{Config, ProfileConfig, Resolved};
use crate::editor::Editor;
use crate::editor::profiles::{self, Profile};
use crate::snapshot::{ProfileSnapshot, Snapshot};
use crate::{extension, jsonc, safety, ui};

/// Shared options for an engine operation.
pub struct Ctx<'a> {
    pub editor: &'a Editor,
    pub dry_run: bool,
    pub non_interactive: bool,
    pub prefer: Option<Prefer>,
    pub profile_filter: Option<String>,
    pub backup_dir: PathBuf,
}

impl Ctx<'_> {
    fn wants(&self, name: &str) -> bool {
        self.profile_filter.as_deref().is_none_or(|f| f.eq_ignore_ascii_case(name))
    }
}

/// The editor's actual tracked state for a profile.
struct Actual {
    settings: BTreeMap<String, Value>,
    extensions: BTreeSet<String>,
}

fn read_actual(editor: &Editor, profile: &Profile) -> Result<Actual> {
    let settings = if profile.inherits("settings") {
        BTreeMap::new()
    } else {
        let raw: BTreeMap<String, Value> =
            jsonc::read_object(&profile.settings_path(editor))?.into_iter().collect();
        crate::config::sanitize_settings(&raw)
    };
    let extensions = if profile.inherits("extensions") {
        BTreeSet::new()
    } else {
        extension::read_membership(editor, profile)?
    };
    Ok(Actual { settings, extensions })
}

/// Map of the editor's current profiles by name.
fn editor_profiles(editor: &Editor) -> Result<BTreeMap<String, Profile>> {
    Ok(profiles::read_all(editor)?.into_iter().map(|p| (p.name.clone(), p)).collect())
}

// ---------------------------------------------------------------------------
// status
// ---------------------------------------------------------------------------

/// Print drift between the config's desired state and the editor, no writes.
pub fn status(ctx: &Ctx<'_>, config: &Config) -> Result<()> {
    let resolved = config.resolve();
    let editors = editor_profiles(ctx.editor)?;
    let names = union_names(&resolved, &editors);

    if names.is_empty() {
        ui::info("No profiles in the config or the editor.");
        return Ok(());
    }

    for name in names {
        if !ctx.wants(&name) {
            continue;
        }
        ui::heading(format!("Profile: {name}"));
        let in_config = resolved.get(&name);
        let editor_profile = editors.get(&name);
        match (in_config, editor_profile) {
            (Some(_), None) => ui::detail("only in config (would be created on push)"),
            (None, Some(_)) => ui::detail("only in editor (would be captured on pull)"),
            (Some(want), Some(profile)) => {
                let actual = read_actual(ctx.editor, profile)?;
                report_drift(want, &actual);
            }
            (None, None) => {}
        }
    }
    Ok(())
}

fn report_drift(want: &Resolved, actual: &Actual) {
    let mut clean = true;
    for (key, value) in &want.settings {
        if actual.settings.get(key) != Some(value) {
            ui::bullet(format!("setting differs: {key}"));
            clean = false;
        }
    }
    for id in want.extensions.difference(&actual.extensions) {
        ui::bullet(format!("extension missing in editor: {id}"));
        clean = false;
    }
    for id in actual.extensions.difference(&want.extensions) {
        ui::bullet(format!("extension only in editor: {id}"));
        clean = false;
    }
    if clean {
        ui::detail("in sync");
    }
}

// ---------------------------------------------------------------------------
// pull (editor -> config)
// ---------------------------------------------------------------------------

/// Capture the editor's profiles into the config (profile-level), updating the
/// snapshot. New profiles are added; existing ones are overwritten by the
/// editor's state expressed as a delta over the profile's groups.
pub fn pull(ctx: &Ctx<'_>, config: &mut Config, snapshot: &mut Snapshot) -> Result<()> {
    let editors = editor_profiles(ctx.editor)?;
    for (name, profile) in &editors {
        if !ctx.wants(name) {
            continue;
        }
        let actual = read_actual(ctx.editor, profile)?;
        capture_profile(config, snapshot, name, profile, &actual.settings, &actual.extensions);
        ui::bullet(format!(
            "captured {name} ({} settings, {} extensions)",
            actual.settings.len(),
            actual.extensions.len()
        ));
    }
    Ok(())
}

/// Write a profile's reconciled state into the config as a delta over its base
/// (global + groups) layers, and record it in the snapshot.
fn capture_profile(
    config: &mut Config,
    snapshot: &mut Snapshot,
    name: &str,
    editor_profile: &Profile,
    settings: &BTreeMap<String, Value>,
    extensions: &BTreeSet<String>,
) {
    let base = baseline(config, name);
    let entry = config.profiles.entry(name.to_owned()).or_default();

    entry.settings = settings
        .iter()
        .filter(|(k, v)| base.settings.get(*k) != Some(*v))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    entry.extensions = extensions.difference(&base.extensions).cloned().collect();
    entry.exclude_extensions = base.extensions.difference(extensions).cloned().collect();
    if editor_profile.icon.is_some() {
        entry.icon.clone_from(&editor_profile.icon);
    }
    entry.use_default.clone_from(&editor_profile.use_default);

    snapshot.profiles.insert(
        name.to_owned(),
        ProfileSnapshot { settings: settings.clone(), extensions: extensions.clone() },
    );
}

/// The desired state of a profile from base layers only (global + its groups),
/// excluding profile-level settings/extensions.
fn baseline(config: &Config, name: &str) -> Resolved {
    let groups = config.profiles.get(name).map(|p| p.groups.clone()).unwrap_or_default();
    let probe = ProfileConfig { groups, ..ProfileConfig::default() };
    let mut probe_config = config.clone();
    probe_config.profiles.clear();
    probe_config.profiles.insert(name.to_owned(), probe);
    probe_config.resolve().remove(name).unwrap_or_default()
}

// ---------------------------------------------------------------------------
// push (config -> editor)
// ---------------------------------------------------------------------------

/// Apply the config's desired state to the editor (non-destructive: sets
/// resolved settings and installs missing extensions; does not delete
/// editor-only settings or uninstall extras).
pub fn push(ctx: &Ctx<'_>, config: &Config, snapshot: &mut Snapshot) -> Result<()> {
    let resolved = config.resolve();
    let mut editors = editor_profiles(ctx.editor)?;

    for (name, want) in &resolved {
        if !ctx.wants(name) {
            continue;
        }
        let profile = if let Some(p) = editors.get(name) { p.clone() } else {
            let created = create_profile(ctx, name, want)?;
            editors.insert(name.clone(), created.clone());
            created
        };

        // Settings: write resolved keys (config wins per key), keep others.
        if effective_inherits(want, &profile, "settings") {
            ui::bullet(format!("{name}: inherits settings from Default (skipped)"));
        } else {
            let sets: BTreeMap<String, Value> = want.settings.clone();
            apply_settings(ctx, &profile, &sets, &[])?;
        }

        // Extensions: install any missing desired ones.
        if effective_inherits(want, &profile, "extensions") {
            ui::bullet(format!("{name}: inherits extensions from Default (skipped)"));
        } else {
            let current = extension::read_membership(ctx.editor, &profile)?;
            for id in want.extensions.difference(&current) {
                install_ext(ctx, &profile, id)?;
            }
            for id in current.difference(&want.extensions) {
                ui::detail(format!("{name}: editor-only extension left installed: {id}"));
            }
        }

        let final_settings = read_actual(ctx.editor, &profile)?;
        snapshot.profiles.insert(
            name.clone(),
            ProfileSnapshot {
                settings: final_settings.settings,
                extensions: final_settings.extensions,
            },
        );
    }
    Ok(())
}

fn effective_inherits(want: &Resolved, profile: &Profile, resource: &str) -> bool {
    want.use_default.get(resource).copied().unwrap_or_else(|| profile.inherits(resource))
}

// ---------------------------------------------------------------------------
// sync (3-way)
// ---------------------------------------------------------------------------

/// Reconcile config and editor using the snapshot as the common ancestor.
pub fn sync(ctx: &Ctx<'_>, config: &mut Config, snapshot: &mut Snapshot) -> Result<()> {
    let resolved = config.resolve();
    let editors = editor_profiles(ctx.editor)?;
    let names = union_names(&resolved, &editors);

    for name in names {
        if !ctx.wants(&name) {
            continue;
        }
        let want = resolved.get(&name).cloned().unwrap_or_default();
        let empty_base = ProfileSnapshot::default();
        let base = snapshot.profile(&name).unwrap_or(&empty_base).clone();

        // Ensure the profile exists in the editor (create if config-only).
        let profile = match editors.get(&name) {
            Some(p) => p.clone(),
            None => create_profile(ctx, &name, &want)?,
        };
        let actual = read_actual(ctx.editor, &profile)?;

        ui::heading(format!("Sync: {name}"));
        let settings = reconcile_settings(ctx, &name, &base, &want.settings, &actual.settings)?;
        let exts = reconcile_extensions(ctx, &name, &base, &want.extensions, &actual.extensions)?;

        // Apply to editor.
        let removes: Vec<String> =
            actual.settings.keys().filter(|k| !settings.contains_key(*k)).cloned().collect();
        if !effective_inherits(&want, &profile, "settings") {
            apply_settings(ctx, &profile, &settings, &removes)?;
        }
        if !effective_inherits(&want, &profile, "extensions") {
            for id in exts.difference(&actual.extensions) {
                install_ext(ctx, &profile, id)?;
            }
            for id in actual.extensions.difference(&exts) {
                uninstall_ext(ctx, &profile, id)?;
            }
        }

        // Apply to config + snapshot.
        capture_profile(config, snapshot, &name, &profile, &settings, &exts);
    }
    Ok(())
}

/// Reconcile a settings map, returning the agreed-upon result.
fn reconcile_settings(
    ctx: &Ctx<'_>,
    profile: &str,
    base: &ProfileSnapshot,
    repo: &BTreeMap<String, Value>,
    editor: &BTreeMap<String, Value>,
) -> Result<BTreeMap<String, Value>> {
    let mut keys: BTreeSet<&String> = BTreeSet::new();
    keys.extend(base.settings.keys());
    keys.extend(repo.keys());
    keys.extend(editor.keys());

    let mut result = BTreeMap::new();
    for key in keys {
        let decision = classify(base.settings.get(key), repo.get(key), editor.get(key));
        let chosen = resolve_decision(
            ctx,
            decision,
            &format!("setting '{key}' in {profile}"),
            repo.get(key),
            editor.get(key),
        )?;
        if let Some(value) = chosen {
            result.insert(key.clone(), value.clone());
        }
    }
    Ok(result)
}

/// Reconcile an extension set. Presence can change on at most one side relative
/// to the base, so these never truly conflict when a base exists.
fn reconcile_extensions(
    ctx: &Ctx<'_>,
    profile: &str,
    base: &ProfileSnapshot,
    repo: &BTreeSet<String>,
    editor: &BTreeSet<String>,
) -> Result<BTreeSet<String>> {
    let mut ids: BTreeSet<&String> = BTreeSet::new();
    ids.extend(base.extensions.iter());
    ids.extend(repo.iter());
    ids.extend(editor.iter());

    let present = Value::Bool(true);
    let mut result = BTreeSet::new();
    for id in ids {
        let b = base.extensions.contains(id).then(|| present.clone());
        let r = repo.contains(id).then(|| present.clone());
        let e = editor.contains(id).then(|| present.clone());
        let decision = classify(b.as_ref(), r.as_ref(), e.as_ref());
        let chosen = resolve_decision(
            ctx,
            decision,
            &format!("extension '{id}' in {profile}"),
            r.as_ref(),
            e.as_ref(),
        )?;
        if chosen.is_some() {
            result.insert(id.clone());
        }
    }
    Ok(result)
}

/// A per-item 3-way classification.
#[derive(Clone, Copy)]
enum Decision {
    Agree,
    TakeRepo,
    TakeEditor,
    Conflict,
}

fn classify(base: Option<&Value>, repo: Option<&Value>, editor: Option<&Value>) -> Decision {
    if repo == editor {
        return Decision::Agree;
    }
    match base {
        Some(_) => {
            if repo == base {
                Decision::TakeEditor
            } else if editor == base {
                Decision::TakeRepo
            } else {
                Decision::Conflict
            }
        }
        None => match (repo, editor) {
            (None, Some(_)) => Decision::TakeEditor,
            (Some(_), None) => Decision::TakeRepo,
            _ => Decision::Conflict,
        },
    }
}

/// Turn a decision into the chosen value (`None` = item absent), prompting on
/// conflict.
fn resolve_decision<'v>(
    ctx: &Ctx<'_>,
    decision: Decision,
    label: &str,
    repo: Option<&'v Value>,
    editor: Option<&'v Value>,
) -> Result<Option<&'v Value>> {
    let take_editor = match decision {
        Decision::Agree | Decision::TakeRepo => false,
        Decision::TakeEditor => true,
        Decision::Conflict => resolve_conflict(ctx, label)?,
    };
    Ok(if take_editor { editor } else { repo })
}

fn resolve_conflict(ctx: &Ctx<'_>, label: &str) -> Result<bool> {
    if let Some(prefer) = ctx.prefer {
        return Ok(prefer == Prefer::Editor);
    }
    if ctx.non_interactive {
        anyhow::bail!("conflict on {label}; rerun with --prefer editor|repo");
    }
    let choice = ui::select(
        &format!("Conflict on {label}"),
        &["keep editor".to_owned(), "keep config".to_owned()],
    )
    .context("reading conflict choice")?;
    Ok(choice == 0)
}

// ---------------------------------------------------------------------------
// editor mutation helpers
// ---------------------------------------------------------------------------

fn apply_settings(
    ctx: &Ctx<'_>,
    profile: &Profile,
    sets: &BTreeMap<String, Value>,
    removes: &[String],
) -> Result<()> {
    let path = profile.settings_path(ctx.editor);
    let mut object = jsonc::read_object(&path)?;
    let mut changed = 0_usize;
    for (key, value) in sets {
        if object.get(key) != Some(value) {
            object.insert(key.clone(), value.clone());
            changed = changed.saturating_add(1);
        }
    }
    for key in removes {
        if object.remove(key).is_some() {
            changed = changed.saturating_add(1);
        }
    }
    if changed == 0 {
        return Ok(());
    }
    if ctx.dry_run {
        ui::bullet(format!("would update {changed} setting(s) in {}", profile.name));
        return Ok(());
    }
    safety::backup_file(&path, &ctx.backup_dir)?;
    let text = jsonc::to_pretty(&Value::Object(object))?;
    safety::atomic_write(&path, &text)?;
    ui::bullet(format!("updated {changed} setting(s) in {}", profile.name));
    Ok(())
}

fn install_ext(ctx: &Ctx<'_>, profile: &Profile, id: &str) -> Result<()> {
    if ctx.dry_run {
        ui::bullet(format!("would install {id} into {}", profile.name));
        return Ok(());
    }
    ui::bullet(format!("installing {id} into {}", profile.name));
    extension::install(ctx.editor, profile.cli_profile(), id)
}

fn uninstall_ext(ctx: &Ctx<'_>, profile: &Profile, id: &str) -> Result<()> {
    if ctx.dry_run {
        ui::bullet(format!("would uninstall {id} from {}", profile.name));
        return Ok(());
    }
    ui::bullet(format!("uninstalling {id} from {}", profile.name));
    extension::uninstall(ctx.editor, profile.cli_profile(), id)
}

/// Create a named profile in the editor's registry and return its model.
fn create_profile(ctx: &Ctx<'_>, name: &str, want: &Resolved) -> Result<Profile> {
    if name == profiles::DEFAULT_PROFILE {
        // The Default profile always exists.
        return Ok(Profile {
            name: name.to_owned(),
            location: None,
            icon: None,
            use_default: BTreeMap::new(),
        });
    }
    if ctx.dry_run {
        ui::bullet(format!("would create profile {name}"));
    } else {
        ui::bullet(format!("creating profile {name}"));
    }
    profiles::create(
        ctx.editor,
        name,
        want.icon.as_deref(),
        &want.use_default,
        ctx.dry_run,
        &ctx.backup_dir,
    )
}

fn union_names(
    resolved: &BTreeMap<String, Resolved>,
    editors: &BTreeMap<String, Profile>,
) -> Vec<String> {
    let mut names: BTreeSet<String> = BTreeSet::new();
    names.extend(resolved.keys().cloned());
    names.extend(editors.keys().cloned());
    names.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{Decision, classify};

    #[test]
    fn classify_three_way_truth_table() {
        let a = json!(1);
        let b = json!(2);
        let c = json!(3);

        assert!(matches!(classify(None, Some(&a), Some(&a)), Decision::Agree));
        assert!(matches!(classify(Some(&a), Some(&b), Some(&a)), Decision::TakeRepo));
        assert!(matches!(classify(Some(&a), Some(&a), Some(&b)), Decision::TakeEditor));
        assert!(matches!(classify(Some(&a), Some(&b), Some(&c)), Decision::Conflict));
    }

    #[test]
    fn classify_without_base_adopts_the_present_side() {
        let a = json!(1);
        let b = json!(2);
        // Only one side has the item and there is no base: take that side.
        assert!(matches!(classify(None, Some(&a), None), Decision::TakeRepo));
        assert!(matches!(classify(None, None, Some(&a)), Decision::TakeEditor));
        // Both present but differing, no base: a genuine conflict.
        assert!(matches!(classify(None, Some(&a), Some(&b)), Decision::Conflict));
    }
}
