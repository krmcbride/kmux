//! Reconciliation of persisted agent observations with live tmux state.
//!
//! The stable sessions surface separates resolved application models,
//! observation precedence and metadata assembly, and live tmux navigation
//! policy. Consumers depend on the settled models and entrypoint rather than
//! the internal policy layout.

mod model;
mod reconcile;
mod tmux_target;

pub use model::{
    AgentTmuxTarget, AgentTmuxUnavailableReason, AgentTmuxWindowCandidate, ResolvedAgentSession,
    activity_status_priority,
};
// Preserve the facade used by crate test-support consumers even when a
// compilation target does not construct these model types directly.
#[allow(unused_imports)]
pub use model::{ResolvedAgentTarget, ResolvedAgentWorkspace};
pub use reconcile::resolved_agent_sessions;
