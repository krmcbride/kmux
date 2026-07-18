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
    /// Create a git branch worktree and tmux window workspace.
    Add(AddArgs),
    /// Set the recorded parent branch for a workspace.
    Parent(ParentArgs),
    /// Restore tmux windows for existing workspaces.
    Restore,
    /// List known workspaces.
    #[command(visible_alias = "ls")]
    List(JsonArgs),
    /// Remove a workspace.
    #[command(visible_alias = "rm")]
    Remove(RemoveArgs),
    /// Show global agent workspace activity.
    Status(StatusArgs),
    /// Toggle the tmux sidebar.
    Sidebar(SidebarArgs),
    /// Generate shell completions.
    Completions {
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
    /// Output kmux workspace slugs for shell completion.
    #[command(name = "_complete-workspaces", hide = true)]
    CompleteWorkspaces,
    /// Output checkoutable git branch refs for shell completion.
    #[command(name = "_complete-add-branches", hide = true)]
    CompleteAddBranches,
    /// Output local git branch refs for shell completion.
    #[command(name = "_complete-git-branches", hide = true)]
    CompleteGitBranches,
    /// Record agent session state from an external integration.
    #[command(
        name = "set-agent-status",
        long_about = "Record agent session state from an external integration. This CLI command is the supported integration surface; persisted kmux state files are internal."
    )]
    SetAgentStatus(Box<SetAgentStatusArgs>),
}

#[derive(Debug, Args)]
pub struct AddArgs {
    /// Branch to create as a workspace.
    pub branch: String,

    /// Local parent branch for the new workspace.
    #[arg(long)]
    pub parent: Option<String>,

    /// Create the tmux window without switching to it.
    #[arg(short, long)]
    pub background: bool,
}

#[derive(Debug, Args)]
pub struct ParentArgs {
    /// Child workspace slug/branch, or parent branch when run inside a workspace.
    pub child_or_parent: String,

    /// Local parent branch for the child workspace.
    pub parent: Option<String>,
}

#[derive(Debug, Args)]
pub struct JsonArgs {
    /// Emit JSON instead of human-readable output.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct RemoveArgs {
    /// Workspace slug, branch, or window name. Defaults to the current kmux workspace.
    pub name: Option<String>,

    /// Remove even when safety checks would normally stop the command.
    #[arg(short, long)]
    pub force: bool,
}

#[derive(Debug, Args)]
pub struct StatusArgs {
    /// Emit JSON instead of human-readable output.
    #[arg(long)]
    pub json: bool,

    /// Include staged, unstaged, and unmerged git info.
    #[arg(long)]
    pub git: bool,
}

#[derive(Debug, Args)]
pub struct SetAgentStatusArgs {
    /// New kmux status. Must be one of the listed values; omit when updating only title, context, or target hints.
    #[arg(value_enum)]
    pub status: Option<AgentStatus>,

    /// Stable integration namespace for the agent implementation, such as opencode or codex.
    #[arg(long)]
    pub agent_kind: String,

    /// Stable integration-defined ID for one agent session; combined with --agent-kind to identify the session.
    #[arg(long)]
    pub session_id: String,

    /// Stable class of reporter contributing observations to this logical
    /// session, such as a server or pane-scoped adapter.
    #[arg(long)]
    pub reporter_kind: String,

    /// Stable ownership scope for one reporter within its class. Repeated
    /// updates replace, and `--delete` removes, only this reporter's observation.
    #[arg(long)]
    pub reporter_instance: String,

    /// Delete the observation identified by this session and reporter key.
    #[arg(long)]
    pub delete: bool,

    /// Delete all reporter observations for the session identified by --agent-kind and --session-id.
    #[arg(long)]
    pub delete_session: bool,

    /// Optional display title supplied by the integration; arbitrary text shown in status output.
    #[arg(long)]
    pub title: Option<String>,

    /// Optional compact display context supplied by the integration, such as usage or current activity.
    #[arg(long)]
    pub context: Option<String>,

    /// Optional tmux instance hint for reports that know which tmux server they observed.
    #[arg(long)]
    pub tmux_instance: Option<String>,

    /// Optional Git repository/project display hint; arbitrary text supplied by the integration.
    #[arg(long)]
    pub git_repo_name: Option<String>,

    /// Optional filesystem path to the main Git repository.
    #[arg(long)]
    pub git_repo_path: Option<String>,

    /// Optional Git branch name hint.
    #[arg(long)]
    pub git_branch: Option<String>,

    /// Primary current directory hint for attaching the agent session to a kmux workspace/window.
    #[arg(long)]
    pub directory: Option<String>,
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
    /// Run the interactive sidebar TUI.
    #[command(hide = true)]
    Run,
    /// Wake the sidebar TUI for a visible tmux window.
    #[command(hide = true)]
    Wake { window_id: String },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum AgentStatus {
    Working,
    Waiting,
    Done,
}
