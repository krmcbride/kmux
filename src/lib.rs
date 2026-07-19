pub(crate) mod agent;
pub(crate) mod cli;
pub(crate) mod commands;
pub(crate) mod completions;
pub(crate) mod config;
pub(crate) mod git;
pub(crate) mod launcher;
pub(crate) mod paths;
pub(crate) mod slug;
pub(crate) mod state;
pub(crate) mod telemetry;
pub(crate) mod tmux;
pub(crate) mod workflows;
pub(crate) mod workspace;

use anyhow::Result;
use clap::Parser;

/// Parse the CLI, dispatch the selected command, and return its process exit code.
pub fn run() -> Result<i32> {
    telemetry::init();
    let args = cli::Cli::parse();

    commands::dispatch(args.command)
}
