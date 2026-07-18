//! Shell completion generation and dynamic completion candidate providers.
//!
//! Static completions come from clap, while dynamic helpers query local Git state
//! and fail closed so shell completion stays quiet outside supported contexts.

use anyhow::Result;
use clap::{Command, CommandFactory};
use clap_complete::{Shell, generate as generate_for_shell};

use crate::cli;
use crate::git::Git;
use crate::paths::RepoPaths;
use crate::workspace::strict_kmux_workspace_records;

/// Print static clap completions plus kmux dynamic completion hooks for a shell.
pub fn generate(shell: Shell) -> Result<()> {
    let command = cli::Cli::command();
    let mut command = public_completion_command(&command);
    let name = command.get_name().to_owned();

    let mut buffer = Vec::new();
    generate_for_shell(shell, &mut command, &name, &mut buffer);
    let base_script = String::from_utf8_lossy(&buffer);

    match shell {
        Shell::Zsh => {
            let base_script = prepare_zsh_base(&base_script, &name);
            println!("{base_script}");
            print_zsh_dynamic_completion();
        }
        _ => {
            print!("{base_script}");
            match shell {
                Shell::Bash => print_bash_dynamic_completion(),
                Shell::Fish => print_fish_dynamic_completion(),
                _ => {}
            }
        }
    }

    Ok(())
}

// clap_complete's ahead-of-time generators include hidden subcommands. Build a
// presentation-only command tree so internal process entrypoints are not
// advertised as shell candidates while the runtime parser can still accept them.
fn public_completion_command(command: &Command) -> Command {
    let mut public = Command::new(command.get_name().to_owned())
        .subcommand_required(command.is_subcommand_required_set())
        .arg_required_else_help(command.is_arg_required_else_help_set())
        .disable_help_subcommand(command.is_disable_help_subcommand_set())
        .disable_help_flag(command.is_disable_help_flag_set())
        .disable_version_flag(command.is_disable_version_flag_set())
        .args(
            command
                .get_arguments()
                .filter(|argument| !argument.is_hide_set())
                .cloned(),
        )
        .subcommands(
            command
                .get_subcommands()
                .filter(|subcommand| !subcommand.is_hide_set())
                .map(public_completion_command),
        );

    if let Some(about) = command.get_about() {
        public = public.about(about.clone());
    }
    if let Some(long_about) = command.get_long_about() {
        public = public.long_about(long_about.clone());
    }
    if let Some(version) = command.get_version() {
        public = public.version(version.to_owned());
    }
    if let Some(long_version) = command.get_long_version() {
        public = public.long_version(long_version.to_owned());
    }
    for alias in command.get_aliases() {
        public = public.alias(alias.to_owned());
    }
    for alias in command.get_visible_aliases() {
        public = public.visible_alias(alias.to_owned());
    }

    public
}

/// Print strict kmux workspace slugs for shell completion.
pub fn complete_workspaces() -> Result<()> {
    for workspace in kmux_workspaces() {
        println!("{workspace}");
    }
    Ok(())
}

/// Print branch refs that can be used with `kmux add` without colliding with worktrees.
pub fn complete_add_branches() -> Result<()> {
    for branch in checkoutable_branch_refs() {
        println!("{branch}");
    }
    Ok(())
}

/// Print local branch refs for parent-selection completion.
pub fn complete_git_branches() -> Result<()> {
    for branch in local_branches() {
        println!("{branch}");
    }
    Ok(())
}

// Dynamic completion must fail closed: outside a Git repo, or while Git is
// transiently unavailable, return no candidates instead of surfacing errors.
fn kmux_workspaces() -> Vec<String> {
    let Ok(cwd) = std::env::current_dir() else {
        return Vec::new();
    };
    let Ok(paths) = RepoPaths::discover(&cwd) else {
        return Vec::new();
    };
    let git = Git::new(&paths.main_worktree);
    let Ok(worktrees) = git.worktrees() else {
        return Vec::new();
    };

    let Ok(records) = strict_kmux_workspace_records(&paths, worktrees) else {
        return Vec::new();
    };
    let mut workspaces = records
        .into_iter()
        .map(|record| record.workspace_slug().to_owned())
        .collect::<Vec<_>>();
    workspaces.sort();
    workspaces.dedup();
    workspaces
}

fn checkoutable_branch_refs() -> Vec<String> {
    local_repo_git()
        .and_then(|git| git.checkoutable_branch_refs().ok())
        .unwrap_or_default()
}

fn local_branches() -> Vec<String> {
    local_repo_git()
        .and_then(|git| git.local_branch_refs().ok())
        .unwrap_or_default()
}

fn local_repo_git() -> Option<Git> {
    let cwd = std::env::current_dir().ok()?;
    let paths = RepoPaths::discover(&cwd).ok()?;
    Some(Git::new(paths.main_worktree))
}

// clap_complete generates a `_kmux` zsh function. Rename it so the custom
// wrapper can own `_kmux` while still delegating to the generated implementation.
fn prepare_zsh_base(script: &str, name: &str) -> String {
    let function_prefix = format!("_{name}");
    let base_function_prefix = format!("_{name}_base");
    let script = script.replace(&function_prefix, &base_function_prefix);

    let funcstack_block = format!(
        "\nif [ \"$funcstack[1]\" = \"{base_function_prefix}\" ]; then\n    \
         {base_function_prefix} \"$@\"\nelse\n    \
         compdef {base_function_prefix} {name}\nfi\n"
    );

    script
        .strip_suffix(&funcstack_block)
        .unwrap_or(&script)
        .to_owned()
}

fn print_bash_dynamic_completion() {
    print!("{}", include_str!("bash_dynamic.bash"));
}

fn print_fish_dynamic_completion() {
    print!("{}", include_str!("fish_dynamic.fish"));
}

fn print_zsh_dynamic_completion() {
    print!("{}", include_str!("zsh_dynamic.zsh"));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prepare_zsh_base_renames_function_identifiers() {
        let input = concat!(
            "#compdef kmux\n",
            "_kmux() {\n",
            "  \":: :_kmux_commands\"\n",
            "}\n",
            "(( $+functions[_kmux_commands] )) ||\n",
            "_kmux_commands() {\n",
            "  _describe -t commands 'kmux commands' commands\n",
            "}\n",
            "\nif [ \"$funcstack[1]\" = \"_kmux\" ]; then\n",
            "    _kmux \"$@\"\n",
            "else\n",
            "    compdef _kmux kmux\n",
            "fi\n",
        );

        let result = prepare_zsh_base(input, "kmux");

        assert!(result.contains("_kmux_base()"));
        assert!(result.contains("_kmux_base_commands"));
        assert!(!result.contains("_kmux()"));
        assert!(result.contains("#compdef kmux"));
        assert!(result.contains("'kmux commands'"));
        assert!(!result.contains("funcstack"));
        assert!(!result.contains("compdef _kmux_base"));
    }

    #[test]
    fn parent_dynamic_completion_follows_parent_then_child_order() {
        let bash = include_str!("bash_dynamic.bash");
        assert!(bash.contains(concat!(
            "if (( positional_before == 1 )); then\n",
            "                        COMPREPLY=($(compgen -W \"$(_kmux_workspaces)\""
        )));
        assert!(bash.contains(concat!(
            "elif (( positional_before == 0 )); then\n",
            "                        COMPREPLY=($(compgen -W \"$(_kmux_git_branches)\""
        )));
        assert!(bash.contains(concat!("else\n", "                        COMPREPLY=()")));

        let zsh = include_str!("zsh_dynamic.zsh");
        assert!(zsh.contains(concat!(
            "if (( positional_before == 1 )); then\n",
            "                _kmux_workspaces\n",
            "            elif (( positional_before == 0 )); then\n",
            "                _kmux_git_branches"
        )));

        let fish = include_str!("fish_dynamic.fish");
        assert!(fish.contains("parent_completed_arg_count) -eq 0' -f -a '(__kmux_git_branches)'"));
        assert!(fish.contains("parent_completed_arg_count) -eq 1' -f -a '(__kmux_workspaces)'"));
        assert!(!fish.contains("parent_completed_arg_count) -ge 1'"));
    }

    #[test]
    fn public_completion_command_excludes_hidden_process_entrypoints() {
        let command = public_completion_command(&cli::Cli::command());
        assert!(
            command
                .get_subcommands()
                .all(|subcommand| !subcommand.is_hide_set())
        );

        let sidebar = command
            .find_subcommand("sidebar")
            .expect("public sidebar command should remain");
        let sidebar_commands = sidebar
            .get_subcommands()
            .map(Command::get_name)
            .collect::<Vec<_>>();
        assert_eq!(sidebar_commands, ["on", "off", "toggle"]);
    }
}
