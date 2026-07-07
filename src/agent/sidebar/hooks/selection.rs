//! `sidebar.select` hook payload construction and orchestration.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use serde::Serialize;

use super::diagnostics::{self, HookAttemptLog};
use super::runner::{self, HookCommand, HookCommandOutcome};
use crate::config::SidebarSelectionHookConfig;
use crate::state::{AgentLocationHints, AgentObservationState, AgentSessionKey, AgentStatus};

const HOOK_EVENT: &str = "sidebar.select";

/// Resolved selected-session data used to build the external hook payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::agent::sidebar) struct SelectionHookInput {
    key: AgentSessionKey,
    status: AgentStatus,
    title: Option<String>,
    context: Option<String>,
    metadata: BTreeMap<String, String>,
    target: AgentLocationHints,
    observations: Vec<AgentObservationState>,
}

/// Run configured selection hooks that match the resolved selected session.
pub(super) fn run_selection_hooks(
    hooks: &[SidebarSelectionHookConfig],
    selected: &SelectionHookInput,
) -> Result<()> {
    if hooks.is_empty() {
        return Ok(());
    }

    run_hooks_for_input(hooks, selected)
}

impl SelectionHookInput {
    /// Build hook input from resolved selected-session data and pre-filtered observations.
    pub(in crate::agent::sidebar) fn new(
        key: AgentSessionKey,
        status: AgentStatus,
        title: Option<String>,
        context: Option<String>,
        metadata: BTreeMap<String, String>,
        target: AgentLocationHints,
        observations: Vec<AgentObservationState>,
    ) -> Self {
        Self {
            key,
            status,
            title,
            context,
            metadata,
            target,
            observations,
        }
    }
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
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    metadata: BTreeMap<String, String>,
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
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    metadata: BTreeMap<String, String>,
    target: SelectionHookTargetPayload,
}

fn run_hooks_for_input(
    hooks: &[SidebarSelectionHookConfig],
    selected: &SelectionHookInput,
) -> Result<()> {
    let log_path = diagnostics::default_log_path().ok();
    run_hooks_for_input_and_log_path(hooks, selected, log_path.as_deref())
}

fn run_hooks_for_input_and_log_path(
    hooks: &[SidebarSelectionHookConfig],
    selected: &SelectionHookInput,
    log_path: Option<&Path>,
) -> Result<()> {
    let payload = payload_for_selection(selected);
    let payload_json = serde_json::to_vec_pretty(&payload)?;
    let mut failures = Vec::new();

    for hook in hooks
        .iter()
        .filter(|hook| hook_matches_payload(hook, &payload))
    {
        if let Err(error) = run_matching_hook(hook, &payload, &payload_json, log_path) {
            failures.push(format!("{}: {error}", hook.command));
        }
    }

    if failures.is_empty() {
        return Ok(());
    }
    bail!("selection hook failed: {}", failures.join("; "))
}

fn run_matching_hook(
    hook: &SidebarSelectionHookConfig,
    payload: &SelectionHookPayload,
    payload_json: &[u8],
    log_path: Option<&Path>,
) -> Result<()> {
    let current_dir = hook_current_dir(&payload.target);
    let command = HookCommand {
        command: &hook.command,
        stdin: payload_json,
        timeout: Duration::from_millis(hook.timeout_ms()),
        env: hook_env(payload, log_path),
        cwd: current_dir.clone(),
    };
    let started = Instant::now();

    match runner::run(command) {
        Ok(outcome) => {
            finish_hook_attempt(log_path, hook, payload, current_dir.as_deref(), outcome)
        }
        Err(error) => {
            let status = error.status_label();
            let message = error.message();
            diagnostics::log_attempt(
                log_path,
                HookAttemptLog {
                    event: payload.event,
                    command: &hook.command,
                    agent_kind: &payload.agent.kind,
                    agent_session_id: &payload.agent.session_id,
                    status,
                    duration: started.elapsed(),
                    exit_status: None,
                    error: Some(message),
                    cwd: current_dir.map(|path| path.display().to_string()),
                    stdout: "",
                    stderr: "",
                },
            );
            Err(error.into_error())
        }
    }
}

fn finish_hook_attempt(
    log_path: Option<&Path>,
    hook: &SidebarSelectionHookConfig,
    payload: &SelectionHookPayload,
    current_dir: Option<&Path>,
    outcome: HookCommandOutcome,
) -> Result<()> {
    let error = outcome.failure_message();
    diagnostics::log_attempt(
        log_path,
        HookAttemptLog {
            event: payload.event,
            command: &hook.command,
            agent_kind: &payload.agent.kind,
            agent_session_id: &payload.agent.session_id,
            status: outcome.status_label(),
            duration: outcome.duration,
            exit_status: outcome.exit_status(),
            error: error.clone(),
            cwd: current_dir.map(|path| path.display().to_string()),
            stdout: &outcome.stdout,
            stderr: &outcome.stderr,
        },
    );

    if let Some(error) = error {
        bail!("{error}")
    }
    Ok(())
}

fn payload_for_selection(selected: &SelectionHookInput) -> SelectionHookPayload {
    SelectionHookPayload {
        version: 1,
        event: HOOK_EVENT,
        agent: SelectionHookAgentPayload {
            kind: selected.key.agent_kind.clone(),
            session_id: selected.key.session_id.clone(),
            metadata: selected.metadata.clone(),
        },
        status: selected.status,
        title: selected.title.clone(),
        context: selected.context.clone(),
        target: SelectionHookTargetPayload::from_hints(&selected.target),
        observations: selected
            .observations
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

fn hook_env(payload: &SelectionHookPayload, log_path: Option<&Path>) -> Vec<(OsString, OsString)> {
    let mut env = Vec::new();
    push_env(&mut env, "KMUX_HOOK_EVENT", payload.event);
    push_env(&mut env, "KMUX_AGENT_KIND", &payload.agent.kind);
    push_env(&mut env, "KMUX_AGENT_SESSION_ID", &payload.agent.session_id);
    push_env(&mut env, "KMUX_AGENT_STATUS", payload.status.as_str());
    push_optional_env(
        &mut env,
        "KMUX_TMUX_INSTANCE",
        &payload.target.tmux_instance,
    );
    push_optional_env(
        &mut env,
        "KMUX_TMUX_SESSION_NAME",
        &payload.target.tmux_session_name,
    );
    push_optional_env(
        &mut env,
        "KMUX_TMUX_WINDOW_NAME",
        &payload.target.tmux_window_name,
    );
    push_optional_env(
        &mut env,
        "KMUX_TMUX_WINDOW_ID",
        &payload.target.tmux_window_id,
    );
    push_optional_env(&mut env, "KMUX_TMUX_PANE_ID", &payload.target.tmux_pane_id);
    push_optional_env(&mut env, "KMUX_DIRECTORY", &payload.target.directory);
    push_optional_env(
        &mut env,
        "KMUX_GIT_WORKTREE_PATH",
        &payload.target.git_worktree_path,
    );
    push_optional_env(&mut env, "KMUX_GIT_BRANCH", &payload.target.git_branch);
    push_optional_env(
        &mut env,
        "KMUX_WORKSPACE_SLUG",
        &payload.target.kmux_workspace_slug,
    );
    push_metadata_env(&mut env, &payload.agent.metadata);
    if let Some(log_path) = log_path {
        env.push((
            OsString::from("KMUX_HOOK_LOG"),
            log_path.as_os_str().to_os_string(),
        ));
    }
    env
}

fn push_env(env: &mut Vec<(OsString, OsString)>, key: &str, value: impl Into<OsString>) {
    env.push((OsString::from(key), value.into()));
}

fn push_optional_env(env: &mut Vec<(OsString, OsString)>, key: &str, value: &Option<String>) {
    if let Some(value) = value.as_deref() {
        push_env(env, key, value);
    }
}

fn push_metadata_env(env: &mut Vec<(OsString, OsString)>, metadata: &BTreeMap<String, String>) {
    let mut candidates = BTreeMap::<String, Option<String>>::new();
    for (key, value) in metadata {
        if let Some(env_name) = metadata_env_name(key) {
            candidates
                .entry(env_name)
                .and_modify(|candidate| *candidate = None)
                .or_insert_with(|| Some(value.clone()));
        }
    }

    for (key, value) in candidates {
        if let Some(value) = value {
            push_env(env, &key, value);
        }
    }
}

fn metadata_env_name(key: &str) -> Option<String> {
    if key.is_empty()
        || !key
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '_')
    {
        return None;
    }

    let mut env_name = String::from("KMUX_AGENT_META_");
    env_name.extend(key.chars().map(|character| character.to_ascii_uppercase()));
    Some(env_name)
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
            metadata: observation.metadata.clone(),
            target: SelectionHookTargetPayload::from_hints(&observation.target),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;
    use std::time::{Duration, Instant};

    use serde_json::{Value, json};
    use tempfile::tempdir;

    use super::*;
    use crate::agent::sessions::AgentSessionView;
    use crate::agent::sidebar::test_support::report_state;
    use crate::state::{AgentObservationKey, AgentSessionKey};

    #[test]
    fn payload_serializes_selected_session_and_observations() {
        let mut view = report_state(AgentStatus::Waiting, 100, "@1", "%1");
        view.title = Some("Implement hooks".to_owned());
        view.context = Some("55.2K".to_owned());
        view.target.directory = Some("/repo/worktree".to_owned());
        view.metadata
            .insert("opaque_ref".to_owned(), "ref_01KTEST".to_owned());
        let mut selected = input_from_view(&view, Vec::new());
        selected.observations = vec![observation_for_input(
            &selected,
            "server",
            "http://127.0.0.1:4096",
        )];

        let payload = payload_for_selection(&selected);
        let json = serde_json::to_value(payload).expect("payload should serialize");

        assert_eq!(
            json,
            json!({
                "version": 1,
                "event": HOOK_EVENT,
                "agent": {
                    "kind": "opencode",
                    "session_id": "ses_%1",
                    "metadata": {
                        "opaque_ref": "ref_01KTEST",
                    },
                },
                "status": "waiting",
                "title": "Implement hooks",
                "context": "55.2K",
                "target": {
                    "tmux_instance": "test",
                    "tmux_session_name": "project",
                    "tmux_window_name": "kmux-feature-sidebar",
                    "tmux_window_id": "@1",
                    "tmux_pane_id": "%1",
                    "tmux_pane_title": "Implement sidebar",
                    "tmux_pane_current_command": "nvim",
                    "git_repo_name": "kmux",
                    "git_repo_path": "/repo",
                    "kmux_workspace_slug": "feature-sidebar",
                    "git_worktree_path": "/repo__worktrees/feature-sidebar",
                    "git_branch": "feature/sidebar",
                    "directory": "/repo/worktree",
                },
                "observations": [{
                    "producer_kind": "server",
                    "producer_instance": "http://127.0.0.1:4096",
                    "status": "waiting",
                    "observed_at": 100,
                    "title": "Implement hooks",
                    "context": "55.2K",
                    "metadata": {
                        "opaque_ref": "ref_01KTEST",
                    },
                    "target": {
                        "tmux_instance": "test",
                        "tmux_session_name": "project",
                        "tmux_window_name": "kmux-feature-sidebar",
                        "tmux_window_id": "@1",
                        "tmux_pane_id": "%1",
                        "tmux_pane_title": "Implement sidebar",
                        "tmux_pane_current_command": "nvim",
                        "git_repo_name": "kmux",
                        "git_repo_path": "/repo",
                        "kmux_workspace_slug": "feature-sidebar",
                        "git_worktree_path": "/repo__worktrees/feature-sidebar",
                        "git_branch": "feature/sidebar",
                        "directory": "/repo/worktree",
                    },
                }],
            })
        );
    }

    #[test]
    fn hook_filters_by_agent_kind_and_producer_kind() {
        let selected = input_with_observation("server", "http://127.0.0.1:4096");
        let payload = payload_for_selection(&selected);

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
        let selected =
            input_from_view(&report_state(AgentStatus::Working, 100, "@1", "%1"), vec![]);
        let payload = payload_for_selection(&selected);

        assert!(hook_matches_payload(
            &hook_config("true", Some("opencode"), None, None),
            &payload
        ));
    }

    #[test]
    fn hook_env_preserves_selection_compatibility_variables() {
        let mut view = report_state(AgentStatus::Working, 100, "@1", "%1");
        view.target.directory = Some("/repo/worktree".to_owned());
        view.metadata
            .insert("opaque_ref".to_owned(), "ref_01KTEST".to_owned());
        let selected = input_from_view(&view, vec![]);
        let payload = payload_for_selection(&selected);

        let env = env_map(hook_env(&payload, None));

        assert_eq!(
            env.get("KMUX_HOOK_EVENT").map(String::as_str),
            Some(HOOK_EVENT)
        );
        assert_eq!(
            env.get("KMUX_AGENT_KIND").map(String::as_str),
            Some("opencode")
        );
        assert_eq!(
            env.get("KMUX_AGENT_SESSION_ID").map(String::as_str),
            Some("ses_%1")
        );
        assert_eq!(
            env.get("KMUX_AGENT_STATUS").map(String::as_str),
            Some("working")
        );
        assert_eq!(
            env.get("KMUX_TMUX_INSTANCE").map(String::as_str),
            Some("test")
        );
        assert_eq!(
            env.get("KMUX_TMUX_SESSION_NAME").map(String::as_str),
            Some("project")
        );
        assert_eq!(
            env.get("KMUX_TMUX_WINDOW_NAME").map(String::as_str),
            Some("kmux-feature-sidebar")
        );
        assert_eq!(
            env.get("KMUX_TMUX_WINDOW_ID").map(String::as_str),
            Some("@1")
        );
        assert_eq!(env.get("KMUX_TMUX_PANE_ID").map(String::as_str), Some("%1"));
        assert_eq!(
            env.get("KMUX_DIRECTORY").map(String::as_str),
            Some("/repo/worktree")
        );
        assert_eq!(
            env.get("KMUX_GIT_WORKTREE_PATH").map(String::as_str),
            Some("/repo__worktrees/feature-sidebar")
        );
        assert_eq!(
            env.get("KMUX_GIT_BRANCH").map(String::as_str),
            Some("feature/sidebar")
        );
        assert_eq!(
            env.get("KMUX_WORKSPACE_SLUG").map(String::as_str),
            Some("feature-sidebar")
        );
        assert_eq!(
            env.get("KMUX_AGENT_META_OPAQUE_REF").map(String::as_str),
            Some("ref_01KTEST")
        );
        assert_eq!(env.get("KMUX_HOOK_LOG"), None);
    }

    #[test]
    fn hook_env_exports_only_unambiguous_env_safe_metadata_keys() {
        let mut view = report_state(AgentStatus::Working, 100, "@1", "%1");
        view.metadata
            .insert("ticket".to_owned(), "T-123".to_owned());
        view.metadata.insert("mixed".to_owned(), "lower".to_owned());
        view.metadata.insert("MiXeD".to_owned(), "mixed".to_owned());
        view.metadata
            .insert("not-env-safe".to_owned(), "skipped".to_owned());
        let selected = input_from_view(&view, vec![]);
        let payload = payload_for_selection(&selected);

        let env = env_map(hook_env(&payload, None));

        assert_eq!(
            env.get("KMUX_AGENT_META_TICKET").map(String::as_str),
            Some("T-123")
        );
        assert!(!env.contains_key("KMUX_AGENT_META_MIXED"));
        assert!(!env.contains_key("KMUX_AGENT_META_NOT_ENV_SAFE"));
    }

    #[test]
    fn hook_current_dir_prefers_worktree_path_and_falls_back_to_directory() -> Result<()> {
        let worktree = tempdir()?;
        let directory = tempdir()?;
        let mut target = SelectionHookTargetPayload {
            git_worktree_path: Some(worktree.path().display().to_string()),
            directory: Some(directory.path().display().to_string()),
            ..SelectionHookTargetPayload::default()
        };

        assert_eq!(hook_current_dir(&target).as_deref(), Some(worktree.path()));

        target.git_worktree_path = Some(worktree.path().join("missing").display().to_string());
        assert_eq!(hook_current_dir(&target).as_deref(), Some(directory.path()));
        Ok(())
    }

    #[test]
    fn matching_hook_receives_payload_env_and_selected_cwd() -> Result<()> {
        let dir = tempdir()?;
        let mut view = report_state(AgentStatus::Working, 100, "@1", "%1");
        view.target.git_worktree_path = Some(dir.path().display().to_string());
        view.metadata
            .insert("opaque_ref".to_owned(), "ref_01KTEST".to_owned());
        let selected = input_from_view(&view, vec![]);
        let payload_path = dir.path().join("payload.json");
        let session_path = dir.path().join("session.txt");
        let metadata_path = dir.path().join("metadata.txt");
        let cwd_path = dir.path().join("cwd.txt");
        let command = format!(
            "cat > '{}'; printf '%s' \"$KMUX_AGENT_SESSION_ID\" > '{}'; printf '%s' \"$KMUX_AGENT_META_OPAQUE_REF\" > '{}'; pwd > '{}'",
            payload_path.display(),
            session_path.display(),
            metadata_path.display(),
            cwd_path.display()
        );

        run_hooks_for_input_and_log_path(
            &[hook_config(&command, Some("opencode"), None, Some(1000))],
            &selected,
            None,
        )?;

        let payload: Value = serde_json::from_str(&fs::read_to_string(payload_path)?)?;
        assert_eq!(payload["agent"]["session_id"], "ses_%1");
        assert_eq!(fs::read_to_string(session_path)?, "ses_%1");
        assert_eq!(fs::read_to_string(metadata_path)?, "ref_01KTEST");
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
        let selected =
            input_from_view(&report_state(AgentStatus::Working, 100, "@1", "%1"), vec![]);
        let command = format!("touch '{}'", marker.display());

        run_hooks_for_input_and_log_path(
            &[hook_config(&command, Some("codex"), None, Some(1000))],
            &selected,
            None,
        )?;

        assert!(!marker.exists());
        Ok(())
    }

    #[test]
    fn failing_hook_logs_stderr_and_log_path_env() -> Result<()> {
        let dir = tempdir()?;
        let log_path = dir.path().join("sidebar-hooks.jsonl");
        let log_env_path = dir.path().join("log-env.txt");
        let selected =
            input_from_view(&report_state(AgentStatus::Working, 100, "@1", "%1"), vec![]);
        let command = format!(
            "printf '%s' \"$KMUX_HOOK_LOG\" > '{}'; printf 'server missing' >&2; exit 7",
            log_env_path.display()
        );

        let error = run_hooks_for_input_and_log_path(
            &[hook_config(&command, Some("opencode"), None, Some(1000))],
            &selected,
            Some(&log_path),
        )
        .expect_err("failing hook should report an error");

        assert!(error.to_string().contains("server missing"));
        assert_eq!(
            fs::read_to_string(log_env_path)?,
            log_path.display().to_string()
        );
        let log = fs::read_to_string(log_path)?;
        let entry: Value = serde_json::from_str(log.trim())?;
        assert_eq!(entry["status"], "failed");
        assert_eq!(entry["agent_session_id"], "ses_%1");
        assert_eq!(entry["stderr"], "server missing");
        assert!(entry["error"].as_str().is_some_and(|error| {
            error.contains("exit status") && error.contains("server missing")
        }));
        Ok(())
    }

    #[test]
    fn successful_hook_logs_ok_status() -> Result<()> {
        let dir = tempdir()?;
        let log_path = dir.path().join("sidebar-hooks.jsonl");
        let selected =
            input_from_view(&report_state(AgentStatus::Working, 100, "@1", "%1"), vec![]);

        run_hooks_for_input_and_log_path(
            &[hook_config(
                "printf selected",
                Some("opencode"),
                None,
                Some(1000),
            )],
            &selected,
            Some(&log_path),
        )?;

        let entry = read_single_log_entry(&log_path)?;
        assert_eq!(entry["status"], "ok");
        assert_eq!(entry["agent_session_id"], "ses_%1");
        assert_eq!(entry["stdout"], "selected");
        assert!(entry.get("error").is_none());
        Ok(())
    }

    #[test]
    fn hook_timeout_is_reported() {
        let selected =
            input_from_view(&report_state(AgentStatus::Working, 100, "@1", "%1"), vec![]);

        let error = run_hooks_for_input_and_log_path(
            &[hook_config("sleep 1", Some("opencode"), None, Some(10))],
            &selected,
            None,
        )
        .expect_err("timeout should fail");

        assert!(error.to_string().contains("timed out"));
    }

    #[test]
    fn timeout_hook_logs_timed_out_status() -> Result<()> {
        let dir = tempdir()?;
        let log_path = dir.path().join("sidebar-hooks.jsonl");
        let selected =
            input_from_view(&report_state(AgentStatus::Working, 100, "@1", "%1"), vec![]);

        let error = run_hooks_for_input_and_log_path(
            &[hook_config("sleep 1", Some("opencode"), None, Some(10))],
            &selected,
            Some(&log_path),
        )
        .expect_err("timeout should fail");

        assert!(error.to_string().contains("timed out"));
        let entry = read_single_log_entry(&log_path)?;
        assert_eq!(entry["status"], "timed_out");
        assert!(
            entry["error"]
                .as_str()
                .is_some_and(|error| error.contains("timed out"))
        );
        Ok(())
    }

    #[test]
    fn hook_timeout_covers_commands_that_ignore_large_stdin() {
        let mut selected = input_with_observation("server", "http://127.0.0.1:4096");
        let mut observation = selected.observations.remove(0);
        observation.title = Some("x".repeat(1024 * 1024));
        selected.observations.push(observation);
        let started = Instant::now();

        let error = run_hooks_for_input_and_log_path(
            &[hook_config("sleep 1", Some("opencode"), None, Some(25))],
            &selected,
            None,
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

    fn input_with_observation(producer_kind: &str, producer_instance: &str) -> SelectionHookInput {
        let mut selected = input_from_view(
            &report_state(AgentStatus::Working, 100, "@1", "%1"),
            Vec::new(),
        );
        selected.observations = vec![observation_for_input(
            &selected,
            producer_kind,
            producer_instance,
        )];
        selected
    }

    fn input_from_view(
        view: &AgentSessionView,
        observations: Vec<AgentObservationState>,
    ) -> SelectionHookInput {
        SelectionHookInput::new(
            view.key.clone(),
            view.status,
            view.title.clone(),
            view.context.clone(),
            view.metadata().clone(),
            view.location_hints().clone(),
            observations,
        )
    }

    fn observation_for_input(
        selected: &SelectionHookInput,
        producer_kind: &str,
        producer_instance: &str,
    ) -> AgentObservationState {
        AgentObservationState {
            key: AgentObservationKey {
                session: AgentSessionKey {
                    agent_kind: selected.key.agent_kind.clone(),
                    session_id: selected.key.session_id.clone(),
                },
                producer_kind: producer_kind.to_owned(),
                producer_instance: producer_instance.to_owned(),
            },
            created_at: 100,
            status: Some(selected.status),
            status_observed_at: Some(100),
            status_changed_at: Some(100),
            working_elapsed_secs: 0,
            observed_at: 100,
            title: selected.title.clone(),
            context: selected.context.clone(),
            metadata: selected.metadata.clone(),
            metadata_cleared: Default::default(),
            target: selected.target.clone(),
        }
    }

    fn env_map(env: Vec<(OsString, OsString)>) -> BTreeMap<String, String> {
        env.into_iter()
            .map(|(key, value)| {
                (
                    key.to_string_lossy().into_owned(),
                    value.to_string_lossy().into_owned(),
                )
            })
            .collect()
    }

    fn read_single_log_entry(log_path: &Path) -> Result<Value> {
        let log = fs::read_to_string(log_path)?;
        Ok(serde_json::from_str(log.trim())?)
    }
}
