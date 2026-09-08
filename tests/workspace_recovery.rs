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
    let values: Vec<Value> = serde_json::from_slice(&output)?;
    values
        .into_iter()
        .find(|item| item["label"] == label)
        .context("workspace")
}

fn path_of(value: &Value) -> Result<PathBuf> {
    Ok(PathBuf::from(
        value["git_worktree_path"].as_str().context("path")?,
    ))
}

fn commit_work(path: &Path) -> Result<String> {
    fs::write(path.join("committed.txt"), "recover this commit\n")?;
    git(path, &["add", "committed.txt"])?;
    git(path, &["commit", "-m", "detached work"])?;
    git_stdout(path, &["rev-parse", "HEAD"])
}

#[test]
fn advanced_detached_work_is_recoverable_after_ephemeral_or_promoted_removal() -> Result<()> {
    for promote in [false, true] {
        let (temp, repo) = init_repo()?;
        let tmux = TmuxFixture::new(&repo)?.context("tmux fixture")?;
        let config = write_config(temp.path(), "")?;
        kmux(&repo, &config, &tmux)?
            .args(["workspace", "create", "--name", "review-alpha"])
            .assert()
            .success();
        let before = workspace(&repo, &config, &tmux, "review-alpha")?;
        let path = path_of(&before)?;
        let head = commit_work(&path)?;
        let id = before["workspace_id"].as_str().context("id")?;
        if promote {
            kmux(&repo, &config, &tmux)?
                .args(["workspace", "promote", id])
                .assert()
                .success();
        }
        let reference = format!("refs/kmux/recovery/{id}/{head}");
        kmux(&repo, &config, &tmux)?
            .args(["workspace", "remove", id])
            .assert()
            .success()
            .stdout(predicate::str::contains(&reference))
            .stdout(predicate::str::contains(
                "recreate: git worktree add --detach",
            ));
        assert!(!path.exists());
        assert!(!path.parent().context("allocation")?.exists());
        assert_eq!(git_stdout(&repo, &["rev-parse", &reference])?, head);
        assert_eq!(
            git_stdout(
                &repo,
                &["for-each-ref", "--format=%(refname:short)", "refs/heads/"]
            )?,
            "main"
        );
        for helper in ["_complete-create-branches", "_complete-git-branches"] {
            kmux(&repo, &config, &tmux)?
                .arg(helper)
                .assert()
                .success()
                .stdout(predicate::str::contains("recovery").not());
        }
        let recovered = temp.path().join("recovered/workspace");
        git(
            &repo,
            &[
                "worktree",
                "add",
                "--detach",
                recovered.to_str().context("path")?,
                &reference,
            ],
        )?;
        assert_eq!(
            fs::read_to_string(recovered.join("committed.txt"))?,
            "recover this commit\n"
        );
        assert_eq!(git_stdout(&recovered, &["rev-parse", "HEAD"])?, head);
        let after = workspace(&repo, &config, &tmux, "review-alpha")?;
        assert_eq!(after["registered"], false);
        assert_eq!(after["presentation"], false);
        assert!(
            after["tmux_window_ids"]
                .as_array()
                .context("windows")?
                .is_empty()
        );
    }
    Ok(())
}

#[test]
fn dirty_and_locked_worktrees_are_refused_before_snapshot_and_force_keeps_only_committed_work()
-> Result<()> {
    let (temp, repo) = init_repo()?;
    let tmux = TmuxFixture::new(&repo)?.context("tmux fixture")?;
    let config = write_config(temp.path(), "")?;
    kmux(&repo, &config, &tmux)?
        .args(["workspace", "create", "--name", "review-alpha"])
        .assert()
        .success();
    let item = workspace(&repo, &config, &tmux, "review-alpha")?;
    let path = path_of(&item)?;
    let head = commit_work(&path)?;
    fs::write(path.join("uncommitted.txt"), "discard only with force\n")?;
    let before = workspace(&repo, &config, &tmux, "review-alpha")?;
    let state = fs::read(repo.join(".git/kmux/state.json"))?;
    kmux(&repo, &config, &tmux)?
        .args(["workspace", "remove", "review-alpha"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("uncommitted changes"));
    assert_eq!(
        git_stdout(&repo, &["for-each-ref", "refs/kmux/recovery/"])?,
        ""
    );
    assert_eq!(fs::read(repo.join(".git/kmux/state.json"))?, state);
    assert_eq!(workspace(&repo, &config, &tmux, "review-alpha")?, before);
    git(&repo, &["worktree", "lock", path.to_str().context("path")?])?;
    kmux(&repo, &config, &tmux)?
        .args(["workspace", "remove", "review-alpha", "--force"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("worktree is locked"));
    assert_eq!(
        git_stdout(&repo, &["for-each-ref", "refs/kmux/recovery/"])?,
        ""
    );
    git(
        &repo,
        &["worktree", "unlock", path.to_str().context("path")?],
    )?;
    kmux(&repo, &config, &tmux)?
        .args(["workspace", "remove", "review-alpha", "--force"])
        .assert()
        .success();
    let reference = format!(
        "refs/kmux/recovery/{}/{head}",
        item["workspace_id"].as_str().context("id")?
    );
    assert_eq!(
        git_stdout(&repo, &["show", &format!("{reference}:committed.txt")])?,
        "recover this commit"
    );
    assert!(git_stdout(&repo, &["show", &format!("{reference}:uncommitted.txt")]).is_err());
    Ok(())
}

#[test]
fn unknown_legacy_creation_anchor_is_protected_even_at_migration_head() -> Result<()> {
    let (temp, repo) = init_repo()?;
    let tmux = TmuxFixture::new(&repo)?.context("tmux fixture")?;
    let config = write_config(temp.path(), "")?;
    let legacy = temp.path().join("project__worktrees/feature-legacy");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-b",
            "feature/legacy",
            legacy.to_str().context("path")?,
        ],
    )?;
    let head = commit_work(&legacy)?;
    let migrated = workspace(&repo, &config, &tmux, "feature-legacy")?;
    assert!(migrated["creation_anchor"].is_null());
    git(&legacy, &["switch", "--detach"])?;
    git(&repo, &["branch", "-D", "feature/legacy"])?;
    kmux(&repo, &config, &tmux)?
        .args(["workspace", "remove", "feature-legacy"])
        .assert()
        .success();
    let reference = format!(
        "refs/kmux/recovery/{}/{head}",
        migrated["workspace_id"].as_str().context("id")?
    );
    assert_eq!(git_stdout(&repo, &["rev-parse", &reference])?, head);
    assert!(!legacy.exists());
    Ok(())
}

#[test]
fn recovery_ref_creation_failure_leaves_files_registration_policy_and_window_intact() -> Result<()>
{
    let (temp, repo) = init_repo()?;
    let tmux = TmuxFixture::new(&repo)?.context("tmux fixture")?;
    let config = write_config(temp.path(), "")?;
    kmux(&repo, &config, &tmux)?
        .args(["workspace", "create", "--name", "review-alpha"])
        .assert()
        .success();
    let path = path_of(&workspace(&repo, &config, &tmux, "review-alpha")?)?;
    let head = commit_work(&path)?;
    git(&repo, &["update-ref", "refs/kmux", &head])?;
    let before = workspace(&repo, &config, &tmux, "review-alpha")?;
    let state = fs::read(repo.join(".git/kmux/state.json"))?;
    kmux(&repo, &config, &tmux)?
        .args(["workspace", "remove", "review-alpha"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("failed to create recovery ref"));
    assert!(path.is_dir());
    assert_eq!(workspace(&repo, &config, &tmux, "review-alpha")?, before);
    assert_eq!(fs::read(repo.join(".git/kmux/state.json"))?, state);
    assert_eq!(git_stdout(&repo, &["rev-parse", "refs/kmux"])?, head);
    Ok(())
}

#[cfg(unix)]
#[test]
fn failed_readback_verification_stops_removal_after_git_reports_ref_creation_success() -> Result<()>
{
    use std::os::unix::fs::PermissionsExt;
    let (temp, repo) = init_repo()?;
    let tmux = TmuxFixture::new(&repo)?.context("tmux fixture")?;
    let config = write_config(temp.path(), "")?;
    kmux(&repo, &config, &tmux)?
        .args(["workspace", "create", "--name", "review-alpha"])
        .assert()
        .success();
    let path = path_of(&workspace(&repo, &config, &tmux, "review-alpha")?)?;
    commit_work(&path)?;
    let hooks = temp.path().join("failure-hooks");
    fs::create_dir(&hooks)?;
    let hook = hooks.join("reference-transaction");
    // Remove only the new loose recovery ref in this test's owned repository.
    fs::write(
        &hook,
        "#!/bin/sh\n[ \"$1\" = committed ] || exit 0\nwhile read -r old new ref; do\n  case \"$ref\" in refs/kmux/recovery/*) rm -f \"$(git rev-parse --git-common-dir)/$ref\" ;; esac\ndone\n",
    )?;
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o700))?;
    git(
        &repo,
        &["config", "core.hooksPath", hooks.to_str().context("hooks")?],
    )?;
    let before = workspace(&repo, &config, &tmux, "review-alpha")?;
    let state = fs::read(repo.join(".git/kmux/state.json"))?;
    kmux(&repo, &config, &tmux)?
        .args(["workspace", "remove", "review-alpha"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("could not be verified"));
    assert_eq!(
        git_stdout(&repo, &["for-each-ref", "refs/kmux/recovery/"])?,
        ""
    );
    assert_eq!(workspace(&repo, &config, &tmux, "review-alpha")?, before);
    assert_eq!(fs::read(repo.join(".git/kmux/state.json"))?, state);
    assert!(path.join("committed.txt").is_file());
    Ok(())
}

#[cfg(unix)]
#[test]
fn checkout_change_during_recovery_stops_removal_and_reports_the_saved_ref() -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let (temp, repo) = init_repo()?;
    let tmux = TmuxFixture::new(&repo)?.context("tmux fixture")?;
    let config = write_config(temp.path(), "")?;
    kmux(&repo, &config, &tmux)?
        .args(["workspace", "create", "--name", "review-alpha"])
        .assert()
        .success();
    let before = workspace(&repo, &config, &tmux, "review-alpha")?;
    let path = path_of(&before)?;
    let saved_head = commit_work(&path)?;
    git(&path, &["commit", "--allow-empty", "-m", "later commit"])?;
    let later_head = git_stdout(&path, &["rev-parse", "HEAD"])?;
    git(&path, &["reset", "--hard", &saved_head])?;
    let git_dir = git_stdout(&path, &["rev-parse", "--absolute-git-dir"])?;
    let hooks = temp.path().join("changing-hooks");
    fs::create_dir(&hooks)?;
    let hook = hooks.join("reference-transaction");
    // Advance only this fixture's detached HEAD while the recovery transaction completes.
    fs::write(
        &hook,
        format!(
            "#!/bin/sh\n[ \"$1\" = committed ] || exit 0\nwhile read -r old new ref; do\n  case \"$ref\" in refs/kmux/recovery/*) git -c core.hooksPath=/dev/null --git-dir='{}' update-ref --no-deref HEAD {later_head} ;; esac\ndone\n",
            git_dir.replace('\'', "'\\''")
        ),
    )?;
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o700))?;
    git(
        &repo,
        &["config", "core.hooksPath", hooks.to_str().context("hooks")?],
    )?;
    let reference = format!(
        "refs/kmux/recovery/{}/{saved_head}",
        before["workspace_id"].as_str().context("id")?
    );
    let state = fs::read(repo.join(".git/kmux/state.json"))?;
    kmux(&repo, &config, &tmux)?
        .args(["workspace", "remove", "review-alpha"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "Git worktree changed during removal",
        ))
        .stderr(predicate::str::contains(&reference));
    assert_eq!(git_stdout(&repo, &["rev-parse", &reference])?, saved_head);
    assert_eq!(git_stdout(&path, &["rev-parse", "HEAD"])?, later_head);
    assert_eq!(fs::read(repo.join(".git/kmux/state.json"))?, state);
    let after = workspace(&repo, &config, &tmux, "review-alpha")?;
    assert_eq!(after["tmux_window_ids"], before["tmux_window_ids"]);
    assert_eq!(after["registered"], true);
    assert!(path.join("committed.txt").is_file());
    Ok(())
}
