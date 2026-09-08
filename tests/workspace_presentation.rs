pub mod support;

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use predicates::prelude::*;
use serde_json::Value;

use support::{
    TmuxFixture, git, git_stdout, init_repo, kmux, kmux_detached, wait_for_nonempty_file,
    write_config,
};

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

fn entry<'a>(items: &'a [Value], path: &Path) -> Result<&'a Value> {
    items
        .iter()
        .find(|item| item["git_worktree_path"] == path.to_string_lossy().as_ref())
        .context("workspace inventory entry")
}

#[test]
fn restore_opens_all_unvisited_external_worktrees_once_and_reopens_after_close() -> Result<()> {
    let (temp, repo) = init_repo()?;
    let tmux = TmuxFixture::new(&repo)?.context("tmux fixture")?;
    let config = write_config(
        temp.path(),
        "window_prefix: kmux-\nwindow:\n  default_launcher: editor\nlaunchers:\n  editor:\n    command: sh\n    args: [-c, 'printf \"%s\\n\" \"$#\" >> .launches', editor]\n",
    )?;
    let first = temp.path().join("external-one/workspace");
    let second = temp.path().join("external-two/workspace");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "--detach",
            first.to_str().context("path")?,
            "HEAD",
        ],
    )?;
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-b",
            "publication/alpha",
            second.to_str().context("path")?,
        ],
    )?;
    let original_refs = git_stdout(&repo, &["show-ref"])?;
    kmux(&repo, &config, &tmux)?
        .args(["workspace", "restore"])
        .assert()
        .success();
    let initial = inventory(&repo, &config, &tmux)?;
    let first_entry = entry(&initial, &first)?;
    let first_id = first_entry["workspace_id"].as_str().context("id")?;
    let first_name = format!(
        "kmux-{}",
        first_entry["workspace_slug"].as_str().context("label")?
    );
    for path in [&first, &second] {
        wait_for_nonempty_file(&path.join(".launches"))?;
        assert_eq!(fs::read_to_string(path.join(".launches"))?, "0\n");
        let item = entry(&initial, path)?;
        assert_eq!(item["authority"], "external");
        assert_eq!(
            item["tmux_window_ids"].as_array().context("windows")?.len(),
            1
        );
        let name = format!("kmux-{}", item["workspace_slug"].as_str().context("label")?);
        let pane = tmux.pane_for_window(&name)?;
        tmux.wait_for_pane_current_path(&pane, path)?;
    }
    kmux(&repo, &config, &tmux)?
        .args(["workspace", "restore"])
        .assert()
        .success();
    assert_eq!(inventory(&repo, &config, &tmux)?, initial);
    assert_eq!(fs::read_to_string(first.join(".launches"))?, "0\n");

    kmux(&repo, &config, &tmux)?
        .args(["workspace", "close", first_id])
        .assert()
        .success();
    assert!(!tmux.window_exists(&first_name)?);
    assert!(first.is_dir());
    assert_eq!(git_stdout(&repo, &["show-ref"])?, original_refs);
    assert_eq!(
        entry(&inventory(&repo, &config, &tmux)?, &first)?["presentation"],
        false
    );
    fs::remove_file(first.join(".launches"))?;
    kmux(&repo, &config, &tmux)?
        .args(["workspace", "restore"])
        .assert()
        .success();
    wait_for_nonempty_file(&first.join(".launches"))?;
    assert!(tmux.window_exists(&first_name)?);
    assert_eq!(fs::read_to_string(first.join(".launches"))?, "0\n");
    for target in [first.to_str().context("path")?, first_id] {
        kmux(&repo, &config, &tmux)?
            .args(["workspace", "remove", "--force", target])
            .assert()
            .failure()
            .stderr(predicate::str::contains("no lifecycle authority"));
    }
    assert!(first.is_dir());
    Ok(())
}

#[test]
fn open_current_external_honors_focus_and_transient_launcher_input() -> Result<()> {
    let (temp, repo) = init_repo()?;
    let tmux = TmuxFixture::new(&repo)?.context("tmux fixture")?;
    let config = write_config(
        temp.path(),
        "window_prefix: kmux-\nlaunchers:\n  editor:\n    command: sh\n    args: [-c, 'printf \"%s\" \"$1\" > .input', editor]\n",
    )?;
    let path = temp.path().join("external/workspace");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "--detach",
            path.to_str().context("path")?,
            "HEAD",
        ],
    )?;
    kmux_detached(&path, &config, &tmux)?
        .args(["workspace", "open"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("pass --background"));
    kmux(&path, &config, &tmux)?
        .args([
            "workspace",
            "open",
            "--launcher",
            "editor",
            "--launcher-input",
            "one-shot-context",
        ])
        .assert()
        .success();
    wait_for_nonempty_file(&path.join(".input"))?;
    assert_eq!(fs::read_to_string(path.join(".input"))?, "one-shot-context");
    let items = inventory(&repo, &config, &tmux)?;
    let workspace = entry(&items, &path)?;
    assert_eq!(workspace["presentation"], true);
    let window = workspace["tmux_window_ids"][0]
        .as_str()
        .context("window id")?;
    assert_eq!(
        tmux.tmux_output(&["display-message", "-p", "-t", window, "#{window_active}"])?,
        "1"
    );
    fs::remove_file(path.join(".input"))?;
    kmux_detached(&repo, &config, &tmux)?
        .args([
            "workspace",
            "open",
            path.to_str().context("path")?,
            "--background",
            "--launcher",
            "editor",
            "--launcher-input",
            "do-not-replay",
        ])
        .assert()
        .success();
    assert!(!path.join(".input").exists());
    assert!(!fs::read_to_string(repo.join(".git/kmux/state.json"))?.contains("one-shot-context"));
    tmux.tmux_output(&["rename-window", "-t", window, "renamed-by-user"])?;
    kmux(&repo, &config, &tmux)?
        .args(["workspace", "open", path.to_str().context("path")?])
        .assert()
        .success();
    assert_eq!(
        entry(&inventory(&repo, &config, &tmux)?, &path)?["tmux_window_ids"][0],
        window
    );
    kmux(&path, &config, &tmux)?
        .args(["workspace", "close"])
        .assert()
        .success();
    assert!(path.is_dir());
    Ok(())
}

#[test]
fn name_collision_cannot_claim_or_close_an_unrelated_window_and_stale_restore_is_safe() -> Result<()>
{
    let (temp, repo) = init_repo()?;
    let tmux = TmuxFixture::new(&repo)?.context("tmux fixture")?;
    let config = write_config(temp.path(), "window_prefix: kmux-\n")?;
    let path = temp.path().join("external/workspace");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "--detach",
            path.to_str().context("path")?,
            "HEAD",
        ],
    )?;
    let items = inventory(&repo, &config, &tmux)?;
    let workspace = entry(&items, &path)?;
    let id = workspace["workspace_id"].as_str().context("id")?;
    let name = format!(
        "kmux-{}",
        workspace["workspace_slug"].as_str().context("label")?
    );
    tmux.tmux_output(&[
        "new-window",
        "-d",
        "-n",
        &name,
        "-c",
        repo.to_str().context("path")?,
    ])?;
    for command in ["open", "close"] {
        kmux(&repo, &config, &tmux)?
            .args(["workspace", command, id])
            .assert()
            .failure()
            .stderr(predicate::str::contains("no verified presentation"));
        assert!(tmux.window_exists(&name)?);
    }
    tmux.tmux_output(&["kill-window", "-t", &name])?;
    kmux(&repo, &config, &tmux)?
        .args(["workspace", "open", id])
        .assert()
        .success();
    git(
        &repo,
        &["worktree", "remove", path.to_str().context("path")?],
    )?;
    let refs = git_stdout(&repo, &["show-ref"])?;
    kmux(&repo, &config, &tmux)?
        .args(["workspace", "restore"])
        .assert()
        .success()
        .stdout(predicate::str::contains("restored 0 workspaces"));
    assert!(!path.exists());
    assert_eq!(git_stdout(&repo, &["show-ref"])?, refs);
    kmux(&repo, &config, &tmux)?
        .args(["workspace", "close", id])
        .assert()
        .success();
    assert!(!tmux.window_exists(&name)?);
    Ok(())
}
