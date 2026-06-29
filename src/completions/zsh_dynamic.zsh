
# Dynamic workspace completion.
_kmux_workspaces() {
    local -a workspaces
    workspaces=("${(@f)$(kmux _complete-workspaces 2>/dev/null)}")
    workspaces=(${workspaces:#})
    (( ${#workspaces} )) && compadd -a workspaces
}

# Branch refs that are not already checked out in a worktree.
_kmux_add_branches() {
    local -a branches
    branches=("${(@f)$(kmux _complete-add-branches 2>/dev/null)}")
    branches=(${branches:#})
    (( ${#branches} )) && compadd -a branches
}

# Branch refs for ref-valued options such as add --base.
_kmux_git_branches() {
    local -a branches
    branches=("${(@f)$(kmux _complete-git-branches 2>/dev/null)}")
    branches=(${branches:#})
    (( ${#branches} )) && compadd -a branches
}

_kmux() {
    emulate -L zsh
    setopt extended_glob
    setopt no_nomatch

    local cmd="${words[2]}"

    if [[ "$cmd" == "add" && "${words[CURRENT-1]}" == "--base" ]]; then
        _kmux_git_branches
        return
    fi

    local -a arg_flags
    arg_flags=()

    if [[ "${words[CURRENT]}" == -* ]] || [[ -n "${arg_flags[(r)${words[CURRENT-1]}]}" ]]; then
        _kmux_base "$@"
        return
    fi

    case "$cmd" in
        open|remove|rm|status)
            _kmux_workspaces
            ;;
        add)
            _kmux_add_branches
            ;;
        *)
            _kmux_base "$@"
            ;;
    esac
}

if [ "$funcstack[1]" = "_kmux" ]; then
    _kmux "$@"
else
    compdef _kmux kmux
fi
