mod support;

use std::fs;

use anyhow::Result;
use predicates::prelude::*;

use support::{
    TmuxFixture, delete_opencode_agent_observation_args, git, git_stdout, init_repo, kmux,
    kmux_parent_link, kmux_with_pane, run, set_opencode_status_args, write_config,
};

fn assert_parent_link(repo: &std::path::Path, branch: &str, parent: &str) -> Result<String> {
    let link = kmux_parent_link(repo, branch)?
        .ok_or_else(|| anyhow::anyhow!("parent link for '{branch}' not found"))?;
    assert_eq!(
        link.get("branch").and_then(serde_json::Value::as_str),
        Some(branch)
    );
    assert_eq!(
        link.get("parent").and_then(serde_json::Value::as_str),
        Some(parent)
    );
    let anchor = link
        .get("anchor")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("parent link anchor missing"))?;
    assert!(!anchor.is_empty());
    Ok(anchor.to_owned())
}

fn assert_no_parent_link(repo: &std::path::Path, branch: &str) -> Result<()> {
    assert!(kmux_parent_link(repo, branch)?.is_none());
    Ok(())
}

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

    kmux(&repo, &config_home, &tmux)?
        .args(["add", "feature/auth"])
        .assert()
        .success()
        .stdout(predicate::str::contains("created feature-auth"));
    assert!(worktree.is_dir());
    assert_parent_link(&repo, "feature/auth", "main")?;
    assert!(tmux.window_exists("kmux-feature-auth")?);

    kmux(&repo, &config_home, &tmux)?
        .args(["add", "feature/auth"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "workspace for 'feature/auth' already exists",
        ));

    kmux(&repo, &config_home, &tmux)?
        .arg("restore")
        .assert()
        .success()
        .stdout(predicate::str::contains("restored feature-auth"));

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
        .args(["list", "--json"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "\"workspace_slug\": \"feature-auth\"",
        ))
        .stdout(predicate::str::contains("\"git_branch\": \"feature/auth\""))
        .stdout(predicate::str::contains("\"git_worktree_path\""));

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
            "\"workspace_slug\": \"feature-auth\"",
        ));

    let active_window_before_restore = tmux.current_window_id()?;
    kmux(&repo, &config_home, &tmux)?
        .arg("restore")
        .assert()
        .success()
        .stdout(predicate::str::contains("restored feature-auth"));
    assert_eq!(tmux.current_window_id()?, active_window_before_restore);

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

    tmux.tmux_output(&["kill-window", "-t", "kmux-feature-auth"])?;
    assert!(!tmux.window_exists("kmux-feature-auth")?);
    kmux(&repo, &config_home, &tmux)?
        .arg("restore")
        .assert()
        .success()
        .stdout(predicate::str::contains("restored feature-auth"));
    assert!(tmux.window_exists("kmux-feature-auth")?);
    let window_count = tmux.unique_window_count()?;
    kmux(&repo, &config_home, &tmux)?
        .arg("restore")
        .assert()
        .success()
        .stdout(predicate::str::contains("restored feature-auth"));
    assert_eq!(tmux.unique_window_count()?, window_count);

    kmux(&repo, &config_home, &tmux)?
        .args(["rm", "feature-auth"])
        .assert()
        .success()
        .stdout(predicate::str::contains("removed feature-auth"));
    assert!(!worktree.exists());
    assert!(!tmux.window_exists("kmux-feature-auth")?);
    assert!(git_stdout(&repo, &["show-ref", "--heads", "feature/auth"]).is_err());
    assert_no_parent_link(&repo, "feature/auth")?;

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
fn add_records_explicit_parent_branch() -> Result<()> {
    let (temp, repo) = init_repo()?;
    let Some(tmux) = TmuxFixture::new(&repo)? else {
        return Ok(());
    };
    git(&repo, &["branch", "integration"])?;
    let config_home = write_config(temp.path(), "window_prefix: kmux-\n")?;

    kmux(&repo, &config_home, &tmux)?
        .args([
            "add",
            "feature/explicit-parent",
            "--parent",
            "integration",
            "--background",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("created feature-explicit-parent"));

    assert_parent_link(&repo, "feature/explicit-parent", "integration")?;
    Ok(())
}

#[test]
fn add_from_detached_head_without_parent_uses_parent_error_wording() -> Result<()> {
    let (temp, repo) = init_repo()?;
    let Some(tmux) = TmuxFixture::new(&repo)? else {
        return Ok(());
    };
    let config_home = write_config(temp.path(), "window_prefix: kmux-\n")?;
    let head = git_stdout(&repo, &["rev-parse", "HEAD"])?;
    git(&repo, &["checkout", "--detach", &head])?;

    kmux(&repo, &config_home, &tmux)?
        .args(["add", "feature/detached", "--background"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "cannot create a branch from detached HEAD without --parent",
        ))
        .stderr(predicate::str::contains("--base").not());

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
        .args([
            "add",
            "origin/remote-only",
            "--parent",
            "main",
            "--background",
        ])
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
    assert_parent_link(&repo, "remote-only", "main")?;
    assert!(tmux.window_exists("kmux-remote-only")?);

    Ok(())
}

#[test]
fn parent_command_sets_replaces_and_defaults_child_to_current_workspace() -> Result<()> {
    let (temp, repo) = init_repo()?;
    let Some(tmux) = TmuxFixture::new(&repo)? else {
        return Ok(());
    };
    let config_home = write_config(temp.path(), "window_prefix: kmux-\n")?;
    git(&repo, &["branch", "feature/base"])?;
    git(&repo, &["branch", "feature/short"])?;
    let worktree = temp.path().join("project__worktrees/feature-child");

    kmux(&repo, &config_home, &tmux)?
        .args(["add", "feature/child", "--background"])
        .assert()
        .success();
    assert_parent_link(&repo, "feature/child", "main")?;

    kmux(&repo, &config_home, &tmux)?
        .args(["parent", "feature-child", "feature/base"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "set parent of feature/child to feature/base",
        ));
    assert_parent_link(&repo, "feature/child", "feature/base")?;

    kmux_with_pane(&worktree, &config_home, &tmux, &tmux.pane_id)?
        .args(["parent", "feature/short"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "set parent of feature/child to feature/short",
        ));
    assert_parent_link(&repo, "feature/child", "feature/short")?;
    assert!(repo.join(".git/kmux/state.json").is_file());
    assert!(!worktree.join(".git/kmux/state.json").exists());

    kmux(&repo, &config_home, &tmux)?
        .args(["parent", "main"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "parent requires a workspace name when run from the main worktree",
        ));

    Ok(())
}

#[test]
fn parent_command_rejects_invalid_relationships_before_writing_state() -> Result<()> {
    let (temp, repo) = init_repo()?;
    let Some(tmux) = TmuxFixture::new(&repo)? else {
        return Ok(());
    };
    let config_home = write_config(temp.path(), "window_prefix: kmux-\n")?;

    kmux(&repo, &config_home, &tmux)?
        .args(["add", "feature/child", "--background"])
        .assert()
        .success();
    let original_anchor = assert_parent_link(&repo, "feature/child", "main")?;

    kmux(&repo, &config_home, &tmux)?
        .args(["parent", "missing-child", "main"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "workspace 'missing-child' not found",
        ));
    kmux(&repo, &config_home, &tmux)?
        .args(["parent", "feature-child", "missing-parent"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "parent branch 'missing-parent' does not exist locally",
        ));
    kmux(&repo, &config_home, &tmux)?
        .args(["parent", "feature-child", "feature/child"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("cannot be its own parent"));

    git(&repo, &["checkout", "--orphan", "orphan-parent"])?;
    fs::remove_file(repo.join("README.md"))?;
    fs::write(repo.join("orphan.txt"), "orphan\n")?;
    git(&repo, &["add", "orphan.txt"])?;
    git(&repo, &["commit", "-m", "orphan"])?;
    git(&repo, &["checkout", "main"])?;
    kmux(&repo, &config_home, &tmux)?
        .args(["parent", "feature-child", "orphan-parent"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("have no merge base"));

    kmux(&repo, &config_home, &tmux)?
        .args(["add", "feature/grandchild", "--background"])
        .assert()
        .success();
    kmux(&repo, &config_home, &tmux)?
        .args(["parent", "feature-grandchild", "feature/child"])
        .assert()
        .success();
    kmux(&repo, &config_home, &tmux)?
        .args(["parent", "feature-child", "feature/grandchild"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("would create a cycle"));

    assert_eq!(
        assert_parent_link(&repo, "feature/child", "main")?,
        original_anchor
    );
    Ok(())
}

#[test]
fn list_renders_parent_tree_order_and_json_parent_fields() -> Result<()> {
    let (temp, repo) = init_repo()?;
    let Some(tmux) = TmuxFixture::new(&repo)? else {
        return Ok(());
    };
    let config_home = write_config(temp.path(), "window_prefix: kmux-\n")?;

    for branch in [
        "feature/child",
        "feature/sibling",
        "feature/grandchild",
        "feature/second-child",
    ] {
        kmux(&repo, &config_home, &tmux)?
            .args(["add", branch, "--background"])
            .assert()
            .success();
    }
    kmux(&repo, &config_home, &tmux)?
        .args(["parent", "feature-grandchild", "feature/child"])
        .assert()
        .success();
    git(&repo, &["branch", "feature/root-only"])?;
    kmux(&repo, &config_home, &tmux)?
        .args(["parent", "feature-sibling", "feature/root-only"])
        .assert()
        .success();

    let list = kmux(&repo, &config_home, &tmux)?
        .arg("ls")
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&list.get_output().stdout);
    assert!(stdout.contains("PARENT"));
    assert!(stdout.contains("├── feature/child"));
    assert!(stdout.contains("│   └── feature/grandchild"));
    assert!(stdout.contains("└── feature/second-child"));
    assert!(stdout.contains("feature/root-only"));
    let main_line = stdout
        .lines()
        .position(|line| line.contains("main"))
        .expect("main row should render");
    let child_line = stdout
        .lines()
        .position(|line| line.contains("feature/child"))
        .expect("child row should render");
    let grandchild_line = stdout
        .lines()
        .position(|line| line.contains("feature/grandchild"))
        .expect("grandchild row should render");
    let sibling_line = stdout
        .lines()
        .position(|line| line.contains("feature/sibling"))
        .expect("sibling row should render");
    let second_child_line = stdout
        .lines()
        .position(|line| line.contains("feature/second-child"))
        .expect("second child row should render");
    assert!(main_line < child_line);
    assert!(child_line < grandchild_line);
    assert!(grandchild_line < second_child_line);
    assert!(second_child_line < sibling_line);

    kmux(&repo, &config_home, &tmux)?
        .args(["list", "--json"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "\"git_parent_branch\": \"feature/child\"",
        ))
        .stdout(predicate::str::contains("\"git_anchor_commit\""));

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

    kmux(&repo, &config_home, &tmux)?
        .arg("restore")
        .assert()
        .success()
        .stdout(predicate::str::contains("restored 0 workspaces"));
    assert!(!worktree.exists());
    assert!(!tmux.window_exists("kmux-feature-existing")?);

    Ok(())
}

#[test]
fn add_rejects_workspace_slug_collision() -> Result<()> {
    let (temp, repo) = init_repo()?;
    let Some(tmux) = TmuxFixture::new(&repo)? else {
        return Ok(());
    };
    let config_home = write_config(temp.path(), "window_prefix: kmux-\n")?;

    kmux(&repo, &config_home, &tmux)?
        .args(["add", "feature/auth", "--background"])
        .assert()
        .success();

    kmux(&repo, &config_home, &tmux)?
        .args(["add", "feature-auth", "--background"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "workspace slug 'feature-auth' already exists",
        ))
        .stderr(predicate::str::contains(
            "for branch 'feature/auth', not 'feature-auth'",
        ));

    assert!(git_stdout(&repo, &["show-ref", "--heads", "feature-auth"]).is_err());
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
    kmux(&repo, &config_home, &tmux)?
        .arg("restore")
        .assert()
        .success()
        .stdout(predicate::str::contains("restored 0 workspaces"));
    assert!(!worktree.exists());

    Ok(())
}

#[test]
fn restore_recreates_all_workspace_windows_without_duplicates() -> Result<()> {
    let (temp, repo) = init_repo()?;
    let Some(tmux) = TmuxFixture::new(&repo)? else {
        return Ok(());
    };
    let config_home = write_config(temp.path(), "window_prefix: kmux-\n")?;

    kmux(&repo, &config_home, &tmux)?
        .args(["add", "feature/one", "--background"])
        .assert()
        .success();
    kmux(&repo, &config_home, &tmux)?
        .args(["add", "feature/two", "--background"])
        .assert()
        .success();
    tmux.tmux_output(&["kill-window", "-t", "kmux-feature-one"])?;
    tmux.tmux_output(&["kill-window", "-t", "kmux-feature-two"])?;
    assert!(!tmux.window_exists("kmux-feature-one")?);
    assert!(!tmux.window_exists("kmux-feature-two")?);
    let active_window_before_restore = tmux.current_window_id()?;

    kmux(&repo, &config_home, &tmux)?
        .arg("restore")
        .assert()
        .success()
        .stdout(predicate::str::contains("restored feature-one"))
        .stdout(predicate::str::contains("restored feature-two"));
    assert_eq!(tmux.current_window_id()?, active_window_before_restore);
    assert!(tmux.window_exists("kmux-feature-one")?);
    assert!(tmux.window_exists("kmux-feature-two")?);

    let window_count = tmux.unique_window_count()?;
    kmux(&repo, &config_home, &tmux)?
        .arg("restore")
        .assert()
        .success();
    assert_eq!(tmux.unique_window_count()?, window_count);

    Ok(())
}

#[test]
fn restore_creates_expected_window_when_only_untracked_window_exists() -> Result<()> {
    let (temp, repo) = init_repo()?;
    let Some(tmux) = TmuxFixture::new(&repo)? else {
        return Ok(());
    };
    let config_home = write_config(temp.path(), "window_prefix: kmux-\n")?;
    let worktree = temp.path().join("project__worktrees/feature-resurrected");
    let worktree_path = worktree.display().to_string();

    kmux(&repo, &config_home, &tmux)?
        .args(["add", "feature/resurrected", "--background"])
        .assert()
        .success();
    tmux.tmux_output(&["kill-window", "-t", "kmux-feature-resurrected"])?;
    tmux.tmux_output(&[
        "new-window",
        "-d",
        "-t",
        "project:",
        "-n",
        "resurrected-feature",
        "-c",
        worktree_path.as_str(),
    ])?;
    let active_window_before_restore = tmux.current_window_id()?;

    kmux(&repo, &config_home, &tmux)?
        .arg("restore")
        .assert()
        .success()
        .stdout(predicate::str::contains("restored feature-resurrected"));
    assert_eq!(tmux.current_window_id()?, active_window_before_restore);
    assert!(tmux.window_exists("kmux-feature-resurrected")?);
    assert!(tmux.window_exists("resurrected-feature")?);

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
        worktree_path.as_str(),
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

    kmux(&repo, &config_home, &tmux)?
        .arg("rm")
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "requires a workspace name when run from the main worktree",
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
        .args(["parent", "feature-child", "feature/parent"])
        .assert()
        .success();

    kmux(&repo, &config_home, &tmux)?
        .args(["remove", "feature-parent"])
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "parent links still reference removed branch 'feature/parent': feature/child",
        ));

    assert_no_parent_link(&repo, "feature/parent")?;
    assert_parent_link(&repo, "feature/child", "feature/parent")?;
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
        .args(["add", "feature/external"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("already checked out outside kmux"));
    kmux(&repo, &config_home, &tmux)?
        .arg("restore")
        .assert()
        .success()
        .stdout(predicate::str::contains("restored 0 workspaces"));
    kmux(&repo, &config_home, &tmux)?
        .args(["remove", "feature/external", "--force"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "workspace 'feature/external' not found",
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
        .arg("restore")
        .assert()
        .success()
        .stdout(predicate::str::contains("restored 0 workspaces"));
    kmux(&repo, &config_home, &tmux)?
        .args(["add", "feature/nested"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("already checked out outside kmux"));
    assert!(nested.is_dir());
    Ok(())
}

#[test]
fn commands_reject_non_branch_derived_kmux_worktree_paths() -> Result<()> {
    let (temp, repo) = init_repo()?;
    let Some(tmux) = TmuxFixture::new(&repo)? else {
        return Ok(());
    };
    let config_home = write_config(temp.path(), "window_prefix: kmux-\n")?;
    let worktree_base = temp.path().join("project__worktrees");
    let legacy = worktree_base.join("custom-auth");
    fs::create_dir(&worktree_base)?;
    let legacy_arg = legacy.display().to_string();
    git(
        &repo,
        &["worktree", "add", "-b", "feature/legacy-auth", &legacy_arg],
    )?;

    kmux(&repo, &config_home, &tmux)?
        .args(["add", "feature/legacy-auth"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("non-derived kmux workspace path"))
        .stderr(predicate::str::contains("expected 'feature-legacy-auth'"));
    kmux(&repo, &config_home, &tmux)?
        .arg("restore")
        .assert()
        .failure()
        .stderr(predicate::str::contains("non-derived kmux workspace path"));
    kmux_with_pane(&legacy, &config_home, &tmux, &tmux.pane_id)?
        .arg("rm")
        .assert()
        .failure()
        .stderr(predicate::str::contains("non-derived kmux workspace path"));

    let list = kmux(&repo, &config_home, &tmux)?
        .args(["ls", "--json"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&list.get_output().stdout);
    assert!(!stdout.contains("custom-auth"));

    Ok(())
}

#[test]
fn commands_reject_branchless_kmux_worktrees() -> Result<()> {
    let (temp, repo) = init_repo()?;
    let Some(tmux) = TmuxFixture::new(&repo)? else {
        return Ok(());
    };
    let config_home = write_config(temp.path(), "window_prefix: kmux-\n")?;
    let worktree_base = temp.path().join("project__worktrees");
    let detached = worktree_base.join("detached");
    fs::create_dir(&worktree_base)?;
    let detached_arg = detached.display().to_string();
    git(
        &repo,
        &["worktree", "add", "--detach", &detached_arg, "HEAD"],
    )?;

    kmux(&repo, &config_home, &tmux)?
        .arg("restore")
        .assert()
        .failure()
        .stderr(predicate::str::contains("has no known git branch"));
    kmux(&repo, &config_home, &tmux)?
        .args(["remove", "detached", "--force"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("has no known git branch"));
    assert!(detached.is_dir());

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
