mod support;

use std::fs;
use std::thread;
use std::time::Duration;

use anyhow::Result;
use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

use support::{
    TmuxFixture, agent_observation_for_key, agent_observation_for_pane, agent_observations_dir,
    delete_opencode_agent_observation_args, delete_opencode_agent_session_args, git, init_repo,
    kmux, kmux_with_pane, set_agent_status_args, set_opencode_status_args, state_timestamp,
    state_u64, write_config,
};

#[test]
fn status_renders_kmux_table_for_current_repo() -> Result<()> {
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
    let repo_path = repo.display().to_string();
    let worktree_path = worktree.display().to_string();

    tmux.set_pane_title(&tmux.pane_id, "Main agent")?;
    let main_window_id = tmux.pane_format(&tmux.pane_id, "#{window_id}")?;
    let main_producer = format!("default/{}", tmux.pane_id);
    kmux(&repo, &config_home, &tmux)?
        .args(set_opencode_status_args(
            Some("done"),
            "ses_main_status",
            "tui",
            &main_producer,
            &[
                ("--pane-id", &tmux.pane_id),
                ("--window-id", &main_window_id),
                ("--session-name", "project"),
                ("--window-name", "project"),
                ("--repo-name", "project"),
                ("--repo-path", &repo_path),
                ("--directory", &repo_path),
                ("--worktree-path", &repo_path),
                ("--worktree-handle", "project"),
                ("--branch", "main"),
            ],
        ))
        .assert()
        .success();

    kmux(&repo, &config_home, &tmux)?
        .args(["add", "feature/status"])
        .assert()
        .success();
    let worktree_pane = tmux.pane_for_window("kmux-feature-status")?;
    let worktree_window_id = tmux.pane_format(&worktree_pane, "#{window_id}")?;
    let feature_producer = format!("default/{worktree_pane}");
    tmux.set_pane_title(&worktree_pane, "Feature agent")?;
    kmux_with_pane(&worktree, &config_home, &tmux, &worktree_pane)?
        .args(set_opencode_status_args(
            Some("working"),
            "ses_feature_status",
            "tui",
            &feature_producer,
            &[
                ("--pane-id", &worktree_pane),
                ("--window-id", &worktree_window_id),
                ("--session-name", "project"),
                ("--window-name", "kmux-feature-status"),
                ("--repo-name", "project"),
                ("--repo-path", &repo_path),
                ("--directory", &worktree_path),
                ("--worktree-path", &worktree_path),
                ("--worktree-handle", "feature-status"),
                ("--branch", "feature/status"),
            ],
        ))
        .assert()
        .success();
    let feature_report = agent_observation_for_pane(&config_home, &worktree_pane)?;
    assert_eq!(
        feature_report
            .pointer("/target/repo_name")
            .and_then(serde_json::Value::as_str),
        Some("project")
    );
    assert_eq!(
        feature_report
            .pointer("/target/repo_path")
            .and_then(serde_json::Value::as_str),
        Some(repo_path.as_str())
    );
    assert_eq!(
        feature_report
            .pointer("/target/branch")
            .and_then(serde_json::Value::as_str),
        Some("feature/status")
    );

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
fn set_agent_status_preserves_elapsed_time_for_same_status() -> Result<()> {
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
    let window_id = tmux.pane_format(&tmux.pane_id, "#{window_id}")?;
    let window_name = tmux.pane_format(&tmux.pane_id, "#{window_name}")?;
    let producer_instance = format!("default/{}", tmux.pane_id);

    kmux(&repo, &config_home, &tmux)?
        .args(set_opencode_status_args(
            Some("working"),
            "ses_visible_root",
            "tui",
            &producer_instance,
            &[
                ("--pane-id", &tmux.pane_id),
                ("--window-id", &window_id),
                ("--session-name", "project"),
                ("--window-name", &window_name),
                ("--title", "Implement richer sidebar"),
                ("--context", "163.2K (41%)"),
            ],
        ))
        .assert()
        .success();
    let first = agent_observation_for_pane(&config_home, &tmux.pane_id)?;
    let first_changed = state_timestamp(&first, "status_changed_at")?;
    let first_observed = state_timestamp(&first, "observed_at")?;
    let first_working_elapsed = state_u64(&first, "working_elapsed_secs")?;
    assert_eq!(
        tmux.window_option(&tmux.pane_id, "@kmux_status")?,
        Some("W".to_owned())
    );
    assert_eq!(first["title"].as_str(), Some("Implement richer sidebar"));
    assert_eq!(first["context"].as_str(), Some("163.2K (41%)"));
    assert_eq!(
        first
            .pointer("/key/session/session_id")
            .and_then(serde_json::Value::as_str),
        Some("ses_visible_root")
    );
    assert_eq!(first_working_elapsed, 0);

    thread::sleep(Duration::from_millis(1100));
    kmux(&repo, &config_home, &tmux)?
        .args(set_opencode_status_args(
            Some("working"),
            "ses_visible_root",
            "tui",
            &producer_instance,
            &[
                ("--pane-id", &tmux.pane_id),
                ("--window-id", &window_id),
                ("--session-name", "project"),
                ("--window-name", &window_name),
                ("--title", "Implement richer sidebar"),
                ("--context", "170.0K (43%)"),
            ],
        ))
        .assert()
        .success();
    let second = agent_observation_for_pane(&config_home, &tmux.pane_id)?;
    let second_changed = state_timestamp(&second, "status_changed_at")?;
    let second_observed = state_timestamp(&second, "observed_at")?;
    let second_working_elapsed = state_u64(&second, "working_elapsed_secs")?;

    assert_eq!(second_changed, first_changed);
    assert!(second_observed > first_observed);
    assert_eq!(second_working_elapsed, 0);
    assert_eq!(second["title"].as_str(), Some("Implement richer sidebar"));
    assert_eq!(second["context"].as_str(), Some("170.0K (43%)"));

    thread::sleep(Duration::from_millis(1100));
    kmux(&repo, &config_home, &tmux)?
        .args(set_opencode_status_args(
            Some("waiting"),
            "ses_visible_root",
            "tui",
            &producer_instance,
            &[
                ("--pane-id", &tmux.pane_id),
                ("--window-id", &window_id),
                ("--session-name", "project"),
                ("--window-name", &window_name),
            ],
        ))
        .assert()
        .success();
    let third = agent_observation_for_pane(&config_home, &tmux.pane_id)?;
    let third_changed = state_timestamp(&third, "status_changed_at")?;
    let third_observed = state_timestamp(&third, "observed_at")?;
    let third_working_elapsed = state_u64(&third, "working_elapsed_secs")?;

    assert!(third_changed > second_changed);
    assert_eq!(third_observed, third_changed);
    assert!(third_working_elapsed > 0);
    Ok(())
}

#[test]
fn set_agent_status_accepts_non_pane_observations() -> Result<()> {
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
        .args(set_opencode_status_args(
            Some("working"),
            "ses_parent",
            "server",
            "http://127.0.0.1:4096",
            &[
                ("--title", "Implement producer"),
                ("--context", "12.3K (6%)"),
                ("--repo-name", "project"),
                ("--repo-path", "/repo/project"),
                ("--directory", "/repo/project"),
                ("--worktree-path", "/repo/project"),
                ("--branch", "main"),
            ],
        ))
        .assert()
        .success();

    let report = agent_observation_for_key(
        &config_home,
        "opencode",
        "ses_parent",
        "server",
        "http://127.0.0.1:4096",
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
            .pointer("/target/repo_name")
            .and_then(serde_json::Value::as_str),
        Some("project")
    );
    assert_eq!(
        report
            .pointer("/target/repo_path")
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
        .args(delete_opencode_agent_observation_args(
            "ses_parent",
            "server",
            "http://127.0.0.1:4096",
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
fn set_agent_status_persists_non_opencode_agent_kind() -> Result<()> {
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
        .args(set_agent_status_args(
            "codex",
            Some("waiting"),
            "codex_session",
            "server",
            "codex-daemon",
            &[("--title", "Codex task")],
        ))
        .assert()
        .success();

    let report = agent_observation_for_key(
        &config_home,
        "codex",
        "codex_session",
        "server",
        "codex-daemon",
    )?;
    assert_eq!(report["status"].as_str(), Some("waiting"));
    assert_eq!(report["title"].as_str(), Some("Codex task"));
    Ok(())
}

#[test]
fn set_agent_status_delete_session_removes_all_producer_observations() -> Result<()> {
    let temp = TempDir::new()?;
    let config_home = write_config(temp.path(), "")?;
    let cwd = temp.path().join("workspace");
    fs::create_dir(&cwd)?;
    for (producer_kind, producer_instance) in
        [("tui", "default/%1"), ("server", "http://127.0.0.1:4096")]
    {
        Command::cargo_bin("kmux")?
            .current_dir(&cwd)
            .env("XDG_CONFIG_HOME", &config_home)
            .env("XDG_STATE_HOME", config_home.with_file_name("state-home"))
            .env_remove("TMUX")
            .env_remove("TMUX_PANE")
            .args(set_opencode_status_args(
                Some("working"),
                "ses_parent",
                producer_kind,
                producer_instance,
                &[],
            ))
            .assert()
            .success();
    }
    assert_eq!(agent_observations_dir(&config_home).read_dir()?.count(), 2);

    Command::cargo_bin("kmux")?
        .current_dir(&cwd)
        .env("XDG_CONFIG_HOME", &config_home)
        .env("XDG_STATE_HOME", config_home.with_file_name("state-home"))
        .env_remove("TMUX")
        .env_remove("TMUX_PANE")
        .args(delete_opencode_agent_session_args(
            "ses_parent",
            "server",
            "http://127.0.0.1:4096",
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
fn explicit_set_agent_status_does_not_inherit_current_tmux_pane() -> Result<()> {
    let (temp, repo) = init_repo()?;
    let Some(tmux) = TmuxFixture::new(&repo)? else {
        return Ok(());
    };
    let config_home = write_config(temp.path(), "")?;

    kmux(&repo, &config_home, &tmux)?
        .args(set_opencode_status_args(
            Some("working"),
            "ses_parent",
            "server",
            "http://127.0.0.1:4096",
            &[("--directory", "/repo/project")],
        ))
        .assert()
        .success();

    let report = agent_observation_for_key(
        &config_home,
        "opencode",
        "ses_parent",
        "server",
        "http://127.0.0.1:4096",
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
fn explicit_set_agent_status_preserves_timing_when_target_window_changes() -> Result<()> {
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
        .args(set_opencode_status_args(
            Some("working"),
            "ses_parent",
            "server",
            "http://127.0.0.1:4096",
            &[("--window-id", "@old")],
        ))
        .assert()
        .success();
    let first = agent_observation_for_key(
        &config_home,
        "opencode",
        "ses_parent",
        "server",
        "http://127.0.0.1:4096",
    )?;
    let first_changed = state_timestamp(&first, "status_changed_at")?;

    thread::sleep(Duration::from_millis(1100));
    Command::cargo_bin("kmux")?
        .current_dir(&cwd)
        .env("XDG_CONFIG_HOME", &config_home)
        .env("XDG_STATE_HOME", config_home.with_file_name("state-home"))
        .env_remove("TMUX")
        .env_remove("TMUX_PANE")
        .args(set_opencode_status_args(
            Some("working"),
            "ses_parent",
            "server",
            "http://127.0.0.1:4096",
            &[("--window-id", "@new")],
        ))
        .assert()
        .success();

    let second = agent_observation_for_key(
        &config_home,
        "opencode",
        "ses_parent",
        "server",
        "http://127.0.0.1:4096",
    )?;
    let second_changed = state_timestamp(&second, "status_changed_at")?;
    assert_eq!(second_changed, first_changed);
    assert_eq!(state_u64(&second, "working_elapsed_secs")?, 0);
    assert_eq!(
        second
            .pointer("/target/window_id")
            .and_then(serde_json::Value::as_str),
        Some("@new")
    );
    Ok(())
}

#[test]
fn explicit_set_agent_status_keeps_tmux_instance_out_of_observation_identity() -> Result<()> {
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
        .args(set_opencode_status_args(
            Some("working"),
            "ses_parent",
            "server",
            "default",
            &[("--tmux-instance", "old-target")],
        ))
        .assert()
        .success();
    let first =
        agent_observation_for_key(&config_home, "opencode", "ses_parent", "server", "default")?;
    let first_changed = state_timestamp(&first, "status_changed_at")?;
    assert_eq!(
        first
            .pointer("/target/tmux_instance")
            .and_then(serde_json::Value::as_str),
        Some("old-target")
    );

    thread::sleep(Duration::from_millis(1100));
    Command::cargo_bin("kmux")?
        .current_dir(&cwd)
        .env("XDG_CONFIG_HOME", &config_home)
        .env("XDG_STATE_HOME", config_home.with_file_name("state-home"))
        .env_remove("TMUX")
        .env_remove("TMUX_PANE")
        .args(set_opencode_status_args(
            Some("working"),
            "ses_parent",
            "server",
            "default",
            &[("--tmux-instance", "new-target")],
        ))
        .assert()
        .success();

    let second =
        agent_observation_for_key(&config_home, "opencode", "ses_parent", "server", "default")?;
    let second_changed = state_timestamp(&second, "status_changed_at")?;
    assert_eq!(second_changed, first_changed);
    assert_eq!(
        second
            .pointer("/target/tmux_instance")
            .and_then(serde_json::Value::as_str),
        Some("new-target")
    );
    Ok(())
}

#[test]
fn explicit_set_agent_status_ignores_stale_tmux_environment() -> Result<()> {
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
        .args(set_opencode_status_args(
            Some("working"),
            "ses_parent",
            "server",
            "http://127.0.0.1:4096",
            &[],
        ))
        .assert()
        .success();

    Command::cargo_bin("kmux")?
        .current_dir(&cwd)
        .env("XDG_CONFIG_HOME", &config_home)
        .env("XDG_STATE_HOME", config_home.with_file_name("state-home"))
        .env("TMUX", "/tmp/missing-tmux-socket,1,0")
        .env("TMUX_PANE", "%999")
        .args(delete_opencode_agent_observation_args(
            "ses_parent",
            "server",
            "http://127.0.0.1:4096",
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
fn non_pane_agent_observation_resolves_to_matching_tmux_worktree_window() -> Result<()> {
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
        .args(set_opencode_status_args(
            Some("working"),
            "ses_parent",
            "server",
            "http://127.0.0.1:4096",
            &[
                ("--title", "Implement producer"),
                ("--worktree-path", &repo_path),
            ],
        ))
        .assert()
        .success();

    let report = agent_observation_for_key(
        &config_home,
        "opencode",
        "ses_parent",
        "server",
        "http://127.0.0.1:4096",
    )?;
    assert_eq!(
        report
            .pointer("/target/repo_name")
            .and_then(serde_json::Value::as_str),
        Some("project")
    );
    assert_eq!(
        report
            .pointer("/target/repo_path")
            .and_then(serde_json::Value::as_str),
        Some(repo_path.as_str())
    );
    assert_eq!(
        report
            .pointer("/target/branch")
            .and_then(serde_json::Value::as_str),
        Some("main")
    );

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
fn explicit_set_agent_status_infers_repo_metadata_from_directory_fallback() -> Result<()> {
    let (temp, repo) = init_repo()?;
    let config_home = write_config(temp.path(), "")?;
    let repo_path = repo.display().to_string();

    Command::cargo_bin("kmux")?
        .current_dir(&repo)
        .env("XDG_CONFIG_HOME", &config_home)
        .env("XDG_STATE_HOME", config_home.with_file_name("state-home"))
        .env_remove("TMUX")
        .env_remove("TMUX_PANE")
        .args(set_opencode_status_args(
            Some("working"),
            "ses_parent",
            "server",
            "default",
            &[
                ("--worktree-path", "/tmp/does-not-exist/kmux-worktree"),
                ("--directory", &repo_path),
            ],
        ))
        .assert()
        .success();

    let report =
        agent_observation_for_key(&config_home, "opencode", "ses_parent", "server", "default")?;
    assert_eq!(
        report
            .pointer("/target/repo_name")
            .and_then(serde_json::Value::as_str),
        Some("project")
    );
    assert_eq!(
        report
            .pointer("/target/repo_path")
            .and_then(serde_json::Value::as_str),
        Some(repo_path.as_str())
    );
    assert_eq!(
        report
            .pointer("/target/branch")
            .and_then(serde_json::Value::as_str),
        Some("main")
    );
    assert_eq!(
        report
            .pointer("/target/worktree_path")
            .and_then(serde_json::Value::as_str),
        Some("/tmp/does-not-exist/kmux-worktree")
    );
    Ok(())
}
