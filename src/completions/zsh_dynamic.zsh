
# Dynamic worktree handle completion.
_kmux_handles() {
    local -a handles
    handles=("${(@f)$(kmux _complete-handles 2>/dev/null)}")
    handles=(${handles:#})
    (( ${#handles} )) && compadd -a handles
}

# Local branches that are not already checked out in a worktree.
_kmux_add_branches() {
    local -a branches
    branches=("${(@f)$(kmux _complete-add-branches 2>/dev/null)}")
    branches=(${branches:#})
    (( ${#branches} )) && compadd -a branches
}

# Local branches for ref-valued options such as add --base.
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
    case "$cmd" in
        add)
            arg_flags=(--name)
            ;;
        *)
            arg_flags=()
            ;;
    esac

    if [[ "${words[CURRENT]}" == -* ]] || [[ -n "${arg_flags[(r)${words[CURRENT-1]}]}" ]]; then
        _kmux_base "$@"
        return
    fi

    case "$cmd" in
        open|close|path|remove|rm|rename|status)
            _kmux_handles
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
