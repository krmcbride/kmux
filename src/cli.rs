use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

#[derive(Debug, Parser)]
#[command(name = "kmux")]
#[command(
    version,
    about = "Lean tmux and git worktree helper",
    disable_help_subcommand = true
)]
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
    /// Manage the tmux sidebar.
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
    /// Output configured launcher names for shell completion.
    #[command(name = "_complete-launchers", hide = true)]
    CompleteLaunchers,
    /// Consume one private transient launcher request inside a workspace pane.
    #[command(name = "_launch", hide = true)]
    Launch(LaunchArgs),
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

    /// Use a named launcher for this initial window only.
    #[arg(long)]
    pub launch: Option<String>,

    /// Append opaque input to the explicit launcher's argv; '-' reads caller stdin.
    #[arg(long, requires = "launch", allow_hyphen_values = true)]
    pub input: Option<String>,
}

#[derive(Debug, Args)]
pub struct LaunchArgs {
    /// Opaque path to a private, one-shot launcher request.
    pub request: PathBuf,
}

#[derive(Debug, Args)]
pub struct ParentArgs {
    /// Local parent branch for the child workspace.
    pub parent: String,

    /// Child workspace slug/branch. Defaults to the current kmux workspace.
    pub child: Option<String>,
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
    pub command: SidebarCommand,
}

#[derive(Debug, Subcommand)]
pub enum SidebarCommand {
    /// Enable sidebar panes in all tmux windows.
    On,
    /// Disable sidebar panes and remove hooks.
    Off,
    /// Toggle sidebar panes on or off.
    Toggle,
    /// Reconcile sidebar panes after tmux window/session changes.
    #[command(name = "_refresh", hide = true)]
    Refresh,
    /// Run the interactive sidebar TUI.
    #[command(name = "_run", hide = true)]
    Run,
    /// Wake the sidebar TUI for a visible tmux window.
    #[command(name = "_wake", hide = true)]
    Wake { window_id: String },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum AgentStatus {
    Working,
    Waiting,
    Done,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_add(arguments: &[&str]) -> AddArgs {
        let cli = Cli::try_parse_from(["kmux", "add"].into_iter().chain(arguments.iter().copied()))
            .expect("add arguments should parse");
        match cli.command {
            Command::Add(args) => Some(args),
            _ => None,
        }
        .expect("expected add command")
    }

    #[test]
    fn add_parses_launcher_and_literal_input_shapes() {
        for (arguments, expected) in [
            (
                ["feature/sidebar", "--launch", "agent", "--input", "prompt"].as_slice(),
                "prompt",
            ),
            (
                [
                    "feature/sidebar",
                    "--launch",
                    "agent",
                    "--input",
                    "--leading-dash",
                ]
                .as_slice(),
                "--leading-dash",
            ),
            (
                [
                    "feature/sidebar",
                    "--launch",
                    "agent",
                    "--input=--leading-dash",
                ]
                .as_slice(),
                "--leading-dash",
            ),
            (
                ["feature/sidebar", "--launch", "agent", "--input", "-"].as_slice(),
                "-",
            ),
            (
                ["feature/sidebar", "--launch", "agent", "--input="].as_slice(),
                "",
            ),
        ] {
            let args = parse_add(arguments);
            assert_eq!(args.launch.as_deref(), Some("agent"));
            assert_eq!(args.input.as_deref(), Some(expected));
        }
    }

    #[test]
    fn add_distinguishes_absent_input_and_requires_explicit_launcher() {
        let args = parse_add(&["feature/sidebar", "--launch", "agent"]);
        assert_eq!(args.launch.as_deref(), Some("agent"));
        assert!(args.input.is_none());

        let error = Cli::try_parse_from(["kmux", "add", "feature/sidebar", "--input", "prompt"])
            .expect_err("input without launch should fail parsing");
        assert!(error.to_string().contains("--launch"));
    }

    #[test]
    fn lifecycle_commands_reject_removed_tmux_session_selector() {
        for arguments in [
            vec![
                "kmux",
                "add",
                "feature/sidebar",
                "--tmux-session",
                "project-alpha",
            ],
            vec!["kmux", "restore", "--tmux-session", "project-alpha"],
            vec![
                "kmux",
                "remove",
                "feature/sidebar",
                "--tmux-session",
                "project-alpha",
            ],
        ] {
            let error = Cli::try_parse_from(arguments)
                .expect_err("project-session ambiguity is no longer a CLI choice");
            assert!(error.to_string().contains("--tmux-session"));
        }

        let restore =
            Cli::try_parse_from(["kmux", "restore"]).expect("argument-free restore should parse");
        assert!(matches!(restore.command, Command::Restore));
    }

    #[test]
    fn hidden_launcher_ingress_parses_only_its_request_path() {
        let cli = Cli::try_parse_from(["kmux", "_launch", "/tmp/request"])
            .expect("hidden ingress should parse");
        let args = match cli.command {
            Command::Launch(args) => Some(args),
            _ => None,
        }
        .expect("expected launcher ingress");
        assert_eq!(args.request, PathBuf::from("/tmp/request"));

        Cli::try_parse_from(["kmux", "_launch", "/tmp/request", "extra"])
            .expect_err("hidden ingress should reject extra arguments");
    }
}
