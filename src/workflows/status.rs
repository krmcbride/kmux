use anyhow::{Result, anyhow};

use crate::agent::observations::{LocationUpdate, ObservationCommand, ObservationUpdate};
use crate::agent::sessions::session_views;
use crate::agent::workspace_activity::workspace_activity_rows;
use crate::agent::{self, status};
use crate::cli;
use crate::config::Config;
use crate::state::{
    AgentObservationKey, AgentSessionKey, AgentStatus as StoredAgentStatus, StateStore,
    now_unix_seconds,
};
use crate::tmux::Tmux;

/// Print active agent sessions from the global workspace activity model.
pub(super) fn run_status(args: cli::StatusArgs) -> Result<()> {
    let cli::StatusArgs { filters, json, git } = args;
    let store = StateStore::new()?;
    let tmux = Tmux::from_env();
    let config = Config::load()?;
    let views = session_views(&store, &tmux)?;
    let activities = workspace_activity_rows(&views, now_unix_seconds());
    let query = status::StatusQuery::new(filters, git);
    let entries = status::status_entries(&activities, &query, &config.status_icons);

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
    agent::observations::apply_observation_command(&store, command)?;
    agent::refresh_observation_surfaces(&store, &tmux, &config.status_icons);
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
    Ok(ObservationCommand::Upsert(Box::new(ObservationUpdate {
        key,
        status: args.status.map(stored_status),
        title: clean_optional(args.title),
        context: clean_optional(args.context),
        target: LocationUpdate {
            tmux_instance: clean_optional(args.tmux_instance),
            git_repo_name: clean_optional(args.git_repo_name),
            git_repo_path: clean_optional(args.git_repo_path),
            git_branch: clean_optional(args.git_branch),
            directory: clean_optional(args.directory),
        },
    })))
}

// The observation key identifies both the logical agent session and one independent
// reporter, allowing partial observations from multiple integrations to coexist.
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
            git_repo_name: None,
            git_repo_path: None,
            git_branch: None,
            directory: None,
        }
    }
}
