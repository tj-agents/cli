# Printed by `tj init bash|zsh`. `f` is a function because a program cannot change its parent shell's
# directory: `tj f` prints the path and the function does the cd.
f() {
    local dir
    dir="$(command tj f "$@")" || return
    [ -n "$dir" ] && cd -- "$dir"
}
cr() { command tj cr "$@"; }
