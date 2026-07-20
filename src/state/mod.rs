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
    pub use super::agent::test_support::observation_state;

    /// Isolated agent state store whose temporary directory lives as long as the store.
    pub struct StateStoreFixture {
        store: super::StateStore,
        _temp_dir: tempfile::TempDir,
    }

    impl StateStoreFixture {
        /// Create an empty state store rooted in an owned temporary directory.
        pub fn new() -> anyhow::Result<Self> {
            let temp_dir = tempfile::TempDir::new()?;
            let store = store_with_path(temp_dir.path().join("state"))?;
            Ok(Self {
                store,
                _temp_dir: temp_dir,
            })
        }

        /// Borrow the state store while retaining its temporary directory owner.
        pub fn store(&self) -> &super::StateStore {
            &self.store
        }
    }

    /// Open an agent state store at a caller-provided path for tests.
    pub fn store_with_path(
        base_path: impl Into<std::path::PathBuf>,
    ) -> anyhow::Result<super::StateStore> {
        super::agent::test_support::store_with_path(base_path)
    }
}
