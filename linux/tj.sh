# Printed by `tj init bash|zsh`. `f` is a function because a program cannot change its parent shell's
# directory: `tj f` prints the path and the function does the cd.
if command -v zoxide >/dev/null 2>&1; then
    if [ -n "${ZSH_VERSION:-}" ]; then
        eval "$(zoxide init zsh --cmd cd)"
    else
        eval "$(zoxide init bash --cmd cd)"
    fi
fi

f() {
    local dir
    dir="$(command tj f "$@")" || return
    [ -n "$dir" ] && cd -- "$dir"
}
cr() { command tj cr "$@"; }
