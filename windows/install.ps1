# Installs `tj` and wires the `f` and `cr` functions into PowerShell. Safe to run any number of times:
# each step reports "ok" when there is nothing to change. Needs no administrator rights.
#
#   windows\install.ps1              install or update
#   windows\install.ps1 -Check       report what is and isn't set up, change nothing
#   windows\install.ps1 -Uninstall   remove the binary, the PATH entry and the profile wiring
[CmdletBinding()]
param(
    [switch] $Check,
    [switch] $Uninstall,
    [string] $BinDir = $(if ($env:TJ_BIN_DIR) { $env:TJ_BIN_DIR } else { Join-Path $env:LOCALAPPDATA 'Programs\tj' })
)

$ErrorActionPreference = 'Stop'
if ($IsLinux -or $IsMacOS) { throw 'install.ps1 is for Windows - use ./install.sh on Linux and macOS' }
$repo = Split-Path -Parent $PSScriptRoot
$begin = '# >>> tj-agents/cli >>>'
$end = '# <<< tj-agents/cli <<<'
$script:status = 0

function Say([string]$State, [string]$What) { '{0,-10} {1}' -f $State, $What }

# Replace (or append, or remove) the marked block in a profile; everything outside the markers stays.
function Set-Block([string]$File, [string]$Body) {
    $current = if (Test-Path -LiteralPath $File) { [IO.File]::ReadAllText($File) } else { '' }
    $pattern = '(?ms)^' + [regex]::Escape($begin) + '.*?^' + [regex]::Escape($end) + '\r?\n?'
    $stripped = ([regex]::Replace($current, $pattern, '')).TrimEnd()
    $block = "$begin`r`n$Body`r`n$end"
    $next = if ($Uninstall) { $stripped } elseif ($stripped) { "$stripped`r`n`r`n$block" } else { $block }
    if ($next) { $next += "`r`n" }
    if ($current -eq $next) { Say ok $File; return }
    if ($Check) { Say missing $File; $script:status = 1; return }
    if (-not $next) { Remove-Item -LiteralPath $File -Force; Say removed $File; return }
    $dir = Split-Path -Parent $File
    if (-not (Test-Path -LiteralPath $dir)) { [void](New-Item -ItemType Directory -Path $dir -Force) }
    [IO.File]::WriteAllText($File, $next, [Text.UTF8Encoding]::new($false))
    Say updated $File
}

function Install-Binaries {
    $python = Get-Command python -ErrorAction SilentlyContinue
    if (-not $python) { throw 'install needs Python 3.12 or newer' }
    $sourcePath = Join-Path $repo 'packages\tj-cli\src'
    $previousPythonPath = $env:PYTHONPATH
    $env:PYTHONPATH = if ($previousPythonPath) { "$sourcePath;$previousPythonPath" } else { $sourcePath }
    try {
        $arguments = @('-m', 'tj_cli.native_install', '--bin-dir', $BinDir, '--source', $repo)
        if ($Check) { $arguments += '--check' }
        if ($Uninstall) { $arguments += '--uninstall' }
        & $python.Source @arguments
        if ($LASTEXITCODE -ne 0) { $script:status = 1 }
    }
    finally { $env:PYTHONPATH = $previousPythonPath }
}

# The user PATH lives in the registry; this process's PATH is updated too so the rest of the script
# (and a profile reload) can find tj straight away.
function Set-UserPath {
    $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    $parts = @($userPath -split ';' | Where-Object { $_ })
    $has = $parts | Where-Object { $_.TrimEnd('\') -ieq $BinDir.TrimEnd('\') }
    if ($Uninstall) {
        if (-not $has) { Say ok "PATH (no entry)"; return }
        [Environment]::SetEnvironmentVariable('Path', (($parts | Where-Object { $_.TrimEnd('\') -ine $BinDir.TrimEnd('\') }) -join ';'), 'User')
        Say removed "$BinDir from user PATH"
        return
    }
    if ($has) { Say ok "PATH has $BinDir"; return }
    if ($Check) { Say missing "$BinDir on user PATH"; $script:status = 1; return }
    [Environment]::SetEnvironmentVariable('Path', (($parts + $BinDir) -join ';'), 'User')
    $env:Path = "$env:Path;$BinDir"
    Say updated "user PATH (+ $BinDir)"
}

function Test-Fzf {
    if ($Uninstall) { return }
    if (Get-Command fzf -ErrorAction SilentlyContinue) { Say ok "fzf $((& fzf --version) -split ' ' | Select-Object -First 1)"; return }
    if ($Check) { Say missing 'fzf'; $script:status = 1; return }
    if (Get-Command winget -ErrorAction SilentlyContinue) {
        & winget install --id junegunn.fzf --exact --silent --accept-source-agreements --accept-package-agreements
        Say installed 'fzf (via winget - open a new terminal for it to be on PATH)'
    }
    else { Say missing 'fzf - install it with: winget install junegunn.fzf'; $script:status = 1 }
}

Install-Binaries
if ($IsWindows -or $PSVersionTable.PSEdition -eq 'Desktop') { Set-UserPath }
Test-Fzf

# Both Windows PowerShell 5.1 and PowerShell 7 read their own profile; wire whichever exist or are installed.
$documents = [Environment]::GetFolderPath('MyDocuments')
$profiles = @(Join-Path $documents 'PowerShell\profile.ps1')
if ($PSVersionTable.PSEdition -eq 'Desktop' -or (Test-Path (Join-Path $documents 'WindowsPowerShell'))) {
    $profiles += Join-Path $documents 'WindowsPowerShell\profile.ps1'
}
$line = 'if (Get-Command tj -ErrorAction SilentlyContinue) { Invoke-Expression (& tj init pwsh | Out-String) }'
foreach ($p in $profiles) { Set-Block $p $line }

if (-not $Check -and -not $Uninstall) { ''; 'Done. Open a new terminal and try: f, cr' }
exit $script:status
