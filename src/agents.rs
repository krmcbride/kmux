use anyhow::Result;

use crate::state::{AgentState, StateStore};
use crate::tmux::Tmux;

pub(crate) fn active_agents(store: &StateStore, tmux: &Tmux) -> Result<Vec<AgentState>> {
    let instance_id = tmux.instance_id();
    let mut agents = Vec::new();
    for mut agent in store.list_agents()? {
        if agent.pane_key.backend != "tmux" || agent.pane_key.instance != instance_id {
            continue;
        }

        match tmux.pane_context(&agent.pane_key.pane_id) {
            Ok(context) if context.window_id == agent.window_id => {
                agent.session_name = context.session_name;
                agent.window_name = context.window_name;
                agent.window_id = context.window_id;
                if let Ok(details) = tmux.pane_details(&agent.pane_key.pane_id) {
                    if details.title.is_some() {
                        agent.pane_title = details.title;
                    }
                    if details.current_command.is_some() {
                        agent.pane_current_command = details.current_command;
                    }
                }
                agents.push(agent);
            }
            Err(_) | Ok(_) => store.delete_agent(&agent.pane_key)?,
        }
    }
    Ok(agents)
}
