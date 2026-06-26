//! Command-line interface definition.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

#[derive(Debug, Parser)]
#[command(name = "code-profile-manager", version, about, long_about = None)]
pub struct Cli {
    #[command(flatten)]
    pub global: GlobalArgs,

    /// Action to perform. With no subcommand, the interactive flow runs.
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Args)]
#[expect(clippy::struct_excessive_bools, reason = "independent CLI flags")]
pub struct GlobalArgs {
    /// Select an editor by name (`nameShort`/`applicationName`) or launcher path.
    #[arg(long, short, global = true)]
    pub editor: Option<String>,

    /// Path to the config file (defaults to one derived from the editor name).
    #[arg(long, short, global = true)]
    pub config: Option<PathBuf>,

    /// Application state directory (defaults to the platform app config dir).
    #[arg(long = "app-dir", global = true)]
    pub app_dir: Option<PathBuf>,

    /// Limit the operation to the named profile(s); repeatable. When set, the run
    /// is scoped (overlay): undefined profiles are never created or deleted.
    #[arg(long, short, global = true)]
    pub profile: Vec<String>,

    /// Show what would change without writing anything.
    #[arg(long, global = true)]
    pub dry_run: bool,

    /// Assume yes for confirmation prompts.
    #[arg(long, short = 'y', global = true)]
    pub yes: bool,

    /// Proceed even if the editor appears to be running.
    #[arg(long, global = true)]
    pub force: bool,

    /// Never prompt; fail instead of asking. Implied when no TTY is desired.
    #[arg(long = "non-interactive", global = true)]
    pub non_interactive: bool,

    /// Default side to keep when a conflict is found.
    #[arg(long, value_enum, global = true)]
    pub prefer: Option<Prefer>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Prefer {
    /// Keep the editor's value on conflict.
    Editor,
    /// Keep the config's value on conflict.
    Repo,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// List editors discovered on this machine.
    Detect,
    /// List the selected editor's profiles.
    ListProfiles,
    /// Show drift between the config and the editor without writing.
    Status,
    /// Create a config from the editor's current profiles.
    Init,
    /// Make the editor mirror the config (config -> editor; deletes editor extras
    /// unless scoped by --profile or `[options] managed`).
    Push,
    /// Make the config mirror the editor (editor -> config; removes config-only
    /// profiles unless scoped by --profile or `[options] managed`).
    Pull,
    /// Reconcile both directions with conflict resolution (non-destructive merge).
    Sync,
}
