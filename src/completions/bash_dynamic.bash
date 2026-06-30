
# Dynamic workspace completion.
_kmux_workspaces() {
    kmux _complete-workspaces 2>/dev/null
}

# Branch refs that are not already checked out in a worktree.
_kmux_add_branches() {
    kmux _complete-add-branches 2>/dev/null
}

# Local branch refs for parent-valued arguments.
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
            remove|rm|status)
                if [[ "$cur" != -* ]]; then
                    COMPREPLY=($(compgen -W "$(_kmux_workspaces)" -- "$cur"))
                    return
                fi
                ;;
            add)
                case "$prev" in
                    --parent)
                        COMPREPLY=($(compgen -W "$(_kmux_git_branches)" -- "$cur"))
                        return
                        ;;
                esac
                if [[ "$cur" != -* ]]; then
                    COMPREPLY=($(compgen -W "$(_kmux_add_branches)" -- "$cur"))
                    return
                fi
                ;;
            parent)
                if [[ "$cur" != -* ]]; then
                    local positional_before=0
                    local index
                    for ((index = 2; index < cword; index++)); do
                        if [[ "${words[index]}" != -* ]]; then
                            ((positional_before++))
                        fi
                    done
                    if (( positional_before >= 1 )); then
                        COMPREPLY=($(compgen -W "$(_kmux_git_branches)" -- "$cur"))
                    else
                        COMPREPLY=($(compgen -W "$(_kmux_workspaces) $(_kmux_git_branches)" -- "$cur"))
                    fi
                    return
                fi
                ;;
        esac
    fi

    _kmux "$@"
}

complete -F _kmux_dynamic -o bashdefault -o default kmux
