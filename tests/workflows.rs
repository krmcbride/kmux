mod support;

use std::fs;

use anyhow::Result;
use predicates::prelude::*;

use support::{
    TmuxFixture, delete_opencode_agent_observation_args, git, git_stdout, init_repo, kmux,
    kmux_with_pane, run, set_opencode_status_args, write_config,
};

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
    let worktree_window_id = tmux.pane_format(&worktree_pane, "#{window_id}")?;
    let worktree_path = worktree.display().to_string();
    let producer_instance = format!("default/{worktree_pane}");
    kmux_with_pane(&repo, &config_home, &tmux, &worktree_pane)?
        .args(set_opencode_status_args(
            Some("working"),
            "ses_feature_auth",
            "tui",
            &producer_instance,
            &[
                ("--tmux-instance", &tmux.socket_name),
                ("--tmux-pane-id", &worktree_pane),
                ("--tmux-window-id", &worktree_window_id),
                ("--directory", &worktree_path),
                ("--git-worktree-path", &worktree_path),
                ("--git-branch", "feature/auth"),
            ],
        ))
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
        .args(delete_opencode_agent_observation_args(
            "ses_feature_auth",
            "tui",
            &producer_instance,
        ))
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
