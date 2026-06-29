
# Dynamic workspace completion.
function __kmux_workspaces
    kmux _complete-workspaces 2>/dev/null
end

# Branch refs that are not already checked out in a worktree.
function __kmux_add_branches
    kmux _complete-add-branches 2>/dev/null
end

# Branch refs for ref-valued options such as add --base.
function __kmux_git_branches
    kmux _complete-git-branches 2>/dev/null
end

complete -c kmux -n '__fish_seen_subcommand_from open remove rm status' -f -a '(__kmux_workspaces)'
complete -c kmux -n '__fish_seen_subcommand_from add; and __fish_prev_arg_in --base' -f -a '(__kmux_git_branches)'
complete -c kmux -n '__fish_seen_subcommand_from add; and not __fish_prev_arg_in --base' -f -a '(__kmux_add_branches)'
