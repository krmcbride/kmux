pub mod support;

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use predicates::prelude::*;
use serde_json::Value;

use support::{TmuxFixture, git, git_stdout, init_repo, kmux, write_config};

fn owned(repo: &Path, config: &Path, tmux: &TmuxFixture) -> Result<Vec<Value>> {
    let output = kmux(repo, config, tmux)?
        .args(["workspace", "list", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let values: Vec<Value> = serde_json::from_slice(&output)?;
    Ok(values
        .into_iter()
        .filter(|item| item["authority"] == "kmux" && item["live"] == true)
        .collect())
}

fn path_of(value: &Value) -> Result<PathBuf> {
    Ok(PathBuf::from(
        value["git_worktree_path"]
            .as_str()
            .context("worktree path")?,
    ))
}

#[test]
fn default_creation_is_detached_ephemeral_and_clean_removal_cleans_its_allocation() -> Result<()> {
    let (temp, repo) = init_repo()?;
    let tmux = TmuxFixture::new(&repo)?.context("tmux fixture")?;
    let config = write_config(temp.path(), "")?;
    let refs = git_stdout(&repo, &["show-ref"])?;
    let anchor = git_stdout(&repo, &["rev-parse", "HEAD"])?;
    kmux(&repo, &config, &tmux)?
        .args(["workspace", "create"])
        .assert()
        .success()
        .stdout(predicate::str::contains("created ephemeral"));
    let items = owned(&repo, &config, &tmux)?;
    assert_eq!(items.len(), 1);
    let item = &items[0];
    let path = path_of(item)?;
    let allocation = path.parent().context("allocation")?;
    assert_eq!(path.file_name().context("basename")?, "project");
    assert_eq!(
        allocation.parent(),
        Some(temp.path().join("home/.kmux/worktrees").as_path())
    );
    assert_eq!(allocation.file_name().context("storage ID")?.len(), 12);
    assert_eq!(item["retention"], "ephemeral");
    assert_eq!(item["detached"], true);
    assert!(item["git_branch"].is_null());
    assert!(item["owned_branch"].is_null());
    assert_eq!(item["creation_anchor"], anchor);
    assert_eq!(git_stdout(&repo, &["show-ref"])?, refs);
    assert!(tmux.window_exists(&format!(
        "kmux-ephemeral-{}",
        item["label"].as_str().context("label")?
    ))?);
    let id = item["workspace_id"].as_str().context("id")?;
    kmux(&repo, &config, &tmux)?
        .args(["workspace", "remove", id])
        .assert()
        .success();
    assert!(!allocation.exists());
    assert_eq!(git_stdout(&repo, &["show-ref"])?, refs);
    Ok(())
}

#[test]
fn detached_source_and_configured_root_preserve_identity_across_publication_branches() -> Result<()>
{
    let (temp, repo) = init_repo()?;
    let tmux = TmuxFixture::new(&repo)?.context("tmux fixture")?;
    let root = temp.path().join("managed worktrees");
    let config = write_config(
        temp.path(),
        &format!("worktree_root: {}\n", serde_json::to_string(&root)?),
    )?;
    let source = temp.path().join("external/source");
    let anchor = git_stdout(&repo, &["rev-parse", "HEAD"])?;
    git(
        &repo,
        &[
            "worktree",
            "add",
            "--detach",
            source.to_str().context("source")?,
            "HEAD",
        ],
    )?;
    fs::write(repo.join("later.txt"), "later\n")?;
    git(&repo, &["add", "later.txt"])?;
    git(&repo, &["commit", "-m", "advance primary"])?;
    kmux(&source, &config, &tmux)?
        .args([
            "workspace",
            "create",
            "--name",
            "review-alpha",
            "--background",
        ])
        .assert()
        .success();
    let items = owned(&repo, &config, &tmux)?;
    let item = &items[0];
    let path = path_of(item)?;
    let id = item["workspace_id"].as_str().context("id")?;
    assert_eq!(item["git_head"], anchor);
    assert_eq!(item["label"], "review-alpha");
    assert_ne!(
        path.parent()
            .context("allocation")?
            .file_name()
            .context("ID")?,
        "review-alpha"
    );
    assert!(path.starts_with(&root));
    git(&path, &["switch", "-c", "publication/one"])?;
    fs::write(path.join("feature.txt"), "feature\n")?;
    git(&path, &["add", "feature.txt"])?;
    git(&path, &["commit", "-m", "publish work"])?;
    git(&path, &["branch", "publication/two"])?;
    git(&path, &["branch", "-m", "publication/renamed"])?;
    git(&path, &["switch", "publication/two"])?;
    let updated = owned(&repo, &config, &tmux)?;
    assert_eq!(updated[0]["workspace_id"], id);
    assert_eq!(updated[0]["retention"], "ephemeral");
    assert_eq!(path_of(&updated[0])?, path);
    assert_eq!(updated[0]["git_branch"], "publication/two");
    assert!(updated[0]["owned_branch"].is_null());
    let refs = git_stdout(&repo, &["show-ref"])?;
    kmux(&repo, &config, &tmux)?
        .args(["workspace", "remove", id])
        .assert()
        .success();
    assert_eq!(git_stdout(&repo, &["show-ref"])?, refs);
    assert!(!path.exists());
    Ok(())
}

#[test]
fn ephemeral_setup_failure_preserves_owned_worktree_and_checks_preflight_before_allocation()
-> Result<()> {
    let (temp, repo) = init_repo()?;
    let tmux = TmuxFixture::new(&repo)?.context("tmux fixture")?;
    let root = temp.path().join("managed");
    let config = write_config(
        temp.path(),
        &format!(
            "worktree_root: {}\npost_create:\n  - 'printf setup > .setup; exit 1'\n",
            serde_json::to_string(&root)?
        ),
    )?;
    kmux(&repo, &config, &tmux)?
        .args(["workspace", "create", "--from", "missing-ref"])
        .assert()
        .failure();
    assert!(!root.exists());
    kmux(&repo, &config, &tmux)?
        .args(["workspace", "create", "--name", "invalid/name"])
        .assert()
        .failure();
    assert!(!root.exists());
    kmux(&repo, &config, &tmux)?
        .args(["workspace", "create", "--name", "setup-alpha"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("post_create command failed"));
    let items = owned(&repo, &config, &tmux)?;
    assert_eq!(items.len(), 1);
    let path = path_of(&items[0])?;
    assert_eq!(fs::read_to_string(path.join(".setup"))?, "setup");
    assert_eq!(items[0]["presentation"], true);
    kmux(&repo, &config, &tmux)?
        .args(["workspace", "restore"])
        .assert()
        .success();
    assert!(tmux.window_exists("kmux-ephemeral-setup-alpha")?);
    kmux(&repo, &config, &tmux)?
        .args(["workspace", "remove", "setup-alpha"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("uncommitted changes"));
    kmux(&repo, &config, &tmux)?
        .args(["workspace", "remove", "setup-alpha", "--force"])
        .assert()
        .success();
    assert!(!path.exists());
    Ok(())
}

#[test]
fn explicit_start_ref_and_root_changes_apply_only_to_future_allocations() -> Result<()> {
    let (temp, repo) = init_repo()?;
    let tmux = TmuxFixture::new(&repo)?.context("tmux fixture")?;
    let anchor = git_stdout(&repo, &["rev-parse", "HEAD"])?;
    fs::write(repo.join("later.txt"), "later\n")?;
    git(&repo, &["add", "later.txt"])?;
    git(&repo, &["commit", "-m", "advance primary"])?;
    let first_root = temp.path().join("first-root");
    let config = write_config(
        temp.path(),
        &format!("worktree_root: {}\n", serde_json::to_string(&first_root)?),
    )?;
    kmux(&repo, &config, &tmux)?
        .args(["workspace", "create", "--from", "HEAD~1", "--name", "first"])
        .assert()
        .success();
    let first = owned(&repo, &config, &tmux)?.remove(0);
    let first_path = path_of(&first)?;
    assert_eq!(first["git_head"], anchor);
    assert!(first_path.starts_with(&first_root));
    let second_root = temp.path().join("second-root");
    write_config(
        temp.path(),
        &format!("worktree_root: {}\n", serde_json::to_string(&second_root)?),
    )?;
    kmux(&repo, &config, &tmux)?
        .args(["workspace", "create", "--name", "second"])
        .assert()
        .success();
    let items = owned(&repo, &config, &tmux)?;
    let second = items
        .iter()
        .find(|item| item["label"] == "second")
        .context("second")?;
    assert!(path_of(second)?.starts_with(&second_root));
    assert_ne!(second["workspace_id"], first["workspace_id"]);
    assert!(first_path.is_dir());
    kmux(&repo, &config, &tmux)?
        .args(["workspace", "remove", "first"])
        .assert()
        .success();
    assert!(!first_path.parent().context("allocation")?.exists());
    assert!(second_root.is_dir());
    Ok(())
}
