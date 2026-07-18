pub mod support;

use std::fs;

use anyhow::Result;
use predicates::prelude::*;

use support::{TmuxFixture, git, git_stdout, init_repo, kmux, kmux_with_pane, run, write_config};

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
        .arg("ls")
        .assert()
        .success()
        .stdout(predicate::str::contains("feature/auth"))
        .stdout(predicate::str::contains("project__worktrees/feature-auth"));

    kmux(&repo, &config_home, &tmux)?
        .args(["rm", "feature-auth"])
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

    Ok(())
}

#[test]
fn parent_short_form_discovers_child_from_current_workspace() -> Result<()> {
    let (temp, repo) = init_repo()?;
    let Some(tmux) = TmuxFixture::new(&repo)? else {
        return Ok(());
    };
    let config_home = write_config(temp.path(), "window_prefix: kmux-\n")?;
    git(&repo, &["branch", "feature/parent"])?;
    let worktree = temp.path().join("project__worktrees/feature-child");

    kmux(&repo, &config_home, &tmux)?
        .args(["add", "feature/child", "--background"])
        .assert()
        .success();

    kmux_with_pane(&worktree, &config_home, &tmux, &tmux.pane_id)?
        .args(["parent", "feature/parent"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "set parent of feature/child to feature/parent",
        ));

    Ok(())
}

#[test]
fn add_is_create_only_when_branch_already_exists() -> Result<()> {
    let (temp, repo) = init_repo()?;
    let Some(tmux) = TmuxFixture::new(&repo)? else {
        return Ok(());
    };
    let config_home = write_config(temp.path(), "window_prefix: kmux-\n")?;
    let worktree = temp.path().join("project__worktrees/feature-existing");
    git(&repo, &["branch", "feature/existing"])?;

    kmux(&repo, &config_home, &tmux)?
        .args(["add", "feature/existing"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "branch 'feature/existing' already exists",
        ));
    assert!(!worktree.exists());
    assert!(!tmux.window_exists("kmux-feature-existing")?);

    Ok(())
}

#[test]
fn add_rejects_worktree_only_partial_workspace() -> Result<()> {
    let (temp, repo) = init_repo()?;
    let Some(tmux) = TmuxFixture::new(&repo)? else {
        return Ok(());
    };
    let config_home = write_config(temp.path(), "window_prefix: kmux-\n")?;
    let worktree = temp.path().join("project__worktrees/feature-partial");

    kmux(&repo, &config_home, &tmux)?
        .args(["add", "feature/partial", "--background"])
        .assert()
        .success();
    tmux.tmux_output(&["kill-window", "-t", "kmux-feature-partial"])?;

    kmux(&repo, &config_home, &tmux)?
        .args(["add", "feature/partial", "--background"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "workspace for 'feature/partial' already exists",
        ));

    assert!(worktree.is_dir());
    assert!(!tmux.window_exists("kmux-feature-partial")?);
    assert!(git_stdout(&repo, &["show-ref", "--heads", "feature/partial"]).is_ok());
    Ok(())
}

#[test]
fn add_rejects_window_only_partial_workspace() -> Result<()> {
    let (temp, repo) = init_repo()?;
    let Some(tmux) = TmuxFixture::new(&repo)? else {
        return Ok(());
    };
    let config_home = write_config(temp.path(), "window_prefix: kmux-\n")?;
    let worktree = temp.path().join("project__worktrees/feature-window-only");
    tmux.tmux_output(&[
        "new-window",
        "-d",
        "-t",
        "project:",
        "-n",
        "kmux-feature-window-only",
    ])?;

    kmux(&repo, &config_home, &tmux)?
        .args(["add", "feature/window-only"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "tmux window 'kmux-feature-window-only' already exists",
        ));
    assert!(!worktree.exists());

    Ok(())
}

#[test]
fn restore_recreates_workspace_window_idempotently() -> Result<()> {
    let (temp, repo) = init_repo()?;
    let Some(tmux) = TmuxFixture::new(&repo)? else {
        return Ok(());
    };
    let config_home = write_config(temp.path(), "window_prefix: kmux-\n")?;

    kmux(&repo, &config_home, &tmux)?
        .args(["add", "feature/restore", "--background"])
        .assert()
        .success();
    tmux.tmux_output(&["kill-window", "-t", "kmux-feature-restore"])?;

    kmux(&repo, &config_home, &tmux)?
        .arg("restore")
        .assert()
        .success()
        .stdout(predicate::str::contains("restored feature-restore"));
    assert!(tmux.window_exists("kmux-feature-restore")?);

    let window_count = tmux.unique_window_count()?;
    kmux(&repo, &config_home, &tmux)?
        .arg("restore")
        .assert()
        .success();
    assert_eq!(tmux.unique_window_count()?, window_count);

    Ok(())
}

#[test]
fn restore_rejects_duplicate_expected_window_names() -> Result<()> {
    let (temp, repo) = init_repo()?;
    let Some(tmux) = TmuxFixture::new(&repo)? else {
        return Ok(());
    };
    let config_home = write_config(temp.path(), "window_prefix: kmux-\n")?;
    let worktree = temp.path().join("project__worktrees/feature-duplicate");
    let worktree_path = worktree.display().to_string();

    kmux(&repo, &config_home, &tmux)?
        .args(["add", "feature/duplicate", "--background"])
        .assert()
        .success();
    tmux.tmux_output(&[
        "new-window",
        "-d",
        "-t",
        "project:",
        "-n",
        "kmux-feature-duplicate",
        "-c",
        &worktree_path,
    ])?;

    kmux(&repo, &config_home, &tmux)?
        .arg("restore")
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "multiple tmux windows are named 'kmux-feature-duplicate'",
        ));
    Ok(())
}

#[test]
fn remove_without_name_targets_current_kmux_workspace() -> Result<()> {
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
fn remove_warns_when_other_links_still_reference_removed_branch() -> Result<()> {
    let (temp, repo) = init_repo()?;
    let Some(tmux) = TmuxFixture::new(&repo)? else {
        return Ok(());
    };
    let config_home = write_config(temp.path(), "window_prefix: kmux-\n")?;

    kmux(&repo, &config_home, &tmux)?
        .args(["add", "feature/parent", "--background"])
        .assert()
        .success();
    kmux(&repo, &config_home, &tmux)?
        .args(["add", "feature/child", "--background"])
        .assert()
        .success();
    kmux(&repo, &config_home, &tmux)?
        .args(["parent", "feature/parent", "feature-child"])
        .assert()
        .success();

    kmux(&repo, &config_home, &tmux)?
        .args(["remove", "feature-parent"])
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "parent links still reference removed branch 'feature/parent': feature/child",
        ));

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

    Ok(())
}
