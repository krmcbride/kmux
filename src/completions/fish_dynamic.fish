
# Dynamic workspace completion.
function __kmux_workspaces
    kmux _complete-workspaces 2>/dev/null
end

# Branch refs that are not already checked out in a worktree.
function __kmux_create_branches
    kmux _complete-create-branches 2>/dev/null
end

# Local branch refs for parent-valued arguments.
function __kmux_git_branches
    kmux _complete-git-branches 2>/dev/null
end

# Configured launcher names.
function __kmux_launchers
    kmux _complete-launchers 2>/dev/null
end

function __kmux_in_workspace_command --argument-names command
    set -l tokens (commandline -opc)
    test (count $tokens) -ge 3
    and test "$tokens[2]" = workspace
    and test "$tokens[3]" = "$command"
end

function __kmux_needs_workspace_command
    set -l tokens (commandline -opc)
    test (count $tokens) -eq 2
    and test "$tokens[2]" = workspace
end

function __kmux_set_parent_completed_arg_count
    set -l tokens (commandline -opc)

    set -l count 0
    set -l after_set_parent 0
    for token in $tokens
        if test $after_set_parent -eq 0
            if test "$token" = set-parent
                set after_set_parent 1
            end
            continue
        end
        if not string match -q -- '-*' "$token"
            set count (math $count + 1)
        end
    end
    echo $count
end

complete -c kmux -n '__kmux_in_workspace_command remove' -f -a '(__kmux_workspaces)'
complete -c kmux -n '__kmux_in_workspace_command create' -l parent -r -f -a '(__kmux_git_branches)'
complete -c kmux -n '__kmux_in_workspace_command create' -l launcher -r -f -a '(__kmux_launchers)'
complete -c kmux -n '__kmux_in_workspace_command create' -l launcher-input -r -f
complete -c kmux -n '__kmux_in_workspace_command create; and not __fish_prev_arg_in --parent --launcher --launcher-input' -f -a '(__kmux_create_branches)'
complete -c kmux -n '__kmux_in_workspace_command set-parent; and test (__kmux_set_parent_completed_arg_count) -eq 0' -f -a '(__kmux_git_branches)'
complete -c kmux -n '__kmux_in_workspace_command set-parent; and test (__kmux_set_parent_completed_arg_count) -eq 1' -f -a '(__kmux_workspaces)'
