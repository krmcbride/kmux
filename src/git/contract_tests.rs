//! Process-backed Git adapter contracts run through an isolated repository.

use std::fs;

use anyhow::{Result, bail};

use super::BranchAction;
use super::contract_support::GitRepoFixture;

pub fn recovery_refs_are_verified_idempotent_and_never_overwrite_collisions() -> Result<()> {
    let fixture = GitRepoFixture::new()?;
    let git = fixture.adapter();
    let old = git.resolve_commit("HEAD")?;
    fixture.commit_file("feature.txt", "feature\n", "advance HEAD")?;
    let head = git.resolve_commit("HEAD")?;
    let branches = git.local_branch_refs()?;
    let base = format!("refs/kmux/recovery/ws-example/{head}");
    fixture.git(&["update-ref", &base, &old])?;
    let recovery = git.create_recovery_ref("ws-example", &head)?;
    assert_eq!(recovery, format!("{base}-1"));
    assert_eq!(git.resolve_commit(&base)?, old);
    assert_eq!(git.resolve_commit(&recovery)?, head);
    assert_eq!(git.create_recovery_ref("ws-example", &head)?, recovery);
    assert_eq!(git.local_branch_refs()?, branches);

    let symbolic = format!("refs/kmux/recovery/ws-symbolic/{head}");
    fixture.git(&["symbolic-ref", &symbolic, "refs/heads/main"])?;
    assert_eq!(
        git.create_recovery_ref("ws-symbolic", &head)?,
        format!("{symbolic}-1")
    );
    git.verify_preserved_branch("main", &head)?;
    assert!(git.verify_preserved_branch("main", &old).is_err());
    Ok(())
}

pub fn recovery_ref_creation_fails_without_replacing_an_occupied_namespace() -> Result<()> {
    let fixture = GitRepoFixture::new()?;
    let git = fixture.adapter();
    let head = git.resolve_commit("HEAD")?;
    fixture.git(&["update-ref", "refs/kmux", &head])?;
    assert!(git.create_recovery_ref("ws-example", &head).is_err());
    assert_eq!(git.resolve_commit("refs/kmux")?, head);
    assert_eq!(git.local_branch_refs()?, ["main"]);
    Ok(())
}

pub fn inventory_preserves_unusual_paths_and_registration_binding() -> Result<()> {
    let fixture = GitRepoFixture::new()?;
    let path = fixture.root().join("workspace \"alpha\"\n ");
    let git = fixture.adapter();
    fixture.git(&[
        "worktree",
        "add",
        "--detach",
        path.to_string_lossy().as_ref(),
        "HEAD",
    ])?;
    fixture.git(&[
        "worktree",
        "lock",
        "--reason",
        "review\nnotes ",
        path.to_string_lossy().as_ref(),
    ])?;
    let entries = git.worktrees()?;
    let entry = entries
        .iter()
        .find(|e| e.path == path)
        .ok_or_else(|| anyhow::anyhow!("missing exact worktree path"))?;
    assert!(entry.detached);
    assert!(entry.branch.is_none());
    assert!(entry.head.is_some());
    assert_eq!(entry.locked.as_deref(), Some("review\nnotes"));
    assert_eq!(fixture.adapter_at(&path).worktree_root()?, path);
    assert!(entry.kmux_binding.is_none());
    let id = git.claim_worktree(&path)?;
    assert_eq!(git.claim_worktree(&path)?, id);
    assert_eq!(
        git.worktrees()?
            .iter()
            .find(|e| e.path == path)
            .and_then(|e| e.kmux_binding.as_deref()),
        Some(id.as_str())
    );
    fixture.git(&["worktree", "unlock", path.to_string_lossy().as_ref()])?;
    git.remove_worktree(&path, false)?;
    fixture.git(&[
        "worktree",
        "add",
        "--detach",
        path.to_string_lossy().as_ref(),
        "HEAD",
    ])?;
    assert!(
        git.worktrees()?
            .iter()
            .find(|e| e.path == path)
            .is_some_and(|e| e.kmux_binding.is_none())
    );
    fs::remove_dir_all(&path)?;
    assert!(
        git.worktrees()?
            .iter()
            .find(|e| e.path == path)
            .is_some_and(|e| e.prunable.is_some())
    );
    Ok(())
}

pub fn discovers_repo_info_from_primary_worktree() -> Result<()> {
    let fixture = GitRepoFixture::new()?;
    let repo = fixture.path();
    let info = fixture.adapter().repo_info()?;

    assert_eq!(info.current_worktree, repo.canonicalize()?);
    assert_eq!(info.git_common_dir, repo.join(".git").canonicalize()?);
    Ok(())
}

pub fn discovers_repo_info_from_linked_worktree() -> Result<()> {
    let fixture = GitRepoFixture::new()?;
    let repo = fixture.path();
    let worktree_base = fixture.root().join("project-alpha__worktrees");
    let linked = worktree_base.join("feature-auth");
    fs::create_dir(&worktree_base)?;
    fixture.git(&[
        "worktree",
        "add",
        "-b",
        "feature/auth",
        linked.to_string_lossy().as_ref(),
    ])?;

    let info = fixture.adapter_at(&linked).repo_info()?;

    assert_eq!(info.current_worktree, linked.canonicalize()?);
    assert_eq!(info.git_common_dir, repo.join(".git").canonicalize()?);
    Ok(())
}

pub fn detects_current_branch_and_detached_head() -> Result<()> {
    let fixture = GitRepoFixture::new()?;
    let git_repo = fixture.adapter();

    assert_eq!(git_repo.current_branch()?.as_deref(), Some("main"));
    let head = git_repo.stdout(["rev-parse", "HEAD"])?;
    fixture.git(&["checkout", "--detach", &head])?;

    assert_eq!(git_repo.current_branch()?, None);
    let error = match git_repo.require_current_branch() {
        Ok(branch) => bail!("detached HEAD unexpectedly resolved branch {branch:?}"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("detached HEAD"));
    Ok(())
}

pub fn creates_branch_from_current_branch_and_reuses_without_moving() -> Result<()> {
    let fixture = GitRepoFixture::new()?;
    let git_repo = fixture.adapter();

    assert_eq!(
        git_repo.ensure_local_branch("feature/auth", None)?,
        BranchAction::Created
    );
    let feature_rev = git_repo.stdout(["rev-parse", "feature/auth"])?;
    fixture.commit_file("after.txt", "after\n", "after feature branch")?;
    let main_rev = git_repo.stdout(["rev-parse", "main"])?;
    assert_ne!(feature_rev, main_rev);
    assert_eq!(
        git_repo.ensure_local_branch("feature/auth", None)?,
        BranchAction::Existing
    );
    assert_eq!(git_repo.stdout(["rev-parse", "feature/auth"])?, feature_rev);
    Ok(())
}

pub fn creates_branch_from_explicit_start_point() -> Result<()> {
    let fixture = GitRepoFixture::new()?;
    let git_repo = fixture.adapter();
    let initial_rev = git_repo.stdout(["rev-parse", "main"])?;
    fixture.commit_file("later.txt", "later\n", "later")?;

    assert_eq!(
        git_repo.ensure_local_branch("from-initial", Some(&initial_rev))?,
        BranchAction::Created
    );
    assert_eq!(git_repo.stdout(["rev-parse", "from-initial"])?, initial_rev);
    assert!(!git_repo.commit_ref_exists("missing-ref")?);
    Ok(())
}

pub fn returns_merge_base_when_branches_share_history() -> Result<()> {
    let fixture = GitRepoFixture::new()?;
    let git_repo = fixture.adapter();
    let initial_rev = git_repo.stdout(["rev-parse", "main"])?;
    git_repo.ensure_local_branch("feature/auth", Some("main"))?;
    fixture.commit_file("later.txt", "later\n", "later")?;

    assert_eq!(
        git_repo.merge_base("feature/auth", "main")?.as_deref(),
        Some(initial_rev.as_str())
    );
    Ok(())
}

pub fn returns_none_when_branches_have_no_merge_base() -> Result<()> {
    let fixture = GitRepoFixture::new()?;
    let repo = fixture.path();
    let git_repo = fixture.adapter();
    git_repo.ensure_local_branch("feature/auth", Some("main"))?;
    fixture.git(&["checkout", "--orphan", "orphan-parent"])?;
    fs::remove_file(repo.join("README.md"))?;
    fixture.commit_file("orphan.txt", "orphan\n", "orphan")?;
    fixture.git(&["checkout", "main"])?;

    assert_eq!(git_repo.merge_base("feature/auth", "orphan-parent")?, None);
    Ok(())
}

pub fn safe_deletion_prefers_configured_upstream_over_local_head() -> Result<()> {
    let fixture = GitRepoFixture::new()?;
    let remote = fixture.root().join("remote.git");
    let remote = remote.to_string_lossy();
    fixture.git(&["init", "--bare", remote.as_ref()])?;
    fixture.git(&["remote", "add", "origin", remote.as_ref()])?;
    fixture.git(&["push", "-u", "origin", "main"])?;
    fixture.git(&["checkout", "-b", "feature/safety"])?;
    fixture.commit_file("feature.txt", "feature\n", "feature change")?;
    fixture.git(&[
        "branch",
        "--set-upstream-to",
        "origin/main",
        "feature/safety",
    ])?;
    fixture.git(&["checkout", "main"])?;
    fixture.git(&[
        "merge",
        "--no-ff",
        "feature/safety",
        "-m",
        "merge feature locally",
    ])?;

    assert!(
        !fixture
            .adapter()
            .branch_is_safely_deletable("feature/safety")?
    );
    Ok(())
}

pub fn adds_and_finds_worktree_by_branch() -> Result<()> {
    let fixture = GitRepoFixture::new()?;
    let git_repo = fixture.adapter();
    let worktree_base = fixture.root().join("project-alpha__worktrees");
    let linked = worktree_base.join("feature-auth");
    fs::create_dir(&worktree_base)?;

    git_repo.ensure_local_branch("feature/auth", None)?;
    git_repo.add_worktree(&linked, "feature/auth")?;

    assert_eq!(
        git_repo
            .find_worktree_by_branch("feature/auth")?
            .map(|worktree| worktree.path),
        Some(linked)
    );
    Ok(())
}

pub fn remove_worktree_guards_dirty_paths_unless_forced() -> Result<()> {
    let fixture = GitRepoFixture::new()?;
    let git_repo = fixture.adapter();
    let worktree_base = fixture.root().join("project-alpha__worktrees");
    let linked = worktree_base.join("feature-auth");
    fs::create_dir(&worktree_base)?;
    git_repo.ensure_local_branch("feature/auth", None)?;
    git_repo.add_worktree(&linked, "feature/auth")?;
    fs::write(linked.join("untracked.txt"), "dirty\n")?;

    assert!(git_repo.worktree_is_dirty(&linked)?);
    let error = match git_repo.remove_worktree(&linked, false) {
        Ok(()) => bail!("dirty worktree removal unexpectedly succeeded"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("uncommitted changes"));

    git_repo.remove_worktree(&linked, true)?;

    assert!(!linked.exists());
    Ok(())
}
