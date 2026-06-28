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
