use std::ffi::OsString;
use std::path::Path;
use std::process::{Command, ExitStatus};

use anyhow::{Context, Result, bail};

#[derive(Debug, Clone, Default)]
pub struct Tmux {
    socket_name: Option<OsString>,
    clear_client_env: bool,
    env: Vec<(OsString, OsString)>,
}

#[derive(Debug)]
pub struct TmuxOutput {
    pub status: ExitStatus,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TmuxContext {
    pub session_name: String,
    pub session_id: String,
    pub window_name: String,
    pub window_id: String,
    pub pane_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TmuxWindow {
    pub session_name: String,
    pub window_id: String,
    pub window_index: String,
    pub window_name: String,
    pub active: bool,
    pub kmux_workspace_slug: Option<String>,
    pub kmux_workspace_path: Option<String>,
    pub kmux_workspace_branch: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TmuxPane {
    pub session_name: String,
    pub window_id: String,
    pub pane_id: String,
    pub kmux_role: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
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
    pub title: Option<String>,
    pub current_command: Option<String>,
    pub current_path: Option<String>,
    pub pane_active: bool,
    pub window_active: bool,
    pub session_attached: bool,
    pub kmux_role: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TmuxSplitSize {
    Cells(u16),
    Percent(u16),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TmuxPaneVisibility {
    pub pane_has_focus: bool,
    pub window_visible: bool,
}

pub const KMUX_WORKSPACE_SLUG_OPTION: &str = "@kmux_workspace_slug";
pub const KMUX_WORKSPACE_PATH_OPTION: &str = "@kmux_workspace_path";
pub const KMUX_WORKSPACE_BRANCH_OPTION: &str = "@kmux_workspace_branch";

const TMUX_FIELD_SEPARATOR: char = '\u{1f}';

impl Tmux {
    pub fn new() -> Self {
        Self::default()
    }

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

    pub fn with_socket_name(socket_name: impl Into<OsString>) -> Self {
        Self {
            socket_name: Some(socket_name.into()),
            clear_client_env: true,
            env: Vec::new(),
        }
    }

    pub fn with_env(mut self, key: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    pub fn instance_id(&self) -> String {
        self.socket_name
            .as_ref()
            .map(|socket_name| socket_name.to_string_lossy().into_owned())
            .filter(|socket_name| !socket_name.is_empty())
            .unwrap_or_else(|| "default".to_owned())
    }

    pub fn output<I, S>(&self, args: I) -> Result<TmuxOutput>
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        let args = args.into_iter().map(Into::into).collect::<Vec<_>>();
        let display_args = display_args(&args);
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
        let output = command
            .args(&args)
            .output()
            .with_context(|| format!("failed to run tmux {display_args}"))?;

        Ok(TmuxOutput {
            status: output.status,
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }

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

    pub fn pane_context(&self, pane_id: &str) -> Result<TmuxContext> {
        self.query_context(Some(pane_id))
    }

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

    pub fn send_keys(&self, pane_id: &str, command: &str) -> Result<()> {
        self.stdout(["send-keys", "-t", pane_id, "-l", command])?;
        self.stdout(["send-keys", "-t", pane_id, "Enter"])?;
        Ok(())
    }

    pub fn send_key(&self, pane_id: &str, key: &str) -> Result<()> {
        self.stdout(["send-keys", "-t", pane_id, key])?;
        Ok(())
    }

    pub fn select_window(&self, session_name: &str, window_name: &str) -> Result<()> {
        let target = window_target(session_name, window_name);
        self.stdout(["select-window", "-t", &target])?;
        Ok(())
    }

    pub fn select_window_id(&self, window_id: &str) -> Result<()> {
        self.stdout(["select-window", "-t", window_id])?;
        Ok(())
    }

    pub fn select_pane(&self, pane_id: &str) -> Result<()> {
        self.stdout(["select-pane", "-t", pane_id])?;
        Ok(())
    }

    pub fn set_pane_title(&self, pane_id: &str, title: &str) -> Result<()> {
        self.stdout(["select-pane", "-t", pane_id, "-T", title])?;
        Ok(())
    }

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

    pub fn switch_client_to_session(&self, session_name: &str) -> Result<()> {
        self.stdout(["switch-client", "-t", session_name])?;
        Ok(())
    }

    pub fn kill_window(&self, session_name: &str, window_name: &str) -> Result<()> {
        if !self.window_exists_by_name(session_name, window_name)? {
            bail!("tmux window '{window_name}' does not exist in session '{session_name}'");
        }

        let target = window_target(session_name, window_name);
        self.stdout(["kill-window", "-t", &target])?;
        Ok(())
    }

    pub fn list_windows(&self, session_name: Option<&str>) -> Result<Vec<TmuxWindow>> {
        let format = "#{session_name}\t#{window_id}\t#{window_index}\t#{window_name}\t#{window_active}\t#{@kmux_workspace_slug}\t#{@kmux_workspace_path}\t#{@kmux_workspace_branch}";
        let output = if let Some(session_name) = session_name {
            let target = format!("{session_name}:");
            self.stdout(["list-windows", "-t", &target, "-F", format])?
        } else {
            self.stdout(["list-windows", "-a", "-F", format])?
        };
        parse_windows(&output)
    }

    pub fn list_panes(&self) -> Result<Vec<TmuxPane>> {
        let format = "#{session_name}\t#{window_id}\t#{pane_id}\t#{@kmux_role}";
        let output = self.stdout(["list-panes", "-a", "-F", format])?;
        parse_panes(&output)
    }

    pub fn list_pane_snapshots(&self) -> Result<Vec<TmuxPaneSnapshot>> {
        let separator = TMUX_FIELD_SEPARATOR;
        let format = format!(
            "#{{session_name}}{separator}#{{window_id}}{separator}#{{window_index}}{separator}#{{window_name}}{separator}#{{pane_id}}{separator}#{{pane_index}}{separator}#{{pane_left}}{separator}#{{pane_width}}{separator}#{{window_width}}{separator}#{{pane_title}}{separator}#{{pane_current_command}}{separator}#{{pane_current_path}}{separator}#{{pane_active}}{separator}#{{window_active}}{separator}#{{session_attached}}{separator}#{{@kmux_role}}"
        );
        let output = self.stdout(["list-panes", "-a", "-F", &format])?;
        parse_pane_snapshots(&output)
    }

    pub fn window_exists_by_name(&self, session_name: &str, window_name: &str) -> Result<bool> {
        Ok(self
            .list_windows(Some(session_name))?
            .iter()
            .any(|window| window.window_name == window_name))
    }

    pub fn set_window_option(&self, target: &str, option_name: &str, value: &str) -> Result<()> {
        validate_user_option(option_name)?;
        self.stdout(["set-option", "-w", "-t", target, option_name, value])?;
        Ok(())
    }

    pub fn unset_window_option(&self, target: &str, option_name: &str) -> Result<()> {
        validate_user_option(option_name)?;
        self.stdout(["set-option", "-uw", "-t", target, option_name])?;
        Ok(())
    }

    pub fn set_pane_option(&self, target: &str, option_name: &str, value: &str) -> Result<()> {
        validate_user_option(option_name)?;
        self.stdout(["set-option", "-p", "-t", target, option_name, value])?;
        Ok(())
    }

    pub fn set_global_option(&self, option_name: &str, value: &str) -> Result<()> {
        validate_user_option(option_name)?;
        self.stdout(["set-option", "-g", option_name, value])?;
        Ok(())
    }

    pub fn show_global_option(&self, option_name: &str) -> Result<Option<String>> {
        validate_user_option(option_name)?;
        let output = self.output(["show-option", "-gqv", option_name])?;
        if !output.status.success() {
            return Ok(None);
        }

        Ok(Some(output.stdout.trim_end().to_owned()).filter(|value| !value.is_empty()))
    }

    pub fn unset_global_option(&self, option_name: &str) -> Result<()> {
        validate_user_option(option_name)?;
        self.stdout(["set-option", "-gu", option_name])?;
        Ok(())
    }

    pub fn set_hook(&self, hook: &str, command: &str) -> Result<()> {
        self.stdout(["set-hook", "-g", hook, command])?;
        Ok(())
    }

    pub fn unset_hook(&self, hook: &str) -> Result<()> {
        self.stdout(["set-hook", "-gu", hook])?;
        Ok(())
    }

    pub fn split_window_left(
        &self,
        target_window: &str,
        size: TmuxSplitSize,
        command: &str,
    ) -> Result<String> {
        let mut args = vec![
            OsString::from("split-window"),
            OsString::from("-d"),
            OsString::from("-h"),
            OsString::from("-b"),
            OsString::from("-t"),
            OsString::from(target_window),
        ];
        match size {
            TmuxSplitSize::Cells(cells) => {
                args.push(OsString::from("-l"));
                args.push(OsString::from(cells.to_string()));
            }
            TmuxSplitSize::Percent(percent) => {
                args.push(OsString::from("-p"));
                args.push(OsString::from(percent.to_string()));
            }
        }
        args.extend([
            OsString::from("-P"),
            OsString::from("-F"),
            OsString::from("#{pane_id}"),
            OsString::from(command),
        ]);
        self.stdout(args)
    }

    pub fn kill_pane(&self, pane_id: &str) -> Result<()> {
        self.stdout(["kill-pane", "-t", pane_id])?;
        Ok(())
    }

    pub fn resize_pane_width(&self, pane_id: &str, width: u16) -> Result<()> {
        self.stdout(["resize-pane", "-t", pane_id, "-x", &width.to_string()])?;
        Ok(())
    }

    pub fn respawn_pane(&self, pane_id: &str, command: &str) -> Result<()> {
        self.stdout(["respawn-pane", "-k", "-t", pane_id, command])?;
        Ok(())
    }

    pub fn wait_for_lock(&self, channel: &str) -> Result<()> {
        self.stdout(["wait-for", "-L", channel])?;
        Ok(())
    }

    pub fn wait_for_unlock(&self, channel: &str) -> Result<()> {
        self.stdout(["wait-for", "-U", channel])?;
        Ok(())
    }

    fn query_context(&self, target: Option<&str>) -> Result<TmuxContext> {
        let format = "#{session_name}\t#{session_id}\t#{window_name}\t#{window_id}\t#{pane_id}";
        let output = if let Some(target) = target {
            self.stdout(["display-message", "-p", "-t", target, format])?
        } else {
            self.stdout(["display-message", "-p", format])?
        };
        parse_context(&output)
    }
}

pub fn window_target(session_name: &str, window_name: &str) -> String {
    format!("{session_name}:={window_name}")
}

fn parse_context(output: &str) -> Result<TmuxContext> {
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
    if !(5..=8).contains(&fields.len()) {
        bail!("unexpected tmux window format: {line:?}");
    }

    Ok(TmuxWindow {
        session_name: fields[0].to_owned(),
        window_id: fields[1].to_owned(),
        window_index: fields[2].to_owned(),
        window_name: fields[3].to_owned(),
        active: fields[4] == "1",
        kmux_workspace_slug: fields.get(5).and_then(|field| non_empty_string(field)),
        kmux_workspace_path: fields.get(6).and_then(|field| non_empty_string(field)),
        kmux_workspace_branch: fields.get(7).and_then(|field| non_empty_string(field)),
    })
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

fn parse_pane_snapshot(line: &str) -> Result<TmuxPaneSnapshot> {
    let fields = line.split(TMUX_FIELD_SEPARATOR).collect::<Vec<_>>();
    if fields.len() != 16 {
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
        title: non_empty_string(fields[9]),
        current_command: non_empty_string(fields[10]),
        current_path: non_empty_string(fields[11]),
        pane_active: tmux_bool(fields[12]),
        window_active: tmux_bool(fields[13]),
        session_attached: tmux_attached(fields[14]),
        kmux_role: non_empty_string(fields[15]),
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
mod tests {
    use super::*;
    use std::thread;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    use tempfile::TempDir;

    struct TmuxFixture {
        tmux: Tmux,
        _socket_dir: TempDir,
    }

    impl TmuxFixture {
        fn new() -> Result<Option<Self>> {
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

    fn create_test_session(tmux: &Tmux, session_name: &str, cwd: &Path) -> Result<()> {
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
            "project{separator}@1{separator}1{separator}kmux-feature{separator}%2{separator}1{separator}0{separator}42{separator}120{separator}kmux{separator}nvim{separator}/repo/feature{separator}1{separator}1{separator}2{separator}sidebar\nproject{separator}@2{separator}2{separator}empty{separator}%3{separator}1{separator}0{separator}80{separator}80{separator}{separator}{separator}{separator}0{separator}0{separator}0{separator}"
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
        assert_eq!(panes[0].title.as_deref(), Some("kmux"));
        assert_eq!(panes[0].current_command.as_deref(), Some("nvim"));
        assert_eq!(panes[0].current_path.as_deref(), Some("/repo/feature"));
        assert!(panes[0].pane_active);
        assert!(panes[0].window_active);
        assert!(panes[0].session_attached);
        assert_eq!(panes[0].kmux_role.as_deref(), Some("sidebar"));
        assert_eq!(panes[1].title, None);
        assert_eq!(panes[1].current_command, None);
        assert!(!panes[1].pane_active);
        assert!(!panes[1].window_active);
        assert!(!panes[1].session_attached);
        assert_eq!(panes[1].kmux_role, None);
        Ok(())
    }

    #[test]
    fn malformed_pane_snapshot_geometry_reports_field_context() {
        let separator = TMUX_FIELD_SEPARATOR;
        let output = format!(
            "project{separator}@1{separator}1{separator}kmux-feature{separator}%2{separator}1{separator}0{separator}wide{separator}120{separator}kmux{separator}nvim{separator}/repo/feature{separator}1{separator}1{separator}2{separator}sidebar"
        );

        let error = parse_pane_snapshots(&output)
            .expect_err("malformed numeric geometry should fail parsing");
        let message = error.to_string();

        assert!(message.contains("pane_width"));
        assert!(message.contains("wide"));
        assert!(message.contains("tmux pane snapshot"));
    }

    #[test]
    fn parses_windows_with_stable_kmux_workspace_metadata() -> Result<()> {
        let windows = parse_windows(
            "project\t@1\t2\tkmux-feature\t1\tfeature\t/tmp/project-feature\tfeature\nproject\t@2\t3\tscratch\t0\t\t\t",
        )?;

        assert_eq!(windows.len(), 2);
        assert_eq!(windows[0].session_name, "project");
        assert_eq!(windows[0].window_id, "@1");
        assert_eq!(windows[0].window_index, "2");
        assert_eq!(windows[0].window_name, "kmux-feature");
        assert!(windows[0].active);
        assert_eq!(windows[0].kmux_workspace_slug.as_deref(), Some("feature"));
        assert_eq!(
            windows[0].kmux_workspace_path.as_deref(),
            Some("/tmp/project-feature")
        );
        assert_eq!(windows[0].kmux_workspace_branch.as_deref(), Some("feature"));
        assert_eq!(windows[1].kmux_workspace_slug, None);
        assert_eq!(windows[1].kmux_workspace_path, None);
        assert_eq!(windows[1].kmux_workspace_branch, None);
        Ok(())
    }

    #[test]
    fn parses_windows_without_stable_kmux_workspace_metadata() -> Result<()> {
        let window = parse_window("project\t@1\t2\tkmux-feature\t1")?;

        assert_eq!(window.session_name, "project");
        assert_eq!(window.window_id, "@1");
        assert_eq!(window.window_name, "kmux-feature");
        assert!(window.active);
        assert_eq!(window.kmux_workspace_slug, None);
        assert_eq!(window.kmux_workspace_path, None);
        assert_eq!(window.kmux_workspace_branch, None);
        Ok(())
    }

    #[test]
    fn creates_selects_lists_and_kills_windows_on_isolated_socket() -> Result<()> {
        let Some(fixture) = TmuxFixture::new()? else {
            return Ok(());
        };
        let temp = TempDir::new()?;
        let tmux = &fixture.tmux;

        create_test_session(tmux, "project", temp.path())?;
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
        let snapshot = tmux
            .list_pane_snapshots()?
            .into_iter()
            .find(|pane| pane.pane_id == pane_id)
            .ok_or_else(|| anyhow::anyhow!("expected created pane in tmux snapshot"))?;
        assert_eq!(snapshot.session_name, "project");
        assert_eq!(snapshot.window_id, context.window_id);
        assert_eq!(snapshot.window_name, "feature-auth");

        tmux.select_window_id(&context.window_id)?;
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
    fn sets_finds_and_unsets_kmux_window_metadata() -> Result<()> {
        let Some(fixture) = TmuxFixture::new()? else {
            return Ok(());
        };
        let temp = TempDir::new()?;
        let tmux = &fixture.tmux;
        create_test_session(tmux, "project", temp.path())?;
        tmux.create_window_with_command("project", "feature-auth", temp.path(), None)?;
        let target = window_target("project", "feature-auth");
        tmux.set_window_option(&target, KMUX_WORKSPACE_SLUG_OPTION, "feature-auth")?;
        tmux.set_window_option(
            &target,
            KMUX_WORKSPACE_PATH_OPTION,
            temp.path().to_string_lossy().as_ref(),
        )?;
        tmux.set_window_option(&target, KMUX_WORKSPACE_BRANCH_OPTION, "feature/auth")?;

        let window = tmux
            .list_windows(Some("project"))?
            .into_iter()
            .find(|window| window.window_name == "feature-auth")
            .ok_or_else(|| anyhow::anyhow!("expected feature-auth window"))?;
        assert_eq!(window.kmux_workspace_slug.as_deref(), Some("feature-auth"));
        assert_eq!(
            window.kmux_workspace_path.as_deref(),
            Some(temp.path().to_string_lossy().as_ref())
        );
        assert_eq!(
            window.kmux_workspace_branch.as_deref(),
            Some("feature/auth")
        );

        tmux.unset_window_option(&target, KMUX_WORKSPACE_SLUG_OPTION)?;
        tmux.unset_window_option(&target, KMUX_WORKSPACE_PATH_OPTION)?;
        tmux.unset_window_option(&target, KMUX_WORKSPACE_BRANCH_OPTION)?;

        assert_eq!(
            show_window_option(tmux, &target, KMUX_WORKSPACE_SLUG_OPTION)?,
            None
        );
        Ok(())
    }

    fn show_window_option(tmux: &Tmux, target: &str, option_name: &str) -> Result<Option<String>> {
        let output = tmux.output(["show-option", "-wqv", "-t", target, option_name])?;
        if !output.status.success() {
            return Ok(None);
        }

        Ok(Some(output.stdout.trim_end().to_owned()).filter(|value| !value.is_empty()))
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
