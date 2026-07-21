use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum, ValueHint};

#[derive(Debug, Parser)]
#[command(name = "kmux")]
#[command(
    version,
    about = "Manage Git worktree workspaces backed by tmux windows",
    long_about = ROOT_LONG_ABOUT,
    after_long_help = ROOT_AFTER_LONG_HELP,
    max_term_width = 80,
    disable_help_subcommand = true
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Manage workspaces in the current Git project.
    #[command(
        long_about = WORKSPACE_LONG_ABOUT,
        after_long_help = WORKSPACE_AFTER_LONG_HELP
    )]
    Workspace(WorkspaceArgs),
    /// Show the resolved configuration.
    #[command(
        long_about = CONFIG_LONG_ABOUT,
        after_long_help = CONFIG_AFTER_LONG_HELP
    )]
    Config(ConfigArgs),
    /// Show global agent workspace activity.
    #[command(
        long_about = STATUS_LONG_ABOUT,
        after_long_help = STATUS_AFTER_LONG_HELP
    )]
    Status(StatusArgs),
    /// Manage the global sidebar for the current tmux server.
    #[command(
        long_about = SIDEBAR_LONG_ABOUT,
        after_long_help = SIDEBAR_AFTER_LONG_HELP
    )]
    Sidebar(SidebarArgs),
    /// Generate a shell completion script on standard output.
    #[command(
        long_about = COMPLETIONS_LONG_ABOUT,
        after_long_help = COMPLETIONS_AFTER_LONG_HELP
    )]
    Completions {
        /// Shell whose completion script should be generated.
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
    /// Output kmux workspace slugs for shell completion.
    #[command(name = "_complete-workspaces", hide = true)]
    CompleteWorkspaces,
    /// Output checkoutable git branch refs for shell completion.
    #[command(name = "_complete-create-branches", hide = true)]
    CompleteCreateBranches,
    /// Output local git branch refs for shell completion.
    #[command(name = "_complete-git-branches", hide = true)]
    CompleteGitBranches,
    /// Output configured launcher names for shell completion.
    #[command(name = "_complete-launchers", hide = true)]
    CompleteLaunchers,
    /// Consume one private transient launcher request inside a workspace pane.
    #[command(name = "_launch", hide = true)]
    Launch(LaunchArgs),
    /// Manage agent session state from an external integration.
    #[command(
        name = "set-agent-status",
        override_usage = SET_AGENT_STATUS_USAGE,
        long_about = SET_AGENT_STATUS_LONG_ABOUT,
        after_long_help = SET_AGENT_STATUS_AFTER_LONG_HELP
    )]
    SetAgentStatus(Box<SetAgentStatusArgs>),
}

#[derive(Debug, Args)]
pub struct WorkspaceArgs {
    #[command(subcommand)]
    pub command: WorkspaceCommand,
}

#[derive(Debug, Subcommand)]
pub enum WorkspaceCommand {
    /// Create a new workspace.
    #[command(
        long_about = CREATE_LONG_ABOUT,
        after_long_help = CREATE_AFTER_LONG_HELP
    )]
    Create(CreateArgs),
    /// List workspaces in the current Git project.
    #[command(long_about = LIST_LONG_ABOUT, after_long_help = LIST_AFTER_LONG_HELP)]
    List(ListArgs),
    /// Remove a workspace and its local branch.
    #[command(
        long_about = REMOVE_LONG_ABOUT,
        after_long_help = REMOVE_AFTER_LONG_HELP
    )]
    Remove(RemoveArgs),
    /// Set the recorded parent branch for a workspace.
    #[command(
        name = "set-parent",
        long_about = SET_PARENT_LONG_ABOUT,
        after_long_help = SET_PARENT_AFTER_LONG_HELP
    )]
    SetParent(SetParentArgs),
    /// Restore missing tmux windows for existing workspaces.
    #[command(
        long_about = RESTORE_LONG_ABOUT,
        after_long_help = RESTORE_AFTER_LONG_HELP
    )]
    Restore,
}

#[derive(Debug, Args)]
pub struct CreateArgs {
    /// New local branch, or REMOTE/BRANCH to track locally.
    #[arg(long_help = CREATE_BRANCH_LONG_HELP, value_hint = ValueHint::Other)]
    pub branch: String,

    /// Start from and record this local parent branch.
    #[arg(
        long,
        long_help = CREATE_PARENT_LONG_HELP,
        value_hint = ValueHint::Other
    )]
    pub parent: Option<String>,

    /// Create the tmux window without selecting it; required outside the target session.
    #[arg(short, long, long_help = CREATE_BACKGROUND_LONG_HELP)]
    pub background: bool,

    /// Start this configured launcher instead of the default.
    #[arg(
        long,
        long_help = CREATE_LAUNCHER_LONG_HELP,
        value_hint = ValueHint::Other
    )]
    pub launcher: Option<String>,

    /// Pass one final argument to the launcher; '-' reads it from stdin.
    #[arg(
        long,
        requires = "launcher",
        allow_hyphen_values = true,
        long_help = CREATE_LAUNCHER_INPUT_LONG_HELP,
        value_name = "INPUT",
        value_hint = ValueHint::Other
    )]
    pub launcher_input: Option<String>,
}

#[derive(Debug, Args)]
pub struct LaunchArgs {
    /// Opaque path to a private, one-shot launcher request.
    pub request: PathBuf,
}

#[derive(Debug, Args)]
pub struct SetParentArgs {
    /// Existing local branch to record as the workspace parent.
    #[arg(value_hint = ValueHint::Other)]
    pub parent: String,

    /// Child workspace slug or branch; omit to use the current kmux workspace.
    #[arg(value_hint = ValueHint::Other)]
    pub child: Option<String>,
}

#[derive(Debug, Args)]
pub struct ListArgs {
    /// Print machine-readable JSON instead of a table.
    #[arg(long, long_help = LIST_JSON_LONG_HELP)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct ConfigArgs {
    /// Emit JSON instead of YAML.
    #[arg(long, long_help = CONFIG_JSON_LONG_HELP)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct RemoveArgs {
    /// Workspace slug, branch, or window name; omit to use the current workspace.
    #[arg(value_hint = ValueHint::Other)]
    pub name: Option<String>,

    /// Allow removal with uncommitted or unmerged work.
    #[arg(short, long, long_help = REMOVE_FORCE_LONG_HELP)]
    pub force: bool,
}

#[derive(Debug, Args)]
pub struct StatusArgs {
    /// Print machine-readable JSON instead of a table.
    #[arg(long, long_help = STATUS_JSON_LONG_HELP)]
    pub json: bool,

    /// Inspect workspace repositories for staged, unstaged, and unmerged Git state.
    #[arg(long, long_help = STATUS_GIT_LONG_HELP)]
    pub git: bool,
}

#[derive(Debug, Args)]
pub struct SetAgentStatusArgs {
    /// New activity state; omit to update metadata without changing prior status timing.
    #[arg(value_enum, long_help = AGENT_STATUS_LONG_HELP)]
    pub status: Option<AgentStatus>,

    /// Stable integration namespace for the agent implementation, such as opencode.
    #[arg(long)]
    pub agent_kind: String,

    /// Stable integration-defined session ID, scoped by --agent-kind.
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

    /// Delete only the observation identified by this session and reporter key.
    #[arg(long, long_help = DELETE_OBSERVATION_LONG_HELP)]
    pub delete: bool,

    /// Delete all reporter observations for this agent session.
    #[arg(long, long_help = DELETE_SESSION_LONG_HELP)]
    pub delete_session: bool,

    /// Display title supplied by the integration and shown in activity output.
    #[arg(long)]
    pub title: Option<String>,

    /// Compact display context, such as usage or current activity.
    #[arg(long)]
    pub context: Option<String>,

    /// Tmux instance hint for integrations that know which server they observed.
    #[arg(long)]
    pub tmux_instance: Option<String>,

    /// Git project display hint supplied by the integration.
    #[arg(long)]
    pub git_repo_name: Option<String>,

    /// Filesystem path to the main Git repository.
    #[arg(long)]
    pub git_repo_path: Option<String>,

    /// Git branch name hint.
    #[arg(long)]
    pub git_branch: Option<String>,

    /// Replacement attachment directory for an upsert; omission clears the prior value.
    #[arg(long, long_help = DIRECTORY_LONG_HELP)]
    pub directory: Option<String>,
}

#[derive(Debug, Args)]
pub struct SidebarArgs {
    #[command(subcommand)]
    pub command: SidebarCommand,
}

#[derive(Debug, Subcommand)]
pub enum SidebarCommand {
    /// Enable or reconcile sidebar panes in all tmux windows.
    #[command(long_about = SIDEBAR_ON_LONG_ABOUT)]
    On,
    /// Disable sidebar panes and remove hooks.
    #[command(long_about = SIDEBAR_OFF_LONG_ABOUT)]
    Off,
    /// Toggle sidebar panes on or off.
    #[command(long_about = SIDEBAR_TOGGLE_LONG_ABOUT)]
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
    /// The agent is actively processing work.
    Working,
    /// The agent is blocked on user input or another external response.
    Waiting,
    /// The agent has finished its current work.
    Done,
}

const ROOT_LONG_ABOUT: &str = "Kmux helps manage parallel agentic development by providing opinionated glue for tmux and git primitives.";

const ROOT_AFTER_LONG_HELP: &str = concat!(
    "Start with 'kmux workspace --help' to manage workspaces.\n",
    "Run 'kmux <COMMAND> --help' for other workflows.\n",
    "Configuration: ${XDG_CONFIG_HOME:-$HOME/.config}/kmux/config.yaml"
);

const WORKSPACE_LONG_ABOUT: &str = concat!(
    "Manage workspaces in the current Git project. Each workspace combines a local branch, linked worktree, and expected tmux window, with optional parent metadata.\n\n",
    "Run these commands from the Git project you want to manage."
);
const WORKSPACE_AFTER_LONG_HELP: &str = concat!(
    "Start with 'kmux workspace create --help' to create a workspace.\n",
    "Use 'kmux workspace list' to inspect existing workspaces."
);

const CREATE_LONG_ABOUT: &str = concat!(
    "Create BRANCH as a local branch, linked worktree, and tmux window. Run this from the Git project you want the workspace to belong to.\n\n",
    "Kmux runs configured workspace setup, then starts the default launcher when one is configured. Use --launcher to select another configured launcher, --launcher-input - to pass it multiline input on stdin, and --background when the caller is not attached to the target tmux session.\n\n",
    "Creation is not rolled back after it begins. Launcher success means the process started; kmux does not wait for its work to finish."
);
const CREATE_AFTER_LONG_HELP: &str = concat!(
    "Examples:\n",
    "  kmux workspace create feature/sidebar\n",
    "  kmux workspace create feature/review \\\n",
    "    --background --launcher review-agent --launcher-input -"
);
const CREATE_BRANCH_LONG_HELP: &str = concat!(
    "Create this new local branch from --parent or the current branch. ",
    "A known REMOTE/BRANCH instead creates and tracks the corresponding local branch."
);
const CREATE_PARENT_LONG_HELP: &str = concat!(
    "Use this local branch as the new branch's start point and recorded parent. ",
    "For REMOTE/BRANCH, it changes only the recorded parent."
);
const CREATE_BACKGROUND_LONG_HELP: &str = "Create the tmux window without selecting it. Required when the caller is not attached to the target tmux session.";
const CREATE_LAUNCHER_LONG_HELP: &str = concat!(
    "Start this configured launcher in the new workspace instead of the default. ",
    "Run 'kmux config' to discover launcher names and descriptions."
);
const CREATE_LAUNCHER_INPUT_LONG_HELP: &str = concat!(
    "Pass one literal final argument to the explicitly selected launcher. ",
    "A value of '-' reads all stdin without trimming. Do not pass secrets."
);

const CONFIG_LONG_ABOUT: &str = concat!(
    "Show the active configuration with defaults resolved, including configured launcher names, descriptions, commands, and workspace setup.\n\n",
    "Output is YAML by default. Use --json for machine-readable discovery."
);
const CONFIG_AFTER_LONG_HELP: &str = "Examples:\n  kmux config\n  kmux config --json";
const CONFIG_JSON_LONG_HELP: &str = "Print the resolved configuration as JSON instead of YAML.";

const SET_PARENT_LONG_ABOUT: &str = concat!(
    "Record PARENT as the logical parent of CHILD. PARENT must be an existing local branch with shared history.\n\n",
    "CHILD may be a workspace slug or branch. Omit it inside a kmux worktree to update the current workspace. This changes only kmux parent metadata."
);
const SET_PARENT_AFTER_LONG_HELP: &str = concat!(
    "Examples:\n",
    "  kmux workspace set-parent main\n",
    "  kmux workspace set-parent main feature/sidebar"
);

const RESTORE_LONG_ABOUT: &str = "Recreate missing tmux windows for workspaces in the current Git project. Existing windows are left unchanged. New windows use the currently configured default launcher.";
const RESTORE_AFTER_LONG_HELP: &str = "Example:\n  kmux workspace restore";

const LIST_LONG_ABOUT: &str = "List workspaces and their parent, Git, tmux, and agent context for the current Git project. This command does not change workspace state.";
const LIST_AFTER_LONG_HELP: &str = "Examples:\n  kmux workspace list\n  kmux workspace list --json";
const LIST_JSON_LONG_HELP: &str = "Print the current project's workspace inventory as JSON.";

const REMOVE_LONG_ABOUT: &str = concat!(
    "Remove a workspace's linked worktree, local branch, tmux window, and parent metadata. NAME may be its slug, branch, or window name; omit it inside a kmux worktree to remove the current workspace.\n\n",
    "Kmux refuses the main worktree, dirty worktrees, unmerged branches, and worktrees used by other panes. Use --force only to bypass the dirty and unmerged checks."
);
const REMOVE_AFTER_LONG_HELP: &str = concat!(
    "Examples:\n",
    "  kmux workspace remove feature/sidebar\n",
    "  kmux workspace remove --force feature/abandoned"
);
const REMOVE_FORCE_LONG_HELP: &str = concat!(
    "Allow removal despite uncommitted or unmerged work, potentially discarding it. ",
    "Other safety checks still apply."
);

const STATUS_LONG_ABOUT: &str =
    "Show agent activity across all observed Git projects, combined with live tmux state.";
const STATUS_AFTER_LONG_HELP: &str =
    "Examples:\n  kmux status\n  kmux status --git\n  kmux status --json";
const STATUS_JSON_LONG_HELP: &str = "Print global activity records as JSON instead of a table.";
const STATUS_GIT_LONG_HELP: &str =
    "Include best-effort staged, unstaged, and unmerged Git state for each workspace.";

const SIDEBAR_LONG_ABOUT: &str = concat!(
    "Manage the agent-activity sidebar for the current tmux server. Sidebar state is global to the server, not scoped to a Git project.\n\n",
    "Use 'on' or 'off' when the desired final state is known. Running 'on' again reconciles existing sidebar panes."
);
const SIDEBAR_AFTER_LONG_HELP: &str = concat!(
    "Examples:\n",
    "  kmux sidebar on\n",
    "  kmux sidebar off\n",
    "  kmux sidebar toggle"
);
const SIDEBAR_ON_LONG_ABOUT: &str = "Enable or reconcile the sidebar in the current tmux server.";
const SIDEBAR_OFF_LONG_ABOUT: &str =
    "Disable the sidebar and remove its panes and hooks from the current tmux server.";
const SIDEBAR_TOGGLE_LONG_ABOUT: &str = "Toggle the sidebar according to its current tmux state.";

const COMPLETIONS_LONG_ABOUT: &str = "Generate a completion script for SHELL on standard output. This command does not install or source the script.";
const COMPLETIONS_AFTER_LONG_HELP: &str = "Example:\n  kmux completions bash > kmux.bash";

const SET_AGENT_STATUS_LONG_ABOUT: &str = concat!(
    "Create, update, or delete activity reported by an external agent integration. Do not edit kmux state files directly.\n\n",
    "--agent-kind and --session-id identify one agent session. --reporter-kind and --reporter-instance identify one independently replaceable observation of that session."
);
const SET_AGENT_STATUS_USAGE: &str = concat!(
    "kmux set-agent-status [OPTIONS] --agent-kind <AGENT_KIND>\n",
    "    --session-id <SESSION_ID> --reporter-kind <REPORTER_KIND>\n",
    "    --reporter-instance <REPORTER_INSTANCE> [STATUS]"
);
const SET_AGENT_STATUS_AFTER_LONG_HELP: &str = concat!(
    "Example:\n",
    "  kmux set-agent-status working \\\n",
    "    --agent-kind example-agent --session-id session-123 \\\n",
    "    --reporter-kind server --reporter-instance local \\\n",
    "    --directory \"$PWD\""
);
const AGENT_STATUS_LONG_HELP: &str = concat!(
    "New activity state. Omit it to preserve prior status timing while updating supplied metadata or location hints. ",
    "Possible values describe active work, waiting for input, or completed work."
);
const DELETE_OBSERVATION_LONG_HELP: &str = concat!(
    "Delete only the observation identified by agent kind, session ID, reporter kind, and reporter instance. ",
    "Other reporters for the logical session remain intact."
);
const DELETE_SESSION_LONG_HELP: &str = "Delete every reporter observation for the logical session identified by agent kind and session ID.";
const DIRECTORY_LONG_HELP: &str = concat!(
    "Replace the observation's workspace attachment directory for an upsert; omission clears the prior directory. ",
    "Only an existing local Git directory can attach the observation to status and sidebar workspaces. ",
    "Other Git and tmux fields are hints, not attachment fallbacks."
);

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_create(arguments: &[&str]) -> CreateArgs {
        let cli = Cli::try_parse_from(
            ["kmux", "workspace", "create"]
                .into_iter()
                .chain(arguments.iter().copied()),
        )
        .expect("create arguments should parse");
        match cli.command {
            Command::Workspace(WorkspaceArgs {
                command: WorkspaceCommand::Create(args),
            }) => Some(args),
            _ => None,
        }
        .expect("expected workspace create command")
    }

    #[test]
    fn create_parses_launcher_and_literal_input_shapes() {
        for (arguments, expected) in [
            (
                [
                    "feature/sidebar",
                    "--launcher",
                    "agent",
                    "--launcher-input",
                    "prompt",
                ]
                .as_slice(),
                "prompt",
            ),
            (
                [
                    "feature/sidebar",
                    "--launcher",
                    "agent",
                    "--launcher-input",
                    "--leading-dash",
                ]
                .as_slice(),
                "--leading-dash",
            ),
            (
                [
                    "feature/sidebar",
                    "--launcher",
                    "agent",
                    "--launcher-input=--leading-dash",
                ]
                .as_slice(),
                "--leading-dash",
            ),
            (
                [
                    "feature/sidebar",
                    "--launcher",
                    "agent",
                    "--launcher-input",
                    "-",
                ]
                .as_slice(),
                "-",
            ),
            (
                [
                    "feature/sidebar",
                    "--launcher",
                    "agent",
                    "--launcher-input=",
                ]
                .as_slice(),
                "",
            ),
        ] {
            let args = parse_create(arguments);
            assert_eq!(args.launcher.as_deref(), Some("agent"));
            assert_eq!(args.launcher_input.as_deref(), Some(expected));
        }
    }

    #[test]
    fn create_distinguishes_absent_launcher_input_and_requires_explicit_launcher() {
        let args = parse_create(&["feature/sidebar", "--launcher", "agent"]);
        assert_eq!(args.launcher.as_deref(), Some("agent"));
        assert!(args.launcher_input.is_none());

        let error = Cli::try_parse_from([
            "kmux",
            "workspace",
            "create",
            "feature/sidebar",
            "--launcher-input",
            "prompt",
        ])
        .expect_err("launcher input without launcher should fail parsing");
        assert!(error.to_string().contains("--launcher"));
    }

    #[test]
    fn create_rejects_removed_launcher_option_names() {
        for arguments in [
            ["kmux", "workspace", "create", "feature/sidebar", "--launch"].as_slice(),
            ["kmux", "workspace", "create", "feature/sidebar", "--input"].as_slice(),
        ] {
            Cli::try_parse_from(arguments)
                .expect_err("removed launcher option name should fail parsing");
        }
    }

    #[test]
    fn lifecycle_commands_reject_removed_tmux_session_selector() {
        for arguments in [
            vec![
                "kmux",
                "workspace",
                "create",
                "feature/sidebar",
                "--tmux-session",
                "project-alpha",
            ],
            vec![
                "kmux",
                "workspace",
                "restore",
                "--tmux-session",
                "project-alpha",
            ],
            vec![
                "kmux",
                "workspace",
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

        let restore = Cli::try_parse_from(["kmux", "workspace", "restore"])
            .expect("argument-free restore should parse");
        assert!(matches!(
            restore.command,
            Command::Workspace(WorkspaceArgs {
                command: WorkspaceCommand::Restore
            })
        ));
    }

    #[test]
    fn removed_root_lifecycle_commands_and_aliases_do_not_parse() {
        for command in ["add", "list", "ls", "remove", "rm", "parent", "restore"] {
            Cli::try_parse_from(["kmux", command])
                .expect_err("removed root lifecycle command should fail parsing");
        }
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
