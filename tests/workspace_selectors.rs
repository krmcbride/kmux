pub mod support;

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use predicates::prelude::*;
use serde_json::Value;

use support::{TmuxFixture, init_repo, kmux, write_config};

fn workspace_path(repo: &Path, config: &Path, tmux: &TmuxFixture, label: &str) -> Result<PathBuf> {
    let output = kmux(repo, config, tmux)?
        .args(["workspace", "list", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let entries: Vec<Value> = serde_json::from_slice(&output)?;
    let path = entries
        .iter()
        .find(|entry| entry["label"] == label)
        .and_then(|entry| entry["git_worktree_path"].as_str())
        .context("workspace path")?;
    Ok(PathBuf::from(path))
}

#[test]
fn ephemeral_window_selectors_preserve_competing_checkouts_with_default_and_custom_prefixes()
-> Result<()> {
    for prefix in ["kmux-", "mux-"] {
        let (temp, repo) = init_repo()?;
        let tmux = TmuxFixture::new(&repo)?.context("tmux fixture")?;
        let config = write_config(temp.path(), &format!("window_prefix: {prefix}\n"))?;
        for label in ["review-alpha", "ephemeral-review-alpha"] {
            kmux(&repo, &config, &tmux)?
                .args(["workspace", "create", "--name", label])
                .assert()
                .success();
        }
        let intended = workspace_path(&repo, &config, &tmux, "review-alpha")?;
        let competing = workspace_path(&repo, &config, &tmux, "ephemeral-review-alpha")?;
        fs::write(intended.join("uncommitted.txt"), "discard this only\n")?;
        fs::write(competing.join("uncommitted.txt"), "preserve this\n")?;
        let intended_window = format!("{prefix}ephemeral-review-alpha");
        let competing_window = format!("{prefix}ephemeral-ephemeral-review-alpha");
        kmux(&repo, &config, &tmux)?
            .args(["workspace", "remove", &intended_window])
            .assert()
            .failure()
            .stderr(predicate::str::contains("uncommitted changes"));
        kmux(&repo, &config, &tmux)?
            .args(["workspace", "remove", "--force", &intended_window])
            .assert()
            .success()
            .stdout(predicate::str::contains("removed review-alpha\n"));
        assert!(!intended.exists());
        assert_eq!(
            fs::read_to_string(competing.join("uncommitted.txt"))?,
            "preserve this\n"
        );
        assert!(!tmux.window_exists(&intended_window)?);
        assert!(tmux.window_exists(&competing_window)?);
        // The ordinary ephemeral window selector also resolves when no competing label remains.
        kmux(&repo, &config, &tmux)?
            .args(["workspace", "remove", "--force", &competing_window])
            .assert()
            .success();
        assert!(!competing.exists());
    }
    Ok(())
}

#[test]
fn a_label_matching_another_presentation_name_is_ambiguous_before_forced_removal() -> Result<()> {
    let (temp, repo) = init_repo()?;
    let tmux = TmuxFixture::new(&repo)?.context("tmux fixture")?;
    let config = write_config(temp.path(), "")?;
    let selector = "kmux-ephemeral-review-alpha";
    for label in ["review-alpha", selector] {
        kmux(&repo, &config, &tmux)?
            .args(["workspace", "create", "--name", label])
            .assert()
            .success();
    }
    let first = workspace_path(&repo, &config, &tmux, "review-alpha")?;
    let second = workspace_path(&repo, &config, &tmux, selector)?;
    for path in [&first, &second] {
        fs::write(path.join("uncommitted.txt"), "preserve this\n")?;
    }
    let state = fs::read(repo.join(".git/kmux/state.json"))?;
    kmux(&repo, &config, &tmux)?
        .args(["workspace", "remove", "--force", selector])
        .assert()
        .failure()
        .stderr(predicate::str::contains("is ambiguous"));
    assert_eq!(fs::read(repo.join(".git/kmux/state.json"))?, state);
    for path in [&first, &second] {
        assert_eq!(
            fs::read_to_string(path.join("uncommitted.txt"))?,
            "preserve this\n"
        );
    }
    assert!(tmux.window_exists(selector)?);
    assert!(tmux.window_exists("kmux-ephemeral-kmux-ephemeral-review-alpha")?);
    Ok(())
}
