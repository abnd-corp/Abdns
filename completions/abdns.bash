# bash completion for abdns
_abdns() {
    local cur prev commands
    cur="${COMP_WORDS[COMP_CWORD]}"
    prev="${COMP_WORDS[COMP_CWORD-1]}"
    commands="status test mode reload check logs token install uninstall completions version help"
    case "$prev" in
        mode) COMPREPLY=($(compgen -W "auto encrypted local" -- "$cur")); return ;;
        completions) COMPREPLY=($(compgen -W "bash zsh fish" -- "$cur")); return ;;
        logs) COMPREPLY=($(compgen -W "-f -n" -- "$cur")); return ;;
        --config) COMPREPLY=($(compgen -f -- "$cur")); return ;;
    esac
    COMPREPLY=($(compgen -W "$commands --config --json" -- "$cur"))
}
complete -F _abdns abdns
