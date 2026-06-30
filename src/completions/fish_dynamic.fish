
# Dynamic workspace completion.
function __kmux_workspaces
    kmux _complete-workspaces 2>/dev/null
end

# Branch refs that are not already checked out in a worktree.
function __kmux_add_branches
    kmux _complete-add-branches 2>/dev/null
end

# Local branch refs for parent-valued arguments.
function __kmux_git_branches
    kmux _complete-git-branches 2>/dev/null
end

function __kmux_parent_completed_arg_count
    set -l tokens (commandline -opc)
    set -l current (commandline -ct)
    if test -n "$current"; and test (count $tokens) -gt 1
        set tokens $tokens[1..-2]
    end

    set -l count 0
    set -l after_parent 0
    for token in $tokens
        if test $after_parent -eq 0
            if test "$token" = parent
                set after_parent 1
            end
            continue
        end
        if not string match -q -- '-*' "$token"
            set count (math $count + 1)
        end
    end
    echo $count
end

complete -c kmux -n '__fish_seen_subcommand_from remove rm status' -f -a '(__kmux_workspaces)'
complete -c kmux -n '__fish_seen_subcommand_from add; and __fish_prev_arg_in --parent' -f -a '(__kmux_git_branches)'
complete -c kmux -n '__fish_seen_subcommand_from add; and not __fish_prev_arg_in --parent' -f -a '(__kmux_add_branches)'
complete -c kmux -n '__fish_seen_subcommand_from parent; and test (__kmux_parent_completed_arg_count) -eq 0' -f -a '(__kmux_workspaces) (__kmux_git_branches)'
complete -c kmux -n '__fish_seen_subcommand_from parent; and test (__kmux_parent_completed_arg_count) -ge 1' -f -a '(__kmux_git_branches)'
