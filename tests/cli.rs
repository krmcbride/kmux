mod support;

use std::fs;

use anyhow::Result;
use assert_cmd::Command;
use predicates::prelude::*;

use support::{git, init_repo, kmux_stdout, run};

#[test]
fn help_shows_core_commands() {
    Command::cargo_bin("kmux")
        .expect("kmux binary should be available")
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("add"))
        .stdout(predicate::str::contains("parent"))
        .stdout(predicate::str::contains("restore"))
        .stdout(predicate::str::contains("remove"))
        .stdout(predicate::str::contains("status"))
        .stdout(predicate::str::contains("set-agent-status"))
        .stdout(predicate::str::contains("completions"))
        .stdout(predicate::str::contains("open").not())
        .stdout(predicate::str::contains("close").not())
        .stdout(predicate::str::contains("path").not())
        .stdout(predicate::str::contains("rename").not());
}

#[test]
fn add_help_has_no_workspace_slug_override() {
    Command::cargo_bin("kmux")
        .expect("kmux binary should be available")
        .args(["add", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--parent"))
        .stdout(predicate::str::contains("--base").not())
        .stdout(predicate::str::contains("--open-if-exists").not())
        .stdout(predicate::str::contains("-o").not())
        .stdout(predicate::str::contains("--name").not());
}

#[test]
fn parent_help_documents_relationship_command() {
    Command::cargo_bin("kmux")
        .expect("kmux binary should be available")
        .args(["parent", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Set the recorded parent branch"))
        .stdout(predicate::str::contains("<CHILD_OR_PARENT>"))
        .stdout(predicate::str::contains("[PARENT]"));
}

#[test]
fn remove_help_has_no_partial_branch_retention_flag() {
    Command::cargo_bin("kmux")
        .expect("kmux binary should be available")
        .args(["remove", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--force"))
        .stdout(predicate::str::contains("--keep-branch").not());
}

#[test]
fn set_agent_status_help_documents_integration_contract() {
    Command::cargo_bin("kmux")
        .expect("kmux binary should be available")
        .args(["set-agent-status", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("supported integration surface"))
        .stdout(predicate::str::contains("--agent-kind"))
        .stdout(predicate::str::contains("--session-id"))
        .stdout(predicate::str::contains("--producer-kind"))
        .stdout(predicate::str::contains("--producer-instance"))
        .stdout(predicate::str::contains("metadata or target hints"))
        .stdout(predicate::str::contains("working"))
        .stdout(predicate::str::contains("waiting"))
        .stdout(predicate::str::contains("done"))
        .stdout(predicate::str::contains(
            "Delete the observation identified by this session and producer key",
        ))
        .stdout(predicate::str::contains("Delete all producer observations"))
        .stdout(predicate::str::contains("--tmux-pane-id"))
        .stdout(predicate::str::contains("--tmux-window-id"))
        .stdout(predicate::str::contains("--git-repo-name"))
        .stdout(predicate::str::contains("--git-worktree-path"))
        .stdout(predicate::str::contains("--pane-id").not())
        .stdout(predicate::str::contains("--window-id").not())
        .stdout(predicate::str::contains("--repo-name").not())
        .stdout(predicate::str::contains("--worktree-handle").not())
        .stdout(predicate::str::contains("--worktree-path").not())
        .stdout(predicate::str::contains("--branch").not())
        .stdout(predicate::str::contains("--session-name").not())
        .stdout(predicate::str::contains("--window-name").not());
}

#[test]
fn completions_command_emits_shell_completion() {
    Command::cargo_bin("kmux")
        .expect("kmux binary should be available")
        .args(["completions", "bash"])
        .assert()
        .success()
        .stdout(predicate::str::contains("_kmux"))
        .stdout(predicate::str::contains("_kmux_workspaces"))
        .stdout(predicate::str::contains("_complete-workspaces"))
        .stdout(predicate::str::contains("_complete-add-branches"))
        .stdout(predicate::str::contains("--parent"))
        .stdout(predicate::str::contains("_complete-git-branches"))
        .stdout(predicate::str::contains("--base").not())
        .stdout(predicate::str::contains("--open-if-exists").not())
        .stdout(predicate::str::contains("open").not());
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

    let workspaces = kmux_stdout(&repo, &["_complete-workspaces"])?;
    assert!(workspaces.lines().any(|line| line == "feature-active"));
    assert!(!workspaces.lines().any(|line| line == "project"));

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
    assert!(git_branches.lines().any(|line| line == "feature/active"));
    assert!(git_branches.lines().any(|line| line == "feature/addable"));
    assert!(!git_branches.lines().any(|line| line == "origin/main"));
    assert!(
        !git_branches
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

#[test]
fn removed_open_command_fails_clearly() {
    Command::cargo_bin("kmux")
        .expect("kmux binary should be available")
        .arg("open")
        .assert()
        .failure()
        .stderr(predicate::str::contains("unrecognized subcommand"));
}

#[test]
fn removed_add_open_if_exists_flag_fails_clearly() {
    Command::cargo_bin("kmux")
        .expect("kmux binary should be available")
        .args(["add", "feature/example", "-o"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unexpected argument"));
}

#[test]
fn removed_add_base_flag_fails_clearly() {
    Command::cargo_bin("kmux")
        .expect("kmux binary should be available")
        .args(["add", "feature/example", "--base", "main"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unexpected argument"));
}
