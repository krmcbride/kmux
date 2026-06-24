use anyhow::Result;

use crate::{agent, cli, completions, workflows};

pub fn dispatch(command: cli::Command) -> Result<()> {
    match command {
        cli::Command::Add(args) => workflows::run_add(args),
        cli::Command::Open(args) => workflows::run_open(args),
        cli::Command::Close(args) => workflows::run_close(args),
        cli::Command::List(args) => workflows::run_list(args),
        cli::Command::Path(args) => workflows::run_path(args),
        cli::Command::Remove(args) => workflows::run_remove(args),
        cli::Command::Rename(args) => workflows::run_rename(args),
        cli::Command::Status(args) => agent::status::run(args),
        cli::Command::Sidebar(args) => agent::sidebar::run(args),
        cli::Command::Completions { shell } => completions::generate(shell),
        cli::Command::CompleteHandles => completions::complete_handles(),
        cli::Command::CompleteAddBranches => completions::complete_add_branches(),
        cli::Command::CompleteGitBranches => completions::complete_git_branches(),
        cli::Command::SetWindowStatus(args) => agent::status::set_window_status(args),
    }
}
