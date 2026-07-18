pub mod support;

use std::fs;
use std::path::Path;

use anyhow::Result;
use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

use support::{
    TmuxFixture, agent_observations_dir, delete_opencode_agent_observation_args, git, init_repo,
    kmux, raw_key_capture_command, set_opencode_status_args, wait_for_nonempty_file, wait_for_path,
    write_config,
};

fn kmux_without_tmux(cwd: &Path, config_home: &Path) -> Result<Command> {
    let mut command = Command::cargo_bin("kmux")?;
    command
        .current_dir(cwd)
        .env("XDG_CONFIG_HOME", config_home)
        .env("XDG_STATE_HOME", config_home.with_file_name("state-home"))
        .env_remove("TMUX")
        .env_remove("TMUX_PANE")
        .env_remove("KMUX_TMUX_SOCKET_NAME")
        .env_remove("KMUX_TMUX_TMPDIR")
        .env_remove("KMUX_DISABLE_SET_AGENT_STATUS");
    Ok(command)
}

#[test]
fn set_agent_status_flows_to_global_status_and_tmux_badge() -> Result<()> {
    let (temp, repo) = init_repo()?;
    let Some(tmux) = TmuxFixture::new(&repo)? else {
        return Ok(());
    };
    let config_home = write_config(
        temp.path(),
        r#"
status_icons:
  working: W
"#,
    )?;
    let repo_path = repo.display().to_string();

    kmux(&repo, &config_home, &tmux)?
        .args(set_opencode_status_args(
            Some("working"),
            "ses_status_smoke",
            "integration",
            "reporter-one",
            &[
                ("--directory", &repo_path),
                ("--git-repo-name", "project"),
                ("--git-repo-path", &repo_path),
                ("--git-branch", "main"),
                ("--title", "Example task"),
            ],
        ))
        .assert()
        .success();

    assert_eq!(
        tmux.window_option(&tmux.pane_id, "@kmux_status")?
            .as_deref(),
        Some("W")
    );

    fs::write(repo.join("staged.txt"), "staged\n")?;
    git(&repo, &["add", "staged.txt"])?;
    fs::write(repo.join("README.md"), "changed\n")?;

    let status = kmux(&repo, &config_home, &tmux)?
        .args(["status", "--git"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&status.get_output().stdout);
    assert!(stdout.contains("WORKSPACE"));
    assert!(stdout.contains("STATUS"));
    assert!(stdout.contains("GIT"));
    assert!(stdout.contains("project (main)"));
    assert!(stdout.contains("working"));
    assert!(stdout.contains("staged,unstaged"));
    assert!(stdout.contains("Example task"));

    kmux(&repo, &config_home, &tmux)?
        .args(["status", "--json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"workspace\": \"project\""))
        .stdout(predicate::str::contains("\"status\": \"working\""))
        .stdout(predicate::str::contains("\"title\": \"Example task\""));

    Ok(())
}

#[test]
fn status_includes_agents_from_other_repos_without_tmux() -> Result<()> {
    let (temp, repo) = init_repo()?;
    let config_home = write_config(temp.path(), "")?;
    let other_repo = temp.path().join("other-project");
    fs::create_dir(&other_repo)?;
    git(&other_repo, &["init", "--initial-branch", "main"])?;
    git(
        &other_repo,
        &["config", "user.email", "test@example.invalid"],
    )?;
    git(&other_repo, &["config", "user.name", "Test User"])?;
    fs::write(other_repo.join("README.md"), "test\n")?;
    git(&other_repo, &["add", "README.md"])?;
    git(&other_repo, &["commit", "-m", "initial"])?;
    let other_repo_path = other_repo.display().to_string();

    kmux_without_tmux(&other_repo, &config_home)?
        .args(set_opencode_status_args(
            Some("working"),
            "ses_other_repo",
            "integration",
            "reporter-one",
            &[
                ("--directory", &other_repo_path),
                ("--git-repo-name", "other-project"),
                ("--git-repo-path", &other_repo_path),
                ("--git-branch", "main"),
                ("--title", "Other repo task"),
            ],
        ))
        .assert()
        .success();

    let status = kmux_without_tmux(&repo, &config_home)?
        .arg("status")
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&status.get_output().stdout);
    assert!(stdout.contains("other-project (main)"));
    assert!(stdout.contains("Other repo task"));

    Ok(())
}

#[test]
fn disabled_set_agent_status_does_not_write_observation() -> Result<()> {
    let temp = TempDir::new()?;
    let config_home = write_config(temp.path(), "")?;
    let cwd = temp.path().join("workspace");
    fs::create_dir(&cwd)?;

    kmux_without_tmux(&cwd, &config_home)?
        .env("KMUX_DISABLE_SET_AGENT_STATUS", "1")
        .args(set_opencode_status_args(
            Some("working"),
            "ses_disabled",
            "integration",
            "reporter-one",
            &[("--title", "Ignored")],
        ))
        .assert()
        .success();

    assert!(!agent_observations_dir(&config_home).exists());
    Ok(())
}

#[test]
fn set_agent_status_ignores_stale_ambient_tmux_environment() -> Result<()> {
    let temp = TempDir::new()?;
    let config_home = write_config(temp.path(), "")?;
    let cwd = temp.path().join("workspace");
    fs::create_dir(&cwd)?;

    kmux_without_tmux(&cwd, &config_home)?
        .env("TMUX", "/tmp/missing-tmux-socket,1,0")
        .env("TMUX_PANE", "%999")
        .args(set_opencode_status_args(
            Some("working"),
            "ses_stale_tmux",
            "integration",
            "reporter-one",
            &[],
        ))
        .assert()
        .success();

    kmux_without_tmux(&cwd, &config_home)?
        .env("TMUX", "/tmp/missing-tmux-socket,1,0")
        .env("TMUX_PANE", "%999")
        .args(delete_opencode_agent_observation_args(
            "ses_stale_tmux",
            "integration",
            "reporter-one",
        ))
        .assert()
        .success();

    assert!(
        agent_observations_dir(&config_home)
            .read_dir()?
            .next()
            .is_none()
    );
    Ok(())
}

#[test]
fn set_agent_status_notifies_live_sidebar_panes() -> Result<()> {
    let (temp, repo) = init_repo()?;
    let Some(tmux) = TmuxFixture::new(&repo)? else {
        return Ok(());
    };
    let config_home = write_config(temp.path(), "")?;
    let window_id = tmux.pane_format(&tmux.pane_id, "#{window_id}")?;
    let capture = temp.path().join("sidebar-refresh.bin");
    let ready = temp.path().join("sidebar-refresh.ready");
    let command = raw_key_capture_command(&capture, &ready);
    let sidebar = tmux.tmux_output(&[
        "split-window",
        "-d",
        "-t",
        &window_id,
        "-P",
        "-F",
        "#{pane_id}",
        &command,
    ])?;
    tmux.tmux_output(&["set-option", "-p", "-t", &sidebar, "@kmux_role", "sidebar"])?;
    wait_for_path(&ready)?;

    let repo_path = repo.display().to_string();
    kmux(&repo, &config_home, &tmux)?
        .args(set_opencode_status_args(
            Some("working"),
            "ses_notify_sidebar",
            "integration",
            "reporter-one",
            &[("--directory", &repo_path)],
        ))
        .assert()
        .success();

    wait_for_nonempty_file(&capture)?;
    Ok(())
}
