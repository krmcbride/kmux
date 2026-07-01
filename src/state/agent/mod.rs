mod model;
mod store;
mod timing;

pub use model::{
    AgentLocationHints, AgentObservationKey, AgentObservationState, AgentSessionKey, AgentStatus,
};
pub use store::{StateStore, now_unix_seconds};
pub use timing::next_observation_timing;

#[cfg(test)]
pub(super) mod test_support {
    /// Open an agent state store at a caller-provided path for tests.
    pub fn store_with_path(
        base_path: impl Into<std::path::PathBuf>,
    ) -> anyhow::Result<super::StateStore> {
        super::StateStore::with_path(base_path)
    }
}
