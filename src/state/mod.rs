//! Persistence surfaces for kmux-owned state.
//!
//! Workspace graph metadata is repo-local under Git's common dir, while external
//! agent observation state is stored separately through the agent state store.

pub mod workspace;

mod agent;

pub use agent::{
    AgentLocationHints, AgentObservationKey, AgentObservationState, AgentSessionKey, AgentStatus,
    StateStore, next_observation_timing, now_unix_seconds,
};

#[cfg(test)]
pub mod test_support {
    /// Open an agent state store at a caller-provided path for tests.
    pub fn store_with_path(
        base_path: impl Into<std::path::PathBuf>,
    ) -> anyhow::Result<super::StateStore> {
        super::agent::test_support::store_with_path(base_path)
    }
}
