//! Tmux pane/window queries and their positional format parsers.
//!
//! One physical window may be linked into multiple sessions. Commands that
//! list all sessions can therefore report the same window and pane IDs more than
//! once; callers that operate on physical windows must deduplicate by ID.

use std::collections::HashMap;

use anyhow::{Context, Result, bail};

use super::context::{
    exact_session_target, is_tmux_pane_id, validate_session_id, validate_window_id,
};
use super::layout::TmuxWindowLayout;
use super::models::{
    TmuxPane, TmuxPaneActivity, TmuxPaneGeometry, TmuxPaneIdentity, TmuxPanePlacement,
    TmuxPaneSnapshot, TmuxPaneVisibility, TmuxWindow,
};
use super::process::{Tmux, bail_tmux, tmux_server_is_absent};

// Unit Separator (U+001F) delimits rich tmux format output where fields such as
// pane titles and current paths may contain tabs.
pub(super) const TMUX_FIELD_SEPARATOR: char = '\u{1f}';

impl Tmux {
    /// Return whether a pane has focus and whether its window is visible to an attached client.
    pub fn pane_visibility(&self, pane_id: &str) -> Result<TmuxPaneVisibility> {
        let output = self.stdout([
            "display-message",
            "-p",
            "-t",
            pane_id,
            "#{pane_active}\t#{window_active}\t#{session_attached}",
        ])?;
        parse_pane_visibility(&output)
    }

    /// List windows in one session, or all sessions when no session is provided.
    pub fn list_windows(&self, session_name: Option<&str>) -> Result<Vec<TmuxWindow>> {
        let format = "#{session_name}\t#{window_id}\t#{window_index}\t#{window_name}\t#{window_width}\t#{window_active}\t#{window_layout}";
        let output = if let Some(session_name) = session_name {
            let target = format!("{}:", exact_session_target(session_name));
            self.stdout(["list-windows", "-t", &target, "-F", format])?
        } else {
            self.stdout(["list-windows", "-a", "-F", format])?
        };
        parse_windows(&output)
    }

    /// List windows in one opaque session id.
    pub fn list_windows_by_id(&self, session_id: &str) -> Result<Vec<TmuxWindow>> {
        validate_session_id(session_id)?;
        let target = format!("{session_id}:");
        let format = "#{session_name}\t#{window_id}\t#{window_index}\t#{window_name}\t#{window_width}\t#{window_active}\t#{window_layout}";
        let output = self.stdout(["list-windows", "-t", &target, "-F", format])?;
        parse_windows(&output)
    }

    /// List lightweight pane identity, placement, cwd, and kmux role snapshots.
    ///
    /// A missing tmux server is represented as an empty list. Other failures
    /// remain errors so callers do not mistake permission or protocol failures
    /// for an empty tmux instance.
    pub fn list_panes(&self) -> Result<Vec<TmuxPane>> {
        let separator = TMUX_FIELD_SEPARATOR;
        let format = format!(
            "#{{session_id}}{separator}#{{session_name}}{separator}#{{window_id}}{separator}#{{window_name}}{separator}#{{window_index}}{separator}#{{pane_id}}{separator}#{{pane_index}}{separator}#{{pane_current_path}}{separator}#{{@kmux_role}}"
        );
        let output = self.output(["list-panes", "-a", "-F", &format])?;
        if !output.status.success() {
            if tmux_server_is_absent(&output.stderr) {
                return Ok(Vec::new());
            }
            return bail_tmux(output);
        }
        parse_panes(&output.stdout)
    }

    /// List rich pane snapshots used by status and sidebar reconciliation.
    pub fn list_pane_snapshots(&self) -> Result<Vec<TmuxPaneSnapshot>> {
        let separator = TMUX_FIELD_SEPARATOR;
        let format = format!(
            "#{{session_id}}{separator}#{{session_name}}{separator}#{{window_id}}{separator}#{{window_name}}{separator}#{{window_index}}{separator}#{{pane_id}}{separator}#{{pane_index}}{separator}#{{pane_left}}{separator}#{{pane_width}}{separator}#{{window_width}}{separator}#{{window_layout}}{separator}#{{pane_title}}{separator}#{{pane_current_command}}{separator}#{{pane_current_path}}{separator}#{{pane_active}}{separator}#{{pane_last}}{separator}#{{window_active}}{separator}#{{window_last_flag}}{separator}#{{session_attached}}{separator}#{{@kmux_role}}"
        );
        let output = self.stdout(["list-panes", "-a", "-F", &format])?;
        parse_pane_snapshots(&output)
    }

    /// Return whether a session contains a window with an exact name match.
    pub fn window_exists_by_name(&self, session_name: &str, window_name: &str) -> Result<bool> {
        Ok(self
            .list_windows(Some(session_name))?
            .iter()
            .any(|window| window.window_name == window_name))
    }

    /// Return whether an opaque session id contains a window with an exact name match.
    pub fn window_exists_by_name_by_id(&self, session_id: &str, window_name: &str) -> Result<bool> {
        Ok(self
            .list_windows_by_id(session_id)?
            .iter()
            .any(|window| window.window_name == window_name))
    }
}

fn parse_windows(output: &str) -> Result<Vec<TmuxWindow>> {
    output.lines().map(parse_window).collect()
}

fn parse_panes(output: &str) -> Result<Vec<TmuxPane>> {
    let panes = output.lines().map(parse_pane).collect::<Result<Vec<_>>>()?;
    validate_consistent_session_names(panes.iter().map(|pane| {
        (
            pane.identity.session_id.as_str(),
            pane.placement.session_name.as_str(),
        )
    }))?;
    Ok(panes)
}

fn parse_pane_snapshots(output: &str) -> Result<Vec<TmuxPaneSnapshot>> {
    let panes = output
        .lines()
        .map(parse_pane_snapshot)
        .collect::<Result<Vec<_>>>()?;
    validate_consistent_session_names(panes.iter().map(|pane| {
        (
            pane.identity.session_id.as_str(),
            pane.placement.session_name.as_str(),
        )
    }))?;
    Ok(panes)
}

fn validate_consistent_session_names<'a>(
    sessions: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> Result<()> {
    let mut names_by_id = HashMap::new();
    for (session_id, session_name) in sessions {
        if let Some(previous_name) = names_by_id.insert(session_id, session_name)
            && previous_name != session_name
        {
            bail!("inconsistent tmux pane records for session id {session_id:?}");
        }
    }
    Ok(())
}

fn parse_window(line: &str) -> Result<TmuxWindow> {
    let fields = line.split('\t').collect::<Vec<_>>();
    if fields.len() != 7 {
        bail!("unexpected tmux window format: {line:?}");
    }

    Ok(TmuxWindow {
        session_name: fields[0].to_owned(),
        window_id: fields[1].to_owned(),
        window_index: fields[2].to_owned(),
        window_name: fields[3].to_owned(),
        window_width: parse_window_u16(line, "window_width", fields[4])?,
        layout: TmuxWindowLayout::parse(fields[6])?,
        active: fields[5] == "1",
    })
}

fn parse_window_u16(line: &str, field: &str, value: &str) -> Result<u16> {
    value
        .parse::<u16>()
        .with_context(|| format!("invalid {field} value {value:?} in tmux window record {line:?}"))
}

fn parse_pane(line: &str) -> Result<TmuxPane> {
    let fields = line.split(TMUX_FIELD_SEPARATOR).collect::<Vec<_>>();
    if fields.len() != 9 {
        bail!("unexpected tmux pane format: {line:?}");
    }
    validate_session_id(fields[0])?;
    validate_window_id(fields[2])?;
    if !is_tmux_pane_id(fields[5]) {
        bail!("invalid tmux pane id {:?}", fields[5]);
    }

    Ok(TmuxPane {
        identity: TmuxPaneIdentity {
            session_id: fields[0].to_owned(),
            window_id: fields[2].to_owned(),
            pane_id: fields[5].to_owned(),
        },
        placement: TmuxPanePlacement {
            session_name: fields[1].to_owned(),
            window_name: fields[3].to_owned(),
            window_index: fields[4].to_owned(),
            pane_index: fields[6].to_owned(),
            current_path: non_empty_string(fields[7]),
        },
        kmux_role: non_empty_string(fields[8]),
    })
}

// Use a unit-separator field delimiter for rich pane snapshots because tmux pane
// titles and paths can contain tabs.
fn parse_pane_snapshot(line: &str) -> Result<TmuxPaneSnapshot> {
    let fields = line.split(TMUX_FIELD_SEPARATOR).collect::<Vec<_>>();
    if fields.len() != 20 {
        bail!("unexpected tmux pane snapshot format: {line:?}");
    }
    validate_session_id(fields[0])?;
    validate_window_id(fields[2])?;
    if !is_tmux_pane_id(fields[5]) {
        bail!("invalid tmux pane id {:?}", fields[5]);
    }

    Ok(TmuxPaneSnapshot {
        identity: TmuxPaneIdentity {
            session_id: fields[0].to_owned(),
            window_id: fields[2].to_owned(),
            pane_id: fields[5].to_owned(),
        },
        placement: TmuxPanePlacement {
            session_name: fields[1].to_owned(),
            window_name: fields[3].to_owned(),
            window_index: fields[4].to_owned(),
            pane_index: fields[6].to_owned(),
            current_path: non_empty_string(fields[13]),
        },
        geometry: TmuxPaneGeometry {
            pane_left: parse_pane_snapshot_u16(line, "pane_left", fields[7])?,
            pane_width: parse_pane_snapshot_u16(line, "pane_width", fields[8])?,
            window_width: parse_pane_snapshot_u16(line, "window_width", fields[9])?,
            window_layout: TmuxWindowLayout::parse(fields[10])?,
        },
        activity: TmuxPaneActivity {
            pane_active: tmux_bool(fields[14]),
            pane_last: tmux_bool(fields[15]),
            window_active: tmux_bool(fields[16]),
            window_last: tmux_bool(fields[17]),
            session_attached: tmux_attached(fields[18]),
        },
        title: non_empty_string(fields[11]),
        current_command: non_empty_string(fields[12]),
        kmux_role: non_empty_string(fields[19]),
    })
}

fn parse_pane_snapshot_u16(line: &str, field_name: &str, value: &str) -> Result<u16> {
    value.parse::<u16>().with_context(|| {
        format!("invalid tmux pane snapshot {field_name} value {value:?} in line: {line:?}")
    })
}

fn parse_pane_visibility(output: &str) -> Result<TmuxPaneVisibility> {
    let fields = output.trim_end().split('\t').collect::<Vec<_>>();
    if fields.len() != 3 {
        bail!("unexpected tmux pane visibility format: {output:?}");
    }

    let pane_active = tmux_bool(fields[0]);
    let window_active = tmux_bool(fields[1]);
    let session_attached = tmux_attached(fields[2]);
    Ok(TmuxPaneVisibility {
        pane_has_focus: pane_active && window_active && session_attached,
        window_visible: window_active && session_attached,
    })
}

fn tmux_bool(value: &str) -> bool {
    value == "1"
}

fn tmux_attached(value: &str) -> bool {
    value.parse::<u16>().unwrap_or(0) > 0
}

fn non_empty_string(value: &str) -> Option<String> {
    Some(value.to_owned()).filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use anyhow::Result;

    use super::{
        TMUX_FIELD_SEPARATOR, parse_pane_snapshots, parse_pane_visibility, parse_panes,
        parse_window, parse_windows,
    };
    use crate::tmux::TmuxPaneVisibility;

    #[test]
    fn parses_lightweight_pane_snapshots() -> Result<()> {
        let separator = TMUX_FIELD_SEPARATOR;
        let output = format!(
            "$1{separator}project:alpha{separator}@1{separator}main{separator}1{separator}%1{separator}0{separator}/repo/project alpha{separator}\n$1{separator}project:alpha{separator}@2{separator}feature{separator}2{separator}%2{separator}0{separator}/repo/project alpha/worktree{separator}sidebar\n$2{separator}other{separator}@3{separator}main{separator}1{separator}%3{separator}0{separator}/repo/other{separator}"
        );

        let panes = parse_panes(&output)?;

        assert_eq!(panes.len(), 3);
        assert_eq!(panes[0].identity.session_id, "$1");
        assert_eq!(panes[0].placement.session_name, "project:alpha");
        assert_eq!(panes[0].identity.window_id, "@1");
        assert_eq!(panes[0].placement.window_name, "main");
        assert_eq!(panes[0].identity.pane_id, "%1");
        assert_eq!(
            panes[0].placement.current_path.as_deref(),
            Some("/repo/project alpha")
        );
        assert_eq!(panes[1].kmux_role.as_deref(), Some("sidebar"));
        assert_eq!(panes[2].identity.session_id, "$2");
        Ok(())
    }

    #[test]
    fn pane_snapshots_reject_inconsistent_names_for_one_session_id() {
        let separator = TMUX_FIELD_SEPARATOR;
        let output = format!(
            "$1{separator}project{separator}@1{separator}main{separator}1{separator}%1{separator}0{separator}/repo/project{separator}\n$1{separator}renamed{separator}@2{separator}other{separator}2{separator}%2{separator}0{separator}/repo/project{separator}"
        );

        let error = parse_panes(&output)
            .expect_err("one opaque session id must not have conflicting names");

        assert!(
            error
                .to_string()
                .contains("inconsistent tmux pane records for session id \"$1\"")
        );
    }

    #[test]
    fn parses_pane_snapshots() -> Result<()> {
        let separator = TMUX_FIELD_SEPARATOR;
        let output = format!(
            "$1{separator}project{separator}@1{separator}kmux-feature{separator}1{separator}%2{separator}1{separator}0{separator}42{separator}120{separator}b25d,120x24,0,0,2{separator}kmux{separator}nvim{separator}/repo/feature{separator}1{separator}0{separator}1{separator}0{separator}2{separator}sidebar\n$1{separator}project{separator}@2{separator}empty{separator}2{separator}%3{separator}1{separator}0{separator}80{separator}80{separator}b25d,80x24,0,0,3{separator}{separator}{separator}{separator}0{separator}1{separator}0{separator}1{separator}0{separator}"
        );

        let panes = parse_pane_snapshots(&output)?;

        assert_eq!(panes.len(), 2);
        assert_eq!(panes[0].identity.session_id, "$1");
        assert_eq!(panes[0].placement.session_name, "project");
        assert_eq!(panes[0].identity.window_id, "@1");
        assert_eq!(panes[0].placement.window_index, "1");
        assert_eq!(panes[0].placement.window_name, "kmux-feature");
        assert_eq!(panes[0].identity.pane_id, "%2");
        assert_eq!(panes[0].placement.pane_index, "1");
        assert_eq!(panes[0].geometry.pane_left, 0);
        assert_eq!(panes[0].geometry.pane_width, 42);
        assert_eq!(panes[0].geometry.window_width, 120);
        assert_eq!(panes[0].geometry.window_layout.minimum_width(None), 1);
        assert_eq!(panes[0].title.as_deref(), Some("kmux"));
        assert_eq!(panes[0].current_command.as_deref(), Some("nvim"));
        assert_eq!(
            panes[0].placement.current_path.as_deref(),
            Some("/repo/feature")
        );
        assert!(panes[0].activity.pane_active);
        assert!(!panes[0].activity.pane_last);
        assert!(panes[0].activity.window_active);
        assert!(!panes[0].activity.window_last);
        assert!(panes[0].activity.session_attached);
        assert_eq!(panes[0].kmux_role.as_deref(), Some("sidebar"));
        assert_eq!(panes[1].title, None);
        assert_eq!(panes[1].current_command, None);
        assert!(!panes[1].activity.pane_active);
        assert!(panes[1].activity.pane_last);
        assert!(!panes[1].activity.window_active);
        assert!(panes[1].activity.window_last);
        assert!(!panes[1].activity.session_attached);
        assert_eq!(panes[1].kmux_role, None);
        Ok(())
    }

    #[test]
    fn malformed_pane_snapshot_geometry_reports_field_context() {
        let separator = TMUX_FIELD_SEPARATOR;
        let output = format!(
            "$1{separator}project{separator}@1{separator}kmux-feature{separator}1{separator}%2{separator}1{separator}0{separator}wide{separator}120{separator}b25d,120x24,0,0,2{separator}kmux{separator}nvim{separator}/repo/feature{separator}1{separator}0{separator}1{separator}0{separator}2{separator}sidebar"
        );

        let error = parse_pane_snapshots(&output)
            .expect_err("malformed numeric geometry should fail parsing");
        let message = error.to_string();

        assert!(message.contains("pane_width"));
        assert!(message.contains("wide"));
        assert!(message.contains("tmux pane snapshot"));
    }

    #[test]
    fn parses_windows() -> Result<()> {
        let windows = parse_windows(
            "project\t@1\t2\tkmux-feature\t120\t1\tb25d,120x24,0,0,1\nproject\t@2\t3\tscratch\t80\t0\tb25d,80x24,0,0,2",
        )?;

        assert_eq!(windows.len(), 2);
        assert_eq!(windows[0].session_name, "project");
        assert_eq!(windows[0].window_id, "@1");
        assert_eq!(windows[0].window_index, "2");
        assert_eq!(windows[0].window_name, "kmux-feature");
        assert_eq!(windows[0].window_width, 120);
        assert_eq!(windows[0].layout.minimum_width(None), 1);
        assert!(windows[0].active);
        assert!(!windows[1].active);
        Ok(())
    }

    #[test]
    fn parses_window() -> Result<()> {
        let window = parse_window("project\t@1\t2\tkmux-feature\t120\t1\tb25d,120x24,0,0,1")?;

        assert_eq!(window.session_name, "project");
        assert_eq!(window.window_id, "@1");
        assert_eq!(window.window_name, "kmux-feature");
        assert_eq!(window.window_width, 120);
        assert!(window.active);
        Ok(())
    }

    #[test]
    fn malformed_window_width_reports_field_context() {
        let error = parse_window("project\t@1\t2\tkmux-feature\twide\t1\tb25d,120x24,0,0,1")
            .expect_err("malformed window width should fail parsing");
        let message = error.to_string();

        assert!(message.contains("window_width"));
        assert!(message.contains("wide"));
        assert!(message.contains("tmux window record"));
    }

    #[test]
    fn parses_pane_visibility_from_tmux_flags() -> Result<()> {
        assert_eq!(
            parse_pane_visibility("1\t1\t1")?,
            TmuxPaneVisibility {
                pane_has_focus: true,
                window_visible: true,
            }
        );
        assert_eq!(
            parse_pane_visibility("0\t1\t1")?,
            TmuxPaneVisibility {
                pane_has_focus: false,
                window_visible: true,
            }
        );
        assert_eq!(
            parse_pane_visibility("1\t0\t1")?,
            TmuxPaneVisibility {
                pane_has_focus: false,
                window_visible: false,
            }
        );
        assert_eq!(
            parse_pane_visibility("1\t1\t0")?,
            TmuxPaneVisibility {
                pane_has_focus: false,
                window_visible: false,
            }
        );
        Ok(())
    }
}
