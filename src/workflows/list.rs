use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;

use crate::agent::query::{WorkspaceMatchMode, WorkspaceTarget, view_matches_workspace};
use crate::agent::sessions::{AgentSessionView, session_views};
use crate::cli;
use crate::config::StatusIcons;
use crate::state::{AgentStatus, StateStore};
use crate::tmux::Tmux;

use super::context::load_repo_context;
use super::resolve::{WorkspaceListItem, list_items};
use crate::paths::same_path;

/// Print workspace inventory, optionally as JSON for machine consumers.
pub(super) fn run(args: cli::JsonArgs) -> Result<()> {
    let repo = load_repo_context()?;
    let items = list_items(&repo)?;

    if args.json {
        println!("{}", serde_json::to_string_pretty(&items)?);
        return Ok(());
    }

    let tmux = Tmux::from_env();
    let agents = StateStore::new()
        .ok()
        .and_then(|store| session_views(&store, &tmux).ok())
        .unwrap_or_default();
    let tmux_session = tmux
        .current_context()
        .ok()
        .flatten()
        .map(|context| context.session_name);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs());
    let current_dir = std::env::current_dir()?;

    let rows = items
        .iter()
        .enumerate()
        .map(|(index, item)| DisplayRow {
            branch: format_branch(&items, index),
            parent: item.git_parent_branch.as_deref().unwrap_or("-").to_owned(),
            age: format_age(item, now),
            agent: format_agent(item, &agents, &repo.config.status_icons),
            mux: format_mux(item, &repo.config, &tmux, tmux_session.as_deref()),
            unmerged: format_unmerged(item, &repo.git),
            path: format_path(Path::new(&item.git_worktree_path), &current_dir),
        })
        .collect::<Vec<_>>();

    print_table(&rows);
    Ok(())
}

struct DisplayRow {
    branch: String,
    parent: String,
    age: String,
    agent: String,
    mux: String,
    unmerged: String,
    path: String,
}

// Render a depth-first forest using standard tree connectors while keeping
// parent labels in their own column for scanability.
fn format_branch(items: &[WorkspaceListItem], index: usize) -> String {
    let item = &items[index];
    let branch = item.git_branch.as_deref().unwrap_or("-");
    if item.tree_depth == 0 {
        return branch.to_owned();
    }

    let mut prefix = String::new();
    for depth in 1..item.tree_depth {
        if has_following_at_depth(items, index, depth) {
            prefix.push_str("│   ");
        } else {
            prefix.push_str("    ");
        }
    }
    if has_following_at_depth(items, index, item.tree_depth) {
        prefix.push_str("├── ");
    } else {
        prefix.push_str("└── ");
    }
    format!("{prefix}{branch}")
}

fn has_following_at_depth(items: &[WorkspaceListItem], index: usize, depth: usize) -> bool {
    items[index + 1..]
        .iter()
        .take_while(|item| item.tree_depth >= depth)
        .any(|item| item.tree_depth == depth)
}

// Main worktree age is intentionally omitted because the column describes kmux workspaces.
fn format_age(item: &WorkspaceListItem, now: u64) -> String {
    if item.is_main {
        return "-".to_owned();
    }

    item.created_at
        .map(|created_at| compact_age(now.saturating_sub(created_at)))
        .unwrap_or_else(|| "-".to_owned())
}

fn compact_age(seconds: u64) -> String {
    if seconds < 60 {
        "<1m".to_owned()
    } else if seconds < 60 * 60 {
        format!("{}m", seconds / 60)
    } else if seconds < 60 * 60 * 24 {
        format!("{}h", seconds / (60 * 60))
    } else if seconds < 60 * 60 * 24 * 7 {
        format!("{}d", seconds / (60 * 60 * 24))
    } else {
        format!("{}w", seconds / (60 * 60 * 24 * 7))
    }
}

// Summarize all agent sessions that provide any hint for this workspace.
fn format_agent(
    item: &WorkspaceListItem,
    agents: &[AgentSessionView],
    icons: &StatusIcons,
) -> String {
    let target = workspace_target(item);
    let matching = agents
        .iter()
        .filter(|agent| view_matches_workspace(agent, &target, WorkspaceMatchMode::AnyHint))
        .collect::<Vec<_>>();
    if matching.is_empty() {
        return "-".to_owned();
    }
    if matching.len() == 1 {
        return status_icon(matching[0].status, icons).to_owned();
    }

    let mut counts = BTreeMap::new();
    for agent in matching {
        *counts
            .entry(status_icon(agent.status, icons).to_owned())
            .or_insert(0usize) += 1;
    }

    counts
        .into_iter()
        .map(|(icon, count)| format!("{count}{icon}"))
        .collect::<Vec<_>>()
        .join(" ")
}

// Match agent observations against the full workspace identity that list rows know.
fn workspace_target(item: &WorkspaceListItem) -> WorkspaceTarget<'_> {
    WorkspaceTarget::new(
        Some(item.workspace_slug.clone()),
        item.git_branch.clone(),
        Path::new(&item.git_worktree_path),
    )
}

fn status_icon(status: AgentStatus, icons: &StatusIcons) -> &str {
    match status {
        AgentStatus::Working => icons.working(),
        AgentStatus::Waiting => icons.waiting(),
        AgentStatus::Done => icons.done(),
    }
}

// tmux status is best-effort list decoration and should not fail inventory output.
fn format_mux(
    item: &WorkspaceListItem,
    config: &crate::config::Config,
    tmux: &Tmux,
    session_name: Option<&str>,
) -> String {
    let Some(session_name) = session_name else {
        return "-".to_owned();
    };
    if item.is_main {
        return "-".to_owned();
    }

    let window_name = config.workspace_window_name(&item.workspace_slug);
    match tmux.window_exists_by_name(session_name, &window_name) {
        Ok(true) => "yes".to_owned(),
        Ok(false) | Err(_) => "-".to_owned(),
    }
}

// Show whether removal would fail the safe-delete check, but keep list resilient
// when Git cannot answer for a transient reason.
fn format_unmerged(item: &WorkspaceListItem, git: &crate::git::Git) -> String {
    if item.is_main {
        return "-".to_owned();
    }

    let Some(branch) = item.git_branch.as_deref() else {
        return "-".to_owned();
    };

    match git.branch_is_safely_deletable(branch) {
        Ok(true) => "-".to_owned(),
        Ok(false) => "yes".to_owned(),
        Err(_) => "-".to_owned(),
    }
}

// Prefer relative paths near the current directory while preserving exact paths for distant repos.
fn format_path(path: &Path, current_dir: &Path) -> String {
    if same_path(path, current_dir) {
        return "(here)".to_owned();
    }
    if let Ok(relative) = path.strip_prefix(current_dir)
        && !relative.as_os_str().is_empty()
    {
        return relative.display().to_string();
    }
    if let Some(parent) = current_dir.parent()
        && let Ok(relative) = path.strip_prefix(parent)
    {
        return PathBuf::from("..").join(relative).display().to_string();
    }
    path.display().to_string()
}

// Width calculations use chars rather than bytes because status icons may be multibyte.
fn print_table(rows: &[DisplayRow]) {
    let headers = [
        "BRANCH", "PARENT", "AGE", "AGENT", "MUX", "UNMERGED", "PATH",
    ];
    let mut widths = headers.map(str::len);

    for row in rows {
        let values = row_values(row);
        for (index, value) in values.iter().enumerate() {
            widths[index] = widths[index].max(value.chars().count());
        }
    }

    println!("{}", format_row(&headers, &widths));
    for row in rows {
        println!("{}", format_row(&row_values(row), &widths));
    }
}

fn row_values(row: &DisplayRow) -> [&str; 7] {
    [
        &row.branch,
        &row.parent,
        &row.age,
        &row.agent,
        &row.mux,
        &row.unmerged,
        &row.path,
    ]
}

fn format_row(values: &[&str; 7], widths: &[usize; 7]) -> String {
    values
        .iter()
        .enumerate()
        .map(|(index, value)| format!("{value:<width$}", width = widths[index]))
        .collect::<Vec<_>>()
        .join("  ")
        .trim_end()
        .to_owned()
}
