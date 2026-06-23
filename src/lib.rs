pub(crate) mod agents;
pub(crate) mod cli;
pub(crate) mod completions;
pub(crate) mod config;
pub(crate) mod git;
pub(crate) mod paths;
pub(crate) mod sidebar;
pub(crate) mod slug;
pub(crate) mod state;
pub(crate) mod tmux;
pub(crate) mod workflows;

use anyhow::Result;
use clap::Parser;

pub fn run() -> Result<()> {
    let args = cli::Cli::parse();

    match args.command {
        cli::Command::Completions { shell } => completions::generate(shell),
        command => workflows::dispatch(command),
    }
}
