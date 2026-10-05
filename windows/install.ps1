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
$target = Join-Path $BinDir 'tj.exe'
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

function Install-Binary {
    if ($Uninstall) {
        if (Test-Path -LiteralPath $target) { Remove-Item -LiteralPath $target -Force; Say removed $target }
        else { Say ok "$target (not installed)" }
        return
    }
    if ($Check) {
        if (Test-Path -LiteralPath $target) { Say ok "$target ($(& $target --version))" }
        else { Say missing $target; $script:status = 1 }
        return
    }

    if (Get-Command cargo -ErrorAction SilentlyContinue) {
        & cargo build --release --locked --quiet --manifest-path (Join-Path $repo 'Cargo.toml')
        if ($LASTEXITCODE -ne 0) { throw 'cargo build failed' }
        $built = Join-Path $repo 'target\release\tj.exe'
    }
    elseif (Get-Command gh -ErrorAction SilentlyContinue) {
        $tmp = Join-Path ([IO.Path]::GetTempPath()) ('tj-' + [guid]::NewGuid().ToString('N'))
        [void](New-Item -ItemType Directory -Path $tmp)
        & gh release download --repo tj-agents/cli --pattern 'tj-x86_64-pc-windows-msvc.zip' --dir $tmp --clobber
        if ($LASTEXITCODE -ne 0) { throw 'gh release download failed' }
        Expand-Archive -LiteralPath (Join-Path $tmp 'tj-x86_64-pc-windows-msvc.zip') -DestinationPath $tmp -Force
        $built = Join-Path $tmp 'tj.exe'
    }
    else { throw 'install needs either cargo (to build) or gh (to download a release)' }

    $same = (Test-Path -LiteralPath $target) -and
        ((Get-FileHash -LiteralPath $built).Hash -eq (Get-FileHash -LiteralPath $target).Hash)
    if ($same) { Say ok $target; return }
    if (-not (Test-Path -LiteralPath $BinDir)) { [void](New-Item -ItemType Directory -Path $BinDir -Force) }
    Copy-Item -LiteralPath $built -Destination $target -Force
    Say installed "$target ($(& $target --version))"
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

Install-Binary
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
