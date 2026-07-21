use anyhow::Result;

use crate::{agent, cli, completions, launcher, workflows};

/// Route a parsed CLI command to the workflow, agent, or completion handler that owns it.
pub fn dispatch(command: cli::Command) -> Result<i32> {
    match command {
        cli::Command::Launch(args) => return launcher::run_ingress(&args.request),
        cli::Command::Workspace(args) => dispatch_workspace(args.command),
        cli::Command::Config(args) => workflows::run_config(args),

        cli::Command::Sidebar(args) => agent::sidebar::run(args),
        cli::Command::Status(args) => workflows::run_status(args),
        cli::Command::SetAgentStatus(args) => workflows::run_set_agent_status(*args),

        cli::Command::Completions { shell } => completions::generate(shell),
        cli::Command::CompleteWorkspaces => completions::complete_workspaces(),
        cli::Command::CompleteCreateBranches => completions::complete_create_branches(),
        cli::Command::CompleteGitBranches => completions::complete_git_branches(),
        cli::Command::CompleteLaunchers => completions::complete_launchers(),
    }?;
    Ok(0)
}

fn dispatch_workspace(command: cli::WorkspaceCommand) -> Result<()> {
    match command {
        cli::WorkspaceCommand::Create(args) => workflows::run_create(args),
        cli::WorkspaceCommand::List(args) => workflows::run_list(args),
        cli::WorkspaceCommand::Remove(args) => workflows::run_remove(args),
        cli::WorkspaceCommand::SetParent(args) => workflows::run_set_parent(args),
        cli::WorkspaceCommand::Restore => workflows::run_restore(),
    }
}
