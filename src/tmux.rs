//! tmux subprocess adapter and metadata model.
//!
//! This module owns tmux target syntax, format-string parsing, user-option
//! access, and socket/environment handling. Higher-level workflows should use
//! this boundary instead of constructing tmux commands or parsing tmux output.
//!
//! A tmux window is tiled by a recursive split tree. Pane leaves may be grouped
//! side by side, where their widths and intervening separators add together, or
//! stacked, where they share width and the widest child determines the group's
//! minimum. Retaining this topology matters because a flat list of pane
//! rectangles cannot describe every nested layout's resize constraints.
//!
//! One physical window may also be linked into multiple sessions. Commands that
//! list all sessions can therefore report the same window and pane IDs more than
//! once; callers that operate on physical windows must deduplicate by ID.

use std::ffi::OsString;
use std::path::Path;
use std::process::{Command, ExitStatus};

use anyhow::{Context, Result, bail};

use crate::telemetry;

#[derive(Debug, Clone, Default)]
/// Thin adapter for running tmux commands, optionally against a specific socket.
pub struct Tmux {
    socket_name: Option<OsString>,
    clear_client_env: bool,
    env: Vec<(OsString, OsString)>,
}

#[derive(Debug)]
/// Raw tmux subprocess output with UTF-8-lossy stdout and stderr text.
pub struct TmuxOutput {
    pub status: ExitStatus,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Current tmux session, window, and pane identity for command workflows.
pub struct TmuxContext {
    pub session_name: String,
    pub session_id: String,
    pub window_name: String,
    pub window_id: String,
    pub pane_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Parsed recursive `#{window_layout}` topology used to calculate minimum pane geometry.
pub struct TmuxWindowLayout {
    root: TmuxLayoutCell,
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
    pub session_name: String,
    pub window_id: String,
    pub pane_id: String,
    pub kmux_role: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Point-in-time pane data used to reconcile agent observations with tmux state.
pub struct TmuxPaneSnapshot {
    pub session_name: String,
    pub window_id: String,
    pub window_index: String,
    pub window_name: String,
    pub pane_id: String,
    pub pane_index: String,
    pub pane_left: u16,
    pub pane_width: u16,
    pub window_width: u16,
    pub window_layout: TmuxWindowLayout,
    pub title: Option<String>,
    pub current_command: Option<String>,
    pub current_path: Option<String>,
    pub pane_active: bool,
    pub pane_last: bool,
    pub window_active: bool,
    pub window_last: bool,
    pub session_attached: bool,
    pub kmux_role: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Visibility state for a tmux pane and its containing window.
pub struct TmuxPaneVisibility {
    pub pane_has_focus: bool,
    pub window_visible: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TmuxLayoutCell {
    /// A physical pane leaf, identified without tmux's leading `%` sigil.
    Pane(String),
    /// Children arranged side by side, consuming the sum of their widths and separators.
    Horizontal(Vec<Self>),
    /// Children stacked top to bottom, sharing the width of their widest child.
    Vertical(Vec<Self>),
}

impl TmuxWindowLayout {
    /// Return the minimum cell width for this recursive layout.
    ///
    /// Each pane needs one content cell. Side-by-side children add their widths
    /// and one separator per boundary, while stacked children use the largest
    /// child width. Excluding a pane models the content layout that remains when
    /// an existing sidebar is removed from the tree before recalculating its size.
    pub fn minimum_width(&self, excluded_pane_id: Option<&str>) -> u16 {
        let excluded_pane_id = excluded_pane_id.map(|pane_id| pane_id.trim_start_matches('%'));
        self.root.minimum_width(excluded_pane_id).unwrap_or(1)
    }

    fn parse(value: &str) -> Result<Self> {
        let (_, body) = value
            .split_once(',')
            .with_context(|| format!("invalid tmux window layout {value:?}: missing checksum"))?;
        let mut parser = TmuxLayoutParser::new(body, value);
        let root = parser.parse_cell()?;
        if !parser.is_finished() {
            bail!(
                "invalid tmux window layout {value:?}: trailing input at byte {}",
                parser.offset
            );
        }
        Ok(Self { root })
    }
}

impl TmuxLayoutCell {
    fn minimum_width(&self, excluded_pane_id: Option<&str>) -> Option<u16> {
        match self {
            Self::Pane(pane_id) => (excluded_pane_id != Some(pane_id.as_str())).then_some(1),
            Self::Horizontal(children) => {
                let mut width = 0u16;
                let mut count = 0u16;
                for child_width in children
                    .iter()
                    .filter_map(|child| child.minimum_width(excluded_pane_id))
                {
                    if count > 0 {
                        width = width.saturating_add(1);
                    }
                    width = width.saturating_add(child_width);
                    count = count.saturating_add(1);
                }
                (count > 0).then_some(width)
            }
            Self::Vertical(children) => children
                .iter()
                .filter_map(|child| child.minimum_width(excluded_pane_id))
                .max(),
        }
    }
}

struct TmuxLayoutParser<'a> {
    body: &'a str,
    original: &'a str,
    offset: usize,
}

impl<'a> TmuxLayoutParser<'a> {
    fn new(body: &'a str, original: &'a str) -> Self {
        Self {
            body,
            original,
            offset: 0,
        }
    }

    fn parse_cell(&mut self) -> Result<TmuxLayoutCell> {
        self.parse_number_before(b'x', "width")?;
        self.parse_number_before(b',', "height")?;
        self.parse_number_before(b',', "x offset")?;
        self.parse_number("y offset")?;

        match self.peek() {
            Some(b',') => {
                self.offset += 1;
                let pane_id = self.parse_number("pane id")?;
                Ok(TmuxLayoutCell::Pane(pane_id.to_owned()))
            }
            Some(b'{') => self
                .parse_children(b'{', b'}')
                .map(TmuxLayoutCell::Horizontal),
            Some(b'[') => self
                .parse_children(b'[', b']')
                .map(TmuxLayoutCell::Vertical),
            _ => bail!(
                "invalid tmux window layout {:?}: expected pane or child layout at byte {}",
                self.original,
                self.offset
            ),
        }
    }

    fn parse_children(&mut self, open: u8, close: u8) -> Result<Vec<TmuxLayoutCell>> {
        self.expect(open)?;
        let mut children = Vec::new();
        loop {
            children.push(self.parse_cell()?);
            match self.peek() {
                Some(b',') => self.offset += 1,
                Some(value) if value == close => {
                    self.offset += 1;
                    return Ok(children);
                }
                _ => {
                    bail!(
                        "invalid tmux window layout {:?}: expected separator or {:?} at byte {}",
                        self.original,
                        char::from(close),
                        self.offset
                    )
                }
            }
        }
    }

    fn parse_number_before(&mut self, delimiter: u8, field: &str) -> Result<()> {
        self.parse_number(field)?;
        self.expect(delimiter)
    }

    fn parse_number(&mut self, field: &str) -> Result<&'a str> {
        let start = self.offset;
        while self.peek().is_some_and(|value| value.is_ascii_digit()) {
            self.offset += 1;
        }
        if start == self.offset {
            bail!(
                "invalid tmux window layout {:?}: expected {field} at byte {}",
                self.original,
                self.offset
            );
        }
        Ok(&self.body[start..self.offset])
    }

    fn expect(&mut self, expected: u8) -> Result<()> {
        if self.peek() != Some(expected) {
            bail!(
                "invalid tmux window layout {:?}: expected {:?} at byte {}",
                self.original,
                char::from(expected),
                self.offset
            );
        }
        self.offset += 1;
        Ok(())
    }

    fn peek(&self) -> Option<u8> {
        self.body.as_bytes().get(self.offset).copied()
    }

    fn is_finished(&self) -> bool {
        self.offset == self.body.len()
    }
}

// Unit Separator (U+001F) delimits rich tmux format output where fields such as
// pane titles and current paths may contain tabs.
const TMUX_FIELD_SEPARATOR: char = '\u{1f}';

impl Tmux {
    /// Create an adapter for the default tmux socket and current process environment.
    pub fn new() -> Self {
        Self::default()
    }

    /// Create an adapter from kmux-specific environment overrides.
    pub fn from_env() -> Self {
        let mut tmux = if let Some(socket_name) = std::env::var_os("KMUX_TMUX_SOCKET_NAME") {
            Self::with_socket_name(socket_name)
        } else {
            Self::new()
        };

        if let Some(tmux_tmpdir) = std::env::var_os("KMUX_TMUX_TMPDIR") {
            tmux = tmux.with_env("TMUX_TMPDIR", tmux_tmpdir);
        }

        tmux
    }

    /// Return a stable identifier for the tmux instance observed by this adapter.
    pub fn instance_id(&self) -> String {
        self.socket_name
            .as_ref()
            .map(|socket_name| socket_name.to_string_lossy().into_owned())
            .filter(|socket_name| !socket_name.is_empty())
            .unwrap_or_else(|| "default".to_owned())
    }

    /// Run a tmux command and return raw output without requiring a successful exit status.
    pub fn output<I, S>(&self, args: I) -> Result<TmuxOutput>
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        let args = args.into_iter().map(Into::into).collect::<Vec<_>>();
        let display_args = display_args(&args);
        let command_name = command_name(&args);
        let output = telemetry::timed_result_event!(
            "subprocess",
            {
                program = "tmux",
                command = %command_name,
            },
            || {
                let mut command = Command::new("tmux");
                if let Some(socket_name) = &self.socket_name {
                    command.arg("-L").arg(socket_name);
                }
                if self.clear_client_env {
                    command.env_remove("TMUX");
                    command.env_remove("TMUX_PANE");
                }
                for (key, value) in &self.env {
                    command.env(key, value);
                }
                command
                    .args(&args)
                    .output()
                    .with_context(|| format!("failed to run tmux {display_args}"))
            },
            ok |output| {
                status_code = output.status.code().unwrap_or(-1),
                success = output.status.success(),
                stdout_bytes = output.stdout.len(),
                stderr_bytes = output.stderr.len(),
            },
        )?;

        Ok(TmuxOutput {
            status: output.status,
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }

    /// Run a tmux command, require success, and return trimmed stdout.
    pub fn stdout<I, S>(&self, args: I) -> Result<String>
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        let output = self.output(args)?;
        if !output.status.success() {
            return bail_tmux(output);
        }
        Ok(output.stdout.trim_end().to_owned())
    }

    /// Return context for the current pane when running inside tmux, otherwise `None`.
    pub fn current_context(&self) -> Result<Option<TmuxContext>> {
        let pane_id = std::env::var("TMUX_PANE").ok();
        if let Some(pane_id) = pane_id {
            return self.pane_context(&pane_id).map(Some);
        }

        if std::env::var_os("TMUX").is_none() {
            return Ok(None);
        }

        self.query_context(None).map(Some)
    }

    /// Create a detached window in `session_name`, optionally sending a startup command.
    pub fn create_window_with_command(
        &self,
        session_name: &str,
        window_name: &str,
        cwd: &Path,
        command: Option<&str>,
    ) -> Result<String> {
        let target = format!("{session_name}:");
        let args = vec![
            OsString::from("new-window"),
            OsString::from("-d"),
            OsString::from("-t"),
            OsString::from(target),
            OsString::from("-n"),
            OsString::from(window_name),
            OsString::from("-c"),
            cwd.as_os_str().to_os_string(),
            OsString::from("-P"),
            OsString::from("-F"),
            OsString::from("#{pane_id}"),
        ];
        let pane_id = self.stdout(args)?;
        self.stdout([
            "set-option",
            "-w",
            "-t",
            &pane_id,
            "automatic-rename",
            "off",
        ])?;
        if let Some(command) = command.filter(|command| !command.trim().is_empty()) {
            self.send_keys(&pane_id, command)?;
        }
        Ok(pane_id)
    }

    /// Send one tmux key token to a pane.
    pub fn send_key(&self, pane_id: &str, key: &str) -> Result<()> {
        self.stdout(["send-keys", "-t", pane_id, key])?;
        Ok(())
    }

    /// Select a window by session and exact window name.
    pub fn select_window(&self, session_name: &str, window_name: &str) -> Result<()> {
        let target = window_target(session_name, window_name);
        self.stdout(["select-window", "-t", &target])?;
        Ok(())
    }

    /// Select a physical window by id within one exact tmux session.
    pub fn select_window_id_in_session(&self, session_name: &str, window_id: &str) -> Result<()> {
        let target = format!("={session_name}:{window_id}");
        self.stdout(["select-window", "-t", &target])?;
        Ok(())
    }

    /// Select a pane by tmux pane id.
    pub fn select_pane(&self, pane_id: &str) -> Result<()> {
        self.stdout(["select-pane", "-t", pane_id])?;
        Ok(())
    }

    /// Set the tmux pane title displayed by clients that expose pane titles.
    pub fn set_pane_title(&self, pane_id: &str, title: &str) -> Result<()> {
        self.stdout(["select-pane", "-t", pane_id, "-T", title])?;
        Ok(())
    }

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

    /// Switch the attached tmux client to a session.
    pub fn switch_client_to_session(&self, session_name: &str) -> Result<()> {
        let target = format!("={session_name}");
        self.stdout(["switch-client", "-t", &target])?;
        Ok(())
    }

    /// Kill a tmux window by exact name within a session.
    pub fn kill_window(&self, session_name: &str, window_name: &str) -> Result<()> {
        if !self.window_exists_by_name(session_name, window_name)? {
            bail!("tmux window '{window_name}' does not exist in session '{session_name}'");
        }

        let target = window_target(session_name, window_name);
        self.stdout(["kill-window", "-t", &target])?;
        Ok(())
    }

    /// List windows in one session, or all sessions when no session is provided.
    pub fn list_windows(&self, session_name: Option<&str>) -> Result<Vec<TmuxWindow>> {
        let format = "#{session_name}\t#{window_id}\t#{window_index}\t#{window_name}\t#{window_width}\t#{window_active}\t#{window_layout}";
        let output = if let Some(session_name) = session_name {
            let target = format!("={session_name}:");
            self.stdout(["list-windows", "-t", &target, "-F", format])?
        } else {
            self.stdout(["list-windows", "-a", "-F", format])?
        };
        parse_windows(&output)
    }

    /// List panes with their kmux role option when set.
    pub fn list_panes(&self) -> Result<Vec<TmuxPane>> {
        let format = "#{session_name}\t#{window_id}\t#{pane_id}\t#{@kmux_role}";
        let output = self.stdout(["list-panes", "-a", "-F", format])?;
        parse_panes(&output)
    }

    /// List rich pane snapshots used by status and sidebar reconciliation.
    pub fn list_pane_snapshots(&self) -> Result<Vec<TmuxPaneSnapshot>> {
        let separator = TMUX_FIELD_SEPARATOR;
        let format = format!(
            "#{{session_name}}{separator}#{{window_id}}{separator}#{{window_index}}{separator}#{{window_name}}{separator}#{{pane_id}}{separator}#{{pane_index}}{separator}#{{pane_left}}{separator}#{{pane_width}}{separator}#{{window_width}}{separator}#{{window_layout}}{separator}#{{pane_title}}{separator}#{{pane_current_command}}{separator}#{{pane_current_path}}{separator}#{{pane_active}}{separator}#{{pane_last}}{separator}#{{window_active}}{separator}#{{window_last_flag}}{separator}#{{session_attached}}{separator}#{{@kmux_role}}"
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

    /// Set a namespaced tmux window user option on a target.
    pub fn set_window_option(&self, target: &str, option_name: &str, value: &str) -> Result<()> {
        validate_user_option(option_name)?;
        self.stdout(["set-option", "-w", "-t", target, option_name, value])?;
        Ok(())
    }

    /// Read a namespaced tmux window user option, returning `None` when unset or blank.
    pub fn show_window_option(&self, target: &str, option_name: &str) -> Result<Option<String>> {
        validate_user_option(option_name)?;
        let output = self.output(["show-option", "-wqv", "-t", target, option_name])?;
        if !output.status.success() {
            return Ok(None);
        }

        Ok(Some(output.stdout.trim_end().to_owned()).filter(|value| !value.is_empty()))
    }

    /// Unset a namespaced tmux window user option on a target.
    pub fn unset_window_option(&self, target: &str, option_name: &str) -> Result<()> {
        validate_user_option(option_name)?;
        self.stdout(["set-option", "-uw", "-t", target, option_name])?;
        Ok(())
    }

    /// Set a namespaced tmux pane user option on a target.
    pub fn set_pane_option(&self, target: &str, option_name: &str, value: &str) -> Result<()> {
        validate_user_option(option_name)?;
        self.stdout(["set-option", "-p", "-t", target, option_name, value])?;
        Ok(())
    }

    /// Set a namespaced global tmux user option.
    pub fn set_global_option(&self, option_name: &str, value: &str) -> Result<()> {
        validate_user_option(option_name)?;
        self.stdout(["set-option", "-g", option_name, value])?;
        Ok(())
    }

    /// Read a namespaced global tmux user option, returning `None` when unset or blank.
    pub fn show_global_option(&self, option_name: &str) -> Result<Option<String>> {
        validate_user_option(option_name)?;
        let output = self.output(["show-option", "-gqv", option_name])?;
        if !output.status.success() {
            return Ok(None);
        }

        Ok(Some(output.stdout.trim_end().to_owned()).filter(|value| !value.is_empty()))
    }

    /// Unset a namespaced global tmux user option.
    pub fn unset_global_option(&self, option_name: &str) -> Result<()> {
        validate_user_option(option_name)?;
        self.stdout(["set-option", "-gu", option_name])?;
        Ok(())
    }

    /// Set a global tmux hook command.
    pub fn set_hook(&self, hook: &str, command: &str) -> Result<()> {
        self.stdout(["set-hook", "-g", hook, command])?;
        Ok(())
    }

    /// Unset a global tmux hook command.
    pub fn unset_hook(&self, hook: &str) -> Result<()> {
        self.stdout(["set-hook", "-gu", hook])?;
        Ok(())
    }

    /// Create a detached full-height split with a concrete cell width at the window's left edge.
    pub fn split_window_left(
        &self,
        target_window: &str,
        width: u16,
        command: &str,
    ) -> Result<String> {
        let args = vec![
            OsString::from("split-window"),
            OsString::from("-d"),
            OsString::from("-h"),
            OsString::from("-b"),
            OsString::from("-f"),
            OsString::from("-t"),
            OsString::from(target_window),
            OsString::from("-l"),
            OsString::from(width.to_string()),
            OsString::from("-P"),
            OsString::from("-F"),
            OsString::from("#{pane_id}"),
            OsString::from(command),
        ];
        self.stdout(args)
    }

    /// Kill a tmux pane by pane id.
    pub fn kill_pane(&self, pane_id: &str) -> Result<()> {
        self.stdout(["kill-pane", "-t", pane_id])?;
        Ok(())
    }

    /// Resize a pane to an absolute cell width.
    pub fn resize_pane_width(&self, pane_id: &str, width: u16) -> Result<()> {
        self.stdout(["resize-pane", "-t", pane_id, "-x", &width.to_string()])?;
        Ok(())
    }

    /// Replace a pane's running command, killing the existing process if needed.
    pub fn respawn_pane(&self, pane_id: &str, command: &str) -> Result<()> {
        self.stdout(["respawn-pane", "-k", "-t", pane_id, command])?;
        Ok(())
    }

    /// Acquire a tmux wait-for lock channel.
    pub fn wait_for_lock(&self, channel: &str) -> Result<()> {
        self.stdout(["wait-for", "-L", channel])?;
        Ok(())
    }

    /// Release a tmux wait-for lock channel.
    pub fn wait_for_unlock(&self, channel: &str) -> Result<()> {
        self.stdout(["wait-for", "-U", channel])?;
        Ok(())
    }

    /// Create an adapter pinned to a named tmux socket.
    ///
    /// The ambient `TMUX` variables are cleared so commands target that socket rather
    /// than the caller's attached client.
    fn with_socket_name(socket_name: impl Into<OsString>) -> Self {
        Self {
            socket_name: Some(socket_name.into()),
            clear_client_env: true,
            env: Vec::new(),
        }
    }

    /// Add one environment override to every tmux subprocess.
    fn with_env(mut self, key: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    /// Return session/window/pane context for a specific pane id.
    fn pane_context(&self, pane_id: &str) -> Result<TmuxContext> {
        self.query_context(Some(pane_id))
    }

    /// Send literal command text to a pane followed by Enter.
    fn send_keys(&self, pane_id: &str, command: &str) -> Result<()> {
        self.stdout(["send-keys", "-t", pane_id, "-l", command])?;
        self.stdout(["send-keys", "-t", pane_id, "Enter"])?;
        Ok(())
    }

    // Use tmux format expansion so callers get IDs from tmux itself rather than
    // reconstructing context from environment variables. Keep the format and
    // parsing together so tmux format changes fail near the adapter boundary.
    fn query_context(&self, target: Option<&str>) -> Result<TmuxContext> {
        let format = "#{session_name}\t#{session_id}\t#{window_name}\t#{window_id}\t#{pane_id}";
        let output = if let Some(target) = target {
            self.stdout(["display-message", "-p", "-t", target, format])?
        } else {
            self.stdout(["display-message", "-p", format])?
        };
        let fields = output.trim_end().split('\t').collect::<Vec<_>>();
        if fields.len() != 5 {
            bail!("unexpected tmux context format: {output:?}");
        }

        Ok(TmuxContext {
            session_name: fields[0].to_owned(),
            session_id: fields[1].to_owned(),
            window_name: fields[2].to_owned(),
            window_id: fields[3].to_owned(),
            pane_id: fields[4].to_owned(),
        })
    }
}

/// Build a tmux command target for a window with this exact name inside a session.
///
/// tmux target strings identify where a command should apply. The `session:=window`
/// form scopes the lookup to one session and uses `=` so tmux matches the full
/// window name instead of accepting a prefix or fuzzy match.
pub fn window_target(session_name: &str, window_name: &str) -> String {
    format!("{session_name}:={window_name}")
}

fn parse_windows(output: &str) -> Result<Vec<TmuxWindow>> {
    output.lines().map(parse_window).collect()
}

fn parse_panes(output: &str) -> Result<Vec<TmuxPane>> {
    output.lines().map(parse_pane).collect()
}

fn parse_pane_snapshots(output: &str) -> Result<Vec<TmuxPaneSnapshot>> {
    output.lines().map(parse_pane_snapshot).collect()
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
    let fields = line.split('\t').collect::<Vec<_>>();
    if !(3..=4).contains(&fields.len()) {
        bail!("unexpected tmux pane format: {line:?}");
    }

    Ok(TmuxPane {
        session_name: fields[0].to_owned(),
        window_id: fields[1].to_owned(),
        pane_id: fields[2].to_owned(),
        kmux_role: fields.get(3).and_then(|field| non_empty_string(field)),
    })
}

// Use a unit-separator field delimiter for rich pane snapshots because tmux pane
// titles and paths can contain tabs.
fn parse_pane_snapshot(line: &str) -> Result<TmuxPaneSnapshot> {
    let fields = line.split(TMUX_FIELD_SEPARATOR).collect::<Vec<_>>();
    if fields.len() != 19 {
        bail!("unexpected tmux pane snapshot format: {line:?}");
    }

    Ok(TmuxPaneSnapshot {
        session_name: fields[0].to_owned(),
        window_id: fields[1].to_owned(),
        window_index: fields[2].to_owned(),
        window_name: fields[3].to_owned(),
        pane_id: fields[4].to_owned(),
        pane_index: fields[5].to_owned(),
        pane_left: parse_pane_snapshot_u16(line, "pane_left", fields[6])?,
        pane_width: parse_pane_snapshot_u16(line, "pane_width", fields[7])?,
        window_width: parse_pane_snapshot_u16(line, "window_width", fields[8])?,
        window_layout: TmuxWindowLayout::parse(fields[9])?,
        title: non_empty_string(fields[10]),
        current_command: non_empty_string(fields[11]),
        current_path: non_empty_string(fields[12]),
        pane_active: tmux_bool(fields[13]),
        pane_last: tmux_bool(fields[14]),
        window_active: tmux_bool(fields[15]),
        window_last: tmux_bool(fields[16]),
        session_attached: tmux_attached(fields[17]),
        kmux_role: non_empty_string(fields[18]),
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

// Restrict user options to kmux-owned names so generic tmux options cannot be
// mutated through this adapter by accident.
fn validate_user_option(option_name: &str) -> Result<()> {
    if !option_name.starts_with("@kmux") {
        bail!("tmux user option must be namespaced under @kmux, got '{option_name}'");
    }
    if !option_name.chars().all(is_user_option_char) {
        bail!("tmux user option contains unsupported characters: '{option_name}'");
    }
    Ok(())
}

fn is_user_option_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '@' | '.' | '_' | '-')
}

fn display_args(args: &[OsString]) -> String {
    let mut display = String::new();
    for arg in args {
        if !display.is_empty() {
            display.push(' ');
        }
        display.push_str(&arg.to_string_lossy());
    }
    display
}

fn command_name(args: &[OsString]) -> String {
    args.first()
        .map(|arg| arg.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn bail_tmux<T>(output: TmuxOutput) -> Result<T> {
    let stderr = output.stderr.trim();
    if stderr.is_empty() {
        bail!("tmux command failed with status {}", output.status);
    }
    bail!(
        "tmux command failed with status {}: {stderr}",
        output.status
    )
}

#[cfg(test)]
pub mod test_support {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    use tempfile::TempDir;

    /// Create an adapter pinned to a unique socket with no running tmux server.
    pub fn disconnected_adapter() -> Tmux {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        Tmux::with_socket_name(format!("kmux-missing-test-{}-{nanos}", std::process::id()))
    }

    /// Isolated tmux server fixture for adapter and sidebar tests.
    pub struct TmuxFixture {
        pub tmux: Tmux,
        _socket_dir: TempDir,
    }

    impl TmuxFixture {
        /// Create an isolated tmux server fixture when tmux is available.
        pub fn new() -> Result<Option<Self>> {
            if !Command::new("tmux")
                .arg("-V")
                .output()
                .is_ok_and(|output| output.status.success())
            {
                return Ok(None);
            }

            let nanos = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
            let socket_name = format!("kmux-test-{}-{nanos}", std::process::id());
            let socket_dir = TempDir::new()?;
            let socket_dir_value = socket_dir.path().as_os_str().to_os_string();
            Ok(Some(Self {
                tmux: Tmux::with_socket_name(socket_name).with_env("TMUX_TMPDIR", socket_dir_value),
                _socket_dir: socket_dir,
            }))
        }
    }

    impl Drop for TmuxFixture {
        fn drop(&mut self) {
            let _ = self.tmux.output(["kill-server"]);
        }
    }

    /// Create a detached test session in the fixture's tmux server.
    pub fn create_test_session(tmux: &Tmux, session_name: &str, cwd: &Path) -> Result<()> {
        tmux.stdout(vec![
            OsString::from("new-session"),
            OsString::from("-d"),
            OsString::from("-s"),
            OsString::from(session_name),
            OsString::from("-c"),
            cwd.as_os_str().to_os_string(),
        ])?;
        Ok(())
    }

    /// Build a horizontal window layout for pane-snapshot tests.
    pub fn test_window_layout(pane_ids: &[&str]) -> TmuxWindowLayout {
        let panes = pane_ids
            .iter()
            .map(|pane_id| TmuxLayoutCell::Pane(pane_id.trim_start_matches('%').to_owned()))
            .collect::<Vec<_>>();
        let root = match panes.as_slice() {
            [pane] => pane.clone(),
            _ => TmuxLayoutCell::Horizontal(panes),
        };
        TmuxWindowLayout { root }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tmux::test_support::{TmuxFixture, create_test_session};
    use std::thread;
    use std::time::{Duration, Instant};

    use tempfile::TempDir;

    fn wait_for_path(path: &Path) -> bool {
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if path.exists() {
                return true;
            }
            thread::sleep(Duration::from_millis(25));
        }
        false
    }

    #[test]
    fn builds_exact_window_targets() {
        assert_eq!(
            window_target("project", "feature-auth"),
            "project:=feature-auth"
        );
    }

    #[test]
    fn parses_pane_snapshots() -> Result<()> {
        let separator = TMUX_FIELD_SEPARATOR;
        let output = format!(
            "project{separator}@1{separator}1{separator}kmux-feature{separator}%2{separator}1{separator}0{separator}42{separator}120{separator}b25d,120x24,0,0,2{separator}kmux{separator}nvim{separator}/repo/feature{separator}1{separator}0{separator}1{separator}0{separator}2{separator}sidebar\nproject{separator}@2{separator}2{separator}empty{separator}%3{separator}1{separator}0{separator}80{separator}80{separator}b25d,80x24,0,0,3{separator}{separator}{separator}{separator}0{separator}1{separator}0{separator}1{separator}0{separator}"
        );

        let panes = parse_pane_snapshots(&output)?;

        assert_eq!(panes.len(), 2);
        assert_eq!(panes[0].session_name, "project");
        assert_eq!(panes[0].window_id, "@1");
        assert_eq!(panes[0].window_index, "1");
        assert_eq!(panes[0].window_name, "kmux-feature");
        assert_eq!(panes[0].pane_id, "%2");
        assert_eq!(panes[0].pane_index, "1");
        assert_eq!(panes[0].pane_left, 0);
        assert_eq!(panes[0].pane_width, 42);
        assert_eq!(panes[0].window_width, 120);
        assert_eq!(panes[0].window_layout.minimum_width(None), 1);
        assert_eq!(panes[0].title.as_deref(), Some("kmux"));
        assert_eq!(panes[0].current_command.as_deref(), Some("nvim"));
        assert_eq!(panes[0].current_path.as_deref(), Some("/repo/feature"));
        assert!(panes[0].pane_active);
        assert!(!panes[0].pane_last);
        assert!(panes[0].window_active);
        assert!(!panes[0].window_last);
        assert!(panes[0].session_attached);
        assert_eq!(panes[0].kmux_role.as_deref(), Some("sidebar"));
        assert_eq!(panes[1].title, None);
        assert_eq!(panes[1].current_command, None);
        assert!(!panes[1].pane_active);
        assert!(panes[1].pane_last);
        assert!(!panes[1].window_active);
        assert!(panes[1].window_last);
        assert!(!panes[1].session_attached);
        assert_eq!(panes[1].kmux_role, None);
        Ok(())
    }

    #[test]
    fn malformed_pane_snapshot_geometry_reports_field_context() {
        let separator = TMUX_FIELD_SEPARATOR;
        let output = format!(
            "project{separator}@1{separator}1{separator}kmux-feature{separator}%2{separator}1{separator}0{separator}wide{separator}120{separator}b25d,120x24,0,0,2{separator}kmux{separator}nvim{separator}/repo/feature{separator}1{separator}0{separator}1{separator}0{separator}2{separator}sidebar"
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
    fn window_layout_calculates_nested_minimum_width() -> Result<()> {
        let horizontal = TmuxWindowLayout::parse("89f5,80x24,0,0{39x24,0,0,0,40x24,40,0,1}")?;
        let vertical = TmuxWindowLayout::parse("1247,80x24,0,0[80x11,0,0,0,80x12,0,12,1]")?;
        let staggered = TmuxWindowLayout::parse(
            "0000,20x20,0,0{9x20,0,0[9x9,0,0{4x9,0,0,0,4x9,5,0,1},9x10,0,10,2],10x20,10,0[10x9,10,0,3,10x10,10,10{4x10,10,10,4,5x10,15,10,5}]}",
        )?;

        assert_eq!(horizontal.minimum_width(None), 3);
        assert_eq!(vertical.minimum_width(None), 1);
        assert_eq!(staggered.minimum_width(None), 7);
        Ok(())
    }

    #[test]
    fn window_layout_excludes_sidebar_pane_from_minimum_width() -> Result<()> {
        let layout = TmuxWindowLayout::parse(
            "0000,20x20,0,0{12x20,0,0,9,7x20,13,0{3x20,13,0,1,3x20,17,0,2}}",
        )?;

        assert_eq!(layout.minimum_width(None), 5);
        assert_eq!(layout.minimum_width(Some("%9")), 3);
        Ok(())
    }

    #[test]
    fn malformed_window_layout_reports_context() {
        let error = TmuxWindowLayout::parse("invalid")
            .expect_err("malformed window layout should fail parsing");

        assert!(error.to_string().contains("tmux window layout"));
    }

    #[test]
    fn creates_selects_lists_and_kills_windows_on_isolated_socket() -> Result<()> {
        let Some(fixture) = TmuxFixture::new()? else {
            return Ok(());
        };
        let temp = TempDir::new()?;
        let tmux = &fixture.tmux;

        create_test_session(tmux, "project", temp.path())?;
        create_test_session(tmux, "project-copy", temp.path())?;
        assert!(
            tmux.output(["has-session", "-t", "project"])?
                .status
                .success()
        );

        let pane_id =
            tmux.create_window_with_command("project", "feature-auth", temp.path(), None)?;
        let context = tmux.pane_context(&pane_id)?;

        assert_eq!(context.session_name, "project");
        assert_eq!(context.window_name, "feature-auth");
        assert_eq!(context.pane_id, pane_id);
        assert!(tmux.window_exists_by_name("project", "feature-auth")?);
        assert!(
            tmux.list_windows(Some("project"))?
                .iter()
                .all(|window| window.session_name == "project")
        );
        let snapshot = tmux
            .list_pane_snapshots()?
            .into_iter()
            .find(|pane| pane.pane_id == pane_id)
            .ok_or_else(|| anyhow::anyhow!("expected created pane in tmux snapshot"))?;
        assert_eq!(snapshot.session_name, "project");
        assert_eq!(snapshot.window_id, context.window_id);
        assert_eq!(snapshot.window_name, "feature-auth");

        tmux.select_window_id_in_session(&context.session_name, &context.window_id)?;
        tmux.select_pane(&pane_id)?;
        tmux.set_pane_title(&pane_id, "kmux")?;
        let updated_snapshot = tmux
            .list_pane_snapshots()?
            .into_iter()
            .find(|pane| pane.pane_id == pane_id)
            .ok_or_else(|| anyhow::anyhow!("expected updated pane in tmux snapshot"))?;
        assert_eq!(updated_snapshot.title.as_deref(), Some("kmux"));
        assert!(!tmux.pane_visibility(&pane_id)?.pane_has_focus);

        tmux.select_window("project", "feature-auth")?;
        let selected = tmux
            .list_windows(Some("project"))?
            .into_iter()
            .find(|window| window.window_name == "feature-auth")
            .ok_or_else(|| anyhow::anyhow!("expected feature-auth window"))?;
        assert!(selected.active);

        tmux.kill_window("project", "feature-auth")?;
        assert!(!tmux.window_exists_by_name("project", "feature-auth")?);
        Ok(())
    }

    #[test]
    fn startup_command_runs_inside_shell_and_window_survives_exit() -> Result<()> {
        let Some(fixture) = TmuxFixture::new()? else {
            return Ok(());
        };
        let temp = TempDir::new()?;
        let tmux = &fixture.tmux;
        let marker = temp.path().join("startup-ran");

        create_test_session(tmux, "project", temp.path())?;
        tmux.create_window_with_command(
            "project",
            "feature-auth",
            temp.path(),
            Some("touch startup-ran"),
        )?;

        assert!(wait_for_path(&marker));
        assert!(tmux.window_exists_by_name("project", "feature-auth")?);
        Ok(())
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
