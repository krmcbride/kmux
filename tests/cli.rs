pub mod support;

use std::fs;

use anyhow::Result;
use assert_cmd::Command;
use predicates::prelude::*;

use support::{git, init_repo, kmux_stdout, run};

#[test]
fn help_shows_current_commands() {
    Command::cargo_bin("kmux")
        .expect("kmux binary should be available")
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("add"))
        .stdout(predicate::str::contains("parent"))
        .stdout(predicate::str::contains("restore"))
        .stdout(predicate::str::contains("list"))
        .stdout(predicate::str::contains("remove"))
        .stdout(predicate::str::contains("status"))
        .stdout(predicate::str::contains("sidebar"))
        .stdout(predicate::str::contains("set-agent-status"))
        .stdout(predicate::str::contains("completions"))
        .stdout(predicate::str::contains("\n  help ").not());
}

#[test]
fn help_subcommands_are_disabled() {
    for args in [["help"].as_slice(), ["sidebar", "help"].as_slice()] {
        Command::cargo_bin("kmux")
            .expect("kmux binary should be available")
            .args(args)
            .assert()
            .failure()
            .stderr(predicate::str::contains("unrecognized subcommand"));
    }
}

#[test]
fn parent_help_uses_stable_parent_then_child_order() {
    Command::cargo_bin("kmux")
        .expect("kmux binary should be available")
        .args(["parent", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Usage: kmux parent <PARENT> [CHILD]",
        ))
        .stdout(predicate::str::contains(
            "Defaults to the current kmux workspace",
        ));
}

#[test]
fn sidebar_help_exposes_only_public_explicit_commands() {
    Command::cargo_bin("kmux")
        .expect("kmux binary should be available")
        .args(["sidebar", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage: kmux sidebar <COMMAND>"))
        .stdout(predicate::str::contains("  on"))
        .stdout(predicate::str::contains("  off"))
        .stdout(predicate::str::contains("  toggle"))
        .stdout(predicate::str::contains("refresh").not())
        .stdout(predicate::str::contains("  run").not())
        .stdout(predicate::str::contains("  wake").not())
        .stdout(predicate::str::contains("  help").not());
}

#[test]
fn private_sidebar_entrypoints_require_underscore_names() {
    for command in ["_refresh", "_run", "_wake"] {
        Command::cargo_bin("kmux")
            .expect("kmux binary should be available")
            .args(["sidebar", command, "--help"])
            .assert()
            .success()
            .stdout(predicate::str::contains(format!(
                "Usage: kmux sidebar {command}"
            )));
    }

    for command in ["refresh", "run", "wake"] {
        Command::cargo_bin("kmux")
            .expect("kmux binary should be available")
            .args(["sidebar", command])
            .assert()
            .failure()
            .stderr(predicate::str::contains("unrecognized subcommand"));
    }
}

#[test]
fn sidebar_requires_an_explicit_command() {
    Command::cargo_bin("kmux")
        .expect("kmux binary should be available")
        .arg("sidebar")
        .assert()
        .failure()
        .stderr(predicate::str::contains("Usage: kmux sidebar <COMMAND>"));
}

#[test]
fn status_help_documents_global_export_options() {
    Command::cargo_bin("kmux")
        .expect("kmux binary should be available")
        .args(["status", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("global agent workspace activity"))
        .stdout(predicate::str::contains("--json"))
        .stdout(predicate::str::contains("--git"));
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
        .stdout(predicate::str::contains("--reporter-kind"))
        .stdout(predicate::str::contains("--reporter-instance"))
        .stdout(predicate::str::contains("title, context, or target hints"))
        .stdout(predicate::str::contains("working"))
        .stdout(predicate::str::contains("waiting"))
        .stdout(predicate::str::contains("done"))
        .stdout(predicate::str::contains(
            "Delete the observation identified by this session and reporter key",
        ))
        .stdout(predicate::str::contains("Delete all reporter observations"))
        .stdout(predicate::str::contains("--title"))
        .stdout(predicate::str::contains("--context"))
        .stdout(predicate::str::contains("--tmux-instance"))
        .stdout(predicate::str::contains("--git-repo-name"))
        .stdout(predicate::str::contains("--git-repo-path"))
        .stdout(predicate::str::contains("--git-branch"))
        .stdout(predicate::str::contains("--directory"));
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
        .stdout(predicate::str::contains("_complete-git-branches"));
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
    let custom = worktree_base.join("custom-completion");
    let custom_arg = custom.display().to_string();
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-b",
            "feature/custom-completion",
            &custom_arg,
        ],
    )?;
    let detached = worktree_base.join("detached-completion");
    let detached_arg = detached.display().to_string();
    git(
        &repo,
        &["worktree", "add", "--detach", &detached_arg, "HEAD"],
    )?;

    let workspaces = kmux_stdout(&repo, &["_complete-workspaces"])?;
    assert!(workspaces.lines().any(|line| line == "feature-active"));
    assert!(!workspaces.lines().any(|line| line == "project"));
    assert!(!workspaces.lines().any(|line| line == "custom-completion"));
    assert!(!workspaces.lines().any(|line| line == "detached-completion"));

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
fn status_rejects_positional_arguments() {
    Command::cargo_bin("kmux")
        .expect("kmux binary should be available")
        .args(["status", "feature/example"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unexpected argument"));
}

#[test]
fn set_agent_status_rejects_unknown_options() {
    Command::cargo_bin("kmux")
        .expect("kmux binary should be available")
        .args([
            "set-agent-status",
            "--agent-kind",
            "example-agent",
            "--session-id",
            "ses_test",
            "--reporter-kind",
            "integration",
            "--reporter-instance",
            "reporter-one",
            "--unknown-option",
            "value",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unexpected argument"));
}
