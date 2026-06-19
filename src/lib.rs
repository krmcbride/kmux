#[doc(hidden)]
pub mod cli;
#[doc(hidden)]
pub mod config;
#[doc(hidden)]
pub mod git;
#[doc(hidden)]
pub mod paths;
#[doc(hidden)]
pub mod slug;
#[doc(hidden)]
pub mod tmux;

use anyhow::{Result, bail};
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
        command => bail!("{} is not implemented yet", command.display_name()),
    }
}
