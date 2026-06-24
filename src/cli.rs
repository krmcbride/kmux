use clap::{Args, Parser, Subcommand, ValueEnum};

#[derive(Debug, Parser)]
#[command(name = "kmux")]
#[command(version, about = "Lean tmux and git worktree helper")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Create a git worktree and tmux window.
    Add(AddArgs),
    /// Open a tmux window for an existing worktree.
    Open(NameArgs),
    /// Close a worktree's tmux window without removing the worktree.
    Close(NameArgs),
    /// List known worktrees.
    #[command(visible_alias = "ls")]
    List(JsonArgs),
    /// Print the filesystem path for a worktree.
    Path(NameArgs),
    /// Remove a worktree and its tmux window.
    #[command(visible_alias = "rm")]
    Remove(RemoveArgs),
    /// Rename a worktree handle and tmux window.
    Rename(RenameArgs),
    /// Show tracked external tool status.
    Status(StatusArgs),
    /// Toggle the tmux sidebar.
    Sidebar(SidebarArgs),
    /// Generate shell completions.
    Completions {
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
    /// Output kmux worktree handles for shell completion.
    #[command(name = "_complete-handles", hide = true)]
    CompleteHandles,
    /// Output checkoutable git branch refs for shell completion.
    #[command(name = "_complete-add-branches", hide = true)]
    CompleteAddBranches,
    /// Output git branch refs for shell completion.
    #[command(name = "_complete-git-branches", hide = true)]
    CompleteGitBranches,
    /// Update the current tmux window status from an external integration.
    #[command(name = "set-window-status", hide = true)]
    SetWindowStatus { status: AgentStatus },
}

#[derive(Debug, Args)]
pub struct AddArgs {
    /// Branch to create or open as a worktree.
    pub branch: String,

    /// Base branch, tag, or commit for a new branch.
    #[arg(long)]
    pub base: Option<String>,

    /// Override the worktree handle and tmux window name.
    #[arg(long)]
    pub name: Option<String>,

    /// Create the tmux window without switching to it.
    #[arg(short, long)]
    pub background: bool,

    /// Open an existing worktree/window instead of failing.
    #[arg(short = 'o', long)]
    pub open_if_exists: bool,
}

#[derive(Debug, Args)]
pub struct NameArgs {
    /// Worktree handle, branch, or window name.
    pub name: String,
}

#[derive(Debug, Args)]
pub struct JsonArgs {
    /// Emit JSON instead of human-readable output.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct RemoveArgs {
    /// Worktree handle, branch, or window name. Defaults to the current kmux worktree.
    pub name: Option<String>,

    /// Remove even when safety checks would normally stop the command.
    #[arg(short, long)]
    pub force: bool,

    /// Keep the git branch after removing the worktree.
    #[arg(long)]
    pub keep_branch: bool,
}

#[derive(Debug, Args)]
pub struct RenameArgs {
    /// Existing worktree handle, branch, or window name.
    pub old: String,

    /// New worktree handle and tmux window name.
    pub new: String,
}

#[derive(Debug, Args)]
pub struct StatusArgs {
    /// Optional worktree, branch, or handle filters.
    pub filters: Vec<String>,

    /// Emit JSON instead of human-readable output.
    #[arg(long)]
    pub json: bool,

    /// Include staged, unstaged, and unmerged git info.
    #[arg(long)]
    pub git: bool,
}

#[derive(Debug, Args)]
pub struct SidebarArgs {
    #[command(subcommand)]
    pub command: Option<SidebarCommand>,
}

#[derive(Debug, Subcommand)]
pub enum SidebarCommand {
    /// Enable sidebar panes in all tmux windows.
    On,
    /// Disable sidebar panes and remove hooks.
    Off,
    /// Reconcile sidebar panes after tmux window/session changes.
    Refresh,
    /// Render sidebar contents once.
    #[command(hide = true)]
    Render,
    /// Run the interactive sidebar TUI.
    #[command(hide = true)]
    Run,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum AgentStatus {
    Working,
    Waiting,
    Done,
    Clear,
}
