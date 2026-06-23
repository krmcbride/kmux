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
}

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

    pub fn select_window(&self, session_name: &str, window_name: &str) -> Result<()> {
        let target = window_target(session_name, window_name);
        self.stdout(["select-window", "-t", &target])?;
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

    pub fn rename_window(
        &self,
        session_name: &str,
        old_window_name: &str,
        new_window_name: &str,
    ) -> Result<()> {
        let target = window_target(session_name, old_window_name);
        self.stdout(["rename-window", "-t", &target, new_window_name])?;
        Ok(())
    }

    pub fn list_windows(&self, session_name: Option<&str>) -> Result<Vec<TmuxWindow>> {
        let format =
            "#{session_name}\t#{window_id}\t#{window_index}\t#{window_name}\t#{window_active}";
        let output = if let Some(session_name) = session_name {
            let target = format!("{session_name}:");
            self.stdout(["list-windows", "-t", &target, "-F", format])?
        } else {
            self.stdout(["list-windows", "-a", "-F", format])?
        };
        parse_windows(&output)
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

    pub fn show_window_option(&self, target: &str, option_name: &str) -> Result<Option<String>> {
        validate_user_option(option_name)?;
        let output = self.output(["show-option", "-wqv", "-t", target, option_name])?;
        if !output.status.success() {
            return Ok(None);
        }

        Ok(Some(output.stdout.trim_end().to_owned()).filter(|value| !value.is_empty()))
    }

    pub fn unset_window_option(&self, target: &str, option_name: &str) -> Result<()> {
        validate_user_option(option_name)?;
        self.stdout(["set-option", "-uw", "-t", target, option_name])?;
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

pub fn kmux_worktree_option(handle: &str, field: &str) -> Result<String> {
    if handle.is_empty() || field.is_empty() {
        bail!("tmux metadata handle and field cannot be empty");
    }
    if !handle.chars().all(is_metadata_component_char)
        || !field.chars().all(is_metadata_component_char)
    {
        bail!(
            "tmux metadata handle and field must use only ASCII letters, numbers, '.', '_', or '-'"
        );
    }

    Ok(format!("@kmux.worktree.{handle}.{field}"))
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

fn parse_window(line: &str) -> Result<TmuxWindow> {
    let fields = line.split('\t').collect::<Vec<_>>();
    if fields.len() != 5 {
        bail!("unexpected tmux window format: {line:?}");
    }

    Ok(TmuxWindow {
        session_name: fields[0].to_owned(),
        window_id: fields[1].to_owned(),
        window_index: fields[2].to_owned(),
        window_name: fields[3].to_owned(),
        active: fields[4] == "1",
    })
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

fn is_metadata_component_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-')
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
    fn builds_namespaced_worktree_options() -> Result<()> {
        assert_eq!(
            kmux_worktree_option("feature-auth", "path")?,
            "@kmux.worktree.feature-auth.path"
        );
        assert!(kmux_worktree_option("feature auth", "path").is_err());
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
        let option = kmux_worktree_option("feature-auth", "path")?;

        tmux.set_window_option(&target, &option, temp.path().to_string_lossy().as_ref())?;

        assert_eq!(
            tmux.show_window_option(&target, &option)?.as_deref(),
            Some(temp.path().to_string_lossy().as_ref())
        );

        tmux.unset_window_option(&target, &option)?;

        assert_eq!(tmux.show_window_option(&target, &option)?, None);
        Ok(())
    }
}
