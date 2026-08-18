//! One-shot launcher model, private transport, and pane-side process ownership.
//!
//! The module has three related roles:
//!
//! - [`ResolvedLauncher`] is the validated in-memory executable, static argv, and
//!   optional final input selected by a workflow. It contains no tmux or agent
//!   concepts.
//! - [`PendingLaunch`] is the caller-side capability. It creates a private
//!   one-shot directory, writes a versioned request, builds the controlled hidden
//!   shell command, waits bounded intervals for ingress claim and spawn
//!   acknowledgment, and owns cleanup even on failure.
//! - [`run_ingress`] is the pane-side adapter invoked by that hidden command. It
//!   consumes the request before spawn, validates it again, launches exact argv in
//!   the worktree with inherited TTY streams, acknowledges spawn, and waits/reaps
//!   the child before returning control to the pane shell.
//!
//! Only the current kmux executable and an opaque capability path pass through
//! tmux/shell command text; launcher argv and input stay in mode-restricted
//! transient storage and are removed before the child lifetime. On Unix, ingress
//! also retains foreground-job ownership across catchable terminal signals so a
//! launcher that handles Ctrl-C cannot outlive its parent and race the resumed
//! shell.

mod ingress;
mod pending;
mod protocol;
mod resolved;

pub use ingress::run_ingress;
pub use pending::PendingLaunch;
pub use resolved::ResolvedLauncher;

#[cfg(feature = "internal-adapter-contract-tests")]
pub mod contract_tests;
