//! Sidebar width policy independent of tmux subprocess orchestration.
//!
//! The preferred width is the floored percentage of the complete tmux window,
//! clamped to the configured inclusive range. Physical feasibility is a final
//! cap: the sidebar must leave the recursive content layout's minimum width and
//! one new pane separator inside the same window.

use crate::config::SidebarWidth;

const TMUX_PANE_SEPARATOR_WIDTH: u16 = 1;

/// Calculate the bounded sidebar width for a tmux window.
///
/// The configured minimum is best-effort: a narrow tmux window may force the
/// sidebar smaller so the existing content layout and new separator still fit.
/// The result remains at least one cell as a defensive fallback for inconsistent
/// or extremely narrow geometry reported by tmux.
pub(super) fn target_width(
    policy: SidebarWidth,
    window_width: u16,
    minimum_content_width: u16,
) -> u16 {
    let proportional = (u32::from(window_width) * u32::from(policy.percent)) / 100;
    let proportional = u16::try_from(proportional).unwrap_or(u16::MAX);
    let configured = proportional.clamp(policy.min, policy.max);
    let feasible = window_width
        .saturating_sub(minimum_content_width.saturating_add(TMUX_PANE_SEPARATOR_WIDTH))
        .max(1);
    configured.min(feasible)
}

#[cfg(test)]
mod tests {
    use super::*;

    const POLICY: SidebarWidth = SidebarWidth {
        min: 36,
        percent: 20,
        max: 52,
    };

    #[test]
    fn target_width_uses_minimum_proportional_and_maximum_regions() {
        assert_eq!(target_width(POLICY, 120, 1), 36);
        assert_eq!(target_width(POLICY, 210, 1), 42);
        assert_eq!(target_width(POLICY, 320, 1), 52);
    }

    #[test]
    fn target_width_uses_inclusive_bounds_and_floor_division() {
        assert_eq!(target_width(POLICY, 180, 1), 36);
        assert_eq!(target_width(POLICY, 209, 1), 41);
        assert_eq!(target_width(POLICY, 260, 1), 52);
    }

    #[test]
    fn target_width_leaves_room_for_content_pane_and_separator() {
        assert_eq!(target_width(POLICY, 20, 1), 18);
        assert_eq!(target_width(POLICY, 20, 3), 16);
        assert_eq!(target_width(POLICY, 1, 1), 1);
    }
}
