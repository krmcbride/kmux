use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::cli;
use crate::config::Config;
use crate::git::Git;
use crate::paths::RepoPaths;
use crate::state::{
    AgentState, AgentStatus as StoredAgentStatus, PaneKey, StateStore, now_unix_seconds,
};
use crate::tmux::{Tmux, kmux_worktree_option};

use super::util::same_path;

const KMUX_STATUS_OPTION: &str = "@kmux_status";

#[derive(Debug)]
struct WindowWorktree {
    handle: Option<String>,
    path: Option<PathBuf>,
    branch: Option<String>,
}

pub(super) fn run(args: cli::StatusArgs) -> Result<()> {
    let store = StateStore::new()?;
    let tmux = Tmux::from_env();
    let mut agents = active_agents(&store, &tmux)?;
    if let Some(filter) = args.filter.as_deref() {
        agents.retain(|agent| agent_matches_filter(agent, filter));
    }

    if args.json {
        println!("{}", serde_json::to_string_pretty(&agents)?);
    } else {
        for agent in agents {
            let handle = agent.worktree_handle.as_deref().unwrap_or("-");
            let branch = agent.branch.as_deref().unwrap_or("-");
            println!(
                "{}\t{}\t{}\t{}\t{}",
                handle,
                branch,
                agent.status.as_str(),
                agent.icon,
                agent.window_name
            );
        }
    }
    Ok(())
}

pub(super) fn set_window_status(status: cli::AgentStatus) -> Result<()> {
    if std::env::var_os("KMUX_DISABLE_SET_WINDOW_STATUS").is_some() {
        return Ok(());
    }

    let config = Config::load()?;
    let tmux = Tmux::from_env();
    let Some(context) = tmux.current_context()? else {
        return Ok(());
    };
    let key = PaneKey::new_tmux(tmux.instance_id(), context.pane_id.clone());

    if status == cli::AgentStatus::Clear {
        tmux.unset_window_option(&context.pane_id, KMUX_STATUS_OPTION)?;
        StateStore::new()?.delete_agent(&key)?;
        return Ok(());
    }

    let (status, icon) = match status {
        cli::AgentStatus::Working => (StoredAgentStatus::Working, config.status_icons.working()),
        cli::AgentStatus::Waiting => (StoredAgentStatus::Waiting, config.status_icons.waiting()),
        cli::AgentStatus::Done => (StoredAgentStatus::Done, config.status_icons.done()),
        cli::AgentStatus::Clear => return Ok(()),
    };
    tmux.set_window_option(&context.pane_id, KMUX_STATUS_OPTION, icon)?;

    let worktree = current_window_worktree(&config, &tmux, &context)?;
    let state = AgentState {
        pane_key: key,
        status,
        icon: icon.to_owned(),
        updated_at: now_unix_seconds(),
        worktree_handle: worktree.handle,
        worktree_path: worktree.path.map(|path| path.display().to_string()),
        branch: worktree.branch,
        session_name: context.session_name,
        window_name: context.window_name,
        window_id: context.window_id,
    };
    StateStore::new()?.upsert_agent(&state)?;
    Ok(())
}

fn active_agents(store: &StateStore, tmux: &Tmux) -> Result<Vec<AgentState>> {
    let instance_id = tmux.instance_id();
    let mut agents = Vec::new();
    for agent in store.list_agents()? {
        if agent.pane_key.backend == "tmux" && agent.pane_key.instance == instance_id {
            match tmux.pane_context(&agent.pane_key.pane_id) {
                Ok(context) if context.window_id == agent.window_id => agents.push(agent),
                Err(_) => store.delete_agent(&agent.pane_key)?,
                Ok(_) => store.delete_agent(&agent.pane_key)?,
            }
        } else {
            agents.push(agent);
        }
    }
    Ok(agents)
}

fn agent_matches_filter(agent: &AgentState, filter: &str) -> bool {
    agent.worktree_handle.as_deref() == Some(filter)
        || agent.branch.as_deref() == Some(filter)
        || agent.window_name == filter
        || agent.worktree_path.as_deref() == Some(filter)
}

fn current_window_worktree(
    config: &Config,
    tmux: &Tmux,
    context: &crate::tmux::TmuxContext,
) -> Result<WindowWorktree> {
    let handle = context
        .window_name
        .strip_prefix(config.window_prefix())
        .filter(|value| !value.is_empty())
        .unwrap_or(&context.window_name)
        .to_owned();

    if let Ok(path_option) = kmux_worktree_option(&handle, "path")
        && let Some(path) = tmux.show_window_option(&context.pane_id, &path_option)?
    {
        let branch = kmux_worktree_option(&handle, "branch")
            .ok()
            .and_then(|option| {
                tmux.show_window_option(&context.pane_id, &option)
                    .ok()
                    .flatten()
            });
        return Ok(WindowWorktree {
            handle: Some(handle),
            path: Some(PathBuf::from(path)),
            branch,
        });
    }

    let cwd = std::env::current_dir().context("failed to read current directory")?;
    if let Ok(paths) = RepoPaths::discover(&cwd)
        && !same_path(&paths.current_worktree, &paths.main_worktree)
    {
        let branch = Git::new(&paths.current_worktree)
            .current_branch()
            .ok()
            .flatten();
        let handle = paths
            .current_worktree
            .file_name()
            .map(|name| name.to_string_lossy().into_owned());
        return Ok(WindowWorktree {
            handle,
            path: Some(paths.current_worktree),
            branch,
        });
    }

    Ok(WindowWorktree {
        handle: Some(handle),
        path: None,
        branch: None,
    })
}
