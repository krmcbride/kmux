use anyhow::Result;
use clap::CommandFactory;
use clap_complete::{Shell, generate as generate_for_shell};

use crate::cli;
use crate::git::Git;
use crate::paths::RepoPaths;
use crate::slug::workspace_slug_from_branch;

pub fn generate(shell: Shell) -> Result<()> {
    let mut command = cli::Cli::command();
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

pub fn complete_workspaces() -> Result<()> {
    for workspace in kmux_workspaces() {
        println!("{workspace}");
    }
    Ok(())
}

pub fn complete_add_branches() -> Result<()> {
    for branch in checkoutable_branch_refs() {
        println!("{branch}");
    }
    Ok(())
}

pub fn complete_git_branches() -> Result<()> {
    for branch in local_branches() {
        println!("{branch}");
    }
    Ok(())
}

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

    let mut workspaces = worktrees
        .into_iter()
        .filter(|worktree| worktree.path.parent() == Some(paths.worktree_base_dir.as_path()))
        .filter(|worktree| {
            let Some(branch) = worktree.branch.as_deref() else {
                return false;
            };
            let Ok(expected_slug) = workspace_slug_from_branch(branch) else {
                return false;
            };
            worktree
                .path
                .file_name()
                .is_some_and(|file_name| file_name == expected_slug.as_str())
        })
        .filter_map(|worktree| {
            worktree
                .path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
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
}
