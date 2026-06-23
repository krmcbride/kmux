
# Dynamic worktree handle completion.
_kmux_handles() {
    kmux _complete-handles 2>/dev/null
}

# Branch refs that are not already checked out in a worktree.
_kmux_add_branches() {
    kmux _complete-add-branches 2>/dev/null
}

# Branch refs for ref-valued options such as add --base.
_kmux_git_branches() {
    kmux _complete-git-branches 2>/dev/null
}

_kmux_dynamic() {
    local cur prev words cword

    if declare -F _init_completion >/dev/null 2>&1; then
        _init_completion || return
    else
        COMPREPLY=()
        cur="${COMP_WORDS[COMP_CWORD]}"
        prev="${COMP_WORDS[COMP_CWORD-1]}"
        words=("${COMP_WORDS[@]}")
        cword=$COMP_CWORD
    fi

    if [[ ${cword} -ge 2 ]]; then
        local cmd="${words[1]}"
        case "$cmd" in
            open|close|path|remove|rm|rename|status)
                if [[ "$cur" != -* ]]; then
                    COMPREPLY=($(compgen -W "$(_kmux_handles)" -- "$cur"))
                    return
                fi
                ;;
            add)
                case "$prev" in
                    --base)
                        COMPREPLY=($(compgen -W "$(_kmux_git_branches)" -- "$cur"))
                        return
                        ;;
                    --name)
                        _kmux "$@"
                        return
                        ;;
                esac
                if [[ "$cur" != -* ]]; then
                    COMPREPLY=($(compgen -W "$(_kmux_add_branches)" -- "$cur"))
                    return
                fi
                ;;
        esac
    fi

    _kmux "$@"
}

complete -F _kmux_dynamic -o bashdefault -o default kmux
