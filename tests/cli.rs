use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

#[test]
fn help_shows_core_commands() {
    Command::cargo_bin("kmux")
        .expect("kmux binary should be available")
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("add"))
        .stdout(predicate::str::contains("open"))
        .stdout(predicate::str::contains("status"))
        .stdout(predicate::str::contains("completions"));
}

#[test]
fn completions_command_emits_shell_completion() {
    Command::cargo_bin("kmux")
        .expect("kmux binary should be available")
        .args(["completions", "bash"])
        .assert()
        .success()
        .stdout(predicate::str::contains("_kmux"))
        .stdout(predicate::str::contains("_kmux_handles"))
        .stdout(predicate::str::contains("_complete-add-branches"));
}

#[test]
fn completion_helpers_emit_contextual_worktrees_and_branches() -> Result<()> {
    let (temp, repo) = init_repo()?;
    let remote = temp.path().join("remote.git");
    let remote_arg = remote.display().to_string();
    run(temp.path(), "git", &["init", "--bare", "remote.git"])?;
    git(&repo, &["remote", "add", "origin", &remote_arg])?;
    git(&repo, &["push", "-u", "origin", "main"])?;
    git(&repo, &["branch", "feature/addable"])?;
    git(&repo, &["branch", "feature/base"])?;
    git(&repo, &["branch", "feature/remote"])?;
    git(&repo, &["push", "origin", "feature/remote"])?;
    git(&repo, &["branch", "-D", "feature/remote"])?;

    let worktree_base = temp.path().join("project__worktrees");
    let active = worktree_base.join("feature-active");
    fs::create_dir(&worktree_base)?;
    let active_arg = active.display().to_string();
    git(
        &repo,
        &["worktree", "add", "-b", "feature/active", &active_arg],
    )?;

    let handles = kmux_stdout(&repo, &["_complete-handles"])?;
    assert!(handles.lines().any(|line| line == "feature-active"));
    assert!(!handles.lines().any(|line| line == "project"));

    let add_branches = kmux_stdout(&repo, &["_complete-add-branches"])?;
    assert!(add_branches.lines().any(|line| line == "feature/addable"));
    assert!(add_branches.lines().any(|line| line == "feature/base"));
    assert!(
        add_branches
            .lines()
            .any(|line| line == "origin/feature/remote")
    );
    assert!(!add_branches.lines().any(|line| line == "main"));
    assert!(!add_branches.lines().any(|line| line == "origin/main"));
    assert!(!add_branches.lines().any(|line| line == "feature/active"));

    let git_branches = kmux_stdout(&repo, &["_complete-git-branches"])?;
    assert!(git_branches.lines().any(|line| line == "main"));
    assert!(git_branches.lines().any(|line| line == "origin/main"));
    assert!(git_branches.lines().any(|line| line == "feature/active"));
    assert!(git_branches.lines().any(|line| line == "feature/addable"));
    assert!(
        git_branches
            .lines()
            .any(|line| line == "origin/feature/remote")
    );

    Ok(())
}

#[test]
fn unknown_commands_fail_clearly() {
    Command::cargo_bin("kmux")
        .expect("kmux binary should be available")
        .arg("not-a-command")
        .assert()
        .failure()
        .stderr(predicate::str::contains("unrecognized subcommand"));
}

struct TmuxFixture {
    socket_name: String,
    socket_dir: TempDir,
    pane_id: String,
}

impl TmuxFixture {
    fn new(cwd: &Path) -> Result<Option<Self>> {
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

    fn tmux_output(&self, args: &[&str]) -> Result<String> {
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

    fn window_exists(&self, window_name: &str) -> Result<bool> {
        let output =
            self.tmux_output(&["list-windows", "-t", "project:", "-F", "#{window_name}"])?;
        Ok(output.lines().any(|line| line == window_name))
    }

    fn sidebar_pane_count(&self) -> Result<usize> {
        let output = self.tmux_output(&["list-panes", "-a", "-F", "#{@kmux_role}"])?;
        Ok(output.lines().filter(|line| *line == "sidebar").count())
    }

    fn sidebar_pane_titles(&self) -> Result<Vec<String>> {
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

    fn sidebar_panes_by_window(&self) -> Result<BTreeMap<String, usize>> {
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

    fn unique_window_count(&self) -> Result<usize> {
        let output = self.tmux_output(&["list-windows", "-a", "-F", "#{window_id}"])?;
        Ok(output
            .lines()
            .map(str::to_owned)
            .collect::<BTreeSet<_>>()
            .len())
    }

    fn has_one_sidebar_per_window(&self) -> Result<bool> {
        let sidebar_panes = self.sidebar_panes_by_window()?;
        Ok(sidebar_panes.len() == self.unique_window_count()?
            && sidebar_panes.values().all(|count| *count == 1))
    }

    fn wait_for_one_sidebar_per_window(&self) -> Result<bool> {
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if self.has_one_sidebar_per_window()? {
                return Ok(true);
            }
            thread::sleep(Duration::from_millis(25));
        }
        Ok(false)
    }

    fn wait_for_sidebar_title(&self, title: &str) -> Result<bool> {
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

    fn wait_for_pane_command(&self, pane_id: &str, command: &str) -> Result<bool> {
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if self.pane_format(pane_id, "#{pane_current_command}")? == command {
                return Ok(true);
            }
            thread::sleep(Duration::from_millis(25));
        }
        Ok(false)
    }

    fn global_option(&self, option_name: &str) -> Result<Option<String>> {
        let output = self.tmux_output(&["show-option", "-gqv", option_name])?;
        Ok(Some(output).filter(|value| !value.is_empty()))
    }

    fn global_hook(&self, hook_name: &str) -> Result<String> {
        self.tmux_output(&["show-hooks", "-g", hook_name])
    }

    fn pane_for_window(&self, window_name: &str) -> Result<String> {
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

    fn pane_format(&self, pane_id: &str, format: &str) -> Result<String> {
        self.tmux_output(&["display-message", "-p", "-t", pane_id, format])
    }

    fn pane_count_for_window(&self, window_id: &str) -> Result<usize> {
        let output = self.tmux_output(&["list-panes", "-t", window_id, "-F", "#{pane_id}"])?;
        Ok(output.lines().count())
    }

    fn set_pane_title(&self, pane_id: &str, title: &str) -> Result<()> {
        self.tmux_output(&["select-pane", "-t", pane_id, "-T", title])?;
        Ok(())
    }

    fn window_option(&self, target: &str, option_name: &str) -> Result<Option<String>> {
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

fn run(cwd: &Path, program: &str, args: &[&str]) -> Result<()> {
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

fn git(cwd: &Path, args: &[&str]) -> Result<()> {
    run(cwd, "git", args)
}

fn git_stdout(cwd: &Path, args: &[&str]) -> Result<String> {
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

fn kmux_stdout(cwd: &Path, args: &[&str]) -> Result<String> {
    let assert = Command::cargo_bin("kmux")?
        .current_dir(cwd)
        .args(args)
        .assert()
        .success();
    Ok(String::from_utf8_lossy(&assert.get_output().stdout)
        .trim()
        .to_owned())
}

fn init_repo() -> Result<(TempDir, PathBuf)> {
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

fn write_config(root: &Path, content: &str) -> Result<PathBuf> {
    let config_home = root.join("config-home");
    let config_dir = config_home.join("kmux");
    fs::create_dir_all(&config_dir)?;
    fs::write(config_dir.join("config.yaml"), content)?;
    Ok(config_home)
}

fn raw_key_capture_command(capture_path: &Path, ready_path: &Path) -> String {
    format!(
        "stty raw -echo; : > {}; dd bs=1 count=16 of={} 2>/dev/null; sleep 5",
        shell_quote(&ready_path.display().to_string()),
        shell_quote(&capture_path.display().to_string())
    )
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn wait_for_path(path: &Path) -> Result<bool> {
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        if path.exists() {
            return Ok(true);
        }
        thread::sleep(Duration::from_millis(25));
    }
    Ok(false)
}

fn wait_for_file_bytes(path: &Path) -> Result<bool> {
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        if fs::read(path).is_ok_and(|bytes| !bytes.is_empty()) {
            return Ok(true);
        }
        thread::sleep(Duration::from_millis(25));
    }
    Ok(false)
}

fn kmux(repo: &Path, config_home: &Path, tmux: &TmuxFixture) -> Result<Command> {
    kmux_with_pane(repo, config_home, tmux, &tmux.pane_id)
}

fn kmux_with_pane(
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

fn agent_report_for_pane(config_home: &Path, pane_id: &str) -> Result<serde_json::Value> {
    find_agent_report(config_home, |value| {
        value
            .pointer("/target/pane_id")
            .and_then(serde_json::Value::as_str)
            == Some(pane_id)
    })?
    .ok_or_else(|| anyhow!("state for pane '{pane_id}' not found"))
}

fn agent_report_for_key(
    config_home: &Path,
    source: &str,
    instance: &str,
    id: &str,
) -> Result<serde_json::Value> {
    find_agent_report(config_home, |value| {
        value
            .pointer("/key/source")
            .and_then(serde_json::Value::as_str)
            == Some(source)
            && value
                .pointer("/key/instance")
                .and_then(serde_json::Value::as_str)
                == Some(instance)
            && value.pointer("/key/id").and_then(serde_json::Value::as_str) == Some(id)
    })?
    .ok_or_else(|| anyhow!("state for report '{source}/{instance}/{id}' not found"))
}

fn find_agent_report(
    config_home: &Path,
    matches: impl Fn(&serde_json::Value) -> bool,
) -> Result<Option<serde_json::Value>> {
    let reports_dir = agent_reports_dir(config_home);
    for entry in fs::read_dir(&reports_dir)
        .with_context(|| format!("failed to read state directory {}", reports_dir.display()))?
    {
        let path = entry?.path();
        let value: serde_json::Value = serde_json::from_slice(&fs::read(&path)?)
            .with_context(|| format!("failed to parse {}", path.display()))?;
        if matches(&value) {
            return Ok(Some(value));
        }
    }
    Ok(None)
}

fn agent_reports_dir(config_home: &Path) -> PathBuf {
    config_home
        .with_file_name("state-home")
        .join("kmux")
        .join("agent-reports")
}

fn state_timestamp(state: &serde_json::Value, field: &str) -> Result<u64> {
    state
        .get(field)
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| anyhow!("state timestamp '{field}' is missing or invalid"))
}

#[test]
fn lifecycle_commands_manage_worktree_and_window() -> Result<()> {
    let (temp, repo) = init_repo()?;
    let Some(tmux) = TmuxFixture::new(&repo)? else {
        return Ok(());
    };
    let config_home = write_config(
        temp.path(),
        r#"
window_prefix: kmux-
status_icons:
  working: W
  waiting: "?"
  done: D
"#,
    )?;
    let worktree = temp.path().join("project__worktrees/feature-auth");
    let renamed_worktree = temp.path().join("project__worktrees/auth-v2");

    kmux(&repo, &config_home, &tmux)?
        .args(["add", "feature/auth"])
        .assert()
        .success()
        .stdout(predicate::str::contains("created feature-auth"));
    assert!(worktree.is_dir());
    assert!(tmux.window_exists("kmux-feature-auth")?);

    kmux(&repo, &config_home, &tmux)?
        .args(["add", "feature/auth", "--open-if-exists"])
        .assert()
        .success()
        .stdout(predicate::str::contains("opened feature-auth"));

    kmux(&repo, &config_home, &tmux)?
        .arg("ls")
        .assert()
        .success()
        .stdout(predicate::str::contains("BRANCH"))
        .stdout(predicate::str::contains("AGE"))
        .stdout(predicate::str::contains("AGENT"))
        .stdout(predicate::str::contains("MUX"))
        .stdout(predicate::str::contains("UNMERGED"))
        .stdout(predicate::str::contains("PATH"))
        .stdout(predicate::str::contains("main"))
        .stdout(predicate::str::contains("feature/auth"))
        .stdout(predicate::str::contains("project__worktrees/feature-auth"));

    kmux(&repo, &config_home, &tmux)?
        .args(["path", "feature/auth"])
        .assert()
        .success()
        .stdout(predicate::str::contains(worktree.display().to_string()));

    kmux(&repo, &config_home, &tmux)?
        .args(["list", "--json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"handle\": \"feature-auth\""))
        .stdout(predicate::str::contains("\"branch\": \"feature/auth\""));

    let worktree_pane = tmux.pane_for_window("kmux-feature-auth")?;
    kmux_with_pane(&repo, &config_home, &tmux, &worktree_pane)?
        .args(["set-window-status", "working"])
        .assert()
        .success();
    assert_eq!(
        tmux.window_option(&worktree_pane, "@kmux_status")?
            .as_deref(),
        Some("W")
    );
    kmux(&repo, &config_home, &tmux)?
        .args(["status", "--json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"status\": \"working\""))
        .stdout(predicate::str::contains(
            "\"worktree_handle\": \"feature-auth\"",
        ));

    kmux(&repo, &config_home, &tmux)?
        .args(["rename", "feature-auth", "auth-v2"])
        .assert()
        .success()
        .stdout(predicate::str::contains("renamed feature-auth"));
    assert!(!worktree.exists());
    assert!(renamed_worktree.is_dir());
    assert!(!tmux.window_exists("kmux-feature-auth")?);
    assert!(tmux.window_exists("kmux-auth-v2")?);
    kmux(&repo, &config_home, &tmux)?
        .args(["status", "--json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"status\": \"working\""))
        .stdout(predicate::str::contains("\"worktree_handle\": \"auth-v2\""));

    kmux_with_pane(&repo, &config_home, &tmux, &worktree_pane)?
        .args(["set-window-status", "clear"])
        .assert()
        .success();
    assert_eq!(tmux.window_option(&worktree_pane, "@kmux_status")?, None);
    kmux(&repo, &config_home, &tmux)?
        .args(["status", "--json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("[]"));

    kmux(&repo, &config_home, &tmux)?
        .args(["close", "auth-v2"])
        .assert()
        .success()
        .stdout(predicate::str::contains("closed auth-v2"));
    assert!(!tmux.window_exists("kmux-auth-v2")?);
    assert!(renamed_worktree.is_dir());

    kmux(&repo, &config_home, &tmux)?
        .args(["open", "auth-v2"])
        .assert()
        .success()
        .stdout(predicate::str::contains("opened auth-v2"));
    assert!(tmux.window_exists("kmux-auth-v2")?);
    for option in [
        "@kmux_worktree_handle",
        "@kmux_worktree_path",
        "@kmux_worktree_branch",
    ] {
        tmux.tmux_output(&["set-option", "-uw", "-t", "kmux-auth-v2", option])?;
    }
    assert_eq!(
        tmux.window_option("kmux-auth-v2", "@kmux_worktree_path")?,
        None
    );

    kmux(&repo, &config_home, &tmux)?
        .args(["open", "auth-v2"])
        .assert()
        .success()
        .stdout(predicate::str::contains("opened auth-v2"));
    assert_eq!(
        tmux.window_option("kmux-auth-v2", "@kmux_worktree_handle")?,
        Some("auth-v2".to_owned())
    );
    assert_eq!(
        tmux.window_option("kmux-auth-v2", "@kmux_worktree_path")?,
        Some(renamed_worktree.display().to_string())
    );
    assert_eq!(
        tmux.window_option("kmux-auth-v2", "@kmux_worktree_branch")?,
        Some("feature/auth".to_owned())
    );

    kmux(&repo, &config_home, &tmux)?
        .args(["rm", "auth-v2"])
        .assert()
        .success()
        .stdout(predicate::str::contains("removed auth-v2"));
    assert!(!renamed_worktree.exists());
    assert!(!tmux.window_exists("kmux-auth-v2")?);
    assert!(git_stdout(&repo, &["show-ref", "--heads", "feature/auth"]).is_err());

    Ok(())
}

#[test]
fn status_renders_workmux_style_table_for_current_repo() -> Result<()> {
    let (temp, repo) = init_repo()?;
    let Some(tmux) = TmuxFixture::new(&repo)? else {
        return Ok(());
    };
    let config_home = write_config(
        temp.path(),
        r#"
window_prefix: kmux-
status_icons:
  working: W
  waiting: "?"
  done: D
"#,
    )?;
    let worktree = temp.path().join("project__worktrees/feature-status");

    tmux.set_pane_title(&tmux.pane_id, "Main agent")?;
    kmux(&repo, &config_home, &tmux)?
        .args(["set-window-status", "done"])
        .assert()
        .success();

    kmux(&repo, &config_home, &tmux)?
        .args(["add", "feature/status"])
        .assert()
        .success();
    let worktree_pane = tmux.pane_for_window("kmux-feature-status")?;
    tmux.set_pane_title(&worktree_pane, "Feature agent")?;
    kmux_with_pane(&worktree, &config_home, &tmux, &worktree_pane)?
        .args(["set-window-status", "working"])
        .assert()
        .success();

    fs::write(worktree.join("staged.txt"), "staged\n")?;
    git(&worktree, &["add", "staged.txt"])?;
    fs::write(worktree.join("README.md"), "changed\n")?;
    tmux.set_pane_title(&tmux.pane_id, "Main agent")?;
    tmux.set_pane_title(&worktree_pane, "Feature agent")?;

    let status = kmux(&repo, &config_home, &tmux)?
        .arg("status")
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&status.get_output().stdout);
    assert!(stdout.contains("WORKTREE"));
    assert!(stdout.contains("STATUS"));
    assert!(stdout.contains("ELAPSED"));
    assert!(stdout.contains("TITLE"));
    assert!(!stdout.contains("GIT"));
    assert!(stdout.contains("project (main)"));
    assert!(stdout.contains("feature-status (feature/status)"));
    assert!(stdout.contains("done"));
    assert!(stdout.contains("working"));

    let git_status = kmux(&repo, &config_home, &tmux)?
        .args(["status", "--git"])
        .assert()
        .success();
    let git_stdout = String::from_utf8_lossy(&git_status.get_output().stdout);
    assert!(git_stdout.contains("GIT"));
    assert!(git_stdout.contains("staged,unstaged"));

    kmux(&repo, &config_home, &tmux)?
        .args(["status", "--json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"worktree\": \"project\""))
        .stdout(predicate::str::contains(
            "\"worktree_handle\": \"feature-status\"",
        ));

    Ok(())
}

#[test]
fn set_window_status_preserves_elapsed_time_for_same_status() -> Result<()> {
    let (temp, repo) = init_repo()?;
    let Some(tmux) = TmuxFixture::new(&repo)? else {
        return Ok(());
    };
    let config_home = write_config(
        temp.path(),
        r#"
status_icons:
  working: W
  waiting: "?"
  done: D
"#,
    )?;

    kmux(&repo, &config_home, &tmux)?
        .args([
            "set-window-status",
            "working",
            "--session-id",
            "ses_visible_root",
            "--title",
            "Implement richer sidebar",
            "--context",
            "163.2K (41%)",
        ])
        .assert()
        .success();
    let first = agent_report_for_pane(&config_home, &tmux.pane_id)?;
    let first_changed = state_timestamp(&first, "status_changed_at")?;
    let first_observed = state_timestamp(&first, "observed_at")?;
    assert_eq!(
        tmux.window_option(&tmux.pane_id, "@kmux_status")?,
        Some("W".to_owned())
    );
    assert_eq!(first["title"].as_str(), Some("Implement richer sidebar"));
    assert_eq!(first["context"].as_str(), Some("163.2K (41%)"));
    assert_eq!(first["session_id"].as_str(), Some("ses_visible_root"));

    thread::sleep(Duration::from_millis(1100));
    kmux(&repo, &config_home, &tmux)?
        .args([
            "set-window-status",
            "working",
            "--title",
            "Implement richer sidebar",
            "--context",
            "170.0K (43%)",
        ])
        .assert()
        .success();
    let second = agent_report_for_pane(&config_home, &tmux.pane_id)?;
    let second_changed = state_timestamp(&second, "status_changed_at")?;
    let second_observed = state_timestamp(&second, "observed_at")?;

    assert_eq!(second_changed, first_changed);
    assert!(second_observed > first_observed);
    assert_eq!(second["title"].as_str(), Some("Implement richer sidebar"));
    assert_eq!(second["context"].as_str(), Some("170.0K (43%)"));

    thread::sleep(Duration::from_millis(1100));
    kmux(&repo, &config_home, &tmux)?
        .args(["set-window-status", "waiting"])
        .assert()
        .success();
    let third = agent_report_for_pane(&config_home, &tmux.pane_id)?;
    let third_changed = state_timestamp(&third, "status_changed_at")?;
    let third_observed = state_timestamp(&third, "observed_at")?;

    assert!(third_changed > second_changed);
    assert_eq!(third_observed, third_changed);
    Ok(())
}

#[test]
fn set_window_status_accepts_non_pane_agent_reports() -> Result<()> {
    let temp = TempDir::new()?;
    let config_home = write_config(temp.path(), "")?;
    let cwd = temp.path().join("workspace");
    fs::create_dir(&cwd)?;

    Command::cargo_bin("kmux")?
        .current_dir(&cwd)
        .env("XDG_CONFIG_HOME", &config_home)
        .env("XDG_STATE_HOME", config_home.with_file_name("state-home"))
        .env_remove("TMUX")
        .env_remove("TMUX_PANE")
        .args([
            "set-window-status",
            "working",
            "--source",
            "opencode-server",
            "--source-instance",
            "http://127.0.0.1:4096",
            "--session-id",
            "ses_parent",
            "--title",
            "Implement producer",
            "--context",
            "12.3K (6%)",
            "--directory",
            "/repo/project",
            "--worktree-path",
            "/repo/project",
            "--branch",
            "main",
        ])
        .assert()
        .success();

    let report = agent_report_for_key(
        &config_home,
        "opencode-server",
        "http://127.0.0.1:4096",
        "ses_parent",
    )?;
    assert_eq!(report["status"].as_str(), Some("working"));
    assert_eq!(report["title"].as_str(), Some("Implement producer"));
    assert_eq!(report["context"].as_str(), Some("12.3K (6%)"));
    assert_eq!(
        report
            .pointer("/target/directory")
            .and_then(serde_json::Value::as_str),
        Some("/repo/project")
    );
    assert_eq!(
        report
            .pointer("/target/worktree_path")
            .and_then(serde_json::Value::as_str),
        Some("/repo/project")
    );

    Command::cargo_bin("kmux")?
        .current_dir(&cwd)
        .env("XDG_CONFIG_HOME", &config_home)
        .env("XDG_STATE_HOME", config_home.with_file_name("state-home"))
        .env_remove("TMUX")
        .env_remove("TMUX_PANE")
        .args([
            "set-window-status",
            "clear",
            "--source",
            "opencode-server",
            "--source-instance",
            "http://127.0.0.1:4096",
            "--session-id",
            "ses_parent",
        ])
        .assert()
        .success();

    assert!(agent_reports_dir(&config_home).read_dir()?.next().is_none());
    Ok(())
}

#[test]
fn explicit_set_window_status_does_not_inherit_current_tmux_pane() -> Result<()> {
    let (temp, repo) = init_repo()?;
    let Some(tmux) = TmuxFixture::new(&repo)? else {
        return Ok(());
    };
    let config_home = write_config(temp.path(), "")?;

    kmux(&repo, &config_home, &tmux)?
        .args([
            "set-window-status",
            "working",
            "--source",
            "opencode-server",
            "--source-instance",
            "http://127.0.0.1:4096",
            "--session-id",
            "ses_parent",
            "--directory",
            "/repo/project",
        ])
        .assert()
        .success();

    let report = agent_report_for_key(
        &config_home,
        "opencode-server",
        "http://127.0.0.1:4096",
        "ses_parent",
    )?;
    assert_eq!(report.pointer("/target/tmux_instance"), None);
    assert_eq!(report.pointer("/target/pane_id"), None);
    assert_eq!(report.pointer("/target/window_id"), None);
    assert_eq!(report.pointer("/target/session_name"), None);
    assert_eq!(report.pointer("/target/window_name"), None);
    assert_eq!(tmux.window_option(&tmux.pane_id, "@kmux_status")?, None);
    Ok(())
}

#[test]
fn explicit_set_window_status_ignores_stale_tmux_environment() -> Result<()> {
    let temp = TempDir::new()?;
    let config_home = write_config(temp.path(), "")?;
    let cwd = temp.path().join("workspace");
    fs::create_dir(&cwd)?;

    Command::cargo_bin("kmux")?
        .current_dir(&cwd)
        .env("XDG_CONFIG_HOME", &config_home)
        .env("XDG_STATE_HOME", config_home.with_file_name("state-home"))
        .env("TMUX", "/tmp/missing-tmux-socket,1,0")
        .env("TMUX_PANE", "%999")
        .args([
            "set-window-status",
            "working",
            "--source",
            "opencode-server",
            "--source-instance",
            "http://127.0.0.1:4096",
            "--session-id",
            "ses_parent",
        ])
        .assert()
        .success();

    Command::cargo_bin("kmux")?
        .current_dir(&cwd)
        .env("XDG_CONFIG_HOME", &config_home)
        .env("XDG_STATE_HOME", config_home.with_file_name("state-home"))
        .env("TMUX", "/tmp/missing-tmux-socket,1,0")
        .env("TMUX_PANE", "%999")
        .args([
            "set-window-status",
            "clear",
            "--source",
            "opencode-server",
            "--source-instance",
            "http://127.0.0.1:4096",
            "--session-id",
            "ses_parent",
        ])
        .assert()
        .success();

    assert!(agent_reports_dir(&config_home).read_dir()?.next().is_none());
    Ok(())
}

#[test]
fn non_pane_agent_report_resolves_to_matching_tmux_worktree_window() -> Result<()> {
    let (temp, repo) = init_repo()?;
    let Some(tmux) = TmuxFixture::new(&repo)? else {
        return Ok(());
    };
    let config_home = write_config(temp.path(), "")?;
    let repo_path = repo.display().to_string();

    tmux.tmux_output(&[
        "set-option",
        "-w",
        "-t",
        &tmux.pane_id,
        "@kmux_worktree_handle",
        "project",
    ])?;
    tmux.tmux_output(&[
        "set-option",
        "-w",
        "-t",
        &tmux.pane_id,
        "@kmux_worktree_path",
        &repo_path,
    ])?;
    tmux.tmux_output(&[
        "set-option",
        "-w",
        "-t",
        &tmux.pane_id,
        "@kmux_worktree_branch",
        "main",
    ])?;

    Command::cargo_bin("kmux")?
        .current_dir(&repo)
        .env("XDG_CONFIG_HOME", &config_home)
        .env("XDG_STATE_HOME", config_home.with_file_name("state-home"))
        .env_remove("KMUX_TMUX_SOCKET_NAME")
        .env_remove("KMUX_TMUX_TMPDIR")
        .env_remove("TMUX")
        .env_remove("TMUX_PANE")
        .args([
            "set-window-status",
            "working",
            "--source",
            "opencode-server",
            "--source-instance",
            "http://127.0.0.1:4096",
            "--session-id",
            "ses_parent",
            "--title",
            "Implement producer",
            "--worktree-path",
            &repo_path,
        ])
        .assert()
        .success();

    let status = kmux(&repo, &config_home, &tmux)?
        .arg("status")
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&status.get_output().stdout);
    assert!(stdout.contains("project"));
    assert!(stdout.contains("working"));
    assert!(stdout.contains("Implement producer"));
    Ok(())
}

#[test]
fn sidebar_toggle_creates_refreshes_and_removes_marked_panes() -> Result<()> {
    let (temp, repo) = init_repo()?;
    let Some(tmux) = TmuxFixture::new(&repo)? else {
        return Ok(());
    };
    let config_home = write_config(temp.path(), "sidebar: {width: 30}\n")?;

    kmux(&repo, &config_home, &tmux)?
        .args(["sidebar", "on"])
        .assert()
        .success();
    assert_eq!(
        tmux.global_option("@kmux_sidebar_enabled")?.as_deref(),
        Some("1")
    );
    assert_eq!(
        tmux.global_option("@kmux_sidebar_width")?.as_deref(),
        Some("30")
    );
    assert_eq!(tmux.sidebar_pane_count()?, 1);
    assert!(tmux.wait_for_sidebar_title("kmux")?);
    assert!(
        tmux.global_hook("after-new-window[90]")?
            .contains("sidebar refresh")
    );
    let wake_hook = tmux.global_hook("after-select-window[90]")?;
    assert!(wake_hook.contains("sidebar wake"));
    assert!(wake_hook.contains("#{window_id}"));
    let pane_wake_hook = tmux.global_hook("after-select-pane[90]")?;
    assert!(pane_wake_hook.contains("sidebar wake"));
    assert!(pane_wake_hook.contains("#{window_id}"));
    let session_wake_hook = tmux.global_hook("client-session-changed[90]")?;
    assert!(session_wake_hook.contains("sidebar wake"));
    assert!(session_wake_hook.contains("#{window_id}"));
    assert!(tmux.has_one_sidebar_per_window()?);

    for index in 0..5 {
        tmux.tmux_output(&[
            "new-window",
            "-d",
            "-t",
            "project:",
            "-n",
            &format!("scratch-{index}"),
        ])?;
    }
    assert!(tmux.wait_for_one_sidebar_per_window()?);
    kmux(&repo, &config_home, &tmux)?
        .args(["sidebar", "refresh"])
        .assert()
        .success();
    assert!(tmux.has_one_sidebar_per_window()?);

    kmux(&repo, &config_home, &tmux)?
        .args(["sidebar", "off"])
        .assert()
        .success();
    assert_eq!(tmux.sidebar_pane_count()?, 0);
    assert_eq!(tmux.global_option("@kmux_sidebar_enabled")?, None);
    assert!(
        !tmux
            .tmux_output(&["show-hooks", "-g"])?
            .contains("sidebar refresh")
    );
    assert!(
        !tmux
            .tmux_output(&["show-hooks", "-g"])?
            .contains("sidebar wake")
    );
    Ok(())
}

#[test]
fn sidebar_on_reuses_restored_sidebar_pane() -> Result<()> {
    let (temp, repo) = init_repo()?;
    let Some(tmux) = TmuxFixture::new(&repo)? else {
        return Ok(());
    };
    let config_home = write_config(temp.path(), "sidebar: {width: 30}\n")?;
    let window_id = tmux.tmux_output(&["display-message", "-p", "#{window_id}"])?;
    let restored_pane = tmux.tmux_output(&[
        "split-window",
        "-d",
        "-h",
        "-b",
        "-t",
        &window_id,
        "-l",
        "10",
        "-P",
        "-F",
        "#{pane_id}",
        "while :; do sleep 60; done",
    ])?;
    tmux.set_pane_title(&restored_pane, "kmux")?;

    assert_eq!(tmux.sidebar_pane_count()?, 0);
    assert_eq!(tmux.pane_count_for_window(&window_id)?, 2);

    kmux(&repo, &config_home, &tmux)?
        .args(["sidebar", "on"])
        .assert()
        .success();

    assert_eq!(tmux.pane_count_for_window(&window_id)?, 2);
    assert_eq!(tmux.sidebar_pane_count()?, 1);
    assert_eq!(
        tmux.pane_format(&restored_pane, "#{@kmux_role}")?.as_str(),
        "sidebar"
    );
    assert_eq!(
        tmux.pane_format(&restored_pane, "#{pane_width}")?
            .parse::<u16>()?,
        30
    );
    assert!(tmux.wait_for_pane_command(&restored_pane, "kmux")?);

    Ok(())
}

#[test]
fn sidebar_wake_sends_key_only_to_target_window_sidebar() -> Result<()> {
    let (temp, repo) = init_repo()?;
    let Some(tmux) = TmuxFixture::new(&repo)? else {
        return Ok(());
    };
    let config_home = write_config(temp.path(), "")?;

    let source_window_id = tmux.tmux_output(&["display-message", "-p", "#{window_id}"])?;
    let source_capture = temp.path().join("source-wake.bin");
    let source_ready = temp.path().join("source-wake.ready");
    let source_command = raw_key_capture_command(&source_capture, &source_ready);
    let source_sidebar = tmux.tmux_output(&[
        "split-window",
        "-d",
        "-t",
        &source_window_id,
        "-P",
        "-F",
        "#{pane_id}",
        &source_command,
    ])?;
    tmux.tmux_output(&[
        "set-option",
        "-p",
        "-t",
        &source_sidebar,
        "@kmux_role",
        "sidebar",
    ])?;

    let target_window_id = tmux.tmux_output(&[
        "new-window",
        "-d",
        "-t",
        "project:",
        "-n",
        "wake-target",
        "-P",
        "-F",
        "#{window_id}",
    ])?;
    let target_capture = temp.path().join("target-wake.bin");
    let target_ready = temp.path().join("target-wake.ready");
    let target_command = raw_key_capture_command(&target_capture, &target_ready);
    let target_sidebar = tmux.tmux_output(&[
        "split-window",
        "-d",
        "-t",
        &target_window_id,
        "-P",
        "-F",
        "#{pane_id}",
        &target_command,
    ])?;
    tmux.tmux_output(&[
        "set-option",
        "-p",
        "-t",
        &target_sidebar,
        "@kmux_role",
        "sidebar",
    ])?;

    assert!(wait_for_path(&source_ready)?);
    assert!(wait_for_path(&target_ready)?);

    kmux(&repo, &config_home, &tmux)?
        .args(["sidebar", "wake", &target_window_id])
        .assert()
        .success();

    assert!(wait_for_file_bytes(&target_capture)?);
    assert_eq!(fs::read(&source_capture).map_or(0, |bytes| bytes.len()), 0);

    Ok(())
}

#[test]
fn add_runs_configured_file_ops_and_post_create() -> Result<()> {
    let (temp, repo) = init_repo()?;
    let Some(tmux) = TmuxFixture::new(&repo)? else {
        return Ok(());
    };
    fs::write(repo.join(".envrc"), "use flake\n")?;
    fs::create_dir(repo.join(".opencode"))?;
    fs::write(repo.join(".opencode/config.json"), "{}\n")?;
    fs::write(repo.join("codebook.toml"), "[book]\n")?;
    let config_home = write_config(
        temp.path(),
        r#"
window_prefix: kmux-
post_create:
  - touch hook-ran
files:
  copy:
    - .envrc
    - .opencode
    - missing-source
  symlink:
    - codebook.toml
"#,
    )?;
    let worktree = temp.path().join("project__worktrees/feature-files");

    kmux(&repo, &config_home, &tmux)?
        .args(["add", "feature/files", "--background"])
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "configured file source missing for copy",
        ));

    assert_eq!(fs::read_to_string(worktree.join(".envrc"))?, "use flake\n");
    assert_eq!(
        fs::read_to_string(worktree.join(".opencode/config.json"))?,
        "{}\n"
    );
    assert!(worktree.join("hook-ran").exists());
    assert!(
        worktree
            .join("codebook.toml")
            .symlink_metadata()?
            .file_type()
            .is_symlink()
    );

    Ok(())
}

#[test]
fn add_remote_branch_creates_local_worktree_without_remote_prefix() -> Result<()> {
    let (temp, repo) = init_repo()?;
    let Some(tmux) = TmuxFixture::new(&repo)? else {
        return Ok(());
    };
    let remote = temp.path().join("remote.git");
    let remote_arg = remote.display().to_string();
    run(temp.path(), "git", &["init", "--bare", "remote.git"])?;
    git(&repo, &["remote", "add", "origin", &remote_arg])?;
    git(&repo, &["push", "-u", "origin", "main"])?;
    git(&repo, &["branch", "remote-only"])?;
    git(&repo, &["push", "origin", "remote-only"])?;
    git(&repo, &["branch", "-D", "remote-only"])?;

    let config_home = write_config(temp.path(), "window_prefix: kmux-\n")?;
    let worktree = temp.path().join("project__worktrees/remote-only");

    kmux(&repo, &config_home, &tmux)?
        .args(["add", "origin/remote-only", "--background"])
        .assert()
        .success()
        .stdout(predicate::str::contains("created remote-only"));

    assert!(worktree.is_dir());
    assert!(git_stdout(&repo, &["show-ref", "--heads", "remote-only"]).is_ok());
    assert!(git_stdout(&repo, &["show-ref", "--heads", "origin/remote-only"]).is_err());
    assert_eq!(
        git_stdout(
            &repo,
            &["rev-parse", "--abbrev-ref", "remote-only@{upstream}"]
        )?,
        "origin/remote-only"
    );
    assert!(tmux.window_exists("kmux-remote-only")?);

    Ok(())
}

#[test]
fn remove_without_name_targets_current_kmux_worktree() -> Result<()> {
    let (temp, repo) = init_repo()?;
    let Some(tmux) = TmuxFixture::new(&repo)? else {
        return Ok(());
    };
    let config_home = write_config(temp.path(), "window_prefix: kmux-\n")?;
    let worktree = temp.path().join("project__worktrees/feature-current");

    kmux(&repo, &config_home, &tmux)?
        .args(["add", "feature/current"])
        .assert()
        .success();
    assert!(worktree.is_dir());
    assert!(tmux.window_exists("kmux-feature-current")?);

    kmux(&repo, &config_home, &tmux)?
        .arg("rm")
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "requires a worktree name when run from the main worktree",
        ));

    let worktree_pane = tmux.pane_for_window("kmux-feature-current")?;
    kmux_with_pane(&worktree, &config_home, &tmux, &worktree_pane)?
        .arg("rm")
        .assert()
        .success()
        .stdout(predicate::str::contains("removed feature-current"));

    assert!(!worktree.exists());
    assert!(!tmux.window_exists("kmux-feature-current")?);
    assert!(git_stdout(&repo, &["show-ref", "--heads", "feature/current"]).is_err());

    Ok(())
}

#[test]
fn commands_reject_external_worktrees_with_matching_branch() -> Result<()> {
    let (temp, repo) = init_repo()?;
    let Some(tmux) = TmuxFixture::new(&repo)? else {
        return Ok(());
    };
    let config_home = write_config(temp.path(), "window_prefix: kmux-\n")?;
    let external = temp.path().join("external-auth");
    let external_arg = external.display().to_string();
    git(
        &repo,
        &["worktree", "add", "-b", "feature/external", &external_arg],
    )?;

    kmux(&repo, &config_home, &tmux)?
        .args(["add", "feature/external", "--open-if-exists"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("already checked out outside kmux"));
    kmux(&repo, &config_home, &tmux)?
        .args(["open", "feature/external"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "worktree 'feature/external' not found",
        ));
    kmux(&repo, &config_home, &tmux)?
        .args(["remove", "feature/external", "--force"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "worktree 'feature/external' not found",
        ));

    assert!(external.is_dir());
    assert!(git_stdout(&repo, &["show-ref", "--heads", "feature/external"]).is_ok());

    let nested_parent = temp.path().join("project__worktrees/archive");
    fs::create_dir_all(&nested_parent)?;
    let nested = nested_parent.join("nested-auth");
    let nested_arg = nested.display().to_string();
    git(
        &repo,
        &["worktree", "add", "-b", "feature/nested", &nested_arg],
    )?;
    kmux(&repo, &config_home, &tmux)?
        .args(["open", "nested-auth"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("worktree 'nested-auth' not found"));
    kmux(&repo, &config_home, &tmux)?
        .args(["add", "feature/nested", "--open-if-exists"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("already checked out outside kmux"));
    assert!(nested.is_dir());
    Ok(())
}

#[test]
fn remove_unmerged_branch_fails_before_deleting_worktree() -> Result<()> {
    let (temp, repo) = init_repo()?;
    let Some(tmux) = TmuxFixture::new(&repo)? else {
        return Ok(());
    };
    let config_home = write_config(temp.path(), "window_prefix: kmux-\n")?;
    let worktree = temp.path().join("project__worktrees/feature-unmerged");

    kmux(&repo, &config_home, &tmux)?
        .args(["add", "feature/unmerged"])
        .assert()
        .success();
    fs::write(worktree.join("change.txt"), "unmerged\n")?;
    git(&worktree, &["add", "change.txt"])?;
    git(&worktree, &["commit", "-m", "unmerged change"])?;

    kmux(&repo, &config_home, &tmux)?
        .args(["remove", "feature-unmerged"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("is not safely merged"));

    assert!(worktree.is_dir());
    assert!(tmux.window_exists("kmux-feature-unmerged")?);
    assert!(git_stdout(&repo, &["show-ref", "--heads", "feature/unmerged"]).is_ok());

    kmux(&repo, &config_home, &tmux)?
        .args(["remove", "feature-unmerged", "--force"])
        .assert()
        .success();
    assert!(!worktree.exists());
    assert!(git_stdout(&repo, &["show-ref", "--heads", "feature/unmerged"]).is_err());
    Ok(())
}

#[test]
fn remove_branch_not_merged_to_upstream_fails_before_deleting_worktree() -> Result<()> {
    let (temp, repo) = init_repo()?;
    let Some(tmux) = TmuxFixture::new(&repo)? else {
        return Ok(());
    };
    let remote = temp.path().join("remote.git");
    let remote_arg = remote.display().to_string();
    run(temp.path(), "git", &["init", "--bare", "remote.git"])?;
    git(&repo, &["remote", "add", "origin", &remote_arg])?;
    git(&repo, &["push", "-u", "origin", "main"])?;

    let config_home = write_config(temp.path(), "window_prefix: kmux-\n")?;
    let worktree = temp.path().join("project__worktrees/feature-upstream");

    kmux(&repo, &config_home, &tmux)?
        .args(["add", "feature/upstream"])
        .assert()
        .success();
    git(
        &repo,
        &[
            "branch",
            "--set-upstream-to",
            "origin/main",
            "feature/upstream",
        ],
    )?;
    fs::write(worktree.join("upstream.txt"), "upstream gap\n")?;
    git(&worktree, &["add", "upstream.txt"])?;
    git(&worktree, &["commit", "-m", "upstream gap"])?;
    git(
        &repo,
        &[
            "merge",
            "--no-ff",
            "feature/upstream",
            "-m",
            "merge upstream gap",
        ],
    )?;

    kmux(&repo, &config_home, &tmux)?
        .args(["remove", "feature-upstream"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("is not safely merged"));
    assert!(worktree.is_dir());
    assert!(tmux.window_exists("kmux-feature-upstream")?);
    assert!(git_stdout(&repo, &["show-ref", "--heads", "feature/upstream"]).is_ok());

    kmux(&repo, &config_home, &tmux)?
        .args(["remove", "feature-upstream", "--force"])
        .assert()
        .success();
    assert!(!worktree.exists());
    assert!(git_stdout(&repo, &["show-ref", "--heads", "feature/upstream"]).is_err());
    Ok(())
}
