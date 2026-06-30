#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use assert_cmd::Command;
use tempfile::TempDir;

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

        Ok(Some(Self {
            socket_name,
            socket_dir,
            pane_id: String::from_utf8_lossy(&output.stdout).trim().to_owned(),
        }))
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

    pub fn sidebar_panes_by_window(&self) -> Result<BTreeMap<String, usize>> {
        let output = self.tmux_output(&[
            "list-panes",
            "-a",
            "-F",
            "#{window_id}\t#{pane_id}\t#{@kmux_role}",
        ])?;
        let mut panes = BTreeMap::new();
        for line in output.lines() {
            let fields = line.split('\t').collect::<Vec<_>>();
            if fields.len() == 3 && fields[2] == "sidebar" {
                *panes.entry(fields[0].to_owned()).or_insert(0) += 1;
            }
        }
        Ok(panes)
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

    pub fn has_one_sidebar_per_window(&self) -> Result<bool> {
        let sidebar_panes = self.sidebar_panes_by_window()?;
        Ok(sidebar_panes.len() == self.unique_window_count()?
            && sidebar_panes.values().all(|count| *count == 1))
    }

    pub fn wait_for_one_sidebar_per_window(&self) -> Result<bool> {
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if self.has_one_sidebar_per_window()? {
                return Ok(true);
            }
            thread::sleep(Duration::from_millis(25));
        }
        Ok(false)
    }

    pub fn wait_for_sidebar_title(&self, title: &str) -> Result<bool> {
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if self
                .sidebar_pane_titles()?
                .iter()
                .any(|pane_title| pane_title == title)
            {
                return Ok(true);
            }
            thread::sleep(Duration::from_millis(25));
        }
        Ok(false)
    }

    pub fn wait_for_pane_command(&self, pane_id: &str, command: &str) -> Result<bool> {
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if self.pane_format(pane_id, "#{pane_current_command}")? == command {
                return Ok(true);
            }
            thread::sleep(Duration::from_millis(25));
        }
        Ok(false)
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

    pub fn pane_count_for_window(&self, window_id: &str) -> Result<usize> {
        let output = self.tmux_output(&["list-panes", "-t", window_id, "-F", "#{pane_id}"])?;
        Ok(output.lines().count())
    }

    pub fn set_pane_title(&self, pane_id: &str, title: &str) -> Result<()> {
        self.tmux_output(&["select-pane", "-t", pane_id, "-T", title])?;
        Ok(())
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

pub fn kmux_workspace_state(repo: &Path) -> Result<serde_json::Value> {
    let path = repo.join(".git/kmux/state.json");
    serde_json::from_slice(&fs::read(&path)?)
        .with_context(|| format!("failed to parse {}", path.display()))
}

pub fn kmux_parent_link(repo: &Path, branch: &str) -> Result<Option<serde_json::Value>> {
    let state = kmux_workspace_state(repo)?;
    Ok(state
        .get("parents")
        .and_then(serde_json::Value::as_array)
        .and_then(|links| {
            links
                .iter()
                .find(|link| link.get("branch").and_then(serde_json::Value::as_str) == Some(branch))
                .cloned()
        }))
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
        "stty raw -echo; : > {}; dd bs=1 count=16 of={} 2>/dev/null; sleep 5",
        shell_quote(&ready_path.display().to_string()),
        shell_quote(&capture_path.display().to_string())
    )
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

pub fn wait_for_path(path: &Path) -> Result<bool> {
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        if path.exists() {
            return Ok(true);
        }
        thread::sleep(Duration::from_millis(25));
    }
    Ok(false)
}

pub fn wait_for_file_bytes(path: &Path) -> Result<bool> {
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        if fs::read(path).is_ok_and(|bytes| !bytes.is_empty()) {
            return Ok(true);
        }
        thread::sleep(Duration::from_millis(25));
    }
    Ok(false)
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

pub fn set_agent_status_args(
    agent_kind: &str,
    status: Option<&str>,
    session_id: &str,
    producer_kind: &str,
    producer_instance: &str,
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
        "--producer-kind".to_owned(),
        producer_kind.to_owned(),
        "--producer-instance".to_owned(),
        producer_instance.to_owned(),
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
    producer_kind: &str,
    producer_instance: &str,
    extra: &[(&str, &str)],
) -> Vec<String> {
    set_agent_status_args(
        "opencode",
        status,
        session_id,
        producer_kind,
        producer_instance,
        extra,
    )
}

pub fn delete_opencode_agent_observation_args(
    session_id: &str,
    producer_kind: &str,
    producer_instance: &str,
) -> Vec<String> {
    let mut args =
        set_opencode_status_args(None, session_id, producer_kind, producer_instance, &[]);
    args.push("--delete".to_owned());
    args
}

pub fn delete_opencode_agent_session_args(
    session_id: &str,
    producer_kind: &str,
    producer_instance: &str,
) -> Vec<String> {
    let mut args =
        set_opencode_status_args(None, session_id, producer_kind, producer_instance, &[]);
    args.push("--delete-session".to_owned());
    args
}

pub fn agent_observation_for_pane(config_home: &Path, pane_id: &str) -> Result<serde_json::Value> {
    find_agent_observation(config_home, |value| {
        value
            .pointer("/target/tmux_pane_id")
            .and_then(serde_json::Value::as_str)
            == Some(pane_id)
    })?
    .ok_or_else(|| anyhow!("state for pane '{pane_id}' not found"))
}

pub fn agent_observation_for_key(
    config_home: &Path,
    agent_kind: &str,
    session_id: &str,
    producer_kind: &str,
    producer_instance: &str,
) -> Result<serde_json::Value> {
    find_agent_observation(config_home, |value| {
        value
            .pointer("/key/session/agent_kind")
            .and_then(serde_json::Value::as_str)
            == Some(agent_kind)
            && value
                .pointer("/key/session/session_id")
                .and_then(serde_json::Value::as_str)
                == Some(session_id)
            && value
                .pointer("/key/producer_kind")
                .and_then(serde_json::Value::as_str)
                == Some(producer_kind)
            && value
                .pointer("/key/producer_instance")
                .and_then(serde_json::Value::as_str)
                == Some(producer_instance)
    })?
    .ok_or_else(|| {
        anyhow!(
            "state for observation '{agent_kind}/{session_id}/{producer_kind}/{producer_instance}' not found"
        )
    })
}

fn find_agent_observation(
    config_home: &Path,
    matches: impl Fn(&serde_json::Value) -> bool,
) -> Result<Option<serde_json::Value>> {
    let observations_dir = agent_observations_dir(config_home);
    if !observations_dir.exists() {
        return Ok(None);
    }
    for entry in fs::read_dir(&observations_dir).with_context(|| {
        format!(
            "failed to read state directory {}",
            observations_dir.display()
        )
    })? {
        let path = entry?.path();
        let value: serde_json::Value = serde_json::from_slice(&fs::read(&path)?)
            .with_context(|| format!("failed to parse {}", path.display()))?;
        if matches(&value) {
            return Ok(Some(value));
        }
    }
    Ok(None)
}

pub fn agent_observations_dir(config_home: &Path) -> PathBuf {
    config_home
        .with_file_name("state-home")
        .join("kmux")
        .join("agent-observations")
}

pub fn state_timestamp(state: &serde_json::Value, field: &str) -> Result<u64> {
    state_u64(state, field)
        .with_context(|| format!("state timestamp '{field}' is missing or invalid"))
}

pub fn state_u64(state: &serde_json::Value, field: &str) -> Result<u64> {
    state
        .get(field)
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| anyhow!("state field '{field}' is missing or invalid"))
}
