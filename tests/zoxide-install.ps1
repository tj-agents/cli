# Exercise the Windows dependency installer without winget, registry writes or downloads.
$ErrorActionPreference = 'Stop'

class Environment {
    static [string] $UserPath
    static [string] $MachinePath
    static [string] GetEnvironmentVariable([string] $Name, [string] $Scope) {
        if ($Scope -eq 'User') { return [Environment]::UserPath }
        return [Environment]::MachinePath
    }
    static [string] ExpandEnvironmentVariables([string] $Value) {
        return [System.Environment]::ExpandEnvironmentVariables($Value)
    }
}

$installer = Join-Path (Split-Path -Parent $PSScriptRoot) 'windows/install.ps1'
$errors = $null
$ast = [System.Management.Automation.Language.Parser]::ParseFile($installer, [ref]$null, [ref]$errors)
if ($errors) { throw ($errors | Out-String) }
$definition = $ast.Find({
    param($node)
    $node -is [System.Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -eq 'Install-Zoxide'
}, $true)
if (-not $definition) { throw 'Install-Zoxide was not found' }
Invoke-Expression $definition.Extent.Text

function Say($State, $What) { }
function Get-Command($Name, $CommandType, $ErrorAction) {
    if ($Name -eq 'winget' -and $script:hasWinget) { return @{ Name = $Name } }
    if ($Name -eq 'zoxide' -and ($script:preinstalled -or ($script:discoveryWorks -and ($env:Path -split ';') -contains 'C:\zoxide'))) {
        return @{ Name = $Name }
    }
}
function zoxide { 'zoxide test' }
function winget {
    $script:calls++
    $global:LASTEXITCODE = $script:wingetExit
    if ($LASTEXITCODE -eq 0) { [Environment]::UserPath = 'C:\zoxide;C:\existing' }
}

$cases = @(
    @{ Name = 'already installed'; Present = $true; Winget = $false; Check = $false; Uninstall = $false; Exit = 0; Discover = $true; Expected = 0; Calls = 0 },
    @{ Name = 'check changes nothing'; Present = $false; Winget = $true; Check = $true; Uninstall = $false; Exit = 0; Discover = $true; Expected = 1; Calls = 0 },
    @{ Name = 'uninstall leaves dependency alone'; Present = $false; Winget = $true; Check = $false; Uninstall = $true; Exit = 0; Discover = $true; Expected = 0; Calls = 0 },
    @{ Name = 'winget refreshes PATH'; Present = $false; Winget = $true; Check = $false; Uninstall = $false; Exit = 0; Discover = $true; Expected = 0; Calls = 1 },
    @{ Name = 'winget failure'; Present = $false; Winget = $true; Check = $false; Uninstall = $false; Exit = 1; Discover = $true; Expected = 1; Calls = 1 },
    @{ Name = 'failed discovery'; Present = $false; Winget = $true; Check = $false; Uninstall = $false; Exit = 0; Discover = $false; Expected = 1; Calls = 1 },
    @{ Name = 'no winget'; Present = $false; Winget = $false; Check = $false; Uninstall = $false; Exit = 0; Discover = $true; Expected = 1; Calls = 0 }
)
$originalPath = $env:Path
try {
    foreach ($case in $cases) {
        $env:Path = 'C:\process-only;C:\existing'
        [Environment]::UserPath = ''
        [Environment]::MachinePath = 'C:\existing;C:\machine'
        $script:preinstalled = $case.Present
        $script:hasWinget = $case.Winget
        $script:discoveryWorks = $case.Discover
        $script:wingetExit = $case.Exit
        $Check = $case.Check
        $Uninstall = $case.Uninstall
        $script:status = 0
        $script:calls = 0
        Install-Zoxide
        if ($script:status -ne $case.Expected -or $script:calls -ne $case.Calls) { throw $case.Name }
        if (-not $env:Path.StartsWith('C:\process-only;C:\existing')) { throw 'process PATH entries lost' }
        if ($case.Name -eq 'winget refreshes PATH') {
            Install-Zoxide
            $Check = $true
            Install-Zoxide
            if ($script:status -ne 0 -or $script:calls -ne 1) { throw 'second install or check retried winget' }
            if (@(($env:Path -split ';') | Where-Object { $_ -eq 'C:\existing' }).Count -ne 1) { throw 'duplicate PATH entry' }
        }
        Write-Output "pass: $($case.Name)"
    }
}
finally { $env:Path = $originalPath }
