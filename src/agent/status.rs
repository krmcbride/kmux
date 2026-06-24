use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::Serialize;

use crate::agent::active::active_agents;
use crate::cli;
use crate::config::Config;
use crate::git::{Git, WorktreeInfo};
use crate::paths::{RepoPaths, same_path};
use crate::state::{
    AgentState, AgentStatus as StoredAgentStatus, PaneKey, StateStore, now_unix_seconds,
};
use crate::tmux::{Tmux, kmux_worktree_option};

const KMUX_STATUS_OPTION: &str = "@kmux_status";

#[derive(Debug)]
struct WindowWorktree {
    handle: Option<String>,
    path: Option<PathBuf>,
    branch: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct GitInfo {
    has_staged: bool,
    has_unstaged: bool,
    has_unmerged_commits: bool,
}

#[derive(Debug, Serialize)]
struct StatusEntry {
    worktree: String,
    branch: String,
    status: String,
    icon: String,
    elapsed_secs: u64,
    title: Option<String>,
    pane_id: String,
    worktree_handle: Option<String>,
    worktree_path: Option<String>,
    session_name: String,
    window_name: String,
    window_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    git: Option<GitInfo>,
}

struct DisplayRow {
    worktree: String,
    status: String,
    elapsed: String,
    git: String,
    title: String,
}

pub fn run(args: cli::StatusArgs) -> Result<()> {
    let store = StateStore::new()?;
    let tmux = Tmux::from_env();
    let agents = active_agents(&store, &tmux)?;
    let entries = status_entries(&agents, &args)?;

    if args.json {
        println!("{}", serde_json::to_string_pretty(&entries)?);
    } else if entries.is_empty() {
        println!("No active agents");
    } else {
        print_table(&entries, args.git);
    }
    Ok(())
}

pub fn set_window_status(status: cli::AgentStatus) -> Result<()> {
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

    let details = tmux.pane_details(&context.pane_id).ok();
    let worktree = current_window_worktree(&config, &tmux, &context)?;
    let state = AgentState {
        pane_key: key,
        status,
        icon: icon.to_owned(),
        updated_at: now_unix_seconds(),
        pane_title: details.as_ref().and_then(|details| details.title.clone()),
        pane_current_command: details.and_then(|details| details.current_command),
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

fn status_entries(agents: &[AgentState], args: &cli::StatusArgs) -> Result<Vec<StatusEntry>> {
    let now = unix_now();
    if !args.filters.is_empty() {
        return Ok(agents
            .iter()
            .filter(|agent| {
                args.filters
                    .iter()
                    .any(|filter| agent_matches_filter(agent, filter))
            })
            .map(|agent| entry_for_agent(agent, None, now, args.git))
            .collect());
    }

    if let Some(entries) = current_repo_entries(agents, now, args.git)? {
        return Ok(entries);
    }

    Ok(agents
        .iter()
        .map(|agent| entry_for_agent(agent, None, now, args.git))
        .collect())
}

fn current_repo_entries(
    agents: &[AgentState],
    now: u64,
    show_git: bool,
) -> Result<Option<Vec<StatusEntry>>> {
    let cwd = std::env::current_dir().context("failed to read current directory")?;
    let Ok(paths) = RepoPaths::discover(&cwd) else {
        return Ok(None);
    };
    let git = Git::new(&paths.main_worktree);
    let worktrees = git.worktrees()?;

    let mut entries = Vec::new();
    for worktree in &worktrees {
        for agent in agents
            .iter()
            .filter(|agent| agent_matches_worktree(agent, worktree))
        {
            entries.push(entry_for_agent(agent, Some(worktree), now, show_git));
        }
    }
    Ok(Some(entries))
}

fn entry_for_agent(
    agent: &AgentState,
    worktree: Option<&WorktreeInfo>,
    now: u64,
    show_git: bool,
) -> StatusEntry {
    let worktree_path = worktree
        .map(|worktree| worktree.path.display().to_string())
        .or_else(|| agent.worktree_path.clone());
    let handle = worktree
        .and_then(|worktree| worktree.path.file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .or_else(|| agent.worktree_handle.clone());
    let branch = worktree
        .and_then(|worktree| worktree.branch.clone())
        .or_else(|| agent.branch.clone())
        .unwrap_or_else(|| "-".to_owned());
    let worktree_name = handle.clone().unwrap_or_else(|| agent.window_name.clone());
    let git = if show_git {
        worktree_path
            .as_deref()
            .map(Path::new)
            .map(|path| compute_git_info(path, &branch))
    } else {
        None
    };

    StatusEntry {
        worktree: worktree_name,
        branch,
        status: agent.status.as_str().to_owned(),
        icon: agent.icon.clone(),
        elapsed_secs: now.saturating_sub(agent.updated_at),
        title: agent.pane_title.clone(),
        pane_id: agent.pane_key.pane_id.clone(),
        worktree_handle: handle,
        worktree_path,
        session_name: agent.session_name.clone(),
        window_name: agent.window_name.clone(),
        window_id: agent.window_id.clone(),
        git,
    }
}

fn agent_matches_filter(agent: &AgentState, filter: &str) -> bool {
    agent.worktree_handle.as_deref() == Some(filter)
        || agent.branch.as_deref() == Some(filter)
        || agent.window_name == filter
        || agent.worktree_path.as_deref() == Some(filter)
}

fn agent_matches_worktree(agent: &AgentState, worktree: &WorktreeInfo) -> bool {
    let agent_path = agent.worktree_path.as_deref().map(Path::new);
    if let Some(agent_path) = agent_path
        && same_path(agent_path, &worktree.path)
    {
        return true;
    }

    let handle = worktree.path.file_name().map(|name| name.to_string_lossy());
    let branch_matches = agent.branch.as_deref() == worktree.branch.as_deref();
    let handle_matches = handle
        .as_deref()
        .is_some_and(|handle| agent.worktree_handle.as_deref() == Some(handle));

    agent_path.is_none() && branch_matches && handle_matches
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
    if let Ok(paths) = RepoPaths::discover(&cwd) {
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

fn compute_git_info(path: &Path, branch: &str) -> GitInfo {
    let git = Git::new(path);
    GitInfo {
        has_staged: git.has_staged_changes().unwrap_or(false),
        has_unstaged: git.has_unstaged_changes().unwrap_or(false),
        has_unmerged_commits: if branch == "-" {
            false
        } else {
            git.branch_is_safely_deletable(branch)
                .map(|safe| !safe)
                .unwrap_or(false)
        },
    }
}

fn print_table(entries: &[StatusEntry], show_git: bool) {
    let rows = entries
        .iter()
        .map(|entry| DisplayRow {
            worktree: format_worktree(entry),
            status: entry.status.clone(),
            elapsed: compact_elapsed(entry.elapsed_secs),
            git: git_label(&entry.git),
            title: entry.title.clone().unwrap_or_else(|| "-".to_owned()),
        })
        .collect::<Vec<_>>();
    let headers = if show_git {
        vec!["WORKTREE", "STATUS", "ELAPSED", "GIT", "TITLE"]
    } else {
        vec!["WORKTREE", "STATUS", "ELAPSED", "TITLE"]
    };
    let mut widths = headers
        .iter()
        .map(|header| header.len())
        .collect::<Vec<_>>();

    for row in &rows {
        let values = row_values(row, show_git);
        for (index, value) in values.iter().enumerate() {
            widths[index] = widths[index].max(value.chars().count());
        }
    }

    println!("{}", format_row(&headers, &widths));
    for row in &rows {
        println!("{}", format_row(&row_values(row, show_git), &widths));
    }
}

fn format_worktree(entry: &StatusEntry) -> String {
    if entry.branch != "-" && entry.branch != entry.worktree {
        format!("{} ({})", entry.worktree, entry.branch)
    } else {
        entry.worktree.clone()
    }
}

fn git_label(git: &Option<GitInfo>) -> String {
    let Some(git) = git else {
        return "-".to_owned();
    };
    let mut parts = Vec::new();
    if git.has_staged {
        parts.push("staged");
    }
    if git.has_unstaged {
        parts.push("unstaged");
    }
    if git.has_unmerged_commits {
        parts.push("unmerged");
    }
    if parts.is_empty() {
        "clean".to_owned()
    } else {
        parts.join(",")
    }
}

fn row_values(row: &DisplayRow, show_git: bool) -> Vec<&str> {
    if show_git {
        vec![
            &row.worktree,
            &row.status,
            &row.elapsed,
            &row.git,
            &row.title,
        ]
    } else {
        vec![&row.worktree, &row.status, &row.elapsed, &row.title]
    }
}

fn format_row(values: &[&str], widths: &[usize]) -> String {
    values
        .iter()
        .enumerate()
        .map(|(index, value)| format!("{value:<width$}", width = widths[index]))
        .collect::<Vec<_>>()
        .join("  ")
        .trim_end()
        .to_owned()
}

fn compact_elapsed(seconds: u64) -> String {
    if seconds < 60 {
        "<1m".to_owned()
    } else if seconds < 60 * 60 {
        format!("{}m", seconds / 60)
    } else if seconds < 60 * 60 * 24 {
        let hours = seconds / (60 * 60);
        let minutes = (seconds % (60 * 60)) / 60;
        if minutes == 0 {
            format!("{hours}h")
        } else {
            format!("{hours}h {minutes}m")
        }
    } else {
        format!("{}d", seconds / (60 * 60 * 24))
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}
