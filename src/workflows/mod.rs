//! Command use-case entrypoints for workspace and agent-status workflows.
//!
//! Workflow modules orchestrate CLI input, config, Git, tmux, files, state, and
//! output while keeping adapter-specific subprocess and storage details outside
//! the use-case layer.

use anyhow::Result;

use crate::cli;

mod add;
mod context;
mod files;
mod launch;
mod list;
mod parent;
mod remove;
mod resolve;
mod restore;
mod status;
mod window;

/// Run the workspace creation workflow.
pub fn run_add(args: cli::AddArgs) -> Result<()> {
    add::run(args)
}

/// Run the parent metadata workflow.
pub fn run_parent(args: cli::ParentArgs) -> Result<()> {
    parent::run(args)
}

/// Reconcile tmux windows for existing strict kmux worktrees.
pub fn run_restore() -> Result<()> {
    restore::run()
}

/// Print workspace inventory in human or JSON form.
pub fn run_list(args: cli::JsonArgs) -> Result<()> {
    list::run(args)
}

/// Remove one kmux workspace and its local branch.
pub fn run_remove(args: cli::RemoveArgs) -> Result<()> {
    remove::run(args)
}

/// Print tracked external agent status.
pub fn run_status(args: cli::StatusArgs) -> Result<()> {
    status::run_status(args)
}

/// Record or delete external agent status observations.
pub fn run_set_agent_status(args: cli::SetAgentStatusArgs) -> Result<()> {
    status::run_set_agent_status(args)
}
