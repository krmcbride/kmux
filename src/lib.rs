pub(crate) mod agent;
pub(crate) mod cli;
pub(crate) mod commands;
pub(crate) mod completions;
pub(crate) mod config;
pub(crate) mod git;
pub(crate) mod paths;
pub(crate) mod slug;
pub(crate) mod state;
pub(crate) mod tmux;
pub(crate) mod workflows;

use anyhow::Result;
use clap::Parser;

pub fn run() -> Result<()> {
    let args = cli::Cli::parse();

    commands::dispatch(args.command)
}
