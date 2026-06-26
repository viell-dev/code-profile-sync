mod app;
mod app_config;
mod cli;
mod config;
mod editor;
mod extension;
mod jsonc;
mod safety;
mod snapshot;
mod sync;
mod ui;

use clap::Parser;

use cli::{Cli, Command};

pub fn run() -> std::process::ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Some(Command::Detect) => app::detect(),
        Some(Command::ListProfiles) => app::list_profiles(&cli.global),
        Some(Command::Status) => app::status(&cli.global),
        Some(Command::Init) => app::init(&cli.global),
        Some(Command::Push) => app::push(&cli.global),
        Some(Command::Pull) => app::pull(&cli.global),
        Some(Command::Sync) => app::sync(&cli.global),
        None => app::interactive(&cli.global),
    };
    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(err) => {
            ui::error(format!("{err:#}"));
            std::process::ExitCode::FAILURE
        }
    }
}
