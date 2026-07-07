use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Result, anyhow, bail};

use crate::agent::observations::{
    LocationUpdate, MetadataUpdate, ObservationCommand, ObservationUpdate,
};
use crate::agent::sessions::session_views;
use crate::agent::{self, status, status_badges};
use crate::cli;
use crate::config::Config;
use crate::state::{
    AgentObservationKey, AgentSessionKey, AgentStatus as StoredAgentStatus, StateStore,
};
use crate::tmux::Tmux;

/// Print active agent sessions, optionally scoped to the current repo and enriched with Git state.
pub(super) fn run_status(args: cli::StatusArgs) -> Result<()> {
    let cli::StatusArgs { filters, json, git } = args;
    let store = StateStore::new()?;
    let tmux = Tmux::from_env();
    let config = Config::load()?;
    let views = session_views(&store, &tmux)?;
    let query = status::StatusQuery::new(filters, git);
    let entries = status::status_entries(&views, &query, &config.status_icons)?;

    if json {
        println!("{}", serde_json::to_string_pretty(&entries)?);
    } else if entries.is_empty() {
        println!("No active agents");
    } else {
        status::print_table(&entries, query.show_git());
    }
    Ok(())
}

/// Record or delete one agent status observation from an external producer.
pub(super) fn run_set_agent_status(args: cli::SetAgentStatusArgs) -> Result<()> {
    if std::env::var_os("KMUX_DISABLE_SET_AGENT_STATUS").is_some() {
        return Ok(());
    }

    let command = observation_command_from_args(args)?;
    let config = Config::load()?;
    let tmux = Tmux::from_env();
    let store = StateStore::new()?;
    let outcome = agent::observations::apply_observation_command(&store, command)?;

    if outcome.should_notify() {
        let _ = status_badges::refresh_window_statuses(&store, &tmux, &config.status_icons);
        let _ = agent::notify_observation_changed(&tmux);
    }
    Ok(())
}

fn observation_command_from_args(args: cli::SetAgentStatusArgs) -> Result<ObservationCommand> {
    let key = observation_key(&args)?;
    if args.delete_session {
        return Ok(ObservationCommand::DeleteSession(key.session));
    }
    if args.delete {
        return Ok(ObservationCommand::DeleteObservation(key));
    }
    let metadata = metadata_update_from_args(&args)?;

    Ok(ObservationCommand::Upsert(Box::new(ObservationUpdate {
        key,
        status: args.status.map(stored_status),
        title: clean_optional(args.title),
        context: clean_optional(args.context),
        metadata,
        target: LocationUpdate {
            tmux_instance: clean_optional(args.tmux_instance),
            git_repo_name: clean_optional(args.git_repo_name),
            git_repo_path: clean_optional(args.git_repo_path),
            git_branch: clean_optional(args.git_branch),
            directory: clean_optional(args.directory),
        },
    })))
}

// The observation key identifies both the logical agent session and the producer
// that reported it, so TUI and server observations can coexist for one session.
fn observation_key(args: &cli::SetAgentStatusArgs) -> Result<AgentObservationKey> {
    Ok(AgentObservationKey {
        session: AgentSessionKey {
            agent_kind: clean_required(&args.agent_kind, "--agent-kind")?,
            session_id: clean_required(&args.session_id, "--session-id")?,
        },
        producer_kind: clean_required(&args.producer_kind, "--producer-kind")?,
        producer_instance: clean_required(&args.producer_instance, "--producer-instance")?,
    })
}

fn stored_status(status: cli::AgentStatus) -> StoredAgentStatus {
    match status {
        cli::AgentStatus::Working => StoredAgentStatus::Working,
        cli::AgentStatus::Waiting => StoredAgentStatus::Waiting,
        cli::AgentStatus::Done => StoredAgentStatus::Done,
    }
}

fn metadata_update_from_args(args: &cli::SetAgentStatusArgs) -> Result<MetadataUpdate> {
    let mut set = BTreeMap::new();
    let mut clear = BTreeSet::new();

    for raw in &args.agent_meta {
        let (key, value) = parse_agent_meta(raw)?;
        if set.insert(key.clone(), value).is_some() {
            bail!("--agent-meta key '{key}' cannot be repeated");
        }
    }

    for raw in &args.clear_agent_meta {
        let key = clean_required(raw, "--clear-agent-meta key")?;
        if !clear.insert(key.clone()) {
            bail!("--clear-agent-meta key '{key}' cannot be repeated");
        }
    }

    for key in set.keys() {
        if clear.contains(key) {
            bail!("agent metadata key '{key}' cannot be set and cleared in the same update");
        }
    }

    Ok(MetadataUpdate { set, clear })
}

fn parse_agent_meta(raw: &str) -> Result<(String, String)> {
    let Some((key, value)) = raw.split_once('=') else {
        bail!("--agent-meta must use KEY=VALUE syntax");
    };
    let key = clean_required(key, "--agent-meta key")?;
    let value = clean_required(value, "--agent-meta value")?;
    Ok((key, value))
}

fn clean_required(value: &str, label: &str) -> Result<String> {
    clean_str(value)
        .map(str::to_owned)
        .ok_or_else(|| anyhow!("{label} cannot be empty"))
}

fn clean_optional(value: Option<String>) -> Option<String> {
    value.and_then(|value| clean_str(&value).map(str::to_owned))
}

fn clean_str(value: &str) -> Option<&str> {
    let value = value.trim();
    (!value.is_empty()).then_some(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_cli_args_to_trimmed_upsert_command() -> Result<()> {
        let mut args = set_status_args();
        args.agent_kind = " opencode ".to_owned();
        args.session_id = " ses_root ".to_owned();
        args.producer_kind = " server ".to_owned();
        args.producer_instance = " default ".to_owned();
        args.title = Some(" Implement status ".to_owned());
        args.context = Some(" 12K ".to_owned());
        args.agent_meta = vec![" workspace_id = wrk_01KTEST ".to_owned()];
        args.git_branch = Some(" feature/auth ".to_owned());
        args.directory = Some(" /repo/project ".to_owned());

        let command = observation_command_from_args(args)?;

        let ObservationCommand::Upsert(update) = command else {
            return Err(anyhow!("expected upsert command"));
        };
        assert_eq!(update.key.session.agent_kind, "opencode");
        assert_eq!(update.key.session.session_id, "ses_root");
        assert_eq!(update.key.producer_kind, "server");
        assert_eq!(update.key.producer_instance, "default");
        assert_eq!(update.status, Some(StoredAgentStatus::Working));
        assert_eq!(update.title.as_deref(), Some("Implement status"));
        assert_eq!(update.context.as_deref(), Some("12K"));
        assert_eq!(
            update.metadata.set.get("workspace_id").map(String::as_str),
            Some("wrk_01KTEST")
        );
        assert!(update.metadata.clear.is_empty());
        assert_eq!(update.target.git_branch.as_deref(), Some("feature/auth"));
        assert_eq!(update.target.directory.as_deref(), Some("/repo/project"));
        Ok(())
    }

    #[test]
    fn rejects_empty_required_values() {
        let mut args = set_status_args();
        args.agent_kind = "   ".to_owned();

        let error = observation_command_from_args(args)
            .err()
            .map(|error| error.to_string());

        assert_eq!(error.as_deref(), Some("--agent-kind cannot be empty"));
    }

    #[test]
    fn delete_session_takes_precedence_over_delete_observation() -> Result<()> {
        let mut args = set_status_args();
        args.delete = true;
        args.delete_session = true;

        let command = observation_command_from_args(args)?;

        let ObservationCommand::DeleteSession(session) = command else {
            return Err(anyhow!("expected delete-session command"));
        };
        assert_eq!(session.agent_kind, "opencode");
        assert_eq!(session.session_id, "ses_root");
        Ok(())
    }

    #[test]
    fn maps_clear_agent_meta_flag() -> Result<()> {
        let mut args = set_status_args();
        args.clear_agent_meta = vec![" workspace_id ".to_owned()];

        let command = observation_command_from_args(args)?;

        let ObservationCommand::Upsert(update) = command else {
            return Err(anyhow!("expected upsert command"));
        };
        assert!(update.metadata.set.is_empty());
        assert!(update.metadata.clear.contains("workspace_id"));
        Ok(())
    }

    #[test]
    fn rejects_malformed_agent_meta() {
        let mut args = set_status_args();
        args.agent_meta = vec!["workspace_id".to_owned()];

        let error = observation_command_from_args(args)
            .err()
            .map(|error| error.to_string());

        assert_eq!(
            error.as_deref(),
            Some("--agent-meta must use KEY=VALUE syntax")
        );
    }

    #[test]
    fn rejects_duplicate_agent_meta_keys() {
        let mut args = set_status_args();
        args.agent_meta = vec![
            "workspace_id=wrk_one".to_owned(),
            "workspace_id=wrk_two".to_owned(),
        ];

        let error = observation_command_from_args(args)
            .err()
            .map(|error| error.to_string());

        assert_eq!(
            error.as_deref(),
            Some("--agent-meta key 'workspace_id' cannot be repeated")
        );
    }

    #[test]
    fn rejects_agent_meta_set_and_clear_conflict() {
        let mut args = set_status_args();
        args.agent_meta = vec!["workspace_id=wrk_01KTEST".to_owned()];
        args.clear_agent_meta = vec!["workspace_id".to_owned()];

        let error = observation_command_from_args(args)
            .err()
            .map(|error| error.to_string());

        assert_eq!(
            error.as_deref(),
            Some("agent metadata key 'workspace_id' cannot be set and cleared in the same update")
        );
    }

    fn set_status_args() -> cli::SetAgentStatusArgs {
        cli::SetAgentStatusArgs {
            status: Some(cli::AgentStatus::Working),
            agent_kind: "opencode".to_owned(),
            session_id: "ses_root".to_owned(),
            producer_kind: "server".to_owned(),
            producer_instance: "default".to_owned(),
            delete: false,
            delete_session: false,
            title: None,
            context: None,
            tmux_instance: None,
            agent_meta: Vec::new(),
            clear_agent_meta: Vec::new(),
            git_repo_name: None,
            git_repo_path: None,
            git_branch: None,
            directory: None,
        }
    }
}
