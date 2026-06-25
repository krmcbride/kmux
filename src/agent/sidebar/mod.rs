mod app;
mod lifecycle;
mod model;
mod render;
mod runtime;

use anyhow::Result;

use crate::cli::{SidebarArgs, SidebarCommand};

pub fn run(args: SidebarArgs) -> Result<()> {
    match args.command {
        Some(SidebarCommand::On) => lifecycle::enable(),
        Some(SidebarCommand::Off) => lifecycle::disable(),
        Some(SidebarCommand::Refresh) => lifecycle::refresh(),
        Some(SidebarCommand::Run) => lifecycle::run_tui(),
        Some(SidebarCommand::Wake { window_id }) => lifecycle::wake(&window_id),
        None => lifecycle::toggle(),
    }
}
