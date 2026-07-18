use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Debug;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use assert_cmd::Command;
use tempfile::TempDir;

const WAIT_TIMEOUT: Duration = Duration::from_secs(3);
const WAIT_INTERVAL: Duration = Duration::from_millis(25);

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

impl TmuxFixture {
    pub fn new(cwd: &Path) -> Result<Option<Self>> {
        if !tmux_available() {
            return Ok(None);
        }

        let socket_dir = TempDir::new()?;
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let socket_name = format!("kmux-cli-test-{}-{nanos}", std::process::id());
        let output = ProcessCommand::new("tmux")
            .env("TMUX_TMPDIR", socket_dir.path())
            .args([
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
        let output = ProcessCommand::new("tmux")
            .env("TMUX_TMPDIR", self.socket_dir.path())
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

    pub fn sidebar_pane_count(&self) -> Result<usize> {
        let output = self.tmux_output(&["list-panes", "-a", "-F", "#{@kmux_role}"])?;
        Ok(output.lines().filter(|line| *line == "sidebar").count())
    }

    pub fn sidebar_pane_titles(&self) -> Result<Vec<String>> {
        let output =
            self.tmux_output(&["list-panes", "-a", "-F", "#{@kmux_role}\t#{pane_title}"])?;
        Ok(output
            .lines()
            .filter_map(|line| {
                let (role, title) = line.split_once('\t')?;
                (role == "sidebar").then(|| title.to_owned())
            })
            .collect())
    }

    fn sidebar_topology(&self) -> Result<SidebarTopology> {
        let output = self.tmux_output(&[
            "list-panes",
            "-a",
            "-F",
            "#{window_id}\t#{@kmux_role}\t#{pane_id}",
        ])?;
        let mut sidebar_counts = BTreeMap::new();
        let mut seen_panes = BTreeSet::new();
        for line in output.lines() {
            let mut fields = line.splitn(3, '\t');
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
        let output = self.tmux_output(&[
            "list-panes",
            "-t",
            window_id,
            "-F",
            "#{pane_id}\t#{@kmux_role}",
        ])?;
        for line in output.lines() {
            if let Some((pane_id, role)) = line.split_once('\t')
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
        let expected = path.display().to_string();
        self.wait_for_pane_format(pane_id, "#{pane_current_path}", &expected)
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
        let output = self.tmux_output(&["list-panes", "-a", "-F", "#{window_name}\t#{pane_id}"])?;
        for line in output.lines() {
            if let Some((name, pane_id)) = line.split_once('\t')
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
        let pane_format = format!("#{{pane_id}}\t{format}");
        let output = self.tmux_output(&["list-panes", "-a", "-F", &pane_format])?;
        Ok(output.lines().find_map(|line| {
            let (observed_pane_id, value) = line.split_once('\t')?;
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
}

impl Drop for TmuxFixture {
    fn drop(&mut self) {
        let _ = ProcessCommand::new("tmux")
            .env("TMUX_TMPDIR", self.socket_dir.path())
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
    let output = ProcessCommand::new(program)
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
    let output = ProcessCommand::new("git")
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
    let assert = Command::cargo_bin("kmux")?
        .current_dir(cwd)
        .args(args)
        .assert()
        .success();
    Ok(String::from_utf8_lossy(&assert.get_output().stdout)
        .trim()
        .to_owned())
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
    let config_dir = config_home.join("kmux");
    fs::create_dir_all(&config_dir)?;
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
    let mut command = Command::cargo_bin("kmux")?;
    command
        .current_dir(repo)
        .env("XDG_CONFIG_HOME", config_home)
        .env("XDG_STATE_HOME", config_home.with_file_name("state-home"));
    tmux.apply_env_with_pane(&mut command, pane_id);
    Ok(command)
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
