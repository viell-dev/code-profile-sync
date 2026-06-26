//! Orchestration: resolve the editor and config, run a command, persist results,
//! and drive the interactive first-run flow.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::app_config::{AppConfig, AppPaths, KnownEditor, identifiers_match, safe_id};
use crate::cli::GlobalArgs;
use crate::config::Config;
use crate::editor::profiles;
use crate::editor::{self, Editor};
use crate::snapshot::Snapshot;
use crate::sync::{self, Ctx};
use crate::{safety, ui};

/// A resolved working session: a target editor, its config, and the snapshot.
struct Session {
    editor: Editor,
    config: Config,
    config_path: PathBuf,
    snapshot: Snapshot,
    snapshot_path: PathBuf,
    backup_dir: PathBuf,
    vendor_dir: PathBuf,
    app_paths: Option<AppPaths>,
    app_config: AppConfig,
}

impl Session {
    /// Load (or default) the config and snapshot for an editor at a config path.
    fn open(
        editor: Editor,
        config_path: PathBuf,
        storage: StoragePaths,
        app_paths: Option<AppPaths>,
        app_config: AppConfig,
    ) -> Result<Self> {
        let config = if config_path.is_file() {
            Config::load(&config_path)?
        } else {
            Config::default()
        };
        let snapshot = Snapshot::load(&storage.snapshot_path)?;
        Ok(Self {
            editor,
            config,
            config_path,
            snapshot,
            snapshot_path: storage.snapshot_path,
            backup_dir: storage.backup_dir,
            vendor_dir: storage.vendor_dir,
            app_paths,
            app_config,
        })
    }

    fn save_config(&self, dry_run: bool) -> Result<()> {
        if dry_run {
            ui::info("(dry run: config not written)");
            return Ok(());
        }
        let text = self.config.to_toml()?;
        safety::atomic_write(&self.config_path, &text)?;
        ui::info(format!("wrote {}", self.config_path.display()));
        Ok(())
    }

    fn save_snapshot(&self, dry_run: bool) -> Result<()> {
        if dry_run {
            return Ok(());
        }
        self.snapshot.save(&self.snapshot_path)
    }

    /// Refuse to write while the editor is running, prompting to close it first.
    fn ensure_editor_closed(&self, g: &GlobalArgs) -> Result<()> {
        if g.dry_run || g.force {
            return Ok(());
        }
        while safety::editor_running(&self.editor) {
            ui::warn(format!(
                "{} appears to be running; writing now is unsafe.",
                self.editor.id()
            ));
            if g.non_interactive {
                anyhow::bail!("editor is running; close it or pass --force");
            }
            if !ui::confirm("Close the editor, then continue?", true)
                .context("reading confirmation")?
            {
                anyhow::bail!("aborted: editor still running");
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
struct StoragePaths {
    snapshot_path: PathBuf,
    backup_dir: PathBuf,
    vendor_dir: PathBuf,
}

impl StoragePaths {
    fn for_app(app_paths: &AppPaths, editor: &Editor) -> Self {
        Self {
            snapshot_path: app_paths.snapshot_path(editor),
            backup_dir: app_paths.backup_dir(),
            vendor_dir: app_paths.vendor_dir.clone(),
        }
    }

    fn for_config(config_path: &Path, editor: &Editor) -> Self {
        let config_dir = config_path.parent().unwrap_or_else(|| Path::new("."));
        let state_dir = config_dir.join(".code-pm");
        Self {
            snapshot_path: state_dir
                .join("snapshots")
                .join(format!("{}.snapshot.json", safe_id(editor.id()))),
            backup_dir: state_dir.join("backups").join(safety::timestamp()),
            vendor_dir: state_dir.join("vendor").join("extensions"),
        }
    }
}

/// Build an engine context borrowing only the editor field, so other session
/// fields (config, snapshot) remain independently borrowable.
fn make_ctx<'a>(
    editor: &'a Editor,
    g: &GlobalArgs,
    backup_dir: PathBuf,
    vendor_dir: PathBuf,
) -> Ctx<'a> {
    Ctx {
        editor,
        dry_run: g.dry_run,
        non_interactive: g.non_interactive,
        assume_yes: g.yes,
        prefer: g.prefer,
        profile_filter: g.profile.clone(),
        backup_dir,
        vendor_dir,
    }
}

fn header(editor: &Editor, config_path: &Path) {
    ui::info(format!(
        "Editor: {} ({})",
        editor.id(),
        editor.launcher.display()
    ));
    ui::info(format!("Config: {}", config_path.display()));
}

fn save_app_config(session: &mut Session, g: &GlobalArgs) -> Result<()> {
    let Some(app_paths) = &session.app_paths else {
        return Ok(());
    };
    if session.app_config.default_editor.is_none() {
        session.app_config.default_editor = Some(session.editor.id().to_owned());
    }
    session.app_config.upsert_editor(&session.editor);
    if g.dry_run {
        ui::info("(dry run: app config not written)");
        return Ok(());
    }
    session.app_config.save(&app_paths.config)
}

// ---------------------------------------------------------------------------
// editor / config resolution
// ---------------------------------------------------------------------------

/// Apply config-level path/binary overrides onto a discovered editor.
fn apply_overrides(mut editor: Editor, config: Option<&Config>) -> Editor {
    if let Some(cfg) = config {
        if let Some(bin) = &cfg.editor.binary {
            editor.launcher.clone_from(bin);
        }
        if let Some(dir) = &cfg.editor.user_dir {
            editor.user_dir.clone_from(dir);
        }
        if let Some(dir) = &cfg.editor.extensions_dir {
            editor.extensions_dir.clone_from(dir);
        }
    }
    editor
}

/// Resolve which editor to operate on from args and an optional loaded config.
fn resolve_editor(
    g: &GlobalArgs,
    config: Option<&Config>,
    app_config: &AppConfig,
) -> Result<Editor> {
    if let Some(selector) = &g.editor {
        return find_editor(selector, app_config);
    }
    if let Some(cfg) = config {
        if let Some(bin) = &cfg.editor.binary {
            return editor::from_path(bin);
        }
        if let Some(name) = &cfg.editor.name {
            return find_editor(name, app_config);
        }
    }
    if let Some(default) = &app_config.default_editor {
        return find_editor(default, app_config);
    }
    let mut found = editor::discover();
    match found.len() {
        0 => anyhow::bail!("no editors found on PATH; pass --editor <name|path>"),
        1 => Ok(found.remove(0)),
        _ => {
            if g.non_interactive {
                anyhow::bail!("multiple editors found; pass --editor to choose");
            }
            choose_editor(found)
        }
    }
}

fn find_editor(selector: &str, app_config: &AppConfig) -> Result<Editor> {
    let as_path = Path::new(selector);
    if as_path.exists() {
        return editor::from_path(as_path);
    }

    let mut matches: Vec<(Editor, Option<&KnownEditor>)> = Vec::new();
    let discovered = editor::discover();
    for editor in discovered {
        let known = app_config
            .editors
            .iter()
            .find(|known| known.matches(selector) && known.matches(editor.id()));
        if editor.matches(selector) || known.is_some() {
            matches.push((editor, known));
        }
    }

    for known in &app_config.editors {
        if !known.matches(selector) {
            continue;
        }
        let Some(binary) = &known.binary else {
            continue;
        };
        if matches
            .iter()
            .any(|(_, existing)| existing.is_some_and(|existing| existing.id == known.id))
        {
            continue;
        }
        let Ok(editor) = editor::from_path(binary) else {
            continue;
        };
        matches.push((editor, Some(known)));
    }

    match matches.as_mut_slice() {
        [] => anyhow::bail!("no editor matched '{selector}'"),
        [(editor, known)] => {
            if let Some(known) = known {
                apply_known_overrides(editor, known);
            }
            Ok(editor.clone())
        }
        _ => {
            let names = matches
                .iter()
                .map(|(editor, _)| editor.id())
                .collect::<Vec<_>>()
                .join(", ");
            anyhow::bail!("editor selector '{selector}' is ambiguous: {names}");
        }
    }
}

fn apply_known_overrides(editor: &mut Editor, known: &KnownEditor) {
    if let Some(binary) = &known.binary
        && let Ok(candidate) = editor::from_path(binary)
        && identifiers_match(candidate.id(), editor.id())
    {
        editor.launcher.clone_from(binary);
    }
    if let Some(user_dir) = &known.user_dir {
        editor.user_dir.clone_from(user_dir);
    }
    if let Some(extensions_dir) = &known.extensions_dir {
        editor.extensions_dir.clone_from(extensions_dir);
    }
}

fn choose_editor(found: Vec<Editor>) -> Result<Editor> {
    let mut labels: Vec<String> = found.iter().map(|e| e.id().to_owned()).collect();
    labels.push("Enter a custom path…".to_owned());
    let choice = ui::select("Select an editor", &labels).context("reading editor choice")?;
    if let Some(editor) = found.into_iter().nth(choice) {
        Ok(editor)
    } else {
        let path = ui::input("Path to the editor launcher or install directory")?;
        editor::from_path(Path::new(path.trim()))
    }
}

fn config_path_for(g: &GlobalArgs, app_paths: &AppPaths, editor: &Editor) -> PathBuf {
    g.config
        .clone()
        .unwrap_or_else(|| app_paths.editor_config_path(editor))
}

fn storage_paths_for(g: &GlobalArgs, app_paths: &AppPaths, editor: &Editor) -> StoragePaths {
    if g.config.is_some() && g.app_dir.is_none() {
        StoragePaths::for_config(&config_path_for(g, app_paths, editor), editor)
    } else {
        StoragePaths::for_app(app_paths, editor)
    }
}

/// Build a session from global args (non-interactive editor selection).
fn open_session(g: &GlobalArgs) -> Result<Session> {
    let app_paths = AppPaths::resolve(g.app_dir.as_deref())?;
    let app_config = AppConfig::load(&app_paths.config)?;
    let loaded = match &g.config {
        Some(path) if path.is_file() => Some(Config::load(path)?),
        _ => None,
    };
    let editor = apply_overrides(
        resolve_editor(g, loaded.as_ref(), &app_config)?,
        loaded.as_ref(),
    );
    let config_path = config_path_for(g, &app_paths, &editor);
    let storage = storage_paths_for(g, &app_paths, &editor);
    let app_paths = (g.config.is_none() || g.app_dir.is_some()).then_some(app_paths);
    Session::open(editor, config_path, storage, app_paths, app_config)
}

// ---------------------------------------------------------------------------
// commands
// ---------------------------------------------------------------------------

/// List editors discovered on this machine.
#[expect(
    clippy::unnecessary_wraps,
    reason = "uniform command signature for dispatch"
)]
pub fn detect() -> Result<()> {
    let found = editor::discover();
    if found.is_empty() {
        ui::info("No editors found on PATH.");
        return Ok(());
    }
    ui::heading("Discovered editors");
    for editor in &found {
        let product = &editor.product;
        let quality = product.quality.as_deref().unwrap_or("unknown");
        ui::info(format!("{} [{}]", editor.id(), product.application_name));
        ui::detail(format!("name:       {} ({quality})", product.name_long));
        if let Some(commit) = &product.commit {
            ui::detail(format!(
                "commit:     {}",
                commit.get(..12).unwrap_or(commit)
            ));
        }
        ui::detail(format!("launcher:   {}", editor.launcher.display()));
        ui::detail(format!("user dir:   {}", editor.user_dir.display()));
        ui::detail(format!("extensions: {}", editor.extensions_dir.display()));
        ui::detail(format!("present:    {}", editor.is_present()));
    }
    Ok(())
}

/// List the selected editor's profiles.
pub fn list_profiles(g: &GlobalArgs) -> Result<()> {
    let session = open_session(g)?;
    header(&session.editor, &session.config_path);
    let profiles = profiles::read_all(&session.editor)?;
    ui::heading(format!("Profiles ({})", profiles.len()));
    for profile in profiles {
        let inherited: Vec<&str> = profile
            .use_default
            .iter()
            .filter(|(_, v)| **v)
            .map(|(k, _)| k.as_str())
            .collect();
        let icon = profile.icon.as_deref().unwrap_or("-");
        ui::info(format!("{} (icon: {icon})", profile.name));
        if !inherited.is_empty() {
            ui::detail(format!("inherits from Default: {}", inherited.join(", ")));
        }
    }
    Ok(())
}

/// Show drift without writing.
pub fn status(g: &GlobalArgs) -> Result<()> {
    let session = open_session(g)?;
    header(&session.editor, &session.config_path);
    if !session.config_path.is_file() {
        ui::warn("no config yet; run `init` or the interactive flow to create one");
    }
    let ctx = make_ctx(
        &session.editor,
        g,
        session.backup_dir.clone(),
        session.vendor_dir.clone(),
    );
    sync::status(&ctx, &session.config)
}

/// Create a config from the editor's current profiles.
pub fn init(g: &GlobalArgs) -> Result<()> {
    let mut session = open_session(g)?;
    header(&session.editor, &session.config_path);
    save_app_config(&mut session, g)?;
    if session.config_path.is_file()
        && !g.yes
        && !g.non_interactive
        && !ui::confirm(
            &format!("{} exists; overwrite?", session.config_path.display()),
            false,
        )?
    {
        anyhow::bail!("aborted");
    }
    run_init(session, g)
}

fn run_init(mut session: Session, g: &GlobalArgs) -> Result<()> {
    session.config = Config::default();
    session.config.editor.name = Some(session.editor.id().to_owned());
    session.snapshot = Snapshot::default();
    ui::heading("Importing profiles from editor");
    let ctx = make_ctx(
        &session.editor,
        g,
        session.backup_dir.clone(),
        session.vendor_dir.clone(),
    );
    sync::pull(&ctx, &mut session.config, &mut session.snapshot)?;
    drop(ctx);

    // Hoist settings/extensions common to every profile into [global]. Default
    // to yes; only ask when interactive.
    let consolidate = g.yes
        || g.non_interactive
        || ui::confirm(
            "Consolidate settings/extensions shared by all profiles into [global]?",
            true,
        )?;
    if consolidate {
        run_consolidate(&mut session);
    }

    session.save_config(g.dry_run)?;
    session.save_snapshot(g.dry_run)?;
    Ok(())
}

fn run_consolidate(session: &mut Session) {
    let report = session.config.consolidate();
    ui::info(format!(
        "consolidated {} setting(s) and {} extension(s) into [global]",
        report.settings, report.extensions
    ));
}

/// Apply the config to the editor.
pub fn push(g: &GlobalArgs) -> Result<()> {
    let session = open_session(g)?;
    header(&session.editor, &session.config_path);
    require_config(&session)?;
    session.ensure_editor_closed(g)?;
    let mut session = session;
    ui::heading("Pushing config to editor");
    let ctx = make_ctx(
        &session.editor,
        g,
        session.backup_dir.clone(),
        session.vendor_dir.clone(),
    );
    sync::push(&ctx, &session.config, &mut session.snapshot)?;
    session.save_snapshot(g.dry_run)?;
    Ok(())
}

/// Update the config from the editor.
pub fn pull(g: &GlobalArgs) -> Result<()> {
    let mut session = open_session(g)?;
    header(&session.editor, &session.config_path);
    ui::heading("Pulling editor into config");
    let ctx = make_ctx(
        &session.editor,
        g,
        session.backup_dir.clone(),
        session.vendor_dir.clone(),
    );
    sync::pull(&ctx, &mut session.config, &mut session.snapshot)?;
    session.save_config(g.dry_run)?;
    session.save_snapshot(g.dry_run)?;
    Ok(())
}

/// Reconcile both directions.
pub fn sync(g: &GlobalArgs) -> Result<()> {
    let session = open_session(g)?;
    header(&session.editor, &session.config_path);
    require_config(&session)?;
    session.ensure_editor_closed(g)?;
    let mut session = session;
    let ctx = make_ctx(
        &session.editor,
        g,
        session.backup_dir.clone(),
        session.vendor_dir.clone(),
    );
    sync::sync(&ctx, &mut session.config, &mut session.snapshot)?;
    session.save_config(g.dry_run)?;
    session.save_snapshot(g.dry_run)?;
    Ok(())
}

fn require_config(session: &Session) -> Result<()> {
    if session.config.profiles.is_empty() && !session.config_path.is_file() {
        anyhow::bail!(
            "no config at {}; run `init` first",
            session.config_path.display()
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// interactive flow
// ---------------------------------------------------------------------------

/// The first-run wizard plus the main menu (default with no subcommand).
pub fn interactive(g: &GlobalArgs) -> Result<()> {
    if g.non_interactive {
        anyhow::bail!("--non-interactive requires a subcommand");
    }

    let app_paths = AppPaths::resolve(g.app_dir.as_deref())?;
    let app_config = AppConfig::load(&app_paths.config)?;

    // 1. Select an editor (honoring --editor for the first pick).
    let editor = if let Some(selector) = &g.editor {
        find_editor(selector, &app_config)?
    } else {
        pick_editor()?
    };
    let session = prepare_session(g, editor, app_paths, app_config)?;

    // 2. Main menu.
    menu_loop(session, g)
}

/// Interactively pick a discovered editor (or a custom path).
fn pick_editor() -> Result<Editor> {
    let found = editor::discover();
    if found.is_empty() {
        ui::warn("no editors found on PATH");
        let path = ui::input("Path to the editor launcher or install directory")?;
        editor::from_path(Path::new(path.trim()))
    } else {
        choose_editor(found)
    }
}

/// Open a session for `editor`, offering to create a config when none exists.
fn prepare_session(
    g: &GlobalArgs,
    editor: Editor,
    app_paths: AppPaths,
    app_config: AppConfig,
) -> Result<Session> {
    let editor = apply_overrides(editor, None);
    let config_path = config_path_for(g, &app_paths, &editor);
    let storage = storage_paths_for(g, &app_paths, &editor);
    let app_paths = (g.config.is_none() || g.app_dir.is_some()).then_some(app_paths);
    let mut session = Session::open(editor, config_path, storage, app_paths, app_config)?;
    header(&session.editor, &session.config_path);
    save_app_config(&mut session, g)?;
    if !session.config_path.is_file() {
        if ui::confirm(
            "No config found. Create one from the editor's current profiles?",
            true,
        )? {
            session = create_config(session, g)?;
        } else {
            ui::info("Continuing with an empty config.");
        }
    }
    Ok(session)
}

fn create_config(session: Session, g: &GlobalArgs) -> Result<Session> {
    let config_path = session.config_path.clone();
    let editor = session.editor.clone();
    let storage = StoragePaths {
        snapshot_path: session.snapshot_path.clone(),
        backup_dir: session.backup_dir.clone(),
        vendor_dir: session.vendor_dir.clone(),
    };
    let app_paths = session.app_paths.clone();
    let app_config = session.app_config.clone();
    run_init(session, g)?;
    Session::open(editor, config_path, storage, app_paths, app_config)
}

fn menu_loop(mut session: Session, g: &GlobalArgs) -> Result<()> {
    let actions = [
        "Sync (reconcile both ways)",
        "Push: make the editor match the config (REPLACES editor profiles)",
        "Pull: make the config match the editor (REPLACES config profiles)",
        "Manage a single profile…",
        "Consolidate shared settings/extensions into [global]",
        "Choose a different editor",
        "Exit",
    ];
    loop {
        let choice = ui::select("What would you like to do?", &actions.map(str::to_owned))
            .context("reading menu choice")?;
        match choice {
            0 => {
                session.ensure_editor_closed(g)?;
                let ctx = make_ctx(
                    &session.editor,
                    g,
                    session.backup_dir.clone(),
                    session.vendor_dir.clone(),
                );
                sync::sync(&ctx, &mut session.config, &mut session.snapshot)?;
                session.save_config(g.dry_run)?;
                session.save_snapshot(g.dry_run)?;
            }
            1 => {
                session.ensure_editor_closed(g)?;
                ui::heading("Pushing config to editor");
                let ctx = make_ctx(
                    &session.editor,
                    g,
                    session.backup_dir.clone(),
                    session.vendor_dir.clone(),
                );
                sync::push(&ctx, &session.config, &mut session.snapshot)?;
                session.save_snapshot(g.dry_run)?;
            }
            2 => {
                ui::heading("Pulling editor into config");
                let ctx = make_ctx(
                    &session.editor,
                    g,
                    session.backup_dir.clone(),
                    session.vendor_dir.clone(),
                );
                sync::pull(&ctx, &mut session.config, &mut session.snapshot)?;
                session.save_config(g.dry_run)?;
                session.save_snapshot(g.dry_run)?;
            }
            3 => manage_profile_menu(&mut session, g)?,
            4 => {
                run_consolidate(&mut session);
                session.save_config(g.dry_run)?;
            }
            5 => {
                let app_paths = if let Some(app_paths) = session.app_paths.clone() {
                    app_paths
                } else {
                    AppPaths::resolve(g.app_dir.as_deref())?
                };
                session =
                    prepare_session(g, pick_editor()?, app_paths, session.app_config.clone())?;
            }
            _ => return Ok(()),
        }
    }
}

/// An engine context scoped to a single profile (overlay mode), so an action
/// touches only that profile and never creates/deletes the others.
fn scoped_ctx<'a>(
    editor: &'a Editor,
    g: &GlobalArgs,
    backup_dir: PathBuf,
    vendor_dir: PathBuf,
    name: &str,
) -> Ctx<'a> {
    let mut ctx = make_ctx(editor, g, backup_dir, vendor_dir);
    ctx.profile_filter = vec![name.to_owned()];
    ctx
}

/// A selectable label: profile name + where it lives + its sync state.
fn profile_label(s: &sync::ProfileSummary) -> String {
    let location = match (s.in_config, s.in_editor) {
        (true, true) => "config + editor",
        (true, false) => "config only",
        (false, true) => "editor only",
        (false, false) => "unknown",
    };
    let state = if s.tombstone {
        "marked for deletion"
    } else {
        match s.in_sync {
            Some(true) => "in sync",
            Some(false) => "drift",
            None => "-",
        }
    };
    format!("{}  [{location}] {state}", s.name)
}

/// Overview of all profiles (config and/or editor); pick one to manage.
fn manage_profile_menu(session: &mut Session, g: &GlobalArgs) -> Result<()> {
    loop {
        let summaries = {
            let mut ctx = make_ctx(
                &session.editor,
                g,
                session.backup_dir.clone(),
                session.vendor_dir.clone(),
            );
            ctx.profile_filter = Vec::new(); // always list every profile
            sync::profile_summaries(&ctx, &session.config)?
        };
        if summaries.is_empty() {
            ui::info("No profiles in the config or the editor.");
            return Ok(());
        }
        ui::heading("Profiles");
        let mut labels: Vec<String> = summaries.iter().map(profile_label).collect();
        labels.push("Back".to_owned());
        let choice =
            ui::select("Select a profile to manage", &labels).context("reading profile choice")?;
        match summaries.get(choice) {
            Some(summary) => profile_action_menu(session, g, &summary.name)?,
            None => return Ok(()), // Back
        }
    }
}

/// A scoped action offered in the per-profile submenu.
#[derive(Clone, Copy)]
enum ProfileAction {
    Status,
    Sync,
    Push,
    Pull,
    Delete,
}

/// Actions scoped to one profile. Returns to the profile list on Back, or after
/// the profile is deleted. Delete is not offered for the Default profile.
fn profile_action_menu(session: &mut Session, g: &GlobalArgs, name: &str) -> Result<()> {
    loop {
        let mut actions: Vec<(ProfileAction, &str)> = vec![
            (ProfileAction::Status, "Status / drift"),
            (ProfileAction::Sync, "Sync this profile"),
            (ProfileAction::Push, "Push this profile (config -> editor)"),
            (ProfileAction::Pull, "Pull this profile (editor -> config)"),
        ];
        if name != profiles::DEFAULT_PROFILE {
            actions.push((ProfileAction::Delete, "Delete this profile"));
        }
        let mut labels: Vec<String> = actions
            .iter()
            .map(|(_, label)| (*label).to_owned())
            .collect();
        labels.push("Back".to_owned());

        let choice =
            ui::select(&format!("Profile: {name}"), &labels).context("reading action choice")?;
        let Some((action, _)) = actions.get(choice) else {
            return Ok(()); // Back
        };
        match action {
            ProfileAction::Status => {
                let ctx = scoped_ctx(
                    &session.editor,
                    g,
                    session.backup_dir.clone(),
                    session.vendor_dir.clone(),
                    name,
                );
                sync::status(&ctx, &session.config)?;
            }
            ProfileAction::Sync => {
                session.ensure_editor_closed(g)?;
                ui::heading(format!("Syncing {name}"));
                let ctx = scoped_ctx(
                    &session.editor,
                    g,
                    session.backup_dir.clone(),
                    session.vendor_dir.clone(),
                    name,
                );
                sync::sync(&ctx, &mut session.config, &mut session.snapshot)?;
                session.save_config(g.dry_run)?;
                session.save_snapshot(g.dry_run)?;
            }
            ProfileAction::Push => {
                session.ensure_editor_closed(g)?;
                ui::heading(format!("Pushing {name} to editor"));
                let ctx = scoped_ctx(
                    &session.editor,
                    g,
                    session.backup_dir.clone(),
                    session.vendor_dir.clone(),
                    name,
                );
                sync::push(&ctx, &session.config, &mut session.snapshot)?;
                session.save_snapshot(g.dry_run)?;
            }
            ProfileAction::Pull => {
                ui::heading(format!("Pulling {name} into config"));
                let ctx = scoped_ctx(
                    &session.editor,
                    g,
                    session.backup_dir.clone(),
                    session.vendor_dir.clone(),
                    name,
                );
                sync::pull(&ctx, &mut session.config, &mut session.snapshot)?;
                session.save_config(g.dry_run)?;
                session.save_snapshot(g.dry_run)?;
            }
            ProfileAction::Delete => {
                if delete_profile_action(session, g, name)? {
                    return Ok(()); // profile gone; back to the list
                }
            }
        }
    }
}

/// Delete a single profile from the editor and the config. Returns whether the
/// profile was removed (so the caller leaves the per-profile submenu).
fn delete_profile_action(session: &mut Session, g: &GlobalArgs, name: &str) -> Result<bool> {
    if name == profiles::DEFAULT_PROFILE {
        ui::warn("the Default profile cannot be deleted");
        return Ok(false);
    }
    let confirmed = g.yes
        || g.non_interactive
        || ui::confirm(
            &format!("Delete profile '{name}' from the editor and config? This cannot be undone."),
            false,
        )
        .context("reading confirmation")?;
    if !confirmed {
        ui::info("aborted");
        return Ok(false);
    }
    session.ensure_editor_closed(g)?;
    if profiles::delete(&session.editor, name, g.dry_run, &session.backup_dir)? {
        ui::bullet(format!(
            "{} profile {name} from editor",
            if g.dry_run { "would delete" } else { "deleted" }
        ));
    } else {
        ui::detail(format!("{name} is not present in the editor"));
    }
    if session.config.profiles.remove(name).is_some() {
        ui::bullet(format!(
            "{} {name} from config",
            if g.dry_run { "would remove" } else { "removed" }
        ));
    }
    session.snapshot.profiles.remove(name);
    session.save_config(g.dry_run)?;
    session.save_snapshot(g.dry_run)?;
    Ok(true)
}
