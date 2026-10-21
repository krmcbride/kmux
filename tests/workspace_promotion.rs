pub mod support;

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use predicates::prelude::*;
use serde_json::Value;

use support::{TmuxFixture, git, git_stdout, init_repo, kmux, write_config};

fn workspace(repo: &Path, config: &Path, tmux: &TmuxFixture, label: &str) -> Result<Value> {
    let output = kmux(repo, config, tmux)?
        .args(["workspace", "list", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let items: Vec<Value> = serde_json::from_slice(&output)?;
    items
        .into_iter()
        .find(|item| item["label"] == label)
        .context("workspace")
}

#[test]
fn detached_promotion_keeps_the_same_checkout_and_live_window() -> Result<()> {
    let (temp, repo) = init_repo()?;
    let tmux = TmuxFixture::new(&repo)?.context("tmux fixture")?;
    let config = write_config(temp.path(), "")?;
    kmux(&repo, &config, &tmux)?
        .args(["workspace", "create", "--name", "review-alpha"])
        .assert()
        .success();
    let before = workspace(&repo, &config, &tmux, "review-alpha")?;
    let path = PathBuf::from(before["git_worktree_path"].as_str().context("path")?);
    let refs = git_stdout(&repo, &["show-ref"])?;
    fs::write(path.join("work-in-progress"), "keep this\n")?;
    kmux(&path, &config, &tmux)?
        .args(["workspace", "promote", "--name", "long-running"])
        .assert()
        .success();
    let after = workspace(&repo, &config, &tmux, "long-running")?;
    for key in [
        "workspace_id",
        "git_worktree_path",
        "git_head",
        "git_branch",
        "detached",
        "creation_anchor",
        "owned_branch",
        "presentation",
        "tmux_window_ids",
    ] {
        assert_eq!(after[key], before[key], "{key} changed");
    }
    assert_eq!(after["retention"], "persistent");
    assert_eq!(
        fs::read_to_string(path.join("work-in-progress"))?,
        "keep this\n"
    );
    assert_eq!(git_stdout(&repo, &["show-ref"])?, refs);
    assert!(!tmux.window_exists("kmux-ephemeral-review-alpha")?);
    assert!(tmux.window_exists("kmux-long-running")?);
    kmux(&repo, &config, &tmux)?
        .args(["workspace", "restore"])
        .assert()
        .success();
    assert_eq!(workspace(&repo, &config, &tmux, "long-running")?, after);
    let id = before["workspace_id"].as_str().context("id")?;
    kmux(&repo, &config, &tmux)?
        .args(["workspace", "close", id])
        .assert()
        .success();
    kmux(&repo, &config, &tmux)?
        .args(["workspace", "open", id])
        .assert()
        .success();
    assert!(tmux.window_exists("kmux-long-running")?);
    kmux(&repo, &config, &tmux)?
        .args(["workspace", "remove", id])
        .assert()
        .failure()
        .stderr(predicate::str::contains("uncommitted changes"));
    kmux(&repo, &config, &tmux)?
        .args(["workspace", "remove", id, "--force"])
        .assert()
        .success();
    assert!(!path.exists());
    Ok(())
}

#[test]
fn attached_promotion_never_acquires_publication_branch_ownership() -> Result<()> {
    let (temp, repo) = init_repo()?;
    let tmux = TmuxFixture::new(&repo)?.context("tmux fixture")?;
    let config = write_config(temp.path(), "")?;
    kmux(&repo, &config, &tmux)?
        .args(["workspace", "create", "--name", "review-alpha"])
        .assert()
        .success();
    let before = workspace(&repo, &config, &tmux, "review-alpha")?;
    let path = PathBuf::from(before["git_worktree_path"].as_str().context("path")?);
    git(&path, &["switch", "-c", "publication/one"])?;
    fs::write(path.join("feature"), "feature\n")?;
    git(&path, &["add", "feature"])?;
    git(&path, &["commit", "-m", "publication work"])?;
    git(&path, &["branch", "publication/two"])?;
    let refs = git_stdout(&repo, &["show-ref"])?;
    let head = git_stdout(&path, &["rev-parse", "HEAD"])?;
    kmux(&repo, &config, &tmux)?
        .args(["workspace", "close", "review-alpha"])
        .assert()
        .success();
    kmux(&repo, &config, &tmux)?
        .args(["workspace", "promote", "publication/one"])
        .assert()
        .success();
    let after = workspace(&repo, &config, &tmux, "review-alpha")?;
    assert_eq!(after["git_branch"], "publication/one");
    assert_eq!(after["git_head"], head);
    assert!(after["owned_branch"].is_null());
    assert_eq!(after["workspace_id"], before["workspace_id"]);
    assert_eq!(after["presentation"], false);
    assert!(
        after["tmux_window_ids"]
            .as_array()
            .context("windows")?
            .is_empty()
    );
    kmux(&repo, &config, &tmux)?
        .args(["workspace", "restore"])
        .assert()
        .success();
    assert!(!tmux.window_exists("kmux-review-alpha")?);
    git(&path, &["switch", "--detach"])?;
    assert_eq!(
        workspace(&repo, &config, &tmux, "review-alpha")?["retention"],
        "persistent"
    );
    git(&path, &["switch", "publication/two"])?;
    kmux(&repo, &config, &tmux)?
        .args(["workspace", "remove", "review-alpha"])
        .assert()
        .success();
    assert_eq!(git_stdout(&repo, &["show-ref"])?, refs);
    assert!(!path.exists());
    Ok(())
}

#[test]
fn promotion_rejects_invalid_authority_labels_and_window_collisions_before_retention_changes()
-> Result<()> {
    let (temp, repo) = init_repo()?;
    let tmux = TmuxFixture::new(&repo)?.context("tmux fixture")?;
    let config = write_config(temp.path(), "")?;
    kmux(&repo, &config, &tmux)?
        .args(["workspace", "create", "--name", "review-alpha"])
        .assert()
        .success();
    let before = workspace(&repo, &config, &tmux, "review-alpha")?;
    for name in ["bad/name", "primary"] {
        kmux(&repo, &config, &tmux)?
            .args(["workspace", "promote", "review-alpha", "--name", name])
            .assert()
            .failure();
        assert_eq!(workspace(&repo, &config, &tmux, "review-alpha")?, before);
    }
    tmux.tmux_output(&[
        "new-window",
        "-d",
        "-n",
        "kmux-collision",
        "-c",
        repo.to_str().context("repo")?,
    ])?;
    kmux(&repo, &config, &tmux)?
        .args([
            "workspace",
            "promote",
            "review-alpha",
            "--name",
            "collision",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("conflicts with another window"));
    assert_eq!(workspace(&repo, &config, &tmux, "review-alpha")?, before);
    let external = temp.path().join("external/workspace");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "--detach",
            external.to_str().context("path")?,
            "HEAD",
        ],
    )?;
    for path in [&repo, &external] {
        kmux(path, &config, &tmux)?
            .args(["workspace", "promote"])
            .assert()
            .failure()
            .stderr(predicate::str::contains("only kmux-owned"));
    }
    kmux(&repo, &config, &tmux)?
        .args(["workspace", "promote", "review-alpha"])
        .assert()
        .success();
    let promoted = workspace(&repo, &config, &tmux, "review-alpha")?;
    kmux(&repo, &config, &tmux)?
        .args(["workspace", "promote", "review-alpha", "--name", "later"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("already persistent"));
    assert_eq!(workspace(&repo, &config, &tmux, "review-alpha")?, promoted);
    Ok(())
}
