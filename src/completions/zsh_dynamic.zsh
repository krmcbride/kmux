
# Dynamic workspace completion.
_kmux_workspaces() {
    local -a workspaces
    workspaces=("${(@f)$(kmux _complete-workspaces 2>/dev/null)}")
    workspaces=(${workspaces:#})
    (( ${#workspaces} )) && compadd -a workspaces
}

# Branch refs that are not already checked out in a worktree.
_kmux_create_branches() {
    local -a branches
    branches=("${(@f)$(kmux _complete-create-branches 2>/dev/null)}")
    branches=(${branches:#})
    (( ${#branches} )) && compadd -a branches
}

# Local branch refs for parent-valued arguments.
_kmux_git_branches() {
    local -a branches
    branches=("${(@f)$(kmux _complete-git-branches 2>/dev/null)}")
    branches=(${branches:#})
    (( ${#branches} )) && compadd -a branches
}

# Configured launcher names.
_kmux_launchers() {
    local -a launchers
    launchers=("${(@f)$(kmux _complete-launchers 2>/dev/null)}")
    launchers=(${launchers:#})
    (( ${#launchers} )) && compadd -a launchers
}

_kmux() {
    emulate -L zsh
    setopt extended_glob
    setopt no_nomatch

    local namespace="${words[2]}"
    local cmd="${words[3]}"

    if [[ "$namespace" != "workspace" ]]; then
        _kmux_base "$@"
        return
    fi

    if [[ "$cmd" == "create" && "${words[CURRENT-1]}" == "--launcher" ]]; then
        _kmux_launchers
        return
    fi

    if [[ "$cmd" == "create" && "${words[CURRENT-1]}" == "--launcher-input" ]]; then
        return
    fi

    if [[ "$cmd" == "create" && "${words[CURRENT-1]}" == "--parent" ]]; then
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
        remove)
            _kmux_workspaces
            ;;
        create)
            _kmux_create_branches
            ;;
        set-parent)
            local positional_before=0
            local index
            for ((index = 4; index < CURRENT; index++)); do
                [[ "${words[index]}" != -* ]] && ((positional_before++))
            done
            if (( positional_before == 1 )); then
                _kmux_workspaces
            elif (( positional_before == 0 )); then
                _kmux_git_branches
            fi
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
