# Printed by `tj init pwsh`. `f` is a function because a program cannot change its parent shell's
# directory: `tj f` prints the path and the function does the cd.
function global:f {
    $dir = & tj f @args
    if ($LASTEXITCODE -eq 0 -and $dir) { Set-Location -LiteralPath $dir }
}
function global:cr { & tj cr @args }
