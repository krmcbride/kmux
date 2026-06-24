use anyhow::Result;

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
mod window;

pub fn run_add(args: cli::AddArgs) -> Result<()> {
    add::run(args)
}

pub fn run_open(args: cli::NameArgs) -> Result<()> {
    open::run(args)
}

pub fn run_close(args: cli::NameArgs) -> Result<()> {
    close::run(args)
}

pub fn run_list(args: cli::JsonArgs) -> Result<()> {
    list::run(args)
}

pub fn run_path(args: cli::NameArgs) -> Result<()> {
    path::run(args)
}

pub fn run_remove(args: cli::RemoveArgs) -> Result<()> {
    remove::run(args)
}

pub fn run_rename(args: cli::RenameArgs) -> Result<()> {
    rename::run(args)
}
