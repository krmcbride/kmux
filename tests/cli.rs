pub mod support;

use std::fs;
use std::process::Command as ProcessCommand;

use anyhow::Result;
use assert_cmd::Command;
use predicates::prelude::*;

use support::{git, init_repo, kmux_stdout, run, write_config};

#[test]
fn help_shows_current_commands() {
    Command::cargo_bin("kmux")
        .expect("kmux binary should be available")
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("workspace"))
        .stdout(predicate::str::contains("config"))
        .stdout(predicate::str::contains("status"))
        .stdout(predicate::str::contains("sidebar"))
        .stdout(predicate::str::contains("set-agent-status"))
        .stdout(predicate::str::contains("completions"))
        .stdout(predicate::str::contains("_complete-").not())
        .stdout(predicate::str::contains("_launch").not())
        .stdout(predicate::str::contains("\n  help ").not());
}

#[test]
fn workspace_help_shows_lifecycle_commands() {
    Command::cargo_bin("kmux")
        .expect("kmux binary should be available")
        .args(["workspace", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage: kmux workspace <COMMAND>"))
        .stdout(predicate::str::contains("  create"))
        .stdout(predicate::str::contains("  list"))
        .stdout(predicate::str::contains("  remove"))
        .stdout(predicate::str::contains("  set-parent"))
        .stdout(predicate::str::contains("  restore"));
}

#[test]
fn long_help_is_capped_at_80_columns() {
    for args in [
        ["workspace", "create", "--help"].as_slice(),
        ["set-agent-status", "--help"].as_slice(),
    ] {
        let output = Command::cargo_bin("kmux")
            .expect("kmux binary should be available")
            .env("COLUMNS", "200")
            .args(args)
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        let help = String::from_utf8_lossy(&output);

        for line in help.lines() {
            assert!(
                line.chars().count() <= 80,
                "help line exceeded 80 columns: {line:?}"
            );
        }
    }
}

#[test]
fn removed_root_lifecycle_commands_and_aliases_fail() {
    for command in ["add", "list", "ls", "remove", "rm", "parent", "restore"] {
        Command::cargo_bin("kmux")
            .expect("kmux binary should be available")
            .arg(command)
            .assert()
            .failure()
            .stderr(predicate::str::contains("unrecognized subcommand"));
    }
}

#[test]
fn workspace_rejects_old_lifecycle_names_and_aliases() {
    for command in ["add", "ls", "rm", "parent"] {
        Command::cargo_bin("kmux")
            .expect("kmux binary should be available")
            .args(["workspace", command])
            .assert()
            .failure()
            .stderr(predicate::str::contains("unrecognized subcommand"));
    }
}

#[test]
fn config_help_exposes_json_option() {
    Command::cargo_bin("kmux")
        .expect("kmux binary should be available")
        .args(["config", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage: kmux config [OPTIONS]"))
        .stdout(predicate::str::contains("--json"));
}

#[test]
fn config_prints_the_same_resolved_shape_as_yaml_and_json() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let config_home = write_config(
        temp.path(),
        r#"
window_prefix: work-
window:
  default_launcher: review-agent
launchers:
  review-agent:
    description: Review project changes and report findings
    command: example-review-agent
    args: ["--mode", "review"]
  code-agent:
    command: example-code-agent
post_create:
  - example-setup
files:
  copy: [.envrc]
  symlink: [local-context]
status_icons:
  working: work
  working_frames: [a, b]
  waiting: ask
  done: done
  sleeping: sleep
sidebar:
  idle_after_seconds: 900
  width: {min: 30, percent: 25, max: 50}
"#,
    )?;

    let yaml_output = Command::cargo_bin("kmux")?
        .current_dir(temp.path())
        .env("XDG_CONFIG_HOME", &config_home)
        .env_remove("TMUX")
        .env_remove("TMUX_PANE")
        .env("PATH", "")
        .arg("config")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let json_output = Command::cargo_bin("kmux")?
        .current_dir(temp.path())
        .env("XDG_CONFIG_HOME", &config_home)
        .env_remove("TMUX")
        .env_remove("TMUX_PANE")
        .env("PATH", "")
        .args(["config", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let yaml: serde_json::Value = yaml_serde::from_slice(&yaml_output)?;
    let json: serde_json::Value = serde_json::from_slice(&json_output)?;
    let expected = serde_json::json!({
        "window_prefix": "work-",
        "window": {"default_launcher": "review-agent"},
        "launchers": {
            "code-agent": {
                "description": null,
                "command": "example-code-agent",
                "args": []
            },
            "review-agent": {
                "description": "Review project changes and report findings",
                "command": "example-review-agent",
                "args": ["--mode", "review"]
            }
        },
        "post_create": ["example-setup"],
        "files": {
            "copy": [".envrc"],
            "symlink": ["local-context"]
        },
        "status_icons": {
            "working": "work",
            "working_frames": ["a", "b"],
            "waiting": "ask",
            "done": "done",
            "sleeping": "sleep"
        },
        "sidebar": {
            "width": {"min": 30, "percent": 25, "max": 50},
            "idle_after_seconds": 900
        }
    });

    assert_eq!(yaml, expected);
    assert_eq!(json, expected);
    Ok(())
}

#[test]
fn config_reports_invalid_launcher_descriptions() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let config_home = write_config(
        temp.path(),
        "launchers: {example: {description: '  ', command: example-agent}}\n",
    )?;

    Command::cargo_bin("kmux")?
        .current_dir(temp.path())
        .env("XDG_CONFIG_HOME", config_home)
        .arg("config")
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "launchers.example.description must not be blank",
        ));
    Ok(())
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
fn set_parent_help_uses_stable_parent_then_child_order() {
    Command::cargo_bin("kmux")
        .expect("kmux binary should be available")
        .args(["workspace", "set-parent", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Usage: kmux workspace set-parent <PARENT> [CHILD]",
        ));
}

#[test]
fn create_help_exposes_launcher_options_without_hidden_ingress() {
    Command::cargo_bin("kmux")
        .expect("kmux binary should be available")
        .args(["workspace", "create", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--launcher <LAUNCHER>"))
        .stdout(predicate::str::contains("--launcher-input <INPUT>"))
        .stdout(predicate::str::contains("--tmux-session").not());
}

#[test]
fn lifecycle_help_omits_project_session_override() {
    for args in [
        ["workspace", "create", "--help"].as_slice(),
        ["workspace", "restore", "--help"].as_slice(),
        ["workspace", "remove", "--help"].as_slice(),
    ] {
        Command::cargo_bin("kmux")
            .expect("kmux binary should be available")
            .args(args)
            .assert()
            .success()
            .stdout(predicate::str::contains("--tmux-session").not());
    }
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
fn workspace_requires_an_explicit_command() {
    Command::cargo_bin("kmux")
        .expect("kmux binary should be available")
        .arg("workspace")
        .assert()
        .failure()
        .stderr(predicate::str::contains("Usage: kmux workspace <COMMAND>"));
}

#[test]
fn status_help_exposes_global_export_options() {
    Command::cargo_bin("kmux")
        .expect("kmux binary should be available")
        .args(["status", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--json"))
        .stdout(predicate::str::contains("--git"));
}

#[test]
fn set_agent_status_help_exposes_integration_options() {
    Command::cargo_bin("kmux")
        .expect("kmux binary should be available")
        .args(["set-agent-status", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--agent-kind"))
        .stdout(predicate::str::contains("--session-id"))
        .stdout(predicate::str::contains("--reporter-kind"))
        .stdout(predicate::str::contains("--reporter-instance"))
        .stdout(predicate::str::contains("working"))
        .stdout(predicate::str::contains("waiting"))
        .stdout(predicate::str::contains("done"))
        .stdout(predicate::str::contains("--delete"))
        .stdout(predicate::str::contains("--delete-session"))
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
        .stdout(predicate::str::contains("_complete-create-branches"))
        .stdout(predicate::str::contains("--parent"))
        .stdout(predicate::str::contains("_complete-git-branches"))
        .stdout(predicate::str::contains("_complete-launchers"))
        .stdout(predicate::str::contains("--launcher"))
        .stdout(predicate::str::contains("--launcher-input"))
        .stdout(predicate::str::contains("--tmux-session").not());
}

#[test]
fn fish_completions_handle_nested_dynamic_values_and_command_like_names() -> Result<()> {
    if ProcessCommand::new("fish")
        .arg("--version")
        .output()
        .is_err()
    {
        return Ok(());
    }

    let temp = tempfile::tempdir()?;
    let completion_path = temp.path().join("kmux.fish");
    let output = Command::cargo_bin("kmux")?
        .args(["completions", "fish"])
        .output()?;
    assert!(output.status.success());
    fs::write(&completion_path, output.stdout)?;

    let complete = |commandline: &str| -> Result<String> {
        let script = r#"
source $argv[1]
function kmux
    switch $argv[1]
        case _complete-git-branches
            echo main
        case _complete-launchers
            echo agent
        case _complete-workspaces
            echo feature-alpha
        case _complete-create-branches
            echo feature-new
    end
end
complete -C $argv[2]
"#;
        let output = ProcessCommand::new("fish")
            .args([
                "--no-config",
                "-c",
                script,
                &completion_path.display().to_string(),
                commandline,
            ])
            .output()?;
        assert!(
            output.status.success(),
            "fish completion failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(String::from_utf8(output.stdout)?)
    };

    assert_eq!(complete("kmux workspace create --parent ")?, "main\n");
    assert_eq!(complete("kmux workspace create --launcher ")?, "agent\n");
    assert_eq!(complete("kmux workspace create --launcher-input ")?, "");
    assert_eq!(
        complete("kmux workspace set-parent main fea")?,
        "feature-alpha\n"
    );

    let command_like_name = complete("kmux workspace remove create --")?;
    for create_option in ["--background", "--launcher-input", "--launcher", "--parent"] {
        assert!(!command_like_name.lines().any(|line| line == create_option));
    }

    Ok(())
}

#[test]
fn launcher_completion_is_sorted_and_fails_closed_for_invalid_config() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let config_home = write_config(
        temp.path(),
        r#"
launchers:
  zebra: {command: example-zebra}
  alpha: {command: example-alpha}
"#,
    )?;

    let output = Command::cargo_bin("kmux")?
        .current_dir(temp.path())
        .env("XDG_CONFIG_HOME", &config_home)
        .arg("_complete-launchers")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(String::from_utf8(output)?, "alpha\nzebra\n");

    fs::write(
        config_home.join("kmux/config.yaml"),
        "launchers: [invalid]\n",
    )?;
    Command::cargo_bin("kmux")?
        .current_dir(temp.path())
        .env("XDG_CONFIG_HOME", &config_home)
        .arg("_complete-launchers")
        .assert()
        .success()
        .stdout(predicate::str::is_empty());

    Ok(())
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

    let create_branches = kmux_stdout(&repo, &["_complete-create-branches"])?;
    assert!(
        create_branches
            .lines()
            .any(|line| line == "feature/addable")
    );
    assert!(create_branches.lines().any(|line| line == "feature/base"));
    assert!(
        create_branches
            .lines()
            .any(|line| line == "origin/feature/remote")
    );
    assert!(!create_branches.lines().any(|line| line == "main"));
    assert!(!create_branches.lines().any(|line| line == "origin/main"));
    assert!(!create_branches.lines().any(|line| line == "feature/active"));

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
