pub mod support;

use std::fs;
use std::path::Path;
use std::process::Stdio;
use std::sync::{Arc, Barrier};
use std::thread;

use anyhow::Result;
use predicates::prelude::*;

use support::{
    TmuxFixture, git, git_stdout, init_repo, kmux, kmux_detached, kmux_process_detached,
    kmux_process_with_pane, kmux_with_pane, run, wait_for_nonempty_file, wait_for_path,
    write_config,
};

fn run_concurrently(
    mut first: std::process::Command,
    mut second: std::process::Command,
) -> Result<(std::process::Output, std::process::Output)> {
    let barrier = Arc::new(Barrier::new(3));
    let first_barrier = Arc::clone(&barrier);
    let first_thread = thread::spawn(move || {
        first_barrier.wait();
        first.output()
    });
    let second_barrier = Arc::clone(&barrier);
    let second_thread = thread::spawn(move || {
        second_barrier.wait();
        second.output()
    });
    barrier.wait();

    let first = first_thread
        .join()
        .map_err(|_| anyhow::anyhow!("first concurrent kmux process panicked"))??;
    let second = second_thread
        .join()
        .map_err(|_| anyhow::anyhow!("second concurrent kmux process panicked"))??;
    Ok((first, second))
}

fn nul_delimited_arguments(path: &Path) -> Result<Vec<String>> {
    let bytes = fs::read(path)?;
    let mut arguments = bytes
        .split(|byte| *byte == 0)
        .map(|argument| String::from_utf8(argument.to_vec()))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    if arguments.last().is_some_and(String::is_empty) {
        arguments.pop();
    }
    Ok(arguments)
}

#[cfg(unix)]
fn process_exists(pid: i32) -> bool {
    if unsafe { libc::kill(pid, 0) } == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
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
fn detached_lifecycle_resolves_and_reuses_unique_project_session() -> Result<()> {
    let (temp, repo) = init_repo()?;
    let Some(tmux) = TmuxFixture::new(&repo)? else {
        return Ok(());
    };
    let config_home = write_config(temp.path(), "window_prefix: kmux-\n")?;
    let worktree = temp.path().join("project__worktrees/feature-detached");

    kmux_detached(&repo, &config_home, &tmux)?
        .env("TMUX", "stale-client-state")
        .args(["add", "feature/detached", "--background"])
        .assert()
        .success()
        .stdout(predicate::str::contains("created feature-detached"));
    assert!(worktree.is_dir());
    assert!(tmux.window_exists("kmux-feature-detached")?);

    tmux.tmux_output(&["kill-window", "-t", "kmux-feature-detached"])?;
    kmux_detached(&worktree, &config_home, &tmux)?
        .arg("restore")
        .assert()
        .success()
        .stdout(predicate::str::contains("restored feature-detached"));
    assert!(tmux.window_exists("kmux-feature-detached")?);

    kmux_detached(&repo, &config_home, &tmux)?
        .args(["remove", "feature-detached", "--force"])
        .assert()
        .success()
        .stdout(predicate::str::contains("removed feature-detached"));
    assert!(!worktree.exists());
    assert!(!tmux.window_exists("kmux-feature-detached")?);
    Ok(())
}

#[test]
fn detached_add_rejects_split_project_and_never_focuses_another_client() -> Result<()> {
    let (temp, repo) = init_repo()?;
    let Some(tmux) = TmuxFixture::new(&repo)? else {
        return Ok(());
    };
    let config_home = write_config(temp.path(), "window_prefix: kmux-\n")?;
    let repo_path = repo.display().to_string();
    tmux.tmux_output(&["new-session", "-d", "-s", "project-copy", "-c", &repo_path])?;

    kmux_detached(&repo, &config_home, &tmux)?
        .args(["add", "feature/ambiguous", "--background"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "live panes in multiple tmux sessions",
        ))
        .stderr(predicate::str::contains("\"project\", \"project-copy\""))
        .stderr(predicate::str::contains("--tmux-session").not());
    assert!(git_stdout(&repo, &["show-ref", "--heads", "feature/ambiguous"]).is_err());

    tmux.tmux_output(&["kill-session", "-t", "project-copy"])?;

    kmux_detached(&repo, &config_home, &tmux)?
        .env("TMUX", "stale-client-state")
        .env("TMUX_PANE", "%999999")
        .args(["add", "feature/focus-refused"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("caller is not attached"))
        .stderr(predicate::str::contains("--background"));
    assert!(git_stdout(&repo, &["show-ref", "--heads", "feature/focus-refused"]).is_err());

    kmux_detached(&repo, &config_home, &tmux)?
        .env("TMUX_PANE", "project:")
        .args(["add", "feature/malformed-context"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("caller is not attached"));
    assert!(git_stdout(&repo, &["show-ref", "--heads", "feature/malformed-context"]).is_err());

    let neutral = tempfile::tempdir()?;
    let neutral_path = neutral.path().display().to_string();
    let neutral_pane = tmux.tmux_output(&[
        "new-session",
        "-d",
        "-s",
        "neutral",
        "-c",
        &neutral_path,
        "-P",
        "-F",
        "#{pane_id}",
    ])?;
    kmux_with_pane(&repo, &config_home, &tmux, &neutral_pane)?
        .args(["add", "feature/wrong-session"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("caller is not attached"));
    assert!(git_stdout(&repo, &["show-ref", "--heads", "feature/wrong-session"]).is_err());
    Ok(())
}

#[test]
fn detached_add_rejects_mixed_project_session_before_mutation() -> Result<()> {
    let (temp, repo) = init_repo()?;
    let (_other_temp, other_repo) = init_repo()?;
    let Some(tmux) = TmuxFixture::new(&repo)? else {
        return Ok(());
    };
    let config_home = write_config(temp.path(), "window_prefix: kmux-\n")?;
    let other_path = other_repo.display().to_string();
    tmux.tmux_output(&[
        "new-window",
        "-d",
        "-t",
        "project:",
        "-n",
        "other-project",
        "-c",
        &other_path,
    ])?;

    kmux_detached(&repo, &config_home, &tmux)?
        .args(["add", "feature/mixed", "--background"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "contains panes from multiple Git projects",
        ));
    assert!(git_stdout(&repo, &["show-ref", "--heads", "feature/mixed"]).is_err());
    Ok(())
}

#[test]
fn detached_add_rejects_project_window_linked_across_sessions() -> Result<()> {
    let (temp, repo) = init_repo()?;
    let neutral = tempfile::tempdir()?;
    let Some(tmux) = TmuxFixture::new(&repo)? else {
        return Ok(());
    };
    let config_home = write_config(temp.path(), "window_prefix: kmux-\n")?;
    let neutral_path = neutral.path().display().to_string();
    tmux.tmux_output(&[
        "new-session",
        "-d",
        "-s",
        "project-copy",
        "-c",
        &neutral_path,
    ])?;
    let project_window = tmux.current_window_id()?;
    tmux.tmux_output(&["link-window", "-s", &project_window, "-t", "project-copy:"])?;

    kmux_detached(&repo, &config_home, &tmux)?
        .args(["add", "feature/linked", "--background"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "live panes in multiple tmux sessions",
        ));
    assert!(git_stdout(&repo, &["show-ref", "--heads", "feature/linked"]).is_err());
    Ok(())
}

#[test]
fn detached_add_ignores_sidebar_only_and_other_project_sessions() -> Result<()> {
    let (temp, repo) = init_repo()?;
    let Some(tmux) = TmuxFixture::new(&repo)? else {
        return Ok(());
    };
    let config_home = write_config(temp.path(), "window_prefix: kmux-\n")?;
    tmux.tmux_output(&[
        "set-option",
        "-p",
        "-t",
        &tmux.pane_id,
        "@kmux_role",
        "sidebar",
    ])?;

    kmux_detached(&repo, &config_home, &tmux)?
        .args(["add", "feature/sidebar-only", "--background"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "requires an existing tmux session containing a live pane for project",
        ));
    assert!(git_stdout(&repo, &["show-ref", "--heads", "feature/sidebar-only"]).is_err());
    kmux_detached(&repo, &config_home, &tmux)?
        .arg("restore")
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "requires an existing tmux session containing a live pane for project",
        ));

    let (other_temp, other_repo) = init_repo()?;
    let other_config = write_config(other_temp.path(), "window_prefix: kmux-\n")?;
    kmux_detached(&other_repo, &other_config, &tmux)?
        .args(["add", "feature/other-project", "--background"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "requires an existing tmux session containing a live pane for project",
        ));
    assert!(
        git_stdout(
            &other_repo,
            &["show-ref", "--heads", "feature/other-project"]
        )
        .is_err()
    );
    Ok(())
}

#[test]
fn restore_rejects_no_evidence_mixed_and_split_topology_before_mutation() -> Result<()> {
    let (temp, repo) = init_repo()?;
    let (_other_temp, other_repo) = init_repo()?;
    let Some(tmux) = TmuxFixture::new(&repo)? else {
        return Ok(());
    };
    let config_home = write_config(temp.path(), "window_prefix: kmux-\n")?;
    let worktree = temp.path().join("project__worktrees/feature-restore-guard");
    let window_name = "kmux-feature-restore-guard";

    kmux(&repo, &config_home, &tmux)?
        .args(["add", "feature/restore-guard", "--background"])
        .assert()
        .success();
    tmux.tmux_output(&["kill-window", "-t", window_name])?;

    let other_path = other_repo.display().to_string();
    tmux.tmux_output(&[
        "new-window",
        "-d",
        "-t",
        "project:",
        "-n",
        "other-project",
        "-c",
        &other_path,
    ])?;
    kmux_detached(&repo, &config_home, &tmux)?
        .arg("restore")
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "contains panes from multiple Git projects",
        ));
    assert!(!tmux.window_exists(window_name)?);
    tmux.tmux_output(&["kill-window", "-t", "other-project"])?;

    let repo_path = repo.display().to_string();
    tmux.tmux_output(&["new-session", "-d", "-s", "project-copy", "-c", &repo_path])?;
    kmux_detached(&repo, &config_home, &tmux)?
        .arg("restore")
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "live panes in multiple tmux sessions",
        ));
    assert!(!tmux.window_exists(window_name)?);
    tmux.tmux_output(&["kill-session", "-t", "project-copy"])?;

    tmux.tmux_output(&[
        "set-option",
        "-p",
        "-t",
        &tmux.pane_id,
        "@kmux_role",
        "sidebar",
    ])?;
    kmux_detached(&repo, &config_home, &tmux)?
        .arg("restore")
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "containing a live pane for project",
        ));
    assert!(!tmux.window_exists(window_name)?);
    assert!(worktree.is_dir());
    Ok(())
}

#[test]
fn remove_rejects_mixed_and_split_topology_before_git_mutation() -> Result<()> {
    let (temp, repo) = init_repo()?;
    let (_other_temp, other_repo) = init_repo()?;
    let Some(tmux) = TmuxFixture::new(&repo)? else {
        return Ok(());
    };
    let config_home = write_config(temp.path(), "window_prefix: kmux-\n")?;
    let worktree = temp.path().join("project__worktrees/feature-remove-guard");
    let window_name = "kmux-feature-remove-guard";

    kmux(&repo, &config_home, &tmux)?
        .args(["add", "feature/remove-guard", "--background"])
        .assert()
        .success();

    let other_path = other_repo.display().to_string();
    tmux.tmux_output(&[
        "new-window",
        "-d",
        "-t",
        "project:",
        "-n",
        "other-project",
        "-c",
        &other_path,
    ])?;
    kmux_detached(&repo, &config_home, &tmux)?
        .args(["remove", "feature-remove-guard", "--force"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "contains panes from multiple Git projects",
        ));
    assert!(worktree.is_dir());
    assert!(tmux.window_exists(window_name)?);
    assert!(git_stdout(&repo, &["show-ref", "--heads", "feature/remove-guard"]).is_ok());
    tmux.tmux_output(&["kill-window", "-t", "other-project"])?;

    let repo_path = repo.display().to_string();
    tmux.tmux_output(&["new-session", "-d", "-s", "project-copy", "-c", &repo_path])?;
    kmux_detached(&repo, &config_home, &tmux)?
        .args(["remove", "feature-remove-guard", "--force"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "live panes in multiple tmux sessions",
        ));
    assert!(worktree.is_dir());
    assert!(tmux.window_exists(window_name)?);
    assert!(git_stdout(&repo, &["show-ref", "--heads", "feature/remove-guard"]).is_ok());
    Ok(())
}

#[test]
fn detached_remove_blocks_scratch_panes_and_is_safe_without_a_session() -> Result<()> {
    let (temp, repo) = init_repo()?;
    let Some(tmux) = TmuxFixture::new(&repo)? else {
        return Ok(());
    };
    let config_home = write_config(temp.path(), "window_prefix: kmux-\n")?;
    let worktree = temp.path().join("project__worktrees/feature-remove-live");

    kmux(&repo, &config_home, &tmux)?
        .args(["add", "feature/remove-live", "--background"])
        .assert()
        .success();
    let worktree_text = worktree.display().to_string();
    tmux.tmux_output(&[
        "new-window",
        "-d",
        "-t",
        "project:",
        "-n",
        "scratch-remove-live",
        "-c",
        &worktree_text,
    ])?;
    kmux_detached(&repo, &config_home, &tmux)?
        .args(["remove", "feature-remove-live", "--force"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "still has a live tmux pane outside its managed window",
        ));
    assert!(worktree.is_dir());

    tmux.tmux_output(&["kill-server"])?;
    kmux_detached(&worktree, &config_home, &tmux)?
        .env("TMUX", "stale-client-state")
        .env("TMUX_PANE", "%999999")
        .args(["remove", "--force"])
        .assert()
        .success()
        .stdout(predicate::str::contains("removed feature-remove-live"));
    assert!(!worktree.exists());
    Ok(())
}

#[test]
fn concurrent_waiter_resnapshots_topology_after_project_lock() -> Result<()> {
    let (temp, repo) = init_repo()?;
    let Some(tmux) = TmuxFixture::new(&repo)? else {
        return Ok(());
    };
    let hook_ready = temp.path().join("topology-hook-ready");
    let hook_release = temp.path().join("topology-hook-release");
    let second_telemetry = temp.path().join("second-telemetry.jsonl");
    let config_home = write_config(
        temp.path(),
        &format!(
            "window_prefix: kmux-\npost_create:\n  - 'touch \"{}\"; while [ ! -e \"{}\" ]; do sleep 0.01; done'\n",
            hook_ready.display(),
            hook_release.display()
        ),
    )?;

    let mut first = kmux_process_with_pane(&repo, &config_home, &tmux, &tmux.pane_id);
    first
        .args(["add", "feature/concurrent-a", "--background"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let first = first.spawn()?;
    wait_for_path(&hook_ready)?;

    let mut second = kmux_process_with_pane(&repo, &config_home, &tmux, &tmux.pane_id);
    second
        .env("KMUX_TELEMETRY", "1")
        .env("KMUX_TELEMETRY_PATH", &second_telemetry)
        .args(["add", "feature/concurrent-b", "--background"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut second = second.spawn()?;
    wait_for_nonempty_file(&second_telemetry)?;
    assert!(
        second.try_wait()?.is_none(),
        "second add should wait while the first owns the project lifecycle lock"
    );

    let repo_path = repo.display().to_string();
    tmux.tmux_output(&["new-session", "-d", "-s", "project-copy", "-c", &repo_path])?;
    fs::write(&hook_release, "release\n")?;

    let first = first.wait_with_output()?;
    let second = second.wait_with_output()?;

    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    assert!(
        String::from_utf8_lossy(&second.stderr).contains("live panes in multiple tmux sessions"),
        "waiting add should re-snapshot strict topology: {}",
        String::from_utf8_lossy(&second.stderr)
    );
    assert!(!second.status.success());
    assert!(
        temp.path()
            .join("project__worktrees/feature-concurrent-a")
            .is_dir()
    );
    assert!(
        !temp
            .path()
            .join("project__worktrees/feature-concurrent-b")
            .exists()
    );
    assert!(tmux.window_exists("kmux-feature-concurrent-a")?);
    assert!(git_stdout(&repo, &["show-ref", "--heads", "feature/concurrent-b"]).is_err());
    Ok(())
}

#[test]
fn concurrent_projects_cannot_claim_a_neutral_session() -> Result<()> {
    let (first_temp, first_repo) = init_repo()?;
    let (second_temp, second_repo) = init_repo()?;
    let neutral = tempfile::tempdir()?;
    let Some(tmux) = TmuxFixture::new(neutral.path())? else {
        return Ok(());
    };
    let first_config = write_config(first_temp.path(), "window_prefix: kmux-\n")?;
    let second_config = write_config(second_temp.path(), "window_prefix: kmux-\n")?;
    let mut first = kmux_process_detached(&first_repo, &first_config, &tmux);
    first.args(["add", "feature/project-a", "--background"]);
    let mut second = kmux_process_detached(&second_repo, &second_config, &tmux);
    second.args(["add", "feature/project-b", "--background"]);
    let (first, second) = run_concurrently(first, second)?;

    assert!(!first.status.success());
    assert!(!second.status.success());
    assert!(String::from_utf8_lossy(&first.stderr).contains("containing a live pane for project"));
    assert!(String::from_utf8_lossy(&second.stderr).contains("containing a live pane for project"));
    assert!(git_stdout(&first_repo, &["show-ref", "--heads", "feature/project-a"]).is_err());
    assert!(git_stdout(&second_repo, &["show-ref", "--heads", "feature/project-b"]).is_err());
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
fn recursive_lifecycle_from_post_create_fails_instead_of_deadlocking() -> Result<()> {
    let (temp, repo) = init_repo()?;
    let Some(tmux) = TmuxFixture::new(&repo)? else {
        return Ok(());
    };
    let config_home = write_config(
        temp.path(),
        "window_prefix: kmux-\npost_create:\n  - '\"$KMUX_RECURSIVE_BIN\" restore'\n",
    )?;
    let worktree = temp.path().join("project__worktrees/feature-recursive");

    kmux(&repo, &config_home, &tmux)?
        .env("KMUX_RECURSIVE_BIN", env!("CARGO_BIN_EXE_kmux"))
        .args(["add", "feature/recursive", "--background"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "lifecycle commands cannot run recursively from post_create",
        ));

    assert!(worktree.is_dir());
    assert!(!tmux.window_exists("kmux-feature-recursive")?);
    Ok(())
}

#[cfg(unix)]
#[test]
fn recursive_lifecycle_from_git_hook_fails_instead_of_deadlocking() -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let (temp, repo) = init_repo()?;
    let Some(tmux) = TmuxFixture::new(&repo)? else {
        return Ok(());
    };
    let config_home = write_config(temp.path(), "window_prefix: kmux-\n")?;
    let hook_output = temp.path().join("hook-output");
    let hook = repo.join(".git/hooks/post-checkout");
    fs::write(
        &hook,
        "#!/bin/sh\n\"$KMUX_RECURSIVE_BIN\" restore > \"$KMUX_HOOK_OUTPUT\" 2>&1\n",
    )?;
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755))?;
    let hooks_path = repo.join(".git/hooks").display().to_string();
    git(&repo, &["config", "core.hooksPath", &hooks_path])?;

    let _assert = kmux(&repo, &config_home, &tmux)?
        .env("KMUX_RECURSIVE_BIN", env!("CARGO_BIN_EXE_kmux"))
        .env("KMUX_HOOK_OUTPUT", &hook_output)
        .args(["add", "feature/hook-recursion", "--background"])
        .assert();

    wait_for_nonempty_file(&hook_output)?;
    assert!(
        fs::read_to_string(&hook_output)?.contains("lifecycle commands cannot run recursively")
    );
    Ok(())
}

#[test]
fn parent_waits_for_add_before_updating_shared_workspace_state() -> Result<()> {
    let (temp, repo) = init_repo()?;
    let Some(tmux) = TmuxFixture::new(&repo)? else {
        return Ok(());
    };
    let config_home = write_config(temp.path(), "window_prefix: kmux-\n")?;
    kmux(&repo, &config_home, &tmux)?
        .args(["add", "feature/child", "--background"])
        .assert()
        .success();

    let hook_ready = temp.path().join("hook-ready");
    write_config(
        temp.path(),
        &format!(
            "window_prefix: kmux-\npost_create:\n  - 'touch \"{}\"; sleep 0.2'\n",
            hook_ready.display()
        ),
    )?;
    let mut add = kmux_process_with_pane(&repo, &config_home, &tmux, &tmux.pane_id);
    add.args(["add", "feature/sibling", "--background"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let add = add.spawn()?;
    wait_for_path(&hook_ready)?;

    let mut parent = kmux_process_with_pane(&repo, &config_home, &tmux, &tmux.pane_id);
    let parent = parent
        .args(["parent", "feature/sibling", "feature/child"])
        .output()?;
    let add = add.wait_with_output()?;
    assert!(
        add.status.success(),
        "concurrent add failed: {}",
        String::from_utf8_lossy(&add.stderr)
    );
    assert!(
        parent.status.success(),
        "concurrent parent failed: {}",
        String::from_utf8_lossy(&parent.stderr)
    );

    let state: serde_json::Value =
        serde_json::from_slice(&fs::read(repo.join(".git/kmux/state.json"))?)?;
    let parents = state
        .get("parents")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("workspace state should contain parent links"))?;
    assert!(parents.iter().any(|link| {
        link.get("branch").and_then(serde_json::Value::as_str) == Some("feature/child")
            && link.get("parent").and_then(serde_json::Value::as_str) == Some("feature/sibling")
    }));
    assert!(parents.iter().any(|link| {
        link.get("branch").and_then(serde_json::Value::as_str) == Some("feature/sibling")
            && link.get("parent").and_then(serde_json::Value::as_str) == Some("main")
    }));
    Ok(())
}

#[test]
fn add_waits_for_delayed_shell_before_starting_launcher() -> Result<()> {
    let (temp, repo) = init_repo()?;
    let Some(tmux) = TmuxFixture::new(&repo)? else {
        return Ok(());
    };
    let config_home = write_config(
        temp.path(),
        r#"
window_prefix: kmux-
window: {default_launcher: editor}
launchers:
  editor: {command: /bin/true}
"#,
    )?;
    tmux.tmux_output(&[
        "set-option",
        "-g",
        "default-command",
        "sleep 2; exec /bin/sh",
    ])?;

    kmux(&repo, &config_home, &tmux)?
        .args(["add", "feature/delayed-shell", "--background"])
        .assert()
        .success()
        .stdout(predicate::str::contains("created feature-delayed-shell"));

    assert!(tmux.window_exists("kmux-feature-delayed-shell")?);
    assert!(
        temp.path()
            .join("project__worktrees/feature-delayed-shell")
            .is_dir()
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn launcher_uses_home_state_when_xdg_state_is_unset() -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let (temp, repo) = init_repo()?;
    let Some(tmux) = TmuxFixture::new(&repo)? else {
        return Ok(());
    };
    let config_home = write_config(
        temp.path(),
        r#"
window_prefix: kmux-
launchers:
  example-launcher: {command: /bin/true}
"#,
    )?;
    let home = temp.path().join("home");
    let runtime = temp.path().join("runtime");
    fs::create_dir(&home)?;
    fs::create_dir(&runtime)?;
    fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700))?;

    kmux(&repo, &config_home, &tmux)?
        .env_remove("XDG_STATE_HOME")
        .env("HOME", &home)
        .env("XDG_RUNTIME_DIR", &runtime)
        .args([
            "add",
            "feature/home-state",
            "--background",
            "--launch",
            "example-launcher",
        ])
        .assert()
        .success();

    let launcher_runtime = home.join(".local/state/kmux/launcher-runtime");
    assert_eq!(
        fs::metadata(&launcher_runtime)?.permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(fs::read_dir(&launcher_runtime)?.count(), 0);
    assert_eq!(fs::read_dir(&runtime)?.count(), 0);
    Ok(())
}

#[cfg(unix)]
#[test]
fn launcher_fails_closed_when_state_storage_is_unusable() -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let (temp, repo) = init_repo()?;
    let Some(tmux) = TmuxFixture::new(&repo)? else {
        return Ok(());
    };
    let config_home = write_config(
        temp.path(),
        r#"
window_prefix: kmux-
launchers:
  example-launcher: {command: /bin/true}
"#,
    )?;
    let state_blocker = temp.path().join("state-blocker");
    let runtime = temp.path().join("runtime");
    fs::write(&state_blocker, "not a directory")?;
    fs::create_dir(&runtime)?;
    fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700))?;

    kmux(&repo, &config_home, &tmux)?
        .env("XDG_STATE_HOME", &state_blocker)
        .env("XDG_RUNTIME_DIR", &runtime)
        .args([
            "add",
            "feature/unusable-state",
            "--background",
            "--launch",
            "example-launcher",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "failed to create kmux state directory",
        ));

    assert_eq!(fs::read_dir(&runtime)?.count(), 0);
    Ok(())
}

#[cfg(unix)]
#[test]
fn add_launcher_preserves_argv_tty_ordering_and_shell_survival() -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let (temp, repo) = init_repo()?;
    let Some(tmux) = TmuxFixture::new(&repo)? else {
        return Ok(());
    };
    fs::create_dir_all(repo.join(".agents/plans"))?;
    fs::write(repo.join(".agents/plans/sidebar.md"), "plan\n")?;
    let output = temp.path().join("launcher-argv");
    let cwd_output = temp.path().join("launcher-cwd");
    let ordering = temp.path().join("launcher-ordering");
    let tty = temp.path().join("launcher-tty");
    let shell_returned = temp.path().join("shell-returned");
    let side_effect = temp.path().join("must-not-exist");
    let telemetry = temp.path().join("telemetry.jsonl");
    let state_home = temp.path().join("state-home");
    fs::create_dir(&state_home)?;
    let config_home = write_config(
        temp.path(),
        &format!(
            r#"
window_prefix: kmux-
post_create:
  - touch hook-ran
files:
  copy: [.agents]
launchers:
  example-launcher:
    command: sh
    args:
      - -c
      - |
          output=$1
          cwd_output=$2
          ordering=$3
          tty=$4
          shift 4
          printf '%s' "$PWD" > "$cwd_output"
          test -f .agents/plans/sidebar.md
          test -f hook-ran
          common_dir=$(git rev-parse --path-format=absolute --git-common-dir)
          test -s "$common_dir/kmux/state.json"
          touch "$ordering"
          test -t 0 && test -t 1 && test -t 2 && touch "$tty"
          printf '%s\0' "$@" > "$output"
          exit 17
      - launcher
      - {}
      - {}
      - {}
      - {}
      - "static two words"
      - ""
      - "--static"
"#,
            output.display(),
            cwd_output.display(),
            ordering.display(),
            tty.display(),
        ),
    )?;
    let worktree = temp.path().join("project__worktrees/feature-launcher");
    let input = format!(
        "--leading input with 'quotes' λ\nand metacharacters ; touch {}",
        side_effect.display()
    );
    let initial_window = tmux.current_window_id()?;
    let shell_command = tmux.pane_format(&tmux.pane_id, "#{pane_current_command}")?;

    kmux(&repo, &config_home, &tmux)?
        .env("XDG_STATE_HOME", &state_home)
        .env("KMUX_TELEMETRY", "1")
        .env("KMUX_TELEMETRY_PATH", &telemetry)
        .args([
            "add",
            "feature/launcher",
            "--background",
            "--launch",
            "example-launcher",
            "--input",
            &input,
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("created feature-launcher"));

    wait_for_nonempty_file(&output)?;
    assert_eq!(
        fs::read_to_string(&cwd_output)?,
        worktree.display().to_string()
    );
    assert!(ordering.exists());
    assert!(tty.exists());
    assert!(!side_effect.exists());
    assert_eq!(
        nul_delimited_arguments(&output)?,
        ["static two words", "", "--static", input.as_str()]
    );
    assert_eq!(tmux.current_window_id()?, initial_window);

    let pane = tmux.pane_for_window("kmux-feature-launcher")?;
    tmux.wait_for_pane_command(&pane, &shell_command)?;
    assert!(tmux.window_exists("kmux-feature-launcher")?);
    assert!(
        !tmux
            .pane_format(&pane, "#{pane_start_command}")?
            .contains(&input)
    );
    assert!(
        !tmux
            .tmux_output(&["capture-pane", "-p", "-t", &pane])?
            .contains(&input)
    );
    let shell_command_text = format!("touch {}", shell_returned.display());
    tmux.tmux_output(&["send-keys", "-t", &pane, "-l", &shell_command_text])?;
    tmux.tmux_output(&["send-keys", "-t", &pane, "Enter"])?;
    wait_for_path(&shell_returned)?;
    let launcher_runtime = state_home.join("kmux/launcher-runtime");
    assert_eq!(
        fs::metadata(&launcher_runtime)?.permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(fs::read_dir(&launcher_runtime)?.count(), 0);
    assert!(!fs::read_to_string(telemetry)?.contains(&input));
    Ok(())
}

#[test]
fn add_stdin_input_preserves_bytes_and_rejects_invalid_data_preflight() -> Result<()> {
    let (temp, repo) = init_repo()?;
    let Some(tmux) = TmuxFixture::new(&repo)? else {
        return Ok(());
    };
    let output = temp.path().join("stdin-input");
    let count = temp.path().join("stdin-count");
    let config_home = write_config(
        temp.path(),
        &format!(
            r#"
window_prefix: kmux-
launchers:
  stdin-launcher:
    command: sh
    args:
      - -c
      - |
          printf '%s' "$#" > "$1"
          test "$#" -eq 2 && printf '%s' "$2" > "$1.input"
      - launcher
      - {}
"#,
            count.display()
        ),
    )?;
    let stdin_text = "first line\nsecond line\n";

    kmux(&repo, &config_home, &tmux)?
        .args([
            "add",
            "feature/stdin",
            "--background",
            "--launch",
            "stdin-launcher",
            "--input",
            "-",
        ])
        .write_stdin(stdin_text)
        .assert()
        .success();
    wait_for_nonempty_file(&count)?;
    assert_eq!(fs::read_to_string(&count)?, "2");
    fs::rename(count.with_extension("input"), &output)?;
    assert_eq!(fs::read_to_string(&output)?, stdin_text);

    fs::remove_file(&count)?;
    kmux(&repo, &config_home, &tmux)?
        .args([
            "add",
            "feature/empty-input",
            "--background",
            "--launch",
            "stdin-launcher",
            "--input",
            "-",
        ])
        .write_stdin("")
        .assert()
        .success();
    wait_for_nonempty_file(&count)?;
    wait_for_path(&count.with_extension("input"))?;
    assert_eq!(fs::read_to_string(&count)?, "2");
    assert_eq!(fs::read(count.with_extension("input"))?, b"");

    fs::remove_file(&count)?;
    kmux(&repo, &config_home, &tmux)?
        .args([
            "add",
            "feature/no-input",
            "--background",
            "--launch",
            "stdin-launcher",
        ])
        .assert()
        .success();
    wait_for_nonempty_file(&count)?;
    assert_eq!(fs::read_to_string(&count)?, "1");

    for (branch, bytes, diagnostic) in [
        ("feature/invalid-utf8", vec![0xff], "valid UTF-8"),
        (
            "feature/nul-input",
            b"before\0after".to_vec(),
            "must not contain NUL",
        ),
    ] {
        kmux(&repo, &config_home, &tmux)?
            .args([
                "add",
                branch,
                "--background",
                "--launch",
                "stdin-launcher",
                "--input",
                "-",
            ])
            .write_stdin(bytes)
            .assert()
            .failure()
            .stderr(predicate::str::contains(diagnostic));
        let slug = branch.replace('/', "-");
        assert!(!temp.path().join("project__worktrees").join(&slug).exists());
        assert!(!tmux.window_exists(&format!("kmux-{slug}"))?);
        assert!(git_stdout(&repo, &["show-ref", "--heads", branch]).is_err());
    }
    Ok(())
}

#[test]
fn default_launcher_selects_after_spawn_and_unknown_override_is_preflight() -> Result<()> {
    let (temp, repo) = init_repo()?;
    let Some(tmux) = TmuxFixture::new(&repo)? else {
        return Ok(());
    };
    let marker = temp.path().join("default-launcher-ran");
    let config_home = write_config(
        temp.path(),
        &format!(
            r#"
window_prefix: kmux-
window: {{default_launcher: editor}}
launchers:
  editor:
    command: sh
    args: [-c, 'touch "$1"', launcher, {}]
"#,
            marker.display()
        ),
    )?;

    kmux(&repo, &config_home, &tmux)?
        .args([
            "add",
            "feature/unknown-launcher",
            "--launch",
            "missing",
            "--input",
            "preflight-sentinel",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unknown launcher \"missing\""))
        .stderr(predicate::str::contains("preflight-sentinel").not());
    assert!(
        !temp
            .path()
            .join("project__worktrees/feature-unknown-launcher")
            .exists()
    );
    assert!(!tmux.window_exists("kmux-feature-unknown-launcher")?);
    assert!(git_stdout(&repo, &["show-ref", "--heads", "feature/unknown-launcher"]).is_err());

    let initial_window = tmux.current_window_id()?;
    kmux(&repo, &config_home, &tmux)?
        .args(["add", "feature/default-launcher"])
        .assert()
        .success();
    wait_for_path(&marker)?;
    let pane = tmux.pane_for_window("kmux-feature-default-launcher")?;
    let selected_window = tmux.pane_format(&pane, "#{window_id}")?;
    assert_ne!(selected_window, initial_window);
    assert_eq!(tmux.current_window_id()?, selected_window);
    Ok(())
}

#[test]
fn restore_uses_only_the_current_default_launcher_without_input() -> Result<()> {
    let (temp, repo) = init_repo()?;
    let Some(tmux) = TmuxFixture::new(&repo)? else {
        return Ok(());
    };
    let agent_output = temp.path().join("agent-output");
    let editor_output = temp.path().join("editor-output");
    let changed_output = temp.path().join("changed-output");
    let config = |default: &str| {
        format!(
            r#"
window_prefix: kmux-
window:
  default_launcher: {default}
launchers:
  agent:
    command: sh
    args: [-c, 'printf "%s" "$2" > "$1"', launcher, {}]
  editor:
    command: sh
    args: [-c, 'printf "%s" "$#" > "$1"', launcher, {}]
  changed:
    command: sh
    args: [-c, 'printf "%s" "$#" > "$1"', launcher, {}]
"#,
            agent_output.display(),
            editor_output.display(),
            changed_output.display(),
        )
    };
    let config_home = write_config(temp.path(), &config("editor"))?;

    kmux(&repo, &config_home, &tmux)?
        .args([
            "add",
            "feature/restore-launcher",
            "--background",
            "--launch",
            "agent",
            "--input",
            "one-shot-context",
        ])
        .assert()
        .success();
    wait_for_nonempty_file(&agent_output)?;
    assert_eq!(fs::read_to_string(&agent_output)?, "one-shot-context");
    assert!(!editor_output.exists());

    tmux.tmux_output(&["kill-window", "-t", "kmux-feature-restore-launcher"])?;
    kmux(&repo, &config_home, &tmux)?
        .arg("restore")
        .assert()
        .success();
    wait_for_nonempty_file(&editor_output)?;
    assert_eq!(fs::read_to_string(&editor_output)?, "1");

    write_config(temp.path(), &config("changed"))?;
    tmux.tmux_output(&["kill-window", "-t", "kmux-feature-restore-launcher"])?;
    kmux(&repo, &config_home, &tmux)?
        .arg("restore")
        .assert()
        .success();
    wait_for_nonempty_file(&changed_output)?;
    assert_eq!(fs::read_to_string(&changed_output)?, "1");

    fs::remove_file(&changed_output)?;
    kmux(&repo, &config_home, &tmux)?
        .arg("restore")
        .assert()
        .success();
    assert!(!changed_output.exists());
    let state = fs::read_to_string(repo.join(".git/kmux/state.json"))?;
    let state_json: serde_json::Value = serde_json::from_str(&state)?;
    let state_object = state_json.as_object().expect("workspace state object");
    assert_eq!(
        state_object.keys().collect::<Vec<_>>(),
        ["parents", "version"]
    );
    for parent in state_object["parents"].as_array().expect("parent links") {
        let fields = parent.as_object().expect("parent link object");
        assert_eq!(
            fields.keys().collect::<Vec<_>>(),
            ["anchor", "branch", "parent"]
        );
    }
    assert!(!state.contains("one-shot-context"));
    Ok(())
}

#[test]
fn launcher_spawn_failure_keeps_workspace_and_existing_window_is_not_relaunched() -> Result<()> {
    let (temp, repo) = init_repo()?;
    let Some(tmux) = TmuxFixture::new(&repo)? else {
        return Ok(());
    };
    let marker = temp.path().join("must-not-launch-on-existing-window");
    let config_home = write_config(
        temp.path(),
        r#"
window_prefix: kmux-
launchers:
  broken-launcher:
    command: kmux-definitely-missing-launcher
"#,
    )?;
    let worktree = temp
        .path()
        .join("project__worktrees/feature-broken-launcher");
    let input = "failure-input-sentinel";

    kmux(&repo, &config_home, &tmux)?
        .args([
            "add",
            "feature/broken-launcher",
            "--background",
            "--launch",
            "broken-launcher",
            "--input",
            input,
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("handoff failed"))
        .stderr(predicate::str::contains(
            "launcher process could not be started",
        ))
        .stderr(predicate::str::contains(
            "inspect the window before manual recovery",
        ))
        .stderr(predicate::str::contains(input).not())
        .stderr(predicate::str::contains("kmux-definitely-missing-launcher").not());
    assert!(worktree.is_dir());
    assert!(tmux.window_exists("kmux-feature-broken-launcher")?);
    assert!(git_stdout(&repo, &["show-ref", "--heads", "feature/broken-launcher"]).is_ok());
    assert!(repo.join(".git/kmux/state.json").is_file());

    write_config(
        temp.path(),
        &format!(
            r#"
window_prefix: kmux-
window: {{default_launcher: editor}}
launchers:
  editor:
    command: sh
    args: [-c, 'touch "$1"', launcher, {}]
"#,
            marker.display()
        ),
    )?;
    kmux(&repo, &config_home, &tmux)?
        .arg("restore")
        .assert()
        .success();
    assert!(!marker.exists());
    Ok(())
}

#[test]
fn restore_ingress_timeout_keeps_first_window_and_stops_before_later_workspaces() -> Result<()> {
    let (temp, repo) = init_repo()?;
    let Some(tmux) = TmuxFixture::new(&repo)? else {
        return Ok(());
    };
    let config_home = write_config(temp.path(), "window_prefix: kmux-\n")?;
    for branch in ["feature/alpha", "feature/beta"] {
        kmux(&repo, &config_home, &tmux)?
            .args(["add", branch, "--background"])
            .assert()
            .success();
    }
    tmux.tmux_output(&["kill-window", "-t", "kmux-feature-alpha"])?;
    tmux.tmux_output(&["kill-window", "-t", "kmux-feature-beta"])?;
    tmux.tmux_output(&["set-option", "-g", "default-command", "sleep 60"])?;
    write_config(
        temp.path(),
        r#"
window_prefix: kmux-
window: {default_launcher: editor}
launchers:
  editor: {command: /bin/true}
"#,
    )?;

    kmux(&repo, &config_home, &tmux)?
        .arg("restore")
        .assert()
        .failure()
        .stderr(predicate::str::contains("timed out after 3s"))
        .stderr(predicate::str::contains(
            "waiting for launcher ingress to consume its request",
        ))
        .stderr(predicate::str::contains("may already be running"))
        .stderr(predicate::str::contains("shell window remains available"));
    assert!(tmux.window_exists("kmux-feature-alpha")?);
    assert!(!tmux.window_exists("kmux-feature-beta")?);
    Ok(())
}

#[test]
fn restore_spawn_failure_keeps_shell_window_and_stops() -> Result<()> {
    let (temp, repo) = init_repo()?;
    let Some(tmux) = TmuxFixture::new(&repo)? else {
        return Ok(());
    };
    let config_home = write_config(temp.path(), "window_prefix: kmux-\n")?;
    for branch in ["feature/alpha-spawn", "feature/beta-spawn"] {
        kmux(&repo, &config_home, &tmux)?
            .args(["add", branch, "--background"])
            .assert()
            .success();
    }
    tmux.tmux_output(&["kill-window", "-t", "kmux-feature-alpha-spawn"])?;
    tmux.tmux_output(&["kill-window", "-t", "kmux-feature-beta-spawn"])?;
    write_config(
        temp.path(),
        r#"
window_prefix: kmux-
window: {default_launcher: broken}
launchers:
  broken: {command: kmux-definitely-missing-launcher}
"#,
    )?;

    kmux(&repo, &config_home, &tmux)?
        .arg("restore")
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "launcher process could not be started",
        ))
        .stderr(predicate::str::contains("shell window remains available"))
        .stderr(predicate::str::contains("kmux-definitely-missing-launcher").not());
    assert!(tmux.window_exists("kmux-feature-alpha-spawn")?);
    assert!(!tmux.window_exists("kmux-feature-beta-spawn")?);
    Ok(())
}

#[cfg(unix)]
#[test]
fn launcher_ctrl_c_and_window_close_follow_tmux_job_control() -> Result<()> {
    let (temp, repo) = init_repo()?;
    let Some(tmux) = TmuxFixture::new(&repo)? else {
        return Ok(());
    };
    let interrupt_ready = temp.path().join("interrupt-ready");
    let interrupted = temp.path().join("interrupted");
    let close_ready = temp.path().join("close-ready");
    let closed = temp.path().join("closed");
    let config_home = write_config(
        temp.path(),
        &format!(
            r#"
window_prefix: kmux-
launchers:
  interrupt:
    command: sh
    args:
      - -c
      - |
          trap 'touch "$2"; exit 130' INT
          touch "$1"
          while :; do sleep 1; done
      - launcher
      - {}
      - {}
  close:
    command: sh
    args:
      - -c
      - |
          trap 'touch "$2"; exit 129' HUP TERM
          touch "$1"
          while :; do sleep 1; done
      - launcher
      - {}
      - {}
"#,
            interrupt_ready.display(),
            interrupted.display(),
            close_ready.display(),
            closed.display(),
        ),
    )?;
    let shell_command = tmux.pane_format(&tmux.pane_id, "#{pane_current_command}")?;

    kmux(&repo, &config_home, &tmux)?
        .args([
            "add",
            "feature/interrupt",
            "--background",
            "--launch",
            "interrupt",
        ])
        .assert()
        .success();
    wait_for_path(&interrupt_ready)?;
    let interrupt_pane = tmux.pane_for_window("kmux-feature-interrupt")?;
    tmux.tmux_output(&["send-keys", "-t", &interrupt_pane, "C-c"])?;
    wait_for_path(&interrupted)?;
    tmux.wait_for_pane_command(&interrupt_pane, &shell_command)?;
    assert!(tmux.window_exists("kmux-feature-interrupt")?);

    kmux(&repo, &config_home, &tmux)?
        .args(["add", "feature/close", "--background", "--launch", "close"])
        .assert()
        .success();
    wait_for_path(&close_ready)?;
    tmux.tmux_output(&["kill-window", "-t", "kmux-feature-close"])?;
    wait_for_path(&closed)?;
    assert!(!tmux.window_exists("kmux-feature-close")?);
    Ok(())
}

#[cfg(unix)]
#[test]
fn launcher_that_handles_ctrl_c_keeps_ingress_as_foreground_owner() -> Result<()> {
    let (temp, repo) = init_repo()?;
    let Some(tmux) = TmuxFixture::new(&repo)? else {
        return Ok(());
    };
    let child_pid_path = temp.path().join("child-pid");
    let ingress_pid_path = temp.path().join("ingress-pid");
    let interrupted = temp.path().join("interrupted-once");
    let release = temp.path().join("release-launcher");
    let config_home = write_config(
        temp.path(),
        &format!(
            r#"
window_prefix: kmux-
launchers:
  resilient:
    command: sh
    args:
      - -c
      - |
          trap 'touch "$3"' INT
          printf '%s' "$$" > "$1"
          printf '%s' "$PPID" > "$2"
          while test ! -e "$4"; do sleep 0.05; done
      - launcher
      - {}
      - {}
      - {}
      - {}
"#,
            child_pid_path.display(),
            ingress_pid_path.display(),
            interrupted.display(),
            release.display(),
        ),
    )?;
    let shell_command = tmux.pane_format(&tmux.pane_id, "#{pane_current_command}")?;

    kmux(&repo, &config_home, &tmux)?
        .args([
            "add",
            "feature/resilient-interrupt",
            "--background",
            "--launch",
            "resilient",
        ])
        .assert()
        .success();
    wait_for_nonempty_file(&child_pid_path)?;
    wait_for_nonempty_file(&ingress_pid_path)?;
    let pane = tmux.pane_for_window("kmux-feature-resilient-interrupt")?;

    tmux.tmux_output(&["send-keys", "-t", &pane, "C-c"])?;
    wait_for_path(&interrupted)?;
    let child_pid = fs::read_to_string(&child_pid_path)?.parse::<i32>()?;
    let ingress_pid = fs::read_to_string(&ingress_pid_path)?.parse::<i32>()?;
    assert!(process_exists(child_pid));
    assert!(process_exists(ingress_pid));
    assert_ne!(
        tmux.pane_format(&pane, "#{pane_current_command}")?,
        shell_command
    );

    fs::write(&release, "release\n")?;
    tmux.wait_for_pane_command(&pane, &shell_command)?;
    assert!(tmux.window_exists("kmux-feature-resilient-interrupt")?);
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
    let repo_path = repo.display().to_string();
    tmux.tmux_output(&[
        "new-window",
        "-d",
        "-t",
        "project:",
        "-n",
        "kmux-feature-window-only",
        "-c",
        &repo_path,
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
fn remove_rejects_duplicate_expected_windows_before_git_mutation() -> Result<()> {
    let (temp, repo) = init_repo()?;
    let Some(tmux) = TmuxFixture::new(&repo)? else {
        return Ok(());
    };
    let config_home = write_config(temp.path(), "window_prefix: kmux-\n")?;
    let worktree = temp
        .path()
        .join("project__worktrees/feature-remove-duplicate");
    let worktree_path = worktree.display().to_string();
    let window_name = "kmux-feature-remove-duplicate";

    kmux(&repo, &config_home, &tmux)?
        .args(["add", "feature/remove-duplicate", "--background"])
        .assert()
        .success();
    tmux.tmux_output(&[
        "new-window",
        "-d",
        "-t",
        "project:",
        "-n",
        window_name,
        "-c",
        &worktree_path,
    ])?;

    kmux(&repo, &config_home, &tmux)?
        .args(["remove", "feature-remove-duplicate", "--force"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("has multiple windows named"));

    assert!(worktree.is_dir());
    assert!(git_stdout(&repo, &["show-ref", "--heads", "feature/remove-duplicate"]).is_ok());
    let names = tmux.tmux_output(&["list-windows", "-t", "project:", "-F", "#{window_name}"])?;
    assert_eq!(names.lines().filter(|name| *name == window_name).count(), 2);
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
