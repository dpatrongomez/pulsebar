use std::io::IsTerminal;

use clap::Parser;
use pulsebar::cli::{Cli, Command};

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    #[cfg(feature = "gui")]
    if let Some(Command::Window { tab, app, quit }) = &cli.command {
        let tab = serde_json::from_value(serde_json::Value::String(tab.clone())).unwrap_or(pulsebar::settings::Tab::Overview);
        return pulsebar::gui::run_window(tab, app.clone(), *quit);
    }
    let gui = match &cli.command {
        Some(Command::App) => true,
        // No subcommand: launched by Finder/launchd (no terminal) → app; typed in a terminal
        // (e.g. via the Homebrew `pulsebar` symlink into the bundle) → status.
        None => !std::io::stdout().is_terminal(),
        Some(_) => false,
    };
    if gui {
        #[cfg(feature = "gui")]
        return pulsebar::gui::run_agent();
        #[cfg(not(feature = "gui"))]
        anyhow::bail!("built without the `gui` feature");
    }
    let cli = if cli.command.is_none() { Cli { json: cli.json, command: Some(Command::Status) } } else { cli };
    pulsebar::cli::run(cli)
}
