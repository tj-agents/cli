# Printed by `tj init fish`. `f` is a function because a program cannot change its parent shell's
# directory: `tj f` prints the path and the function does the cd.
function f
    set -l dir (command tj f $argv); or return
    test -n "$dir"; and cd -- $dir
end
function cr
    command tj cr $argv
end
