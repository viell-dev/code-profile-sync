//! Orchestration: resolve the editor and config, run a command, persist results,
//! and drive the interactive first-run flow.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::cli::GlobalArgs;
use crate::config::Config;
use crate::editor::profiles;
use crate::editor::{self, Editor};
use crate::snapshot::{self, Snapshot};
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
}

impl Session {
    /// Load (or default) the config and snapshot for an editor at a config path.
    fn open(editor: Editor, config_path: PathBuf) -> Result<Self> {
        let config = if config_path.is_file() {
            Config::load(&config_path)?
        } else {
            Config::default()
        };
        let snapshot_path = snapshot::path_for(&config_path, editor.id());
        let snapshot = Snapshot::load(&snapshot_path)?;
        let backup_dir = config_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(".code-profile-sync")
            .join("backups")
            .join(safety::timestamp());
        Ok(Self {
            editor,
            config,
            config_path,
            snapshot,
            snapshot_path,
            backup_dir,
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

/// Build an engine context borrowing only the editor field, so other session
/// fields (config, snapshot) remain independently borrowable.
fn make_ctx<'a>(editor: &'a Editor, g: &GlobalArgs, backup_dir: PathBuf) -> Ctx<'a> {
    Ctx {
        editor,
        dry_run: g.dry_run,
        non_interactive: g.non_interactive,
        prefer: g.prefer,
        profile_filter: g.profile.clone(),
        backup_dir,
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
fn resolve_editor(g: &GlobalArgs, config: Option<&Config>) -> Result<Editor> {
    if let Some(selector) = &g.editor {
        return editor::find(selector).with_context(|| format!("no editor matched '{selector}'"));
    }
    if let Some(cfg) = config {
        if let Some(bin) = &cfg.editor.binary {
            return editor::from_path(bin);
        }
        if let Some(name) = &cfg.editor.name {
            return editor::find(name).with_context(|| format!("no editor matched '{name}'"));
        }
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

fn default_config_path(editor: &Editor) -> PathBuf {
    let name = editor
        .product
        .application_name
        .replace([' ', '/', '\\'], "-");
    PathBuf::from(format!("{name}.toml"))
}

/// Build a session from global args (non-interactive editor selection).
fn open_session(g: &GlobalArgs) -> Result<Session> {
    let loaded = match &g.config {
        Some(path) if path.is_file() => Some(Config::load(path)?),
        _ => None,
    };
    let editor = apply_overrides(resolve_editor(g, loaded.as_ref())?, loaded.as_ref());
    let config_path = g
        .config
        .clone()
        .unwrap_or_else(|| default_config_path(&editor));
    Session::open(editor, config_path)
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
    let ctx = make_ctx(&session.editor, g, session.backup_dir.clone());
    sync::status(&ctx, &session.config)
}

/// Create a config from the editor's current profiles.
pub fn init(g: &GlobalArgs) -> Result<()> {
    let session = open_session(g)?;
    header(&session.editor, &session.config_path);
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
    let ctx = make_ctx(&session.editor, g, session.backup_dir.clone());
    sync::pull(&ctx, &mut session.config, &mut session.snapshot)?;
    drop(ctx);
    session.save_config(g.dry_run)?;
    session.save_snapshot(g.dry_run)?;
    Ok(())
}

/// Apply the config to the editor.
pub fn push(g: &GlobalArgs) -> Result<()> {
    let session = open_session(g)?;
    header(&session.editor, &session.config_path);
    require_config(&session)?;
    session.ensure_editor_closed(g)?;
    let mut session = session;
    ui::heading("Pushing config to editor");
    let ctx = make_ctx(&session.editor, g, session.backup_dir.clone());
    sync::push(&ctx, &session.config, &mut session.snapshot)?;
    session.save_snapshot(g.dry_run)?;
    Ok(())
}

/// Update the config from the editor.
pub fn pull(g: &GlobalArgs) -> Result<()> {
    let mut session = open_session(g)?;
    header(&session.editor, &session.config_path);
    ui::heading("Pulling editor into config");
    let ctx = make_ctx(&session.editor, g, session.backup_dir.clone());
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
    let ctx = make_ctx(&session.editor, g, session.backup_dir.clone());
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

    // 1. Select an editor.
    let editor = if let Some(selector) = &g.editor {
        editor::find(selector).with_context(|| format!("no editor matched '{selector}'"))?
    } else {
        let found = editor::discover();
        if found.is_empty() {
            ui::warn("no editors found on PATH");
            let path = ui::input("Path to the editor launcher or install directory")?;
            editor::from_path(Path::new(path.trim()))?
        } else {
            choose_editor(found)?
        }
    };
    let config_path = g
        .config
        .clone()
        .unwrap_or_else(|| default_config_path(&editor));
    let mut session = Session::open(apply_overrides(editor, None), config_path)?;
    header(&session.editor, &session.config_path);

    // 2. Offer to create a config from existing profiles when missing.
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

    // 3. Main menu.
    menu_loop(session, g)
}

fn create_config(session: Session, g: &GlobalArgs) -> Result<Session> {
    let config_path = session.config_path.clone();
    let editor = session.editor.clone();
    run_init(session, g)?;
    Session::open(editor, config_path)
}

fn menu_loop(mut session: Session, g: &GlobalArgs) -> Result<()> {
    let actions = [
        "Sync (reconcile both ways)",
        "Overwrite profiles from config (push)",
        "Overwrite config from profiles (pull)",
        "Exit",
    ];
    loop {
        let choice = ui::select("What would you like to do?", &actions.map(str::to_owned))
            .context("reading menu choice")?;
        match choice {
            0 => {
                session.ensure_editor_closed(g)?;
                let ctx = make_ctx(&session.editor, g, session.backup_dir.clone());
                sync::sync(&ctx, &mut session.config, &mut session.snapshot)?;
                session.save_config(g.dry_run)?;
                session.save_snapshot(g.dry_run)?;
            }
            1 => {
                session.ensure_editor_closed(g)?;
                ui::heading("Pushing config to editor");
                let ctx = make_ctx(&session.editor, g, session.backup_dir.clone());
                sync::push(&ctx, &session.config, &mut session.snapshot)?;
                session.save_snapshot(g.dry_run)?;
            }
            2 => {
                ui::heading("Pulling editor into config");
                let ctx = make_ctx(&session.editor, g, session.backup_dir.clone());
                sync::pull(&ctx, &mut session.config, &mut session.snapshot)?;
                session.save_config(g.dry_run)?;
                session.save_snapshot(g.dry_run)?;
            }
            _ => return Ok(()),
        }
    }
}
