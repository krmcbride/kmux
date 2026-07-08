//! Row refresh query for the sidebar UI.
//!
//! This module owns the side effects needed to turn persisted agent observations
//! and live tmux state into display-ready sidebar rows. `SidebarApp` consumes the
//! resulting snapshot and only synchronizes UI state.

use anyhow::Result;

use crate::agent::sessions::session_views;
use crate::agent::sidebar::model::{SidebarIcons, SidebarRow, build_rows_with_working_icon};
use crate::agent::workspace_activity::workspace_activity_rows;
use crate::state::{StateStore, now_unix_seconds};
use crate::tmux::{Tmux, TmuxPaneVisibility};

/// Request to refresh rows for the current sidebar state.
#[derive(Debug, Clone, Copy)]
pub(super) struct SidebarRefreshRowsIntent<'a> {
    pub(super) working_icon: Option<&'a str>,
}

/// Display-ready sidebar rows plus the tmux visibility facts that drove focus behavior.
#[derive(Debug, Clone)]
pub(super) struct SidebarRowsSnapshot {
    pub(super) visibility: TmuxPaneVisibility,
    pub(super) rows: Vec<SidebarRow>,
    pub(super) view_count: usize,
}

#[derive(Debug, Clone)]
/// Query service that loads resolved sessions and builds sidebar row models.
pub(super) struct SidebarRowsQuery {
    store: StateStore,
    tmux: Tmux,
    icons: SidebarIcons,
    idle_after_seconds: u64,
}

impl SidebarRowsQuery {
    /// Create a row query backed by the agent observation store and tmux adapter.
    pub(super) fn new(
        store: StateStore,
        tmux: Tmux,
        icons: SidebarIcons,
        idle_after_seconds: u64,
    ) -> Self {
        Self {
            store,
            tmux,
            icons,
            idle_after_seconds,
        }
    }

    /// Return sidebar pane visibility, falling back to a visible, unfocused window on tmux errors.
    pub(super) fn visibility(&self, sidebar_pane_id: Option<&str>) -> TmuxPaneVisibility {
        self.sidebar_visibility(sidebar_pane_id)
    }

    /// Load current sidebar rows using the requested spinner frame and known visibility.
    pub(super) fn load(
        &self,
        intent: SidebarRefreshRowsIntent<'_>,
        visibility: TmuxPaneVisibility,
    ) -> Result<SidebarRowsSnapshot> {
        let views = session_views(&self.store, &self.tmux)?;
        let activities = workspace_activity_rows(&views, now_unix_seconds());
        let view_count = activities.len();
        let rows = build_rows_with_working_icon(
            &activities,
            &self.icons,
            intent.working_icon,
            self.idle_after_seconds,
        );

        Ok(SidebarRowsSnapshot {
            visibility,
            rows,
            view_count,
        })
    }

    fn sidebar_visibility(&self, sidebar_pane_id: Option<&str>) -> TmuxPaneVisibility {
        let Some(pane_id) = sidebar_pane_id else {
            return default_visibility();
        };
        self.tmux
            .pane_visibility(pane_id)
            .unwrap_or_else(|_| default_visibility())
    }
}

fn default_visibility() -> TmuxPaneVisibility {
    TmuxPaneVisibility {
        pane_has_focus: false,
        window_visible: true,
    }
}
