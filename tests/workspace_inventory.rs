pub mod support;

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use serde_json::Value;

use support::{git, git_stdout, init_repo, kmux_stdout};

fn inventory(repo: &Path) -> Result<Vec<Value>> {
    Ok(serde_json::from_str(&kmux_stdout(
        repo,
        &["workspace", "list", "--json"],
    )?)?)
}

fn at_path<'a>(items: &'a [Value], path: &Path) -> Result<&'a Value> {
    items
        .iter()
        .find(|item| {
            item["git_worktree_path"] == path.to_string_lossy().as_ref()
                && item["registered"] == true
        })
        .context("registered inventory entry")
}

#[test]
fn legacy_migration_preserves_paths_branches_parents_and_only_runs_once() -> Result<()> {
    let (temp, repo) = init_repo()?;
    let legacy = temp.path().join("project__worktrees/feature-alpha");
    let detached = temp.path().join("arbitrary/location/workspace");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-b",
            "feature/alpha",
            legacy.to_str().expect("fixture path"),
        ],
    )?;
    git(
        &repo,
        &[
            "worktree",
            "add",
            "--detach",
            detached.to_str().expect("fixture path"),
            "HEAD",
        ],
    )?;
    let anchor = git_stdout(&repo, &["rev-parse", "HEAD"])?;
    let metadata = repo.join(".git/kmux");
    fs::create_dir_all(&metadata)?;
    fs::write(
        metadata.join("state.json"),
        serde_json::to_vec(&serde_json::json!({
            "version": 1, "parents": [{"branch": "feature/alpha", "parent": "main", "anchor": anchor}]
        }))?,
    )?;
    let first = inventory(&repo)?;
    assert_eq!(first.len(), 3);
    assert_eq!(at_path(&first, &repo)?["authority"], "primary");
    let owned = at_path(&first, &legacy)?;
    assert_eq!(owned["authority"], "kmux");
    assert_eq!(owned["retention"], "persistent");
    assert_eq!(owned["workspace_slug"], "feature-alpha");
    assert_eq!(owned["git_parent_branch"], "main");
    assert_eq!(owned["git_anchor_commit"], anchor);
    let external = at_path(&first, &detached)?;
    assert_eq!(external["authority"], "external");
    assert!(external["retention"].is_null());
    assert_eq!(external["detached"], true);
    assert!(external["git_branch"].is_null());
    assert_eq!(external["git_head"], anchor);
    assert_eq!(
        git_stdout(&legacy, &["symbolic-ref", "--short", "HEAD"])?,
        "feature/alpha"
    );
    assert_eq!(
        fs::read_to_string(legacy.join("README.md"))?,
        fs::read_to_string(repo.join("README.md"))?
    );

    let later = temp.path().join("project__worktrees/feature-later");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-b",
            "feature/later",
            later.to_str().expect("fixture path"),
        ],
    )?;
    git(&legacy, &["switch", "--detach"])?;
    let second = inventory(&repo)?;
    assert_eq!(at_path(&second, &later)?["authority"], "external");
    assert_eq!(
        at_path(&second, &legacy)?["workspace_id"],
        owned["workspace_id"]
    );
    assert_eq!(at_path(&second, &legacy)?["retention"], "persistent");
    assert_eq!(at_path(&second, &legacy)?["detached"], true);
    Ok(())
}

#[test]
fn reused_owned_path_is_external_and_old_identity_is_stale() -> Result<()> {
    let (temp, repo) = init_repo()?;
    let path = temp.path().join("project__worktrees/feature-alpha");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-b",
            "feature/alpha",
            path.to_str().expect("fixture path"),
        ],
    )?;
    let before = inventory(&repo)?;
    let owned_id = at_path(&before, &path)?["workspace_id"].clone();
    git(
        &repo,
        &["worktree", "remove", path.to_str().expect("fixture path")],
    )?;
    git(
        &repo,
        &[
            "worktree",
            "add",
            path.to_str().expect("fixture path"),
            "feature/alpha",
        ],
    )?;
    let after = inventory(&repo)?;
    assert_eq!(at_path(&after, &path)?["authority"], "external");
    let stale = after
        .iter()
        .find(|item| item["workspace_id"] == owned_id)
        .expect("stale identity");
    assert_eq!(stale["registered"], false);
    assert_eq!(stale["live"], false);
    assert_eq!(inventory(&repo)?, after);
    Ok(())
}

#[test]
fn inventory_reports_locked_prunable_and_unregistered_records_without_pruning() -> Result<()> {
    let (temp, repo) = init_repo()?;
    let locked = temp.path().join("locked workspace");
    let prunable = temp.path().join("missing workspace");
    for path in [&locked, &prunable] {
        git(
            &repo,
            &[
                "worktree",
                "add",
                "--detach",
                path.to_str().expect("fixture path"),
                "HEAD",
            ],
        )?;
    }
    git(
        &repo,
        &[
            "worktree",
            "lock",
            "--reason",
            "review",
            locked.to_str().expect("fixture path"),
        ],
    )?;
    let before = inventory(&repo)?;
    assert_eq!(at_path(&before, &locked)?["locked"], "review");
    fs::remove_dir_all(&prunable)?;
    let after = inventory(&repo)?;
    assert!(at_path(&after, &prunable)?["prunable"].is_string());
    assert_eq!(at_path(&after, &prunable)?["live"], false);
    assert!(git_stdout(&repo, &["worktree", "list", "--porcelain"])?.contains("missing workspace"));
    git(&repo, &["worktree", "prune"])?;
    let stale = inventory(&repo)?;
    assert!(stale.iter().any(|item| item["git_worktree_path"]
        == prunable.to_string_lossy().as_ref()
        && item["registered"] == false));
    Ok(())
}
