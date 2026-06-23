use anyhow::{Result, bail};

use crate::cli;

mod add;
mod close;
mod context;
mod files;
mod list;
mod metadata;
mod open;
mod path;
mod remove;
mod rename;
mod resolve;
mod status;
mod util;
mod window;

pub fn dispatch(command: cli::Command) -> Result<()> {
    match command {
        cli::Command::Add(args) => add::run(args),
        cli::Command::Open(args) => open::run(args),
        cli::Command::Close(args) => close::run(args),
        cli::Command::List(args) => list::run(args),
        cli::Command::Path(args) => path::run(args),
        cli::Command::Remove(args) => remove::run(args),
        cli::Command::Rename(args) => rename::run(args),
        cli::Command::Status(args) => status::run(args),
        cli::Command::Sidebar(args) => crate::sidebar::run(args),
        cli::Command::CompleteHandles => crate::completions::complete_handles(),
        cli::Command::CompleteAddBranches => crate::completions::complete_add_branches(),
        cli::Command::CompleteGitBranches => crate::completions::complete_git_branches(),
        cli::Command::SetWindowStatus { status } => status::set_window_status(status),
        command => bail!("{} is not implemented yet", command.display_name()),
    }
}
