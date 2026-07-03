//! Persistence surfaces for kmux-owned state.
//!
//! Workspace graph metadata is repo-local under Git's common dir, while external
//! agent observation and sidebar UI state are stored separately under XDG state.

pub mod workspace;

mod agent;
mod sidebar;

pub use agent::{
    AgentLocationHints, AgentObservationKey, AgentObservationState, AgentSessionKey, AgentStatus,
    StateStore, next_observation_timing, now_unix_seconds,
};
pub use sidebar::SidebarSelectionStore;

#[cfg(test)]
pub mod test_support {
    /// Open an agent state store at a caller-provided path for tests.
    pub fn store_with_path(
        base_path: impl Into<std::path::PathBuf>,
    ) -> anyhow::Result<super::StateStore> {
        super::agent::test_support::store_with_path(base_path)
    }

    /// Open a sidebar selection store at a caller-provided path for tests.
    pub fn sidebar_selection_store_with_path(
        base_path: impl Into<std::path::PathBuf>,
    ) -> super::SidebarSelectionStore {
        super::sidebar::test_support::store_with_path(base_path)
    }
}
