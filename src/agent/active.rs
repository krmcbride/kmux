use std::collections::HashMap;

use anyhow::Result;

use crate::state::{AgentState, PaneKey, StateStore};
use crate::tmux::{Tmux, TmuxPaneSnapshot};

pub fn active_agents(store: &StateStore, tmux: &Tmux) -> Result<Vec<AgentState>> {
    let instance_id = tmux.instance_id();
    let candidates = store
        .list_agents()?
        .into_iter()
        .filter(|agent| is_current_tmux_instance(&agent.pane_key, &instance_id))
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        return Ok(Vec::new());
    }

    let panes = match tmux.list_pane_snapshots() {
        Ok(panes) => panes
            .into_iter()
            .map(|pane| (pane.pane_id.clone(), pane))
            .collect::<HashMap<_, _>>(),
        Err(_) => {
            prune_agents(store, &candidates)?;
            return Ok(Vec::new());
        }
    };
    reconcile_active_agents(store, candidates, &panes)
}

fn is_current_tmux_instance(key: &PaneKey, instance_id: &str) -> bool {
    key.backend == "tmux" && key.instance == instance_id
}

fn reconcile_active_agents(
    store: &StateStore,
    agents: Vec<AgentState>,
    panes: &HashMap<String, TmuxPaneSnapshot>,
) -> Result<Vec<AgentState>> {
    let mut active = Vec::new();
    for agent in agents {
        let Some(pane) = panes.get(&agent.pane_key.pane_id) else {
            store.delete_agent(&agent.pane_key)?;
            continue;
        };

        if pane.window_id != agent.window_id {
            store.delete_agent(&agent.pane_key)?;
            continue;
        }

        let mut agent = agent;
        agent.session_name = pane.session_name.clone();
        agent.window_name = pane.window_name.clone();
        agent.window_id = pane.window_id.clone();
        if pane.title.is_some() {
            agent.pane_title = pane.title.clone();
        }
        if pane.current_command.is_some() {
            agent.pane_current_command = pane.current_command.clone();
        }
        active.push(agent);
    }
    Ok(active)
}

fn prune_agents(store: &StateStore, agents: &[AgentState]) -> Result<()> {
    for agent in agents {
        store.delete_agent(&agent.pane_key)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{AgentStatus, PaneKey};

    use tempfile::TempDir;

    #[test]
    fn reconciles_agents_from_batched_tmux_snapshot() -> Result<()> {
        let temp = TempDir::new()?;
        let store = StateStore::test_with_path(temp.path())?;
        let agent = agent_state("%1", "@1");
        store.upsert_agent(&agent)?;

        let panes = HashMap::from([(
            "%1".to_owned(),
            pane_snapshot(
                "%1",
                "@1",
                "project",
                "renamed-window",
                Some("kmux"),
                Some("nvim"),
            ),
        )]);

        let active = reconcile_active_agents(&store, vec![agent], &panes)?;

        assert_eq!(active.len(), 1);
        assert_eq!(active[0].session_name, "project");
        assert_eq!(active[0].window_name, "renamed-window");
        assert_eq!(active[0].pane_title.as_deref(), Some("kmux"));
        assert_eq!(active[0].pane_current_command.as_deref(), Some("nvim"));
        Ok(())
    }

    #[test]
    fn prunes_missing_or_reused_agent_panes() -> Result<()> {
        let temp = TempDir::new()?;
        let store = StateStore::test_with_path(temp.path())?;
        let missing = agent_state("%1", "@1");
        let reused = agent_state("%2", "@2");
        store.upsert_agent(&missing)?;
        store.upsert_agent(&reused)?;

        let panes = HashMap::from([(
            "%2".to_owned(),
            pane_snapshot("%2", "@not-recorded", "project", "other", None, None),
        )]);

        let active =
            reconcile_active_agents(&store, vec![missing.clone(), reused.clone()], &panes)?;

        assert!(active.is_empty());
        assert!(store.get_agent(&missing.pane_key)?.is_none());
        assert!(store.get_agent(&reused.pane_key)?.is_none());
        Ok(())
    }

    #[test]
    fn active_agents_prunes_candidates_when_tmux_snapshot_fails() -> Result<()> {
        use std::time::{SystemTime, UNIX_EPOCH};

        let temp = TempDir::new()?;
        let store = StateStore::test_with_path(temp.path())?;
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let socket_name = format!("kmux-missing-test-{}-{nanos}", std::process::id());
        let tmux = Tmux::with_socket_name(&socket_name);
        let mut agent = agent_state("%1", "@1");
        agent.pane_key.instance = socket_name;
        store.upsert_agent(&agent)?;

        let active = active_agents(&store, &tmux)?;

        assert!(active.is_empty());
        assert!(store.get_agent(&agent.pane_key)?.is_none());
        Ok(())
    }

    fn agent_state(pane_id: &str, window_id: &str) -> AgentState {
        AgentState {
            pane_key: PaneKey::new_tmux("test", pane_id),
            status: AgentStatus::Working,
            icon: "?".to_owned(),
            status_changed_at: 100,
            observed_at: 100,
            agent_title: None,
            context_usage: None,
            pane_title: Some("old-title".to_owned()),
            pane_current_command: Some("old-command".to_owned()),
            worktree_handle: Some("feature".to_owned()),
            worktree_path: Some("/repo__worktrees/feature".to_owned()),
            branch: Some("feature".to_owned()),
            session_name: "old-session".to_owned(),
            window_name: "old-window".to_owned(),
            window_id: window_id.to_owned(),
        }
    }

    fn pane_snapshot(
        pane_id: &str,
        window_id: &str,
        session_name: &str,
        window_name: &str,
        title: Option<&str>,
        current_command: Option<&str>,
    ) -> TmuxPaneSnapshot {
        TmuxPaneSnapshot {
            session_name: session_name.to_owned(),
            window_id: window_id.to_owned(),
            window_name: window_name.to_owned(),
            pane_id: pane_id.to_owned(),
            title: title.map(str::to_owned),
            current_command: current_command.map(str::to_owned),
            pane_active: false,
            window_active: false,
            session_attached: false,
            kmux_role: None,
        }
    }
}
