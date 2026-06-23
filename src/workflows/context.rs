use anyhow::{Context, Result, anyhow};

use crate::config::Config;
use crate::git::Git;
use crate::paths::RepoPaths;
use crate::tmux::Tmux;

pub(super) struct RepoContext {
    pub(super) config: Config,
    pub(super) paths: RepoPaths,
    pub(super) git: Git,
}

pub(super) struct TmuxContext {
    pub(super) tmux: Tmux,
    pub(super) session_name: String,
}

pub(super) fn load_repo_context() -> Result<RepoContext> {
    let cwd = std::env::current_dir().context("failed to read current directory")?;
    let paths = RepoPaths::discover(&cwd)?;
    let config = Config::load()?;
    let git = Git::new(&paths.main_worktree);

    Ok(RepoContext { config, paths, git })
}

pub(super) fn load_tmux_context() -> Result<TmuxContext> {
    let tmux = Tmux::from_env();
    let context = tmux.current_context()?.ok_or_else(|| {
        anyhow!("tmux session could not be determined; run this command from inside tmux")
    })?;

    Ok(TmuxContext {
        tmux,
        session_name: context.session_name,
    })
}
