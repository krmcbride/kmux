use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
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
        .stdout(predicate::str::contains("_kmux"));
}

#[test]
fn unimplemented_commands_fail_clearly() {
    Command::cargo_bin("kmux")
        .expect("kmux binary should be available")
        .args(["status"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("status is not implemented yet"));
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

    fn apply_env(&self, command: &mut Command) {
        command
            .env("KMUX_TMUX_SOCKET_NAME", &self.socket_name)
            .env("KMUX_TMUX_TMPDIR", self.socket_dir.path())
            .env("TMUX_PANE", &self.pane_id);
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

fn kmux(repo: &Path, config_home: &Path, tmux: &TmuxFixture) -> Result<Command> {
    let mut command = Command::cargo_bin("kmux")?;
    command
        .current_dir(repo)
        .env("XDG_CONFIG_HOME", config_home);
    tmux.apply_env(&mut command);
    Ok(command)
}

#[test]
fn lifecycle_commands_manage_worktree_and_window() -> Result<()> {
    let (temp, repo) = init_repo()?;
    let Some(tmux) = TmuxFixture::new(&repo)? else {
        return Ok(());
    };
    let config_home = write_config(temp.path(), "window_prefix: kmux-\n")?;
    let worktree = temp.path().join("project__worktrees/feature-auth");

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

    kmux(&repo, &config_home, &tmux)?
        .args(["close", "feature-auth"])
        .assert()
        .success()
        .stdout(predicate::str::contains("closed feature-auth"));
    assert!(!tmux.window_exists("kmux-feature-auth")?);
    assert!(worktree.is_dir());

    kmux(&repo, &config_home, &tmux)?
        .args(["open", "feature-auth"])
        .assert()
        .success()
        .stdout(predicate::str::contains("opened feature-auth"));
    assert!(tmux.window_exists("kmux-feature-auth")?);

    kmux(&repo, &config_home, &tmux)?
        .args(["remove", "feature-auth"])
        .assert()
        .success()
        .stdout(predicate::str::contains("removed feature-auth"));
    assert!(!worktree.exists());
    assert!(!tmux.window_exists("kmux-feature-auth")?);
    assert!(git_stdout(&repo, &["show-ref", "--heads", "feature/auth"]).is_err());

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
