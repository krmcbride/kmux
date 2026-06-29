use anyhow::Result;

use crate::cli;

mod add;
mod context;
mod files;
mod list;
mod metadata;
mod open;
mod remove;
mod resolve;
mod window;

pub fn run_add(args: cli::AddArgs) -> Result<()> {
    add::run(args)
}

pub fn run_open(args: cli::WorkspaceNameArgs) -> Result<()> {
    open::run(args)
}

pub fn run_list(args: cli::JsonArgs) -> Result<()> {
    list::run(args)
}

pub fn run_remove(args: cli::RemoveArgs) -> Result<()> {
    remove::run(args)
}
