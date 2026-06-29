use anyhow::Result;

use crate::{agent, cli, completions, workflows};

pub fn dispatch(command: cli::Command) -> Result<()> {
    match command {
        cli::Command::Add(args) => workflows::run_add(args),
        cli::Command::Restore => workflows::run_restore(),
        cli::Command::List(args) => workflows::run_list(args),
        cli::Command::Remove(args) => workflows::run_remove(args),

        cli::Command::Sidebar(args) => agent::sidebar::run(args),
        cli::Command::Status(args) => agent::status::run(args),
        cli::Command::SetAgentStatus(args) => agent::status::set_agent_status(*args),

        cli::Command::Completions { shell } => completions::generate(shell),
        cli::Command::CompleteWorkspaces => completions::complete_workspaces(),
        cli::Command::CompleteAddBranches => completions::complete_add_branches(),
        cli::Command::CompleteGitBranches => completions::complete_git_branches(),
    }
}
