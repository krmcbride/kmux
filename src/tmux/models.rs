//! Pane and window metadata returned by tmux queries.

use super::layout::TmuxWindowLayout;

#[derive(Debug, Clone, PartialEq, Eq)]
/// Stable tmux IDs that address one pane and its containing session/window.
pub struct TmuxPaneIdentity {
    pub session_id: String,
    pub window_id: String,
    pub pane_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Pane placement within tmux plus its current filesystem location.
pub struct TmuxPanePlacement {
    pub session_name: String,
    pub window_name: String,
    pub window_index: String,
    pub pane_index: String,
    pub current_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Pane/window dimensions and recursive split structure at one point in time.
pub struct TmuxPaneGeometry {
    pub pane_left: u16,
    pub pane_width: u16,
    pub window_width: u16,
    pub window_layout: TmuxWindowLayout,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Pane/window selection and attachment state at one point in time.
pub struct TmuxPaneActivity {
    pub pane_active: bool,
    pub pane_last: bool,
    pub window_active: bool,
    pub window_last: bool,
    pub session_attached: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Tmux window identity, width, and activity metadata used by workflows.
pub struct TmuxWindow {
    pub session_name: String,
    pub window_id: String,
    pub window_index: String,
    pub window_name: String,
    pub window_width: u16,
    pub layout: TmuxWindowLayout,
    pub active: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Minimal pane metadata used for sidebar ownership and cleanup.
pub struct TmuxPane {
    pub identity: TmuxPaneIdentity,
    pub placement: TmuxPanePlacement,
    pub kmux_role: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Point-in-time pane data used to reconcile agent observations with tmux state.
pub struct TmuxPaneSnapshot {
    pub identity: TmuxPaneIdentity,
    pub placement: TmuxPanePlacement,
    pub geometry: TmuxPaneGeometry,
    pub activity: TmuxPaneActivity,
    pub title: Option<String>,
    pub current_command: Option<String>,
    pub kmux_role: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Visibility state for a tmux pane and its containing window.
pub struct TmuxPaneVisibility {
    pub pane_has_focus: bool,
    pub window_visible: bool,
}
