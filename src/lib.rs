pub(crate) mod cli;
pub(crate) mod config;
pub(crate) mod git;
pub(crate) mod paths;
pub(crate) mod slug;
pub(crate) mod state;
pub(crate) mod tmux;
pub(crate) mod workflows;

use anyhow::Result;
use clap::{CommandFactory, Parser};

pub fn run() -> Result<()> {
    let args = cli::Cli::parse();

    match args.command {
        cli::Command::Completions { shell } => {
            let mut command = cli::Cli::command();
            let name = command.get_name().to_owned();
            clap_complete::generate(shell, &mut command, name, &mut std::io::stdout());
            Ok(())
        }
        command => workflows::dispatch(command),
    }
}
