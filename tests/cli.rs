use assert_cmd::Command;
use predicates::prelude::*;

#[test]
fn help_shows_core_commands() {
    Command::cargo_bin("kmux")
        .unwrap()
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
        .unwrap()
        .args(["completions", "bash"])
        .assert()
        .success()
        .stdout(predicate::str::contains("_kmux"));
}

#[test]
fn unimplemented_commands_fail_clearly() {
    Command::cargo_bin("kmux")
        .unwrap()
        .args(["status"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("status is not implemented yet"));
}
