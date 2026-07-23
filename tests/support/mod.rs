use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Debug;
use std::fs;
use std::ops::{Deref, DerefMut};
use std::path::{Path, PathBuf};
use std::process::{Child, Command as ProcessCommand, Output};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use assert_cmd::Command;
use tempfile::TempDir;

const WAIT_TIMEOUT: Duration = Duration::from_secs(10);
const WAIT_INTERVAL: Duration = Duration::from_millis(25);
const TMUX_FIELD_SEPARATOR: char = '\u{1f}';

#[derive(Debug)]
struct SidebarTopology {
    sidebar_counts: BTreeMap<String, usize>,
}

impl SidebarTopology {
    fn has_one_sidebar_per_window(&self) -> bool {
        !self.sidebar_counts.is_empty() && self.sidebar_counts.values().all(|count| *count == 1)
    }
}

#[derive(Debug)]
struct PathObservation {
    exists: bool,
    len: Option<u64>,
}

pub struct TmuxFixture {
    pub socket_name: String,
    socket_dir: TempDir,
    pub pane_id: String,
}

/// A kmux command that owns its isolated HOME/XDG directory until execution ends.
pub struct IsolatedKmuxCommand {
    command: Command,
    _environment: TempDir,
}

/// A spawned test process that is killed and reaped if an assertion returns early.
pub struct ChildGuard {
    child: Option<Child>,
}

impl ChildGuard {
    pub fn spawn(command: &mut ProcessCommand) -> Result<Self> {
        Ok(Self {
            child: Some(command.spawn()?),
        })
    }

    pub fn try_wait(&mut self) -> Result<Option<std::process::ExitStatus>> {
        self.child
            .as_mut()
            .ok_or_else(|| anyhow!("test child has already been reaped"))?
            .try_wait()
            .context("failed to inspect test child")
    }

    pub fn wait_with_output(mut self) -> Result<Output> {
        self.child
            .take()
            .ok_or_else(|| anyhow!("test child has already been reaped"))?
            .wait_with_output()
            .context("failed to wait for test child")
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// A marker that releases a file-synchronized child even when the test returns early.
pub struct ReleaseFile {
    path: PathBuf,
    released: bool,
}

impl ReleaseFile {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            released: false,
        }
    }

    pub fn release(&mut self) -> Result<()> {
        fs::write(&self.path, "release\n")?;
        self.released = true;
        Ok(())
    }
}

impl Drop for ReleaseFile {
    fn drop(&mut self) {
        if !self.released {
            let _ = fs::write(&self.path, "release\n");
        }
    }
}

impl Deref for IsolatedKmuxCommand {
    type Target = Command;

    fn deref(&self) -> &Self::Target {
        &self.command
    }
}

impl DerefMut for IsolatedKmuxCommand {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.command
    }
}

impl TmuxFixture {
    pub fn new(cwd: &Path) -> Result<Option<Self>> {
        if !tmux_available() {
            bail!("tmux is required to run this integration test");
        }

        let socket_dir = TempDir::new()?;
        let socket_name = "kmux-cli-test".to_owned();
        prepare_external_environment(socket_dir.path())?;
        let mut command = ProcessCommand::new("tmux");
        apply_external_environment(&mut command, socket_dir.path());
        let output = command
            .env("TMUX_TMPDIR", socket_dir.path())
            .args([
                "-u",
                "-f",
                "/dev/null",
                "-L",
                &socket_name,
                "new-session",
                "-d",
                "-s",
                "project",
                "-c",
            ])
            .arg(cwd)
            .args(["-P", "-F", "#{pane_id}"])
            .output()
            .context("failed to create isolated tmux session")?;
        if !output.status.success() {
            bail!(
                "failed to create isolated tmux session: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }

        let fixture = Self {
            socket_name,
            socket_dir,
            pane_id: String::from_utf8_lossy(&output.stdout).trim().to_owned(),
        };
        fixture.wait_for_pane_current_path(&fixture.pane_id, cwd)?;

        Ok(Some(fixture))
    }

    pub fn tmux_output(&self, args: &[&str]) -> Result<String> {
        let mut command = ProcessCommand::new("tmux");
        apply_external_environment(&mut command, self.socket_dir.path());
        let output = command
            .env("TMUX_TMPDIR", self.socket_dir.path())
            .arg("-u")
            .arg("-L")
            .arg(&self.socket_name)
            .args(args)
            .output()
            .with_context(|| format!("failed to run tmux {}", args.join(" ")))?;
        if !output.status.success() {
            bail!(
                "tmux {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    }

    pub fn window_exists(&self, window_name: &str) -> Result<bool> {
        let output =
            self.tmux_output(&["list-windows", "-t", "project:", "-F", "#{window_name}"])?;
        Ok(output.lines().any(|line| line == window_name))
    }

    /// Create a detached session and wait until its shell reports the requested cwd.
    pub fn create_session(&self, session_name: &str, cwd: &Path) -> Result<String> {
        self.create_session_with_command(session_name, cwd, None)
    }

    /// Create a detached session running a command and wait for its requested cwd.
    pub fn create_session_with_command(
        &self,
        session_name: &str,
        cwd: &Path,
        command: Option<&str>,
    ) -> Result<String> {
        let cwd_text = cwd.display().to_string();
        let mut args = vec![
            "new-session",
            "-d",
            "-s",
            session_name,
            "-c",
            &cwd_text,
            "-P",
            "-F",
            "#{pane_id}",
        ];
        if let Some(command) = command {
            args.push(command);
        }
        let pane_id = self.tmux_output(&args)?;
        self.wait_for_pane_current_path(&pane_id, cwd)?;
        Ok(pane_id)
    }

    /// Create a detached window and wait until its shell reports the requested cwd.
    pub fn create_window(&self, target: &str, window_name: &str, cwd: &Path) -> Result<String> {
        let cwd_text = cwd.display().to_string();
        let pane_id = self.tmux_output(&[
            "new-window",
            "-d",
            "-t",
            target,
            "-n",
            window_name,
            "-c",
            &cwd_text,
            "-P",
            "-F",
            "#{pane_id}",
        ])?;
        self.wait_for_pane_current_path(&pane_id, cwd)?;
        Ok(pane_id)
    }

    pub fn sidebar_pane_count(&self) -> Result<usize> {
        let output = self.tmux_output(&["list-panes", "-a", "-F", "#{@kmux_role}"])?;
        Ok(output.lines().filter(|line| *line == "sidebar").count())
    }

    pub fn sidebar_pane_titles(&self) -> Result<Vec<String>> {
        let format = format!("#{{@kmux_role}}{TMUX_FIELD_SEPARATOR}#{{pane_title}}");
        let output = self.tmux_output(&["list-panes", "-a", "-F", &format])?;
        Ok(output
            .lines()
            .filter_map(|line| {
                let (role, title) = line.split_once(TMUX_FIELD_SEPARATOR)?;
                (role == "sidebar").then(|| title.to_owned())
            })
            .collect())
    }

    fn sidebar_topology(&self) -> Result<SidebarTopology> {
        let format = format!(
            "#{{window_id}}{TMUX_FIELD_SEPARATOR}#{{@kmux_role}}{TMUX_FIELD_SEPARATOR}#{{pane_id}}"
        );
        let output = self.tmux_output(&["list-panes", "-a", "-F", &format])?;
        let mut sidebar_counts = BTreeMap::new();
        let mut seen_panes = BTreeSet::new();
        for line in output.lines() {
            let mut fields = line.splitn(3, TMUX_FIELD_SEPARATOR);
            let (Some(window_id), Some(role), Some(pane_id)) =
                (fields.next(), fields.next(), fields.next())
            else {
                continue;
            };
            if !seen_panes.insert((window_id.to_owned(), pane_id.to_owned())) {
                continue;
            }
            let count = sidebar_counts.entry(window_id.to_owned()).or_insert(0);
            if role == "sidebar" {
                *count += 1;
            }
        }
        Ok(SidebarTopology { sidebar_counts })
    }

    pub fn sidebar_pane_for_window(&self, window_id: &str) -> Result<String> {
        let format = format!("#{{pane_id}}{TMUX_FIELD_SEPARATOR}#{{@kmux_role}}");
        let output = self.tmux_output(&["list-panes", "-t", window_id, "-F", &format])?;
        for line in output.lines() {
            if let Some((pane_id, role)) = line.split_once(TMUX_FIELD_SEPARATOR)
                && role == "sidebar"
            {
                return Ok(pane_id.to_owned());
            }
        }
        Err(anyhow!(
            "sidebar pane for tmux window '{window_id}' not found"
        ))
    }

    pub fn unique_window_count(&self) -> Result<usize> {
        let output = self.tmux_output(&["list-windows", "-a", "-F", "#{window_id}"])?;
        Ok(output
            .lines()
            .map(str::to_owned)
            .collect::<BTreeSet<_>>()
            .len())
    }

    pub fn current_window_id(&self) -> Result<String> {
        self.tmux_output(&["display-message", "-p", "-t", "project:", "#{window_id}"])
    }

    pub fn resize_window_and_wait(
        &self,
        window_id: &str,
        observed_pane_id: &str,
        width: u16,
    ) -> Result<()> {
        let width = width.to_string();
        self.tmux_output(&["resize-window", "-t", window_id, "-x", &width])?;
        self.wait_for_pane_format(observed_pane_id, "#{window_width}", &width)
    }

    pub fn has_one_sidebar_per_window(&self) -> Result<bool> {
        Ok(self.sidebar_topology()?.has_one_sidebar_per_window())
    }

    pub fn wait_for_one_sidebar_per_window(&self) -> Result<()> {
        wait_until(
            "one sidebar pane in every tmux window",
            || self.sidebar_topology(),
            SidebarTopology::has_one_sidebar_per_window,
        )
    }

    pub fn wait_for_sidebar_title(&self, title: &str) -> Result<()> {
        wait_until(
            &format!("a sidebar pane title equal to {title:?}"),
            || self.sidebar_pane_titles(),
            |titles| titles.iter().any(|pane_title| pane_title == title),
        )
    }

    pub fn wait_for_pane_command(&self, pane_id: &str, command: &str) -> Result<()> {
        self.wait_for_pane_format(pane_id, "#{pane_current_command}", command)
    }

    pub fn wait_for_pane_current_path(&self, pane_id: &str, path: &Path) -> Result<()> {
        wait_until(
            &format!(
                "tmux pane {pane_id} current path to equal {}",
                path.display()
            ),
            || self.pane_format_if_present(pane_id, "#{pane_current_path}"),
            |value| {
                value
                    .as_deref()
                    .is_some_and(|value| same_filesystem_path(Path::new(value), path))
            },
        )
    }

    pub fn wait_for_pane_format(&self, pane_id: &str, format: &str, expected: &str) -> Result<()> {
        wait_until(
            &format!("tmux pane {pane_id} format {format:?} to equal {expected:?}"),
            || self.pane_format_if_present(pane_id, format),
            |value| value.as_deref() == Some(expected),
        )
    }

    pub fn global_option(&self, option_name: &str) -> Result<Option<String>> {
        let output = self.tmux_output(&["show-option", "-gqv", option_name])?;
        Ok(Some(output).filter(|value| !value.is_empty()))
    }

    pub fn global_hook(&self, hook_name: &str) -> Result<String> {
        self.tmux_output(&["show-hooks", "-g", hook_name])
    }

    pub fn pane_for_window(&self, window_name: &str) -> Result<String> {
        let format = format!("#{{window_name}}{TMUX_FIELD_SEPARATOR}#{{pane_id}}");
        let output = self.tmux_output(&["list-panes", "-a", "-F", &format])?;
        for line in output.lines() {
            if let Some((name, pane_id)) = line.split_once(TMUX_FIELD_SEPARATOR)
                && name == window_name
            {
                return Ok(pane_id.to_owned());
            }
        }
        Err(anyhow!("pane for tmux window '{window_name}' not found"))
    }

    pub fn pane_format(&self, pane_id: &str, format: &str) -> Result<String> {
        self.tmux_output(&["display-message", "-p", "-t", pane_id, format])
    }

    fn pane_format_if_present(&self, pane_id: &str, format: &str) -> Result<Option<String>> {
        let pane_format = format!("#{{pane_id}}{TMUX_FIELD_SEPARATOR}{format}");
        let output = self.tmux_output(&["list-panes", "-a", "-F", &pane_format])?;
        Ok(output.lines().find_map(|line| {
            let (observed_pane_id, value) = line.split_once(TMUX_FIELD_SEPARATOR)?;
            (observed_pane_id == pane_id).then(|| value.to_owned())
        }))
    }

    pub fn pane_count_for_window(&self, window_id: &str) -> Result<usize> {
        let output = self.tmux_output(&["list-panes", "-t", window_id, "-F", "#{pane_id}"])?;
        Ok(output.lines().count())
    }

    pub fn window_option(&self, target: &str, option_name: &str) -> Result<Option<String>> {
        let output = self.tmux_output(&["show-option", "-wqv", "-t", target, option_name])?;
        Ok(Some(output).filter(|value| !value.is_empty()))
    }

    fn apply_env_with_pane(&self, command: &mut Command, pane_id: &str) {
        command
            .env("KMUX_TMUX_SOCKET_NAME", &self.socket_name)
            .env("KMUX_TMUX_TMPDIR", self.socket_dir.path())
            .env("TMUX_PANE", pane_id);
    }

    fn apply_env_without_pane(&self, command: &mut Command) {
        command
            .env("KMUX_TMUX_SOCKET_NAME", &self.socket_name)
            .env("KMUX_TMUX_TMPDIR", self.socket_dir.path())
            .env_remove("TMUX")
            .env_remove("TMUX_PANE");
    }
}

impl Drop for TmuxFixture {
    fn drop(&mut self) {
        let mut command = ProcessCommand::new("tmux");
        apply_external_environment(&mut command, self.socket_dir.path());
        let _ = command
            .env("TMUX_TMPDIR", self.socket_dir.path())
            .arg("-u")
            .arg("-L")
            .arg(&self.socket_name)
            .arg("kill-server")
            .output();
    }
}

fn tmux_available() -> bool {
    ProcessCommand::new("tmux")
        .arg("-V")
        .output()
        .is_ok_and(|output| output.status.success())
}

pub fn run(cwd: &Path, program: &str, args: &[&str]) -> Result<()> {
    let mut command = ProcessCommand::new(program);
    apply_git_environment(&mut command, cwd);
    let output = command
        .args(args)
        .current_dir(cwd)
        .output()
        .with_context(|| format!("failed to run {} {}", program, args.join(" ")))?;
    if !output.status.success() {
        bail!(
            "{} {} failed\nstdout: {}\nstderr: {}",
            program,
            args.join(" "),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}

pub fn git(cwd: &Path, args: &[&str]) -> Result<()> {
    run(cwd, "git", args)
}

pub fn git_stdout(cwd: &Path, args: &[&str]) -> Result<String> {
    let mut command = ProcessCommand::new("git");
    apply_git_environment(&mut command, cwd);
    let output = command
        .args(args)
        .current_dir(cwd)
        .output()
        .with_context(|| format!("failed to run git {}", args.join(" ")))?;
    if !output.status.success() {
        bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

pub fn kmux_stdout(cwd: &Path, args: &[&str]) -> Result<String> {
    let config_home = cwd.parent().unwrap_or(cwd).join("kmux-test-config-home");
    prepare_test_environment(&config_home)?;
    let mut command = Command::cargo_bin("kmux")?;
    apply_kmux_environment(&mut command, &config_home);
    let assert = command.current_dir(cwd).args(args).assert().success();
    Ok(String::from_utf8_lossy(&assert.get_output().stdout)
        .trim()
        .to_owned())
}

/// Build kmux with a private empty HOME and XDG environment.
pub fn kmux_command() -> Result<IsolatedKmuxCommand> {
    let environment = TempDir::new()?;
    let config_home = write_config(environment.path(), "")?;
    let command = kmux_command_for(&config_home)?;
    Ok(IsolatedKmuxCommand {
        command,
        _environment: environment,
    })
}

/// Build kmux against a caller-owned isolated config environment.
pub fn kmux_command_for(config_home: &Path) -> Result<Command> {
    let mut command = Command::cargo_bin("kmux")?;
    apply_kmux_environment(&mut command, config_home);
    Ok(command)
}

/// Build an external tool command with only the test's minimal shell environment.
pub fn isolated_process_command(program: &str, root: &Path) -> Result<ProcessCommand> {
    prepare_external_environment(root)?;
    let mut command = ProcessCommand::new(program);
    apply_external_environment(&mut command, root);
    Ok(command)
}

pub fn init_repo() -> Result<(TempDir, PathBuf)> {
    let temp = TempDir::new()?;
    let repo = temp.path().join("project");
    fs::create_dir(&repo)?;
    git(&repo, &["init", "--initial-branch", "main"])?;
    git(&repo, &["config", "user.email", "test@example.invalid"])?;
    git(&repo, &["config", "user.name", "Test User"])?;
    fs::write(repo.join("README.md"), "test\n")?;
    git(&repo, &["add", "README.md"])?;
    git(&repo, &["commit", "-m", "initial"])?;
    Ok((temp, repo))
}

pub fn write_config(root: &Path, content: &str) -> Result<PathBuf> {
    let config_home = root.join("config-home");
    prepare_test_environment(&config_home)?;
    let config_dir = config_home.join("kmux");
    fs::write(config_dir.join("config.yaml"), content)?;
    Ok(config_home)
}

pub fn raw_key_capture_command(capture_path: &Path, ready_path: &Path) -> String {
    format!(
        "sh -c 'stty raw -echo; : > \"$1\"; dd bs=1 count=16 of=\"$2\" 2>/dev/null; sleep 5' sh {} {}",
        shell_quote(&ready_path.display().to_string()),
        shell_quote(&capture_path.display().to_string())
    )
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

pub fn wait_for_path(path: &Path) -> Result<()> {
    wait_until(
        &format!("path {} to exist", path.display()),
        || path_observation(path),
        |observation| observation.exists,
    )
}

pub fn wait_for_nonempty_file(path: &Path) -> Result<()> {
    wait_until(
        &format!("file {} to contain captured bytes", path.display()),
        || path_observation(path),
        |observation| observation.len.is_some_and(|len| len > 0),
    )
}

pub fn kmux(repo: &Path, config_home: &Path, tmux: &TmuxFixture) -> Result<Command> {
    kmux_with_pane(repo, config_home, tmux, &tmux.pane_id)
}

pub fn kmux_with_pane(
    repo: &Path,
    config_home: &Path,
    tmux: &TmuxFixture,
    pane_id: &str,
) -> Result<Command> {
    let mut command = kmux_command_for(config_home)?;
    command.current_dir(repo);
    tmux.apply_env_with_pane(&mut command, pane_id);
    Ok(command)
}

pub fn kmux_detached(repo: &Path, config_home: &Path, tmux: &TmuxFixture) -> Result<Command> {
    let mut command = kmux_command_for(config_home)?;
    command.current_dir(repo);
    tmux.apply_env_without_pane(&mut command);
    Ok(command)
}

pub fn kmux_process_with_pane(
    repo: &Path,
    config_home: &Path,
    tmux: &TmuxFixture,
    pane_id: &str,
) -> ProcessCommand {
    let mut command = ProcessCommand::new(env!("CARGO_BIN_EXE_kmux"));
    apply_kmux_process_environment(&mut command, config_home);
    command
        .current_dir(repo)
        .env("KMUX_TMUX_SOCKET_NAME", &tmux.socket_name)
        .env("KMUX_TMUX_TMPDIR", tmux.socket_dir.path())
        .env("TMUX_PANE", pane_id);
    command
}

pub fn kmux_process_detached(
    repo: &Path,
    config_home: &Path,
    tmux: &TmuxFixture,
) -> ProcessCommand {
    let mut command = ProcessCommand::new(env!("CARGO_BIN_EXE_kmux"));
    apply_kmux_process_environment(&mut command, config_home);
    command
        .current_dir(repo)
        .env("KMUX_TMUX_SOCKET_NAME", &tmux.socket_name)
        .env("KMUX_TMUX_TMPDIR", tmux.socket_dir.path())
        .env_remove("TMUX")
        .env_remove("TMUX_PANE");
    command
}

fn set_agent_status_args(
    agent_kind: &str,
    status: Option<&str>,
    session_id: &str,
    reporter_kind: &str,
    reporter_instance: &str,
    extra: &[(&str, &str)],
) -> Vec<String> {
    let mut args = vec!["set-agent-status".to_owned()];
    if let Some(status) = status {
        args.push(status.to_owned());
    }
    args.extend([
        "--agent-kind".to_owned(),
        agent_kind.to_owned(),
        "--session-id".to_owned(),
        session_id.to_owned(),
        "--reporter-kind".to_owned(),
        reporter_kind.to_owned(),
        "--reporter-instance".to_owned(),
        reporter_instance.to_owned(),
    ]);
    for (flag, value) in extra {
        args.push((*flag).to_owned());
        args.push((*value).to_owned());
    }
    args
}

pub fn set_opencode_status_args(
    status: Option<&str>,
    session_id: &str,
    reporter_kind: &str,
    reporter_instance: &str,
    extra: &[(&str, &str)],
) -> Vec<String> {
    set_agent_status_args(
        "opencode",
        status,
        session_id,
        reporter_kind,
        reporter_instance,
        extra,
    )
}

pub fn delete_opencode_agent_observation_args(
    session_id: &str,
    reporter_kind: &str,
    reporter_instance: &str,
) -> Vec<String> {
    let mut args =
        set_opencode_status_args(None, session_id, reporter_kind, reporter_instance, &[]);
    args.push("--delete".to_owned());
    args
}

pub fn agent_observations_dir(config_home: &Path) -> PathBuf {
    config_home
        .with_file_name("state-home")
        .join("kmux")
        .join("agent-observations")
}

fn prepare_test_environment(config_home: &Path) -> Result<()> {
    for path in [
        config_home.join("kmux"),
        config_home.with_file_name("home"),
        config_home.with_file_name("state-home"),
        config_home.with_file_name("cache-home"),
        config_home.with_file_name("data-home"),
        config_home.with_file_name("runtime-dir"),
        config_home.with_file_name("tmp"),
        config_home.with_file_name("empty-hooks"),
    ] {
        fs::create_dir_all(path)?;
    }
    set_private_directory_permissions(&config_home.with_file_name("runtime-dir"))?;
    let hooks_path = config_home.with_file_name("empty-hooks");
    fs::write(
        config_home.with_file_name("gitconfig"),
        format!(
            "[commit]\n\tgpgSign = false\n[core]\n\thooksPath = {}\n",
            hooks_path.display()
        ),
    )?;
    Ok(())
}

fn prepare_external_environment(root: &Path) -> Result<()> {
    for path in [
        root.join("home"),
        root.join("config-home"),
        root.join("state-home"),
        root.join("cache-home"),
        root.join("data-home"),
        root.join("runtime-dir"),
        root.join("tmp"),
    ] {
        fs::create_dir_all(path)?;
    }
    set_private_directory_permissions(&root.join("runtime-dir"))?;
    fs::write(root.join("gitconfig"), "[commit]\n\tgpgSign = false\n")?;
    Ok(())
}

#[cfg(unix)]
fn set_private_directory_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_private_directory_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

fn apply_base_environment(command: &mut ProcessCommand, home: &Path) {
    let path = std::env::var_os("PATH").unwrap_or_default();
    command
        .env_clear()
        .env("HOME", home)
        .env("PATH", path)
        .env("SHELL", "/bin/sh")
        .env("LANG", "C")
        .env("LC_ALL", "C");
}

fn apply_external_environment(command: &mut ProcessCommand, root: &Path) {
    apply_base_environment(command, &root.join("home"));
    command
        .env("XDG_CONFIG_HOME", root.join("config-home"))
        .env("XDG_STATE_HOME", root.join("state-home"))
        .env("XDG_CACHE_HOME", root.join("cache-home"))
        .env("XDG_DATA_HOME", root.join("data-home"))
        .env("XDG_RUNTIME_DIR", root.join("runtime-dir"))
        .env("TMPDIR", root.join("tmp"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", root.join("gitconfig"));
}

fn apply_git_environment(command: &mut ProcessCommand, cwd: &Path) {
    apply_base_environment(command, cwd);
    command
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "Test User")
        .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
        .env("GIT_COMMITTER_NAME", "Test User")
        .env("GIT_COMMITTER_EMAIL", "test@example.invalid");
}

fn apply_kmux_environment(command: &mut Command, config_home: &Path) {
    let path = std::env::var_os("PATH").unwrap_or_default();
    command
        .env_clear()
        .env("HOME", config_home.with_file_name("home"))
        .env("PATH", path)
        .env("SHELL", "/bin/sh")
        .env("LANG", "C")
        .env("LC_ALL", "C")
        .env("XDG_CONFIG_HOME", config_home)
        .env("XDG_STATE_HOME", config_home.with_file_name("state-home"))
        .env("XDG_CACHE_HOME", config_home.with_file_name("cache-home"))
        .env("XDG_DATA_HOME", config_home.with_file_name("data-home"))
        .env("XDG_RUNTIME_DIR", config_home.with_file_name("runtime-dir"))
        .env("TMPDIR", config_home.with_file_name("tmp"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", config_home.with_file_name("gitconfig"))
        .env("GIT_AUTHOR_NAME", "Test User")
        .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
        .env("GIT_COMMITTER_NAME", "Test User")
        .env("GIT_COMMITTER_EMAIL", "test@example.invalid");
}

fn apply_kmux_process_environment(command: &mut ProcessCommand, config_home: &Path) {
    apply_base_environment(command, &config_home.with_file_name("home"));
    command
        .env("XDG_CONFIG_HOME", config_home)
        .env("XDG_STATE_HOME", config_home.with_file_name("state-home"))
        .env("XDG_CACHE_HOME", config_home.with_file_name("cache-home"))
        .env("XDG_DATA_HOME", config_home.with_file_name("data-home"))
        .env("XDG_RUNTIME_DIR", config_home.with_file_name("runtime-dir"))
        .env("TMPDIR", config_home.with_file_name("tmp"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", config_home.with_file_name("gitconfig"))
        .env("GIT_AUTHOR_NAME", "Test User")
        .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
        .env("GIT_COMMITTER_NAME", "Test User")
        .env("GIT_COMMITTER_EMAIL", "test@example.invalid");
}

fn same_filesystem_path(left: &Path, right: &Path) -> bool {
    left == right
        || fs::canonicalize(left)
            .and_then(|left| fs::canonicalize(right).map(|right| left == right))
            .unwrap_or(false)
}

fn wait_until<T>(
    description: &str,
    mut observe: impl FnMut() -> Result<T>,
    ready: impl Fn(&T) -> bool,
) -> Result<()>
where
    T: Debug,
{
    let started = Instant::now();
    loop {
        let observation =
            observe().with_context(|| format!("failed while waiting for {description}"))?;
        if ready(&observation) {
            return Ok(());
        }

        let elapsed = started.elapsed();
        if elapsed >= WAIT_TIMEOUT {
            bail!(
                "timed out after {elapsed:?} waiting for {description}; final observed state: {observation:#?}"
            );
        }
        thread::sleep(WAIT_INTERVAL.min(WAIT_TIMEOUT.saturating_sub(elapsed)));
    }
}

fn path_observation(path: &Path) -> Result<PathObservation> {
    match fs::metadata(path) {
        Ok(metadata) => Ok(PathObservation {
            exists: true,
            len: metadata.is_file().then_some(metadata.len()),
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(PathObservation {
            exists: false,
            len: None,
        }),
        Err(error) => Err(error).with_context(|| format!("failed to inspect {}", path.display())),
    }
}
