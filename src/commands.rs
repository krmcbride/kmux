use anyhow::Result;

use crate::{agent, cli, completions, workflows};

/// Route a parsed CLI command to the workflow, agent, or completion handler that owns it.
pub fn dispatch(command: cli::Command) -> Result<()> {
    match command {
        cli::Command::Add(args) => workflows::run_add(args),
        cli::Command::Parent(args) => workflows::run_parent(args),
        cli::Command::Restore => workflows::run_restore(),
        cli::Command::List(args) => workflows::run_list(args),
        cli::Command::Remove(args) => workflows::run_remove(args),

        cli::Command::Sidebar(args) => agent::sidebar::run(args),
        cli::Command::Status(args) => workflows::run_status(args),
        cli::Command::SetAgentStatus(args) => workflows::run_set_agent_status(*args),

        cli::Command::Completions { shell } => completions::generate(shell),
        cli::Command::CompleteWorkspaces => completions::complete_workspaces(),
        cli::Command::CompleteAddBranches => completions::complete_add_branches(),
        cli::Command::CompleteGitBranches => completions::complete_git_branches(),
    }
}
