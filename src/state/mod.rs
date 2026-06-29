mod model;
mod store;
mod timing;

pub use model::{
    AgentLocationHints, AgentObservationKey, AgentObservationState, AgentSessionKey, AgentStatus,
};
pub use store::{StateStore, now_unix_seconds};
pub use timing::next_observation_timing;

#[cfg(test)]
pub mod test_support {
    pub fn store_with_path(
        base_path: impl Into<std::path::PathBuf>,
    ) -> anyhow::Result<super::StateStore> {
        super::StateStore::with_path(base_path)
    }
}
