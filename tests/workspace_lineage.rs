pub mod support;

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use predicates::prelude::*;
use serde_json::Value;

use support::{TmuxFixture, git, git_stdout, init_repo, kmux, write_config};

fn inventory(repo: &Path, config: &Path, tmux: &TmuxFixture) -> Result<Vec<Value>> {
    let output = kmux(repo, config, tmux)?
        .args(["workspace", "list", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    Ok(serde_json::from_slice(&output)?)
}

fn entry<'a>(items: &'a [Value], label: &str) -> Result<&'a Value> {
    items
        .iter()
        .find(|item| item["label"] == label)
        .context("workspace entry")
}

fn path_of(value: &Value) -> Result<PathBuf> {
    Ok(PathBuf::from(
        value["git_worktree_path"].as_str().context("path")?,
    ))
}

#[test]
fn detached_lineage_survives_promotion_branch_rename_and_parent_removal() -> Result<()> {
    let (temp, repo) = init_repo()?;
    let tmux = TmuxFixture::new(&repo)?.context("tmux fixture")?;
    let config = write_config(temp.path(), "")?;
    kmux(&repo, &config, &tmux)?
        .args(["workspace", "create", "--name", "source"])
        .assert()
        .success();
    let initial = inventory(&repo, &config, &tmux)?;
    let source = entry(&initial, "source")?;
    let source_path = path_of(source)?;
    let source_id = source["workspace_id"].as_str().context("id")?;
    assert_eq!(
        source["lineage"]["parent"]["workspace_id"],
        entry(&initial, "primary")?["workspace_id"]
    );
    kmux(&source_path, &config, &tmux)?
        .args(["workspace", "create", "--name", "child", "--background"])
        .assert()
        .success();
    kmux(&repo, &config, &tmux)?
        .args([
            "workspace",
            "create",
            "feature/leaf",
            "--parent",
            "child",
            "--background",
        ])
        .assert()
        .success();
    let initial = inventory(&repo, &config, &tmux)?;
    let child = entry(&initial, "child")?;
    let child_lineage = child["lineage"].clone();
    assert_eq!(child_lineage["parent"]["workspace_id"], source_id);
    assert_eq!(
        entry(&initial, "feature-leaf")?["lineage"]["parent"]["workspace_id"],
        child["workspace_id"]
    );
    let branch_path = path_of(entry(&initial, "feature-leaf")?)?;
    assert_eq!(
        git_stdout(&branch_path, &["symbolic-ref", "--short", "HEAD"])?,
        "feature/leaf"
    );
    kmux(&repo, &config, &tmux)?
        .args(["workspace", "promote", source_id, "--name", "kept-source"])
        .assert()
        .success();
    git(&source_path, &["switch", "-c", "publication/one"])?;
    git(&source_path, &["branch", "-m", "publication/renamed"])?;
    let updated = inventory(&repo, &config, &tmux)?;
    assert_eq!(entry(&updated, "child")?["lineage"], child_lineage);
    assert_eq!(entry(&updated, "child")?["parent_label"], "kept-source");
    assert_eq!(entry(&updated, "kept-source")?["workspace_id"], source_id);
    kmux(&repo, &config, &tmux)?
        .args(["workspace", "set-parent", "feature/leaf", source_id])
        .assert()
        .failure()
        .stderr(predicate::str::contains("would create a cycle"));
    assert_eq!(inventory(&repo, &config, &tmux)?, updated);
    kmux(&repo, &config, &tmux)?
        .args(["workspace", "remove", source_id])
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "parent links still reference removed workspace",
        ));
    let remaining = inventory(&repo, &config, &tmux)?;
    assert_eq!(entry(&remaining, "child")?["lineage"], child_lineage);
    assert_eq!(entry(&remaining, "child")?["parent_missing"], true);
    assert_eq!(
        entry(&remaining, "child")?["parent_label"],
        "kept-source (missing)"
    );
    assert_eq!(entry(&remaining, "kept-source")?["registered"], false);
    assert_eq!(inventory(&repo, &config, &tmux)?, remaining);
    assert!(git_stdout(&repo, &["show-ref", "--heads", "publication/renamed"]).is_ok());
    Ok(())
}

#[test]
fn explicit_git_refs_and_external_lineage_do_not_change_checkout_or_authority() -> Result<()> {
    let (temp, repo) = init_repo()?;
    let tmux = TmuxFixture::new(&repo)?.context("tmux fixture")?;
    let config = write_config(temp.path(), "")?;
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
    git(&repo, &["tag", "base"])?;
    kmux(&repo, &config, &tmux)?
        .args([
            "workspace",
            "create",
            "--from",
            "refs/tags/base",
            "--name",
            "child",
        ])
        .assert()
        .success();
    let initial = inventory(&repo, &config, &tmux)?;
    let child = entry(&initial, "child")?;
    let child_path = path_of(child)?;
    let anchor = child["creation_anchor"].clone();
    assert_eq!(child["lineage"]["parent"]["kind"], "git_ref");
    assert_eq!(child["lineage"]["parent"]["reference"], "refs/tags/base");
    let refs = git_stdout(&repo, &["show-ref"])?;
    kmux(&external, &config, &tmux)?
        .args(["workspace", "set-parent", "child"])
        .assert()
        .success();
    let items = inventory(&repo, &config, &tmux)?;
    let external_item = items
        .iter()
        .find(|item| item["git_worktree_path"] == external.to_string_lossy().as_ref())
        .context("external")?;
    assert_eq!(external_item["authority"], "external");
    assert!(external_item["retention"].is_null());
    assert_eq!(
        external_item["lineage"]["parent"]["workspace_id"],
        child["workspace_id"]
    );
    kmux(&child_path, &config, &tmux)?
        .args(["workspace", "set-parent", "--git-ref", "refs/tags/base"])
        .assert()
        .success();
    assert_eq!(git_stdout(&repo, &["show-ref"])?, refs);
    assert_eq!(
        entry(&inventory(&repo, &config, &tmux)?, "child")?["creation_anchor"],
        anchor
    );
    kmux(&child_path, &config, &tmux)?
        .args(["workspace", "set-parent", "--git-ref", "missing-ref"])
        .assert()
        .failure();
    assert_eq!(
        entry(&inventory(&repo, &config, &tmux)?, "child")?["lineage"],
        child["lineage"]
    );
    kmux(&repo, &config, &tmux)?
        .args([
            "workspace",
            "remove",
            external.to_str().context("path")?,
            "--force",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("no lifecycle authority"));
    Ok(())
}

#[test]
fn legacy_links_migrate_known_endpoints_once_and_keep_unmatched_refs() -> Result<()> {
    let (temp, repo) = init_repo()?;
    let tmux = TmuxFixture::new(&repo)?.context("tmux fixture")?;
    let config = write_config(temp.path(), "")?;
    let parent = temp.path().join("project__worktrees/feature-parent");
    let child = temp.path().join("project__worktrees/feature-child");
    for (path, branch) in [(&parent, "feature/parent"), (&child, "feature/child")] {
        git(
            &repo,
            &[
                "worktree",
                "add",
                "-b",
                branch,
                path.to_str().context("path")?,
            ],
        )?;
    }
    let anchor = git_stdout(&repo, &["rev-parse", "HEAD"])?;
    fs::create_dir_all(repo.join(".git/kmux"))?;
    fs::write(
        repo.join(".git/kmux/state.json"),
        serde_json::to_vec(&serde_json::json!({
            "version": 1,
            "parents": [
                {"branch": "feature/child", "parent": "feature/parent", "anchor": anchor},
                {"branch": "feature/parent", "parent": "historical/base", "anchor": anchor},
                {"branch": "historical/child", "parent": "historical/base", "anchor": anchor}
            ]
        }))?,
    )?;
    let initial = inventory(&repo, &config, &tmux)?;
    let child_before = entry(&initial, "feature-child")?;
    let parent_before = entry(&initial, "feature-parent")?;
    assert_eq!(
        child_before["lineage"]["parent"]["workspace_id"],
        parent_before["workspace_id"]
    );
    assert_eq!(child_before["lineage"]["anchor"], anchor);
    assert!(child_before["creation_anchor"].is_null());
    assert_eq!(parent_before["lineage"]["parent"]["kind"], "git_ref");
    let state: Value = serde_json::from_slice(&fs::read(repo.join(".git/kmux/state.json"))?)?;
    assert_eq!(state["version"], 3);
    assert_eq!(state["parents"].as_array().context("legacy refs")?.len(), 1);
    assert_eq!(state["parents"][0]["branch"], "historical/child");
    git(&parent, &["branch", "-m", "publication/renamed"])?;
    git(&child, &["switch", "--detach"])?;
    assert_eq!(
        entry(&inventory(&repo, &config, &tmux)?, "feature-child")?["lineage"],
        child_before["lineage"]
    );
    git(
        &repo,
        &["worktree", "remove", parent.to_str().context("path")?],
    )?;
    let missing = inventory(&repo, &config, &tmux)?;
    assert_eq!(
        entry(&missing, "feature-child")?["lineage"],
        child_before["lineage"]
    );
    assert_eq!(entry(&missing, "feature-child")?["parent_missing"], true);
    assert_eq!(
        entry(&missing, "feature-child")?["git_parent_branch"],
        "feature/parent"
    );
    Ok(())
}
