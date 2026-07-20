use anyhow::{Context, Result};

use crate::config::Config;
use crate::git::Git;
use crate::paths::RepoPaths;

/// Common repo objects shared by workspace workflows.
pub(super) struct RepoContext {
    pub(super) config: Config,
    pub(super) paths: RepoPaths,
    pub(super) git: Git,
}

/// Load repo paths, config, and a Git adapter rooted at the main worktree.
pub(super) fn load_repo_context() -> Result<RepoContext> {
    let cwd = std::env::current_dir().context("failed to read current directory")?;
    let paths = RepoPaths::discover(&cwd)?;
    let config = Config::load()?;
    let git = Git::new(&paths.main_worktree);

    Ok(RepoContext { config, paths, git })
}
