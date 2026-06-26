//! The sync engine: read editor state, compare against the config and snapshot,
//! and apply changes in either or both directions.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde_json::Value;

use crate::cli::Prefer;
use crate::config::{Config, DefaultProfile, ProfileConfig, Resolved};
use crate::editor::Editor;
use crate::editor::profiles::{self, Profile};
use crate::extension::Catalog;
use crate::snapshot::{ProfileSnapshot, Snapshot};
use crate::{extension, jsonc, safety, ui};

/// Shared options for an engine operation.
pub struct Ctx<'a> {
    pub editor: &'a Editor,
    pub dry_run: bool,
    pub non_interactive: bool,
    /// Skip the destructive-change confirmation (from `--yes`).
    pub assume_yes: bool,
    pub prefer: Option<Prefer>,
    /// Profiles to limit the run to (empty = all). A non-empty filter scopes the
    /// run to overlay mode.
    pub profile_filter: Vec<String>,
    pub backup_dir: PathBuf,
    /// Directory where local (VSIX-source) extensions are vendored for portability.
    pub vendor_dir: PathBuf,
}

impl Ctx<'_> {
    fn wants(&self, name: &str) -> bool {
        self.profile_filter.is_empty()
            || self
                .profile_filter
                .iter()
                .any(|f| f.eq_ignore_ascii_case(name))
    }

    /// Whether the run is scoped by `--profile` (forces overlay mode regardless of
    /// the config's `managed` flag).
    fn scoped(&self) -> bool {
        !self.profile_filter.is_empty()
    }

    /// Mirror mode deletes undefined profiles; overlay mode never does. A run is
    /// mirror only when neither `--profile` nor `[options] managed` is in effect.
    fn mirror(&self, config: &Config) -> bool {
        !self.scoped() && !config.options.managed
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
        let raw: BTreeMap<String, Value> = jsonc::read_object(&profile.settings_path(editor))?
            .into_iter()
            .collect();
        crate::config::sanitize_settings(&raw)
    };
    let extensions = if profile.inherits("extensions") {
        BTreeSet::new()
    } else {
        extension::read_membership(editor, profile)?
    };
    Ok(Actual {
        settings,
        extensions,
    })
}

/// Map of the editor's current profiles by name.
fn editor_profiles(editor: &Editor) -> Result<BTreeMap<String, Profile>> {
    Ok(profiles::read_all(editor)?
        .into_iter()
        .map(|p| (p.name.clone(), p))
        .collect())
}

// ---------------------------------------------------------------------------
// status
// ---------------------------------------------------------------------------

/// Print drift between the config's desired state and the editor, no writes.
pub fn status(ctx: &Ctx<'_>, config: &Config) -> Result<()> {
    let resolved = config.resolve();
    let editors = editor_profiles(ctx.editor)?;
    let deletions = config.deletions();
    let mirror = ctx.mirror(config);
    let mut names = union_names(&resolved, &editors);
    names.extend(deletions.iter().cloned());
    names.sort();
    names.dedup();

    if names.is_empty() {
        ui::info("No profiles in the config or the editor.");
        return Ok(());
    }

    for name in names {
        if !ctx.wants(&name) {
            continue;
        }
        ui::heading(format!("Profile: {name}"));
        if deletions.contains(&name) {
            if editors.contains_key(&name) {
                ui::detail("marked for deletion (would be removed from the editor on push)");
            } else {
                ui::detail("marked for deletion (not present in the editor)");
            }
            continue;
        }
        let in_config = resolved.get(&name);
        let editor_profile = editors.get(&name);
        match (in_config, editor_profile) {
            (Some(_), None) => ui::detail(if mirror {
                "only in config (created on push; REMOVED from config on pull)"
            } else {
                "only in config (created on push; kept on pull \u{2014} overlay)"
            }),
            (None, Some(_)) => ui::detail(if mirror {
                "only in editor (DELETED on push; captured on pull)"
            } else {
                "only in editor (kept on push \u{2014} overlay; captured on pull)"
            }),
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

/// Where a profile exists and whether it is in sync, for the interactive
/// per-profile menu's overview list.
pub struct ProfileSummary {
    pub name: String,
    pub in_config: bool,
    pub in_editor: bool,
    pub tombstone: bool,
    /// `Some` only when the profile is present on both sides; `true` = in sync.
    pub in_sync: Option<bool>,
}

/// Summarize every profile across the config and the editor (ignores the profile
/// filter; the overview always lists everything).
pub fn profile_summaries(ctx: &Ctx<'_>, config: &Config) -> Result<Vec<ProfileSummary>> {
    let resolved = config.resolve();
    let editors = editor_profiles(ctx.editor)?;
    let deletions = config.deletions();

    let mut names: BTreeSet<String> = BTreeSet::new();
    names.extend(resolved.keys().cloned());
    names.extend(editors.keys().cloned());
    names.extend(deletions.iter().cloned());

    let mut out = Vec::new();
    for name in names {
        let tombstone = deletions.contains(&name);
        let want = resolved.get(&name);
        let profile = editors.get(&name);
        let in_sync = match (want, profile) {
            (Some(want), Some(profile)) if !tombstone => {
                let actual = read_actual(ctx.editor, profile)?;
                Some(
                    want.settings
                        .iter()
                        .all(|(k, v)| actual.settings.get(k) == Some(v))
                        && want.extensions == actual.extensions,
                )
            }
            _ => None,
        };
        out.push(ProfileSummary {
            name,
            in_config: tombstone || want.is_some(),
            in_editor: profile.is_some(),
            tombstone,
            in_sync,
        });
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// pull (editor -> config)
// ---------------------------------------------------------------------------

/// Make the config mirror the editor (editor is authoritative). Captures each
/// in-scope editor profile and, in mirror mode, removes named config profiles
/// that no longer exist in the editor. A captured profile that was tombstoned has
/// its `delete = true` cleared (the editor is truth). Config removals are
/// confirmed before any write.
pub fn pull(ctx: &Ctx<'_>, config: &mut Config, snapshot: &mut Snapshot) -> Result<()> {
    let editors = editor_profiles(ctx.editor)?;
    let catalog = extension::pool_catalog(ctx.editor)?;
    let mirror = ctx.mirror(config);

    // Mirror mode: named config profiles absent from the editor are removed
    // (but standing tombstones are preserved, and out-of-scope profiles kept).
    let to_remove: Vec<String> = if mirror {
        config
            .profiles
            .iter()
            .filter(|(name, p)| ctx.wants(name) && !p.delete && !editors.contains_key(*name))
            .map(|(name, _)| name.clone())
            .collect()
    } else {
        Vec::new()
    };
    let destructive: Vec<String> = to_remove
        .iter()
        .map(|n| format!("remove profile '{n}' from config"))
        .collect();
    if !confirm_destructive(ctx, &destructive)? {
        ui::info("aborted; nothing written");
        return Ok(());
    }

    let mut all_extensions: BTreeSet<String> = BTreeSet::new();
    for (name, profile) in &editors {
        if !ctx.wants(name) {
            continue;
        }
        // Overlay mode manages only profiles the config already defines (plus the
        // Default profile); it does not import undefined editor profiles.
        if !mirror && name != profiles::DEFAULT_PROFILE && !config.profiles.contains_key(name) {
            continue;
        }
        if config.profiles.get(name).is_some_and(|p| p.delete) {
            ui::warn(format!(
                "{name}: clearing delete = true (the profile exists in the editor)"
            ));
        }
        let actual = read_actual(ctx.editor, profile)?;
        all_extensions.extend(actual.extensions.iter().cloned());
        capture_profile(
            config,
            snapshot,
            name,
            profile,
            &actual.settings,
            &actual.extensions,
        );
        ui::bullet(format!(
            "captured {name} ({} settings, {} extensions)",
            actual.settings.len(),
            actual.extensions.len()
        ));
    }

    for name in &to_remove {
        config.profiles.remove(name);
        snapshot.profiles.remove(name);
        ui::bullet(format!(
            "{} profile {name} from config",
            if ctx.dry_run {
                "would remove"
            } else {
                "removed"
            }
        ));
    }

    vendor_step(ctx, &catalog, &all_extensions);
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
    let settings_delta: BTreeMap<String, Value> = settings
        .iter()
        .filter(|(k, v)| base.settings.get(*k) != Some(*v))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let ext_add: Vec<String> = extensions.difference(&base.extensions).cloned().collect();
    let ext_excl: Vec<String> = base.extensions.difference(extensions).cloned().collect();

    if name == profiles::DEFAULT_PROFILE {
        let d = &mut config.default;
        d.settings = settings_delta;
        d.extensions = ext_add;
        d.exclude_extensions = ext_excl;
    } else {
        let entry = config.profiles.entry(name.to_owned()).or_default();
        entry.delete = false; // capturing a real profile clears any tombstone
        entry.settings = settings_delta;
        entry.extensions = ext_add;
        entry.exclude_extensions = ext_excl;
        if editor_profile.icon.is_some() {
            entry.icon.clone_from(&editor_profile.icon);
        }
        entry.use_default.clone_from(&editor_profile.use_default);
    }

    snapshot.profiles.insert(
        name.to_owned(),
        ProfileSnapshot {
            settings: settings.clone(),
            extensions: extensions.clone(),
        },
    );
}

/// The desired state of a profile from base layers only (global + its groups),
/// excluding profile-level settings/extensions.
fn baseline(config: &Config, name: &str) -> Resolved {
    let mut probe = config.clone();
    probe.profiles.clear();
    probe.default = DefaultProfile::default();
    if name == profiles::DEFAULT_PROFILE {
        probe.default.groups.clone_from(&config.default.groups);
    } else {
        let groups = config
            .profiles
            .get(name)
            .map(|p| p.groups.clone())
            .unwrap_or_default();
        probe.profiles.insert(
            name.to_owned(),
            ProfileConfig {
                groups,
                ..ProfileConfig::default()
            },
        );
    }
    probe.resolve().remove(name).unwrap_or_default()
}

// ---------------------------------------------------------------------------
// push (config -> editor)
// ---------------------------------------------------------------------------

/// Make the editor mirror the config (config is authoritative). In mirror mode
/// editor profiles not in the config are deleted; in overlay mode (`--profile`
/// or `[options] managed`) only the config's own profiles are managed and
/// `delete = true` tombstones remove specific profiles. Settings/extensions of
/// managed profiles are mirrored (extras removed). Destructive changes are
/// summarized and confirmed before any write.
pub fn push(ctx: &Ctx<'_>, config: &Config, snapshot: &mut Snapshot) -> Result<()> {
    let resolved = config.resolve();
    let mut editors = editor_profiles(ctx.editor)?;
    let catalog = extension::pool_catalog(ctx.editor)?;
    let mirror = ctx.mirror(config);
    ensure_tombstones_allowed(ctx, config, mirror)?;

    let to_delete =
        profiles_to_delete_from_editor(ctx, &resolved, &config.deletions(), &editors, mirror);
    let destructive = push_destructive_summary(ctx, &resolved, &editors, &to_delete)?;
    if !confirm_destructive(ctx, &destructive)? {
        ui::info("aborted; nothing written");
        return Ok(());
    }

    // Deletions first, so re-created names (unlikely) start clean.
    for name in &to_delete {
        if profiles::delete(ctx.editor, name, ctx.dry_run, &ctx.backup_dir)? {
            ui::bullet(format!(
                "{} profile {name}",
                if ctx.dry_run {
                    "would delete"
                } else {
                    "deleted"
                }
            ));
        }
        editors.remove(name);
        snapshot.profiles.remove(name);
    }

    let mut failures = 0_usize;
    for (name, want) in &resolved {
        if !ctx.wants(name) {
            continue;
        }
        let profile = if let Some(p) = editors.get(name) {
            p.clone()
        } else {
            let created = create_profile(ctx, name, want)?;
            editors.insert(name.clone(), created.clone());
            created
        };

        // Settings: mirror resolved keys, removing editor-only ones.
        if effective_inherits(want, &profile, "settings") {
            ui::bullet(format!("{name}: inherits settings from Default (skipped)"));
        } else {
            let actual = read_actual(ctx.editor, &profile)?;
            let removes: Vec<String> = actual
                .settings
                .keys()
                .filter(|k| !want.settings.contains_key(*k))
                .cloned()
                .collect();
            apply_settings(ctx, &profile, &want.settings, &removes)?;
        }

        // Extensions: install missing and uninstall extras (mirror).
        if effective_inherits(want, &profile, "extensions") {
            ui::bullet(format!(
                "{name}: inherits extensions from Default (skipped)"
            ));
        } else {
            let current = extension::read_membership(ctx.editor, &profile)?;
            // Never prune the Default profile's list (it is the shared pool).
            failures = failures.saturating_add(apply_extensions(
                ctx,
                &profile,
                &want.extensions,
                &current,
                &catalog,
                !profile.is_default(),
            ));
        }

        let final_state = read_actual(ctx.editor, &profile)?;
        snapshot.profiles.insert(
            name.clone(),
            ProfileSnapshot {
                settings: final_state.settings,
                extensions: final_state.extensions,
            },
        );
    }
    report_failures(failures);
    Ok(())
}

/// Editor profiles that this push will delete: in-scope `delete = true`
/// tombstones present in the editor, plus (mirror mode only) editor profiles not
/// defined in the config. The Default profile is never deleted.
fn profiles_to_delete_from_editor(
    ctx: &Ctx<'_>,
    resolved: &BTreeMap<String, Resolved>,
    deletions: &BTreeSet<String>,
    editors: &BTreeMap<String, Profile>,
    mirror: bool,
) -> Vec<String> {
    let mut out: BTreeSet<String> = BTreeSet::new();
    for name in deletions {
        if ctx.wants(name) && editors.contains_key(name) {
            out.insert(name.clone());
        }
    }
    if mirror {
        for name in editors.keys() {
            if name != profiles::DEFAULT_PROFILE && ctx.wants(name) && !resolved.contains_key(name)
            {
                out.insert(name.clone());
            }
        }
    }
    out.into_iter().collect()
}

/// Human-readable destructive actions a push will perform (profile deletions,
/// settings-key removals, extension uninstalls), for the confirmation gate.
fn push_destructive_summary(
    ctx: &Ctx<'_>,
    resolved: &BTreeMap<String, Resolved>,
    editors: &BTreeMap<String, Profile>,
    to_delete: &[String],
) -> Result<Vec<String>> {
    let mut lines: Vec<String> = to_delete
        .iter()
        .map(|name| format!("delete profile '{name}'"))
        .collect();
    for (name, want) in resolved {
        if !ctx.wants(name) {
            continue;
        }
        let Some(profile) = editors.get(name) else {
            continue; // a create is not destructive
        };
        let actual = read_actual(ctx.editor, profile)?;
        if !effective_inherits(want, profile, "settings") {
            let removed = actual
                .settings
                .keys()
                .filter(|k| !want.settings.contains_key(*k))
                .count();
            if removed > 0 {
                lines.push(format!("{name}: remove {removed} setting(s)"));
            }
        }
        // The Default profile's extension list is the shared pool and is never
        // pruned, so its extras are not destructive.
        if !profile.is_default() && !effective_inherits(want, profile, "extensions") {
            let removed = actual.extensions.difference(&want.extensions).count();
            if removed > 0 {
                lines.push(format!("{name}: uninstall {removed} extension(s)"));
            }
        }
    }
    Ok(lines)
}

/// Reject `delete = true` tombstones in mirror mode (they are only meaningful in
/// overlay mode; in mirror mode removing the profile block already deletes it).
fn ensure_tombstones_allowed(ctx: &Ctx<'_>, config: &Config, mirror: bool) -> Result<()> {
    if !mirror {
        return Ok(());
    }
    let blocked: Vec<String> = config
        .deletions()
        .into_iter()
        .filter(|n| ctx.wants(n))
        .collect();
    if !blocked.is_empty() {
        anyhow::bail!(
            "delete = true on {blocked:?} requires overlay mode ([options] managed = true or --profile); in mirror mode remove the profile block instead"
        );
    }
    Ok(())
}

/// Print a destructive-change summary and gate on confirmation. Returns whether
/// to proceed. Dry runs proceed (callers only print "would ..."); `--yes` and
/// non-interactive-without-`--yes` are honored.
fn confirm_destructive(ctx: &Ctx<'_>, destructive: &[String]) -> Result<bool> {
    if destructive.is_empty() {
        return Ok(true);
    }
    ui::warn(format!("{} destructive change(s):", destructive.len()));
    for line in destructive {
        ui::bullet(line.clone());
    }
    if ctx.dry_run || ctx.assume_yes {
        return Ok(true);
    }
    if ctx.non_interactive {
        anyhow::bail!("destructive changes require confirmation; rerun with --yes");
    }
    ui::confirm("Proceed with these destructive changes?", false).context("reading confirmation")
}

/// Warn about extensions that could not be installed/removed, without failing
/// the whole run (the snapshot still reflects what actually happened).
fn report_failures(failures: usize) {
    if failures > 0 {
        ui::warn(format!(
            "{failures} extension operation(s) failed; see warnings above"
        ));
    }
}

fn effective_inherits(want: &Resolved, profile: &Profile, resource: &str) -> bool {
    want.use_default
        .get(resource)
        .copied()
        .unwrap_or_else(|| profile.inherits(resource))
}

// ---------------------------------------------------------------------------
// sync (3-way)
// ---------------------------------------------------------------------------

/// Reconcile config and editor using the snapshot as the common ancestor.
pub fn sync(ctx: &Ctx<'_>, config: &mut Config, snapshot: &mut Snapshot) -> Result<()> {
    let resolved = config.resolve();
    let editors = editor_profiles(ctx.editor)?;
    let deletions = config.deletions();
    ensure_tombstones_allowed(ctx, config, ctx.mirror(config))?;
    apply_sync_tombstones(ctx, &deletions, &editors, snapshot)?;

    let mut names = union_names(&resolved, &editors);
    names.retain(|n| !deletions.contains(n)); // tombstones handled above
    let catalog = extension::pool_catalog(ctx.editor)?;
    let mut failures = 0_usize;
    let mut all_extensions: BTreeSet<String> = BTreeSet::new();

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
        let removes: Vec<String> = actual
            .settings
            .keys()
            .filter(|k| !settings.contains_key(*k))
            .cloned()
            .collect();
        if !effective_inherits(&want, &profile, "settings") {
            apply_settings(ctx, &profile, &settings, &removes)?;
        }
        if !effective_inherits(&want, &profile, "extensions") {
            failures = failures.saturating_add(apply_extensions(
                ctx,
                &profile,
                &exts,
                &actual.extensions,
                &catalog,
                true,
            ));
        }

        // Apply to config + snapshot.
        all_extensions.extend(exts.iter().cloned());
        capture_profile(config, snapshot, &name, &profile, &settings, &exts);
    }
    vendor_step(ctx, &catalog, &all_extensions);
    report_failures(failures);
    Ok(())
}

/// Apply `delete = true` tombstones during sync: delete in-scope tombstoned
/// profiles that exist in the editor. If a profile changed in the editor since
/// the last snapshot, treat it as a conflict (keep editor vs honor the deletion).
fn apply_sync_tombstones(
    ctx: &Ctx<'_>,
    deletions: &BTreeSet<String>,
    editors: &BTreeMap<String, Profile>,
    snapshot: &mut Snapshot,
) -> Result<()> {
    let mut to_delete: Vec<String> = Vec::new();
    for name in deletions {
        if !ctx.wants(name) {
            continue;
        }
        let Some(profile) = editors.get(name) else {
            continue; // already absent
        };
        let actual = read_actual(ctx.editor, profile)?;
        let changed = snapshot.profile(name).map_or(
            !actual.settings.is_empty() || !actual.extensions.is_empty(),
            |base| base.settings != actual.settings || base.extensions != actual.extensions,
        );
        if changed
            && resolve_conflict(
                ctx,
                &format!("delete '{name}' vs keep its editor changes since last sync"),
            )?
        {
            ui::warn(format!(
                "{name}: kept (editor has changes); tombstone not applied"
            ));
            continue;
        }
        to_delete.push(name.clone());
    }

    let destructive: Vec<String> = to_delete
        .iter()
        .map(|n| format!("delete profile '{n}'"))
        .collect();
    if !confirm_destructive(ctx, &destructive)? {
        anyhow::bail!("aborted; nothing written");
    }
    for name in &to_delete {
        if profiles::delete(ctx.editor, name, ctx.dry_run, &ctx.backup_dir)? {
            ui::bullet(format!(
                "{} profile {name}",
                if ctx.dry_run {
                    "would delete"
                } else {
                    "deleted"
                }
            ));
        }
        snapshot.profiles.remove(name);
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
        ui::bullet(format!(
            "would update {changed} setting(s) in {}",
            profile.name
        ));
        return Ok(());
    }
    safety::backup_file(&path, &ctx.backup_dir)?;
    let text = jsonc::to_pretty(&Value::Object(object))?;
    safety::atomic_write(&path, &text)?;
    ui::bullet(format!("updated {changed} setting(s) in {}", profile.name));
    Ok(())
}

/// Install missing extensions and (when `prune`) remove extras. Failures are
/// reported and counted, never aborting the run.
fn apply_extensions(
    ctx: &Ctx<'_>,
    profile: &Profile,
    desired: &BTreeSet<String>,
    current: &BTreeSet<String>,
    catalog: &Catalog,
    prune: bool,
) -> usize {
    let mut failures = 0_usize;
    for id in desired.difference(current) {
        if let Err(err) = install_ext(ctx, profile, id, catalog) {
            ui::warn(format!(
                "could not install {id} into {}: {err:#}",
                profile.name
            ));
            failures = failures.saturating_add(1);
        }
    }
    for id in current.difference(desired) {
        if prune {
            if let Err(err) = uninstall_ext(ctx, profile, id) {
                ui::warn(format!(
                    "could not remove {id} from {}: {err:#}",
                    profile.name
                ));
                failures = failures.saturating_add(1);
            }
        } else {
            ui::detail(format!(
                "{}: editor-only extension left installed: {id}",
                profile.name
            ));
        }
    }
    failures
}

fn install_ext(ctx: &Ctx<'_>, profile: &Profile, id: &str, catalog: &Catalog) -> Result<()> {
    if ctx.dry_run {
        ui::bullet(format!("would install {id} into {}", profile.name));
        return Ok(());
    }
    let method = extension::add_member(
        ctx.editor,
        profile,
        id,
        catalog,
        &ctx.vendor_dir,
        &ctx.backup_dir,
    )?;
    let how = match method {
        extension::AddMethod::Pool => "from pool",
        extension::AddMethod::Vendor => "from vendored copy",
        extension::AddMethod::Cli => "via CLI",
    };
    ui::bullet(format!("installed {id} into {} ({how})", profile.name));
    Ok(())
}

/// Vendor local (VSIX-source) extensions referenced by `ids` into the repo.
fn vendor_step(ctx: &Ctx<'_>, catalog: &Catalog, ids: &BTreeSet<String>) {
    match extension::vendor_local(ctx.editor, catalog, ids, &ctx.vendor_dir, ctx.dry_run) {
        Ok(0) => {}
        Ok(n) => ui::bullet(format!("vendored {n} local extension(s)")),
        Err(err) => ui::warn(format!("vendoring local extensions failed: {err:#}")),
    }
}

fn uninstall_ext(ctx: &Ctx<'_>, profile: &Profile, id: &str) -> Result<()> {
    // The Default profile's list is the shared pool catalog; removing from it
    // would affect every editor sharing the pool, so leave it alone.
    if profile.is_default() {
        ui::warn(format!(
            "not removing {id} from Default (shared extension pool)"
        ));
        return Ok(());
    }
    if ctx.dry_run {
        ui::bullet(format!("would remove {id} from {}", profile.name));
        return Ok(());
    }
    if extension::remove_member(ctx.editor, profile, id, &ctx.backup_dir)? {
        ui::bullet(format!("removed {id} from {}", profile.name));
    }
    Ok(())
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
    use std::collections::{BTreeMap, BTreeSet};
    use std::fs;
    use std::path::{Path, PathBuf};

    use anyhow::Result;
    use serde_json::json;
    use tempfile::{TempDir, tempdir};

    use super::{Ctx, Decision, classify, pull, push, sync};
    use crate::config::{Config, ProfileConfig};
    use crate::editor::Editor;
    use crate::editor::product::Product;
    use crate::extension;
    use crate::snapshot::{ProfileSnapshot, Snapshot};

    struct Fixture {
        _temp: TempDir,
        editor: Editor,
        backup_dir: PathBuf,
        vendor_dir: PathBuf,
    }

    impl Fixture {
        fn new() -> Result<Self> {
            let temp = tempdir()?;
            let user_dir = temp.path().join("User");
            let extensions_dir = temp.path().join("extensions");
            fs::create_dir_all(user_dir.join("profiles").join("-rust"))?;
            fs::create_dir_all(user_dir.join("globalStorage"))?;
            fs::create_dir_all(&extensions_dir)?;
            write_json(
                &user_dir.join("globalStorage").join("storage.json"),
                &json!({
                    "userDataProfiles": [
                        {
                            "location": "-rust",
                            "name": "Rust",
                            "icon": "package",
                            "useDefaultFlags": { "keybindings": true }
                        }
                    ]
                }),
            )?;
            write_json(&extensions_dir.join("extensions.json"), &json!([]))?;

            let editor = Editor {
                product: Product {
                    name_short: "VSCodium".to_owned(),
                    name_long: "VSCodium".to_owned(),
                    application_name: "codium".to_owned(),
                    data_folder_name: ".vscode-oss".to_owned(),
                    quality: Some("stable".to_owned()),
                    commit: Some("abc123".to_owned()),
                },
                launcher: PathBuf::from("codium"),
                launcher_aliases: vec!["codium".to_owned()],
                user_dir,
                extensions_dir,
            };
            Ok(Self {
                backup_dir: temp.path().join("backups"),
                vendor_dir: temp.path().join("vendor").join("extensions"),
                _temp: temp,
                editor,
            })
        }

        fn ctx(&self) -> Ctx<'_> {
            Ctx {
                editor: &self.editor,
                dry_run: false,
                non_interactive: true,
                assume_yes: true,
                prefer: None,
                profile_filter: Vec::new(),
                backup_dir: self.backup_dir.clone(),
                vendor_dir: self.vendor_dir.clone(),
            }
        }

        fn rust_settings_path(&self) -> PathBuf {
            self.editor
                .user_dir
                .join("profiles")
                .join("-rust")
                .join("settings.json")
        }

        fn rust_extensions_path(&self) -> PathBuf {
            self.editor
                .user_dir
                .join("profiles")
                .join("-rust")
                .join("extensions.json")
        }

        fn write_rust_settings(&self, settings: &serde_json::Value) -> Result<()> {
            write_json(&self.rust_settings_path(), settings)
        }

        fn write_rust_extensions(&self, ids: &[&str]) -> Result<()> {
            write_json(&self.rust_extensions_path(), &entries(ids))
        }

        fn write_pool_extensions(&self, ids: &[&str]) -> Result<()> {
            write_json(
                &self.editor.extensions_dir.join("extensions.json"),
                &entries(ids),
            )
        }

        /// Replace the editor's profile registry with the given (name, location)
        /// pairs, creating each profile directory.
        fn set_storage_profiles(&self, profiles: &[(&str, &str)]) -> Result<()> {
            let arr: Vec<serde_json::Value> = profiles
                .iter()
                .map(|(name, loc)| json!({ "location": loc, "name": name }))
                .collect();
            write_json(
                &self
                    .editor
                    .user_dir
                    .join("globalStorage")
                    .join("storage.json"),
                &json!({ "userDataProfiles": arr }),
            )?;
            for (_, loc) in profiles {
                fs::create_dir_all(self.editor.user_dir.join("profiles").join(loc))?;
            }
            Ok(())
        }

        fn editor_profile_names(&self) -> Result<Vec<String>> {
            Ok(crate::editor::profiles::read_all(&self.editor)?
                .into_iter()
                .map(|p| p.name)
                .collect())
        }

        fn profile_dir_exists(&self, location: &str) -> bool {
            self.editor
                .user_dir
                .join("profiles")
                .join(location)
                .is_dir()
        }

        fn ctx_scoped(&self, names: &[&str]) -> Ctx<'_> {
            let mut ctx = self.ctx();
            ctx.profile_filter = names.iter().map(|n| (*n).to_owned()).collect();
            ctx
        }
    }

    fn tombstone() -> ProfileConfig {
        ProfileConfig {
            delete: true,
            ..ProfileConfig::default()
        }
    }

    fn write_json(path: &Path, value: &serde_json::Value) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut text = serde_json::to_string_pretty(value)?;
        text.push('\n');
        fs::write(path, text)?;
        Ok(())
    }

    fn entries(ids: &[&str]) -> serde_json::Value {
        serde_json::Value::Array(ids.iter().map(|id| entry(id)).collect())
    }

    fn entry(id: &str) -> serde_json::Value {
        let relative = format!("{}-1.0.0", id.to_ascii_lowercase());
        json!({
            "identifier": { "id": id },
            "version": "1.0.0",
            "location": {
                "$mid": 1,
                "path": format!("/fake/extensions/{relative}"),
                "scheme": "file"
            },
            "relativeLocation": relative,
            "metadata": {
                "source": "gallery",
                "targetPlatform": "universal"
            }
        })
    }

    fn rust_config(settings: &[(&str, serde_json::Value)], extensions: &[&str]) -> Config {
        let mut config = Config::default();
        config.profiles.insert(
            "Rust".to_owned(),
            ProfileConfig {
                settings: settings
                    .iter()
                    .map(|(key, value)| ((*key).to_owned(), value.clone()))
                    .collect(),
                extensions: extensions.iter().map(|id| (*id).to_owned()).collect(),
                ..ProfileConfig::default()
            },
        );
        config
    }

    #[test]
    fn push_writes_settings_and_adds_pool_extension_to_named_profile() -> Result<()> {
        let fixture = Fixture::new()?;
        fixture.write_pool_extensions(&["Pub.One"])?;
        let config = rust_config(&[("editor.tabSize", json!(2))], &["pub.one"]);
        let mut snapshot = Snapshot::default();

        push(&fixture.ctx(), &config, &mut snapshot)?;

        let settings = crate::jsonc::read_object(&fixture.rust_settings_path())?;
        assert_eq!(settings.get("editor.tabSize"), Some(&json!(2)));
        assert_eq!(
            extension::read_membership_file(&fixture.rust_extensions_path())?,
            BTreeSet::from(["pub.one".to_owned()])
        );
        let rust = snapshot.profile("Rust");
        assert_eq!(
            rust.and_then(|profile| profile.settings.get("editor.tabSize")),
            Some(&json!(2))
        );
        assert_eq!(
            rust.map(|profile| profile.extensions.clone()),
            Some(BTreeSet::from(["pub.one".to_owned()]))
        );
        Ok(())
    }

    #[test]
    fn pull_captures_editor_state_into_config_and_snapshot() -> Result<()> {
        let fixture = Fixture::new()?;
        fixture.write_pool_extensions(&["Pub.Editor"])?;
        fixture.write_rust_settings(&json!({
            "editor.formatOnSave": true,
            "ignored.null": null
        }))?;
        fixture.write_rust_extensions(&["Pub.Editor"])?;
        let mut config = Config::default();
        let mut snapshot = Snapshot::default();

        pull(&fixture.ctx(), &mut config, &mut snapshot)?;

        let rust = config.profiles.get("Rust");
        assert_eq!(
            rust.and_then(|profile| profile.settings.get("editor.formatOnSave")),
            Some(&json!(true))
        );
        assert_eq!(
            rust.map(|profile| profile.extensions.clone()),
            Some(vec!["pub.editor".to_owned()])
        );
        assert_eq!(
            snapshot
                .profile("Rust")
                .and_then(|profile| profile.settings.get("ignored.null")),
            None
        );
        assert_eq!(
            snapshot
                .profile("Rust")
                .map(|profile| profile.extensions.clone()),
            Some(BTreeSet::from(["pub.editor".to_owned()]))
        );
        Ok(())
    }

    #[test]
    fn sync_reconciles_repo_and_editor_changes_against_snapshot() -> Result<()> {
        let fixture = Fixture::new()?;
        fixture.write_pool_extensions(&["Pub.Base", "Pub.Repo"])?;
        fixture.write_rust_settings(&json!({
            "repo.changed": 1,
            "editor.changed": "editor",
            "repo.removed": true
        }))?;
        fixture.write_rust_extensions(&["Pub.Base"])?;

        let mut config = rust_config(
            &[
                ("repo.changed", json!(2)),
                ("editor.changed", json!("base")),
                ("repo.added", json!(true)),
            ],
            &["pub.repo"],
        );
        let mut snapshot = Snapshot {
            profiles: BTreeMap::from([(
                "Rust".to_owned(),
                ProfileSnapshot {
                    settings: BTreeMap::from([
                        ("repo.changed".to_owned(), json!(1)),
                        ("editor.changed".to_owned(), json!("base")),
                        ("repo.removed".to_owned(), json!(true)),
                    ]),
                    extensions: BTreeSet::from(["pub.base".to_owned()]),
                },
            )]),
        };

        sync(&fixture.ctx(), &mut config, &mut snapshot)?;

        let settings = crate::jsonc::read_object(&fixture.rust_settings_path())?;
        assert_eq!(settings.get("repo.changed"), Some(&json!(2)));
        assert_eq!(settings.get("editor.changed"), Some(&json!("editor")));
        assert_eq!(settings.get("repo.added"), Some(&json!(true)));
        assert_eq!(settings.get("repo.removed"), None);
        assert_eq!(
            extension::read_membership_file(&fixture.rust_extensions_path())?,
            BTreeSet::from(["pub.repo".to_owned()])
        );

        let rust = config.profiles.get("Rust");
        assert_eq!(
            rust.and_then(|profile| profile.settings.get("editor.changed")),
            Some(&json!("editor"))
        );
        assert_eq!(
            snapshot
                .profile("Rust")
                .and_then(|profile| profile.settings.get("repo.removed")),
            None
        );
        assert_eq!(
            snapshot
                .profile("Rust")
                .map(|profile| profile.extensions.clone()),
            Some(BTreeSet::from(["pub.repo".to_owned()]))
        );
        Ok(())
    }

    #[test]
    fn profile_summaries_report_location_and_sync_state() -> Result<()> {
        let fixture = Fixture::new()?;
        fixture.set_storage_profiles(&[("Rust", "-rust"), ("Extra", "-extra")])?;
        // Rust is in sync; config defines Rust and tombstones Legacy.
        let mut config = rust_config(&[], &[]);
        config.options.managed = true;
        config.profiles.insert("Legacy".to_owned(), tombstone());

        let summaries = super::profile_summaries(&fixture.ctx(), &config)?;
        let by_name = |n: &str| summaries.iter().find(|s| s.name == n);

        assert!(
            by_name("Rust").is_some_and(|s| s.in_config && s.in_editor && s.in_sync == Some(true)),
            "Rust is in config + editor and in sync"
        );
        assert!(
            by_name("Extra").is_some_and(|s| s.in_editor && !s.in_config && s.in_sync.is_none()),
            "Extra is editor-only"
        );
        assert!(
            by_name("Legacy").is_some_and(|s| s.tombstone && s.in_config && !s.in_editor),
            "Legacy is a config tombstone"
        );
        assert!(by_name("Default").is_some(), "Default always listed");
        Ok(())
    }

    #[test]
    fn classify_three_way_truth_table() {
        let a = json!(1);
        let b = json!(2);
        let c = json!(3);

        assert!(matches!(
            classify(None, Some(&a), Some(&a)),
            Decision::Agree
        ));
        assert!(matches!(
            classify(Some(&a), Some(&b), Some(&a)),
            Decision::TakeRepo
        ));
        assert!(matches!(
            classify(Some(&a), Some(&a), Some(&b)),
            Decision::TakeEditor
        ));
        assert!(matches!(
            classify(Some(&a), Some(&b), Some(&c)),
            Decision::Conflict
        ));
    }

    #[test]
    fn mirror_push_deletes_undefined_editor_profile() -> Result<()> {
        let fixture = Fixture::new()?;
        fixture.set_storage_profiles(&[("Rust", "-rust"), ("Extra", "-extra")])?;
        let config = rust_config(&[], &[]); // config defines only Rust
        let mut snapshot = Snapshot::default();

        push(&fixture.ctx(), &config, &mut snapshot)?;

        let names = fixture.editor_profile_names()?;
        assert!(names.contains(&"Rust".to_owned()));
        assert!(
            !names.contains(&"Extra".to_owned()),
            "undefined profile removed"
        );
        assert!(!fixture.profile_dir_exists("-extra"), "profile dir removed");
        assert!(snapshot.profile("Extra").is_none());
        Ok(())
    }

    #[test]
    fn managed_push_keeps_undefined_editor_profile() -> Result<()> {
        let fixture = Fixture::new()?;
        fixture.set_storage_profiles(&[("Rust", "-rust"), ("Extra", "-extra")])?;
        let mut config = rust_config(&[], &[]);
        config.options.managed = true; // overlay: leave undefined profiles alone
        let mut snapshot = Snapshot::default();

        push(&fixture.ctx(), &config, &mut snapshot)?;

        let names = fixture.editor_profile_names()?;
        assert!(
            names.contains(&"Extra".to_owned()),
            "undefined profile kept"
        );
        assert!(fixture.profile_dir_exists("-extra"));
        Ok(())
    }

    #[test]
    fn overlay_push_honors_delete_tombstone() -> Result<()> {
        let fixture = Fixture::new()?;
        fixture.set_storage_profiles(&[("Rust", "-rust"), ("Legacy", "-legacy")])?;
        let mut config = rust_config(&[], &[]);
        config.options.managed = true;
        config.profiles.insert("Legacy".to_owned(), tombstone());
        let mut snapshot = Snapshot::default();

        push(&fixture.ctx(), &config, &mut snapshot)?;

        let names = fixture.editor_profile_names()?;
        assert!(!names.contains(&"Legacy".to_owned()), "tombstone deleted");
        assert!(!fixture.profile_dir_exists("-legacy"));
        assert!(names.contains(&"Rust".to_owned()));
        Ok(())
    }

    #[test]
    fn mirror_push_with_tombstone_bails() -> Result<()> {
        let fixture = Fixture::new()?;
        fixture.set_storage_profiles(&[("Rust", "-rust")])?;
        let mut config = rust_config(&[], &[]);
        config.profiles.insert("Legacy".to_owned(), tombstone()); // mirror mode
        let mut snapshot = Snapshot::default();

        let result = push(&fixture.ctx(), &config, &mut snapshot);
        assert!(result.is_err(), "delete = true is invalid in mirror mode");
        Ok(())
    }

    #[test]
    fn scoped_push_leaves_out_of_scope_profile() -> Result<()> {
        let fixture = Fixture::new()?;
        fixture.set_storage_profiles(&[("Rust", "-rust"), ("Extra", "-extra")])?;
        let config = rust_config(&[], &[]);
        let mut snapshot = Snapshot::default();

        push(&fixture.ctx_scoped(&["Rust"]), &config, &mut snapshot)?;

        let names = fixture.editor_profile_names()?;
        assert!(
            names.contains(&"Extra".to_owned()),
            "out-of-scope profile untouched"
        );
        Ok(())
    }

    #[test]
    fn mirror_pull_removes_config_only_profile_but_managed_keeps_it() -> Result<()> {
        let fixture = Fixture::new()?;
        fixture.set_storage_profiles(&[("Rust", "-rust")])?; // editor has only Rust

        // Mirror: config-only "Gone" is removed.
        let mut config = rust_config(&[], &[]);
        config
            .profiles
            .insert("Gone".to_owned(), ProfileConfig::default());
        let mut snapshot = Snapshot::default();
        pull(&fixture.ctx(), &mut config, &mut snapshot)?;
        assert!(config.profiles.contains_key("Rust"));
        assert!(
            !config.profiles.contains_key("Gone"),
            "config-only profile removed"
        );

        // Managed: config-only "Gone" is kept.
        let mut managed = rust_config(&[], &[]);
        managed.options.managed = true;
        managed
            .profiles
            .insert("Gone".to_owned(), ProfileConfig::default());
        let mut snap2 = Snapshot::default();
        pull(&fixture.ctx(), &mut managed, &mut snap2)?;
        assert!(
            managed.profiles.contains_key("Gone"),
            "config-only profile kept in overlay"
        );
        Ok(())
    }

    #[test]
    fn pull_clears_tombstone_when_editor_has_profile() -> Result<()> {
        let fixture = Fixture::new()?;
        fixture.set_storage_profiles(&[("Rust", "-rust")])?;
        fixture.write_rust_settings(&json!({ "editor.tabSize": 4 }))?;
        let mut config = rust_config(&[], &[]);
        config.options.managed = true;
        config.profiles.insert("Rust".to_owned(), tombstone());
        let mut snapshot = Snapshot::default();

        pull(&fixture.ctx(), &mut config, &mut snapshot)?;

        let rust = config.profiles.get("Rust");
        assert_eq!(rust.map(|p| p.delete), Some(false), "tombstone cleared");
        assert_eq!(
            rust.and_then(|p| p.settings.get("editor.tabSize")),
            Some(&json!(4))
        );
        Ok(())
    }

    #[test]
    fn classify_without_base_adopts_the_present_side() {
        let a = json!(1);
        let b = json!(2);
        // Only one side has the item and there is no base: take that side.
        assert!(matches!(classify(None, Some(&a), None), Decision::TakeRepo));
        assert!(matches!(
            classify(None, None, Some(&a)),
            Decision::TakeEditor
        ));
        // Both present but differing, no base: a genuine conflict.
        assert!(matches!(
            classify(None, Some(&a), Some(&b)),
            Decision::Conflict
        ));
    }
}
