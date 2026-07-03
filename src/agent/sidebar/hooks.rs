//! Selection hook payload construction and execution for sidebar rows.
//!
//! Hooks are a generic kmux extension point: they receive a versioned JSON
//! payload about the selected agent row and decide what, if anything, that means
//! for their own integration.

use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde::Serialize;

use crate::agent::sidebar::model::SidebarRow;
use crate::config::SidebarSelectionHookConfig;
use crate::state::{AgentLocationHints, AgentObservationState, AgentStatus, StateStore};

const HOOK_EVENT: &str = "sidebar.select";
static HOOK_STDIN_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Run configured selection hooks that match the selected sidebar row.
pub(super) fn run_selection_hooks(
    hooks: &[SidebarSelectionHookConfig],
    store: &StateStore,
    tmux_instance: &str,
    row: &SidebarRow,
) -> Result<()> {
    if hooks.is_empty() {
        return Ok(());
    }

    let observations = selected_observations(store, row, tmux_instance)?;
    run_hooks_with_observations(hooks, row, &observations)
}

#[derive(Debug, Clone, Serialize)]
struct SelectionHookPayload {
    version: u8,
    event: &'static str,
    agent: SelectionHookAgentPayload,
    status: AgentStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    context: Option<String>,
    target: SelectionHookTargetPayload,
    observations: Vec<SelectionHookObservationPayload>,
}

#[derive(Debug, Clone, Serialize)]
struct SelectionHookAgentPayload {
    kind: String,
    session_id: String,
}

#[derive(Debug, Clone, Default, Serialize)]
struct SelectionHookTargetPayload {
    #[serde(skip_serializing_if = "Option::is_none")]
    tmux_instance: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tmux_session_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tmux_window_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tmux_window_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tmux_pane_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tmux_pane_title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tmux_pane_current_command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tmux_pane_current_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    git_repo_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    git_repo_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    kmux_workspace_slug: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    git_worktree_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    git_branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    directory: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct SelectionHookObservationPayload {
    producer_kind: String,
    producer_instance: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    status: Option<AgentStatus>,
    observed_at: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    context: Option<String>,
    target: SelectionHookTargetPayload,
}

fn selected_observations(
    store: &StateStore,
    row: &SidebarRow,
    tmux_instance: &str,
) -> Result<Vec<AgentObservationState>> {
    let session = &row.selection.key;
    Ok(store
        .list_observations()?
        .into_iter()
        .filter(|observation| {
            observation.key.session == *session
                && observation
                    .target
                    .tmux_instance
                    .as_deref()
                    .is_none_or(|target_instance| target_instance == tmux_instance)
        })
        .collect())
}

fn run_hooks_with_observations(
    hooks: &[SidebarSelectionHookConfig],
    row: &SidebarRow,
    observations: &[AgentObservationState],
) -> Result<()> {
    let payload = payload_for_row(row, observations);
    let payload_json = serde_json::to_vec_pretty(&payload)?;
    let mut failures = Vec::new();

    for hook in hooks
        .iter()
        .filter(|hook| hook_matches_payload(hook, &payload))
    {
        if let Err(error) = run_hook_command(hook, &payload, &payload_json) {
            failures.push(format!("{}: {error}", hook.command));
        }
    }

    if failures.is_empty() {
        return Ok(());
    }
    bail!("selection hook failed: {}", failures.join("; "))
}

fn payload_for_row(
    row: &SidebarRow,
    observations: &[AgentObservationState],
) -> SelectionHookPayload {
    SelectionHookPayload {
        version: 1,
        event: HOOK_EVENT,
        agent: SelectionHookAgentPayload {
            kind: row.selection.key.agent_kind.clone(),
            session_id: row.selection.key.session_id.clone(),
        },
        status: row.selection.status,
        title: row.selection.title.clone(),
        context: row.selection.context.clone(),
        target: SelectionHookTargetPayload::from_hints(&row.selection.target),
        observations: observations
            .iter()
            .map(SelectionHookObservationPayload::from_observation)
            .collect(),
    }
}

fn hook_matches_payload(hook: &SidebarSelectionHookConfig, payload: &SelectionHookPayload) -> bool {
    if hook
        .agent_kind
        .as_deref()
        .is_some_and(|agent_kind| agent_kind != payload.agent.kind)
    {
        return false;
    }

    if let Some(producer_kind) = hook.producer_kind.as_deref() {
        return payload
            .observations
            .iter()
            .any(|observation| observation.producer_kind == producer_kind);
    }

    true
}

fn run_hook_command(
    hook: &SidebarSelectionHookConfig,
    payload: &SelectionHookPayload,
    payload_json: &[u8],
) -> Result<()> {
    let mut command = Command::new("sh");
    let (stdin_file, stdin_path) = payload_stdin_file(payload_json)?;
    command
        .arg("-c")
        .arg(&hook.command)
        .stdin(Stdio::from(stdin_file))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .env("KMUX_HOOK_EVENT", payload.event)
        .env("KMUX_AGENT_KIND", &payload.agent.kind)
        .env("KMUX_AGENT_SESSION_ID", &payload.agent.session_id)
        .env("KMUX_AGENT_STATUS", payload.status.as_str());
    set_optional_env(
        &mut command,
        "KMUX_TMUX_INSTANCE",
        &payload.target.tmux_instance,
    );
    set_optional_env(
        &mut command,
        "KMUX_TMUX_SESSION_NAME",
        &payload.target.tmux_session_name,
    );
    set_optional_env(
        &mut command,
        "KMUX_TMUX_WINDOW_NAME",
        &payload.target.tmux_window_name,
    );
    set_optional_env(
        &mut command,
        "KMUX_TMUX_WINDOW_ID",
        &payload.target.tmux_window_id,
    );
    set_optional_env(
        &mut command,
        "KMUX_TMUX_PANE_ID",
        &payload.target.tmux_pane_id,
    );
    set_optional_env(&mut command, "KMUX_DIRECTORY", &payload.target.directory);
    set_optional_env(
        &mut command,
        "KMUX_GIT_WORKTREE_PATH",
        &payload.target.git_worktree_path,
    );
    set_optional_env(&mut command, "KMUX_GIT_BRANCH", &payload.target.git_branch);
    set_optional_env(
        &mut command,
        "KMUX_WORKSPACE_SLUG",
        &payload.target.kmux_workspace_slug,
    );
    if let Some(current_dir) = hook_current_dir(&payload.target) {
        command.current_dir(current_dir);
    }

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            let _ = fs::remove_file(&stdin_path);
            return Err(error)
                .with_context(|| format!("failed to start selection hook: {}", hook.command));
        }
    };
    let _ = fs::remove_file(stdin_path);
    let timeout = Duration::from_millis(hook.timeout_ms());
    let status = wait_with_timeout(&mut child, timeout)?;
    if status.success() {
        return Ok(());
    }
    bail!("exited with status {status}")
}

fn set_optional_env(command: &mut Command, key: &str, value: &Option<String>) {
    if let Some(value) = value.as_deref() {
        command.env(key, value);
    }
}

fn hook_current_dir(target: &SelectionHookTargetPayload) -> Option<PathBuf> {
    [
        target.git_worktree_path.as_deref(),
        target.directory.as_deref(),
    ]
    .into_iter()
    .flatten()
    .map(Path::new)
    .find(|path| path.is_dir())
    .map(Path::to_path_buf)
}

fn payload_stdin_file(payload_json: &[u8]) -> Result<(File, PathBuf)> {
    for _ in 0..16 {
        let counter = HOOK_STDIN_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "kmux-sidebar-hook-{}-{counter}.json",
            std::process::id()
        ));

        match open_payload_file(&path) {
            Ok(mut file) => {
                if let Err(error) = write_payload_file(&mut file, payload_json) {
                    let _ = fs::remove_file(&path);
                    return Err(error);
                }
                return Ok((file, path));
            }
            Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "failed to create selection hook stdin file at {}",
                        path.display()
                    )
                });
            }
        }
    }

    bail!("failed to create unique selection hook stdin file")
}

fn open_payload_file(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

fn write_payload_file(file: &mut File, payload_json: &[u8]) -> Result<()> {
    match file.write_all(payload_json) {
        Ok(()) => {
            file.seek(SeekFrom::Start(0))?;
            Ok(())
        }
        Err(error) if error.kind() == ErrorKind::BrokenPipe => Ok(()),
        Err(error) => Err(error).context("failed to write selection hook payload"),
    }
}

fn wait_with_timeout(child: &mut Child, timeout: Duration) -> Result<ExitStatus> {
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        if started.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            bail!("timed out after {}ms", timeout.as_millis());
        }
        thread::sleep(Duration::from_millis(10));
    }
}

impl SelectionHookTargetPayload {
    fn from_hints(target: &AgentLocationHints) -> Self {
        Self {
            tmux_instance: target.tmux_instance.clone(),
            tmux_session_name: target.tmux_session_name.clone(),
            tmux_window_name: target.tmux_window_name.clone(),
            tmux_window_id: target.tmux_window_id.clone(),
            tmux_pane_id: target.tmux_pane_id.clone(),
            tmux_pane_title: target.tmux_pane_title.clone(),
            tmux_pane_current_command: target.tmux_pane_current_command.clone(),
            tmux_pane_current_path: target.tmux_pane_current_path.clone(),
            git_repo_name: target.git_repo_name.clone(),
            git_repo_path: target.git_repo_path.clone(),
            kmux_workspace_slug: target.kmux_workspace_slug.clone(),
            git_worktree_path: target.git_worktree_path.clone(),
            git_branch: target.git_branch.clone(),
            directory: target.directory.clone(),
        }
    }
}

impl SelectionHookObservationPayload {
    fn from_observation(observation: &AgentObservationState) -> Self {
        Self {
            producer_kind: observation.key.producer_kind.clone(),
            producer_instance: observation.key.producer_instance.clone(),
            status: observation.status,
            observed_at: observation.observed_at,
            title: observation.title.clone(),
            context: observation.context.clone(),
            target: SelectionHookTargetPayload::from_hints(&observation.target),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::{Duration, Instant};

    use serde_json::Value;
    use tempfile::tempdir;

    use super::*;
    use crate::agent::sidebar::test_support::{report_state, row_from_view};
    use crate::state::{AgentObservationKey, AgentSessionKey};

    #[test]
    fn payload_serializes_selected_row_and_observations() {
        let mut view = report_state(AgentStatus::Waiting, 100, "@1", "%1");
        view.title = Some("Implement hooks".to_owned());
        view.context = Some("55.2K".to_owned());
        view.target.directory = Some("/repo/worktree".to_owned());
        let row = row_from_view(&view, 100);
        let observation = observation_for_row(&row, "server", "http://127.0.0.1:4096");

        let payload = payload_for_row(&row, &[observation]);
        let json = serde_json::to_value(payload).expect("payload should serialize");

        assert_eq!(json["version"], 1);
        assert_eq!(json["event"], HOOK_EVENT);
        assert_eq!(json["agent"]["kind"], "opencode");
        assert_eq!(json["agent"]["session_id"], "ses_%1");
        assert_eq!(json["status"], "waiting");
        assert_eq!(json["target"]["tmux_window_id"], "@1");
        assert_eq!(json["target"]["directory"], "/repo/worktree");
        assert_eq!(json["observations"][0]["producer_kind"], "server");
        assert_eq!(
            json["observations"][0]["producer_instance"],
            "http://127.0.0.1:4096"
        );
    }

    #[test]
    fn hook_filters_by_agent_kind_and_producer_kind() {
        let row = row_from_view(&report_state(AgentStatus::Working, 100, "@1", "%1"), 100);
        let observation = observation_for_row(&row, "server", "http://127.0.0.1:4096");
        let payload = payload_for_row(&row, &[observation]);

        assert!(hook_matches_payload(
            &hook_config("true", Some("opencode"), Some("server"), None),
            &payload
        ));
        assert!(!hook_matches_payload(
            &hook_config("true", Some("codex"), Some("server"), None),
            &payload
        ));
        assert!(!hook_matches_payload(
            &hook_config("true", Some("opencode"), Some("tui"), None),
            &payload
        ));
    }

    #[test]
    fn agent_kind_only_hook_matches_without_observations() {
        let row = row_from_view(&report_state(AgentStatus::Working, 100, "@1", "%1"), 100);
        let payload = payload_for_row(&row, &[]);

        assert!(hook_matches_payload(
            &hook_config("true", Some("opencode"), None, None),
            &payload
        ));
    }

    #[test]
    fn matching_hook_receives_payload_env_and_selected_cwd() -> Result<()> {
        let dir = tempdir()?;
        let mut view = report_state(AgentStatus::Working, 100, "@1", "%1");
        view.target.git_worktree_path = Some(dir.path().display().to_string());
        let row = row_from_view(&view, 100);
        let payload_path = dir.path().join("payload.json");
        let session_path = dir.path().join("session.txt");
        let cwd_path = dir.path().join("cwd.txt");
        let command = format!(
            "cat > '{}'; printf '%s' \"$KMUX_AGENT_SESSION_ID\" > '{}'; pwd > '{}'",
            payload_path.display(),
            session_path.display(),
            cwd_path.display()
        );

        run_hooks_with_observations(
            &[hook_config(&command, Some("opencode"), None, Some(1000))],
            &row,
            &[],
        )?;

        let payload: Value = serde_json::from_str(&fs::read_to_string(payload_path)?)?;
        assert_eq!(payload["agent"]["session_id"], "ses_%1");
        assert_eq!(fs::read_to_string(session_path)?, "ses_%1");
        assert_eq!(
            fs::read_to_string(cwd_path)?.trim(),
            dir.path().display().to_string()
        );
        Ok(())
    }

    #[test]
    fn non_matching_hook_is_not_run() -> Result<()> {
        let dir = tempdir()?;
        let marker = dir.path().join("marker");
        let row = row_from_view(&report_state(AgentStatus::Working, 100, "@1", "%1"), 100);
        let command = format!("touch '{}'", marker.display());

        run_hooks_with_observations(
            &[hook_config(&command, Some("codex"), None, Some(1000))],
            &row,
            &[],
        )?;

        assert!(!marker.exists());
        Ok(())
    }

    #[test]
    fn hook_timeout_is_reported() {
        let row = row_from_view(&report_state(AgentStatus::Working, 100, "@1", "%1"), 100);

        let error = run_hooks_with_observations(
            &[hook_config("sleep 1", Some("opencode"), None, Some(10))],
            &row,
            &[],
        )
        .expect_err("timeout should fail");

        assert!(error.to_string().contains("timed out"));
    }

    #[test]
    fn hook_timeout_covers_commands_that_ignore_large_stdin() {
        let row = row_from_view(&report_state(AgentStatus::Working, 100, "@1", "%1"), 100);
        let mut observation = observation_for_row(&row, "server", "http://127.0.0.1:4096");
        observation.title = Some("x".repeat(1024 * 1024));
        let started = Instant::now();

        let error = run_hooks_with_observations(
            &[hook_config("sleep 1", Some("opencode"), None, Some(25))],
            &row,
            &[observation],
        )
        .expect_err("timeout should fail before large stdin can block");

        assert!(error.to_string().contains("timed out"));
        assert!(started.elapsed() < Duration::from_millis(500));
    }

    fn hook_config(
        command: &str,
        agent_kind: Option<&str>,
        producer_kind: Option<&str>,
        timeout_ms: Option<u64>,
    ) -> SidebarSelectionHookConfig {
        SidebarSelectionHookConfig {
            command: command.to_owned(),
            agent_kind: agent_kind.map(str::to_owned),
            producer_kind: producer_kind.map(str::to_owned),
            timeout_ms,
        }
    }

    fn observation_for_row(
        row: &SidebarRow,
        producer_kind: &str,
        producer_instance: &str,
    ) -> AgentObservationState {
        AgentObservationState {
            key: AgentObservationKey {
                session: AgentSessionKey {
                    agent_kind: row.selection.key.agent_kind.clone(),
                    session_id: row.selection.key.session_id.clone(),
                },
                producer_kind: producer_kind.to_owned(),
                producer_instance: producer_instance.to_owned(),
            },
            created_at: 100,
            status: Some(row.selection.status),
            status_observed_at: Some(100),
            status_changed_at: Some(100),
            working_elapsed_secs: 0,
            observed_at: 100,
            title: row.selection.title.clone(),
            context: row.selection.context.clone(),
            target: row.selection.target.clone(),
        }
    }
}
