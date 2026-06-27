use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::Serialize;

use crate::agent::active::active_reports;
use crate::cli;
use crate::config::{Config, StatusIcons};
use crate::git::{Git, WorktreeInfo};
use crate::paths::{RepoPaths, same_path};
use crate::state::{
    AgentReportKey, AgentReportState, AgentStatus as StoredAgentStatus, AgentTargetHints,
    StateStore, next_report_timing, now_unix_seconds,
};
use crate::tmux::{
    KMUX_WORKTREE_BRANCH_OPTION, KMUX_WORKTREE_HANDLE_OPTION, KMUX_WORKTREE_PATH_OPTION, Tmux,
    kmux_worktree_option,
};

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
    context: Option<String>,
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
    let config = Config::load()?;
    let reports = active_reports(&store, &tmux)?;
    let entries = status_entries(&reports, &args, &config.status_icons)?;

    if args.json {
        println!("{}", serde_json::to_string_pretty(&entries)?);
    } else if entries.is_empty() {
        println!("No active agents");
    } else {
        print_table(&entries, args.git);
    }
    Ok(())
}

pub fn set_window_status(args: cli::SetWindowStatusArgs) -> Result<()> {
    if std::env::var_os("KMUX_DISABLE_SET_WINDOW_STATUS").is_some() {
        return Ok(());
    }

    let config = Config::load()?;
    let tmux = Tmux::from_env();
    let store = StateStore::new()?;
    let explicit_identity = has_explicit_identity(&args);
    let context = if explicit_identity {
        None
    } else {
        tmux.current_context()?
    };
    let Some(key) = report_key(&args, &tmux, context.as_ref(), explicit_identity)? else {
        return Ok(());
    };

    if args.status == cli::AgentStatus::Clear {
        if !explicit_identity && let Some(context) = context.as_ref() {
            tmux.unset_window_option(&context.pane_id, KMUX_STATUS_OPTION)?;
        }
        store.delete_report(&key)?;
        return Ok(());
    }

    let (status, icon) = match args.status {
        cli::AgentStatus::Working => (StoredAgentStatus::Working, config.status_icons.working()),
        cli::AgentStatus::Waiting => (StoredAgentStatus::Waiting, config.status_icons.waiting()),
        cli::AgentStatus::Done => (StoredAgentStatus::Done, config.status_icons.done()),
        cli::AgentStatus::Clear => return Ok(()),
    };
    if !explicit_identity && let Some(context) = context.as_ref() {
        tmux.set_window_option(&context.pane_id, KMUX_STATUS_OPTION, icon)?;
    }

    let now = now_unix_seconds();
    let target = report_target(&args, &config, &tmux, context.as_ref(), explicit_identity)?;
    let previous = store.get_report(&key)?;
    let session_id = clean_optional(args.session_id);
    let timing = next_report_timing(previous.as_ref(), &key, session_id.as_deref(), status, now);
    let state = AgentReportState {
        key,
        session_id,
        status,
        status_changed_at: timing.status_changed_at,
        working_elapsed_secs: timing.working_elapsed_secs,
        observed_at: now,
        title: clean_optional(args.title),
        context: clean_optional(args.context),
        target,
    };
    store.upsert_report(&state)?;
    Ok(())
}

fn has_explicit_identity(args: &cli::SetWindowStatusArgs) -> bool {
    args.source.is_some() || args.source_instance.is_some()
}

fn report_key(
    args: &cli::SetWindowStatusArgs,
    tmux: &Tmux,
    context: Option<&crate::tmux::TmuxContext>,
    explicit_identity: bool,
) -> Result<Option<AgentReportKey>> {
    if !explicit_identity {
        return Ok(context.map(|context| {
            AgentReportKey::tmux_pane(tmux.instance_id(), context.pane_id.clone())
        }));
    }

    let source = clean_optional_ref(args.source.as_ref())
        .ok_or_else(|| anyhow::anyhow!("--source is required for explicit reports"))?;
    let instance =
        clean_optional_ref(args.source_instance.as_ref()).unwrap_or_else(|| "default".to_owned());
    let id = clean_optional_ref(args.session_id.as_ref())
        .or_else(|| clean_optional_ref(args.pane_id.as_ref()))
        .ok_or_else(|| {
            anyhow::anyhow!("--session-id or --pane-id is required for explicit reports")
        })?;

    Ok(Some(AgentReportKey::new(source, instance, id)))
}

fn report_target(
    args: &cli::SetWindowStatusArgs,
    config: &Config,
    tmux: &Tmux,
    context: Option<&crate::tmux::TmuxContext>,
    explicit_identity: bool,
) -> Result<AgentTargetHints> {
    let inferred_context = (!explicit_identity).then_some(context).flatten();
    let details = inferred_context.and_then(|context| tmux.pane_details(&context.pane_id).ok());
    let worktree = if let Some(context) = inferred_context {
        Some(current_window_worktree(config, tmux, context)?)
    } else {
        None
    };

    Ok(AgentTargetHints {
        tmux_instance: clean_optional_ref(args.tmux_instance.as_ref())
            .or_else(|| inferred_context.map(|_| tmux.instance_id())),
        pane_id: clean_optional_ref(args.pane_id.as_ref())
            .or_else(|| inferred_context.map(|context| context.pane_id.clone())),
        window_id: clean_optional_ref(args.window_id.as_ref())
            .or_else(|| inferred_context.map(|context| context.window_id.clone())),
        session_name: clean_optional_ref(args.session_name.as_ref())
            .or_else(|| inferred_context.map(|context| context.session_name.clone())),
        window_name: clean_optional_ref(args.window_name.as_ref())
            .or_else(|| inferred_context.map(|context| context.window_name.clone())),
        pane_title: details.as_ref().and_then(|details| details.title.clone()),
        pane_current_command: details.and_then(|details| details.current_command),
        worktree_handle: clean_optional_ref(args.worktree_handle.as_ref()).or_else(|| {
            worktree
                .as_ref()
                .and_then(|worktree| worktree.handle.clone())
        }),
        worktree_path: clean_optional_ref(args.worktree_path.as_ref()).or_else(|| {
            worktree
                .as_ref()
                .and_then(|worktree| worktree.path.as_ref())
                .map(|path| path.display().to_string())
        }),
        branch: clean_optional_ref(args.branch.as_ref()).or_else(|| {
            worktree
                .as_ref()
                .and_then(|worktree| worktree.branch.clone())
        }),
        directory: clean_optional_ref(args.directory.as_ref()),
    })
}

fn clean_optional(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let value = value.trim();
        (!value.is_empty()).then(|| value.to_owned())
    })
}

fn clean_optional_ref(value: Option<&String>) -> Option<String> {
    value.and_then(|value| {
        let value = value.trim();
        (!value.is_empty()).then(|| value.to_owned())
    })
}

fn status_entries(
    reports: &[AgentReportState],
    args: &cli::StatusArgs,
    icons: &StatusIcons,
) -> Result<Vec<StatusEntry>> {
    let now = unix_now();
    if !args.filters.is_empty() {
        return Ok(reports
            .iter()
            .filter(|report| {
                args.filters
                    .iter()
                    .any(|filter| report_matches_filter(report, filter))
            })
            .map(|report| entry_for_report(report, None, now, args.git, icons))
            .collect());
    }

    if let Some(entries) = current_repo_entries(reports, now, args.git, icons)? {
        return Ok(entries);
    }

    Ok(reports
        .iter()
        .map(|report| entry_for_report(report, None, now, args.git, icons))
        .collect())
}

fn current_repo_entries(
    reports: &[AgentReportState],
    now: u64,
    show_git: bool,
    icons: &StatusIcons,
) -> Result<Option<Vec<StatusEntry>>> {
    let cwd = std::env::current_dir().context("failed to read current directory")?;
    let Ok(paths) = RepoPaths::discover(&cwd) else {
        return Ok(None);
    };
    let git = Git::new(&paths.main_worktree);
    let worktrees = git.worktrees()?;

    let mut entries = Vec::new();
    for worktree in &worktrees {
        for report in reports
            .iter()
            .filter(|report| report_matches_worktree(report, worktree))
        {
            entries.push(entry_for_report(
                report,
                Some(worktree),
                now,
                show_git,
                icons,
            ));
        }
    }
    Ok(Some(entries))
}

fn entry_for_report(
    report: &AgentReportState,
    worktree: Option<&WorktreeInfo>,
    now: u64,
    show_git: bool,
    icons: &StatusIcons,
) -> StatusEntry {
    let worktree_path = worktree
        .map(|worktree| worktree.path.display().to_string())
        .or_else(|| report.target.worktree_path.clone());
    let handle = worktree
        .and_then(|worktree| worktree.path.file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .or_else(|| report.target.worktree_handle.clone());
    let branch = worktree
        .and_then(|worktree| worktree.branch.clone())
        .or_else(|| report.target.branch.clone())
        .unwrap_or_else(|| "-".to_owned());
    let worktree_name = handle
        .clone()
        .or_else(|| report.target.window_name.clone())
        .unwrap_or_else(|| report.key.id.clone());
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
        status: report.status.as_str().to_owned(),
        icon: status_icon(report.status, icons).to_owned(),
        elapsed_secs: report.elapsed_secs(now),
        title: report
            .title
            .clone()
            .or_else(|| report.target.pane_title.clone()),
        context: report.context.clone(),
        pane_id: report.target.pane_id.clone().unwrap_or_default(),
        worktree_handle: handle,
        worktree_path,
        session_name: report.target.session_name.clone().unwrap_or_default(),
        window_name: report.target.window_name.clone().unwrap_or_default(),
        window_id: report.target.window_id.clone().unwrap_or_default(),
        git,
    }
}

fn report_matches_filter(report: &AgentReportState, filter: &str) -> bool {
    report.target.worktree_handle.as_deref() == Some(filter)
        || report.target.branch.as_deref() == Some(filter)
        || report.target.window_name.as_deref() == Some(filter)
        || report.target.worktree_path.as_deref() == Some(filter)
        || report.target.directory.as_deref() == Some(filter)
        || report.key.id == filter
}

fn report_matches_worktree(report: &AgentReportState, worktree: &WorktreeInfo) -> bool {
    let report_path = report.target.worktree_path.as_deref().map(Path::new);
    if let Some(report_path) = report_path
        && same_path(report_path, &worktree.path)
    {
        return true;
    }

    let handle = worktree.path.file_name().map(|name| name.to_string_lossy());
    let branch_matches = report.target.branch.as_deref() == worktree.branch.as_deref();
    let handle_matches = handle
        .as_deref()
        .is_some_and(|handle| report.target.worktree_handle.as_deref() == Some(handle));

    report_path.is_none() && branch_matches && handle_matches
}

fn status_icon(status: StoredAgentStatus, icons: &StatusIcons) -> &str {
    match status {
        StoredAgentStatus::Working => icons.working(),
        StoredAgentStatus::Waiting => icons.waiting(),
        StoredAgentStatus::Done => icons.done(),
    }
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

    if let Some(path) = tmux.show_window_option(&context.pane_id, KMUX_WORKTREE_PATH_OPTION)? {
        let branch = tmux.show_window_option(&context.pane_id, KMUX_WORKTREE_BRANCH_OPTION)?;
        let stable_handle = tmux
            .show_window_option(&context.pane_id, KMUX_WORKTREE_HANDLE_OPTION)?
            .unwrap_or_else(|| handle.clone());
        return Ok(WindowWorktree {
            handle: Some(stable_handle),
            path: Some(PathBuf::from(path)),
            branch,
        });
    }

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
