//! Tmux subprocess adapter and metadata model.
//!
//! This module owns tmux target syntax, format-string parsing, user-option
//! access, and socket/environment handling. Higher-level workflows should use
//! this boundary instead of constructing tmux commands or parsing tmux output.

mod context;
mod layout;
mod models;
mod mutations;
mod options;
mod process;
mod queries;

pub use context::TmuxContext;
// Preserve crate-visible test construction and consumer imports even when a
// particular compilation target does not instantiate every metadata shape.
#[allow(unused_imports)]
pub use layout::TmuxWindowLayout;
#[allow(unused_imports)]
pub use models::{
    TmuxPane, TmuxPaneActivity, TmuxPaneGeometry, TmuxPaneIdentity, TmuxPanePlacement,
    TmuxPaneSnapshot, TmuxPaneVisibility, TmuxWindow,
};
pub use process::Tmux;
// Preserve the existing crate-visible raw-output type even though current callers
// use it only through `Tmux::output`'s inferred return value.
#[allow(unused_imports)]
pub use process::TmuxOutput;

#[cfg(test)]
pub mod test_support {
    use super::TmuxWindowLayout;

    /// Build a horizontal window layout for pane-snapshot tests.
    pub fn test_window_layout(pane_ids: &[&str]) -> TmuxWindowLayout {
        super::layout::test_window_layout(pane_ids)
    }
}

#[cfg(feature = "internal-adapter-contract-tests")]
/// Crate-wide exception: sidebar contracts need the same owned tmux server
/// fixture as adapter contracts, and no narrower visibility spans those modules.
pub(crate) mod contract_support;

#[cfg(feature = "internal-adapter-contract-tests")]
pub mod contract_tests;
