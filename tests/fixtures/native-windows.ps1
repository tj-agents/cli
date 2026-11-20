# The hosted runner is elevated. Stock Codex requires a non-administrative daemon owner.
param([string] $WorkerDirectory)
$ErrorActionPreference = 'Stop'
$repo = (Resolve-Path (Join-Path $PSScriptRoot '../..')).Path
$fixture = Join-Path $PSScriptRoot 'native_refresh.py'
Set-Location -LiteralPath $repo

if ($WorkerDirectory) {
    $env:TEMP = $WorkerDirectory
    $env:TMP = $WorkerDirectory
    $log = Join-Path $WorkerDirectory 'acceptance.log'
    & "$repo/.venv/Scripts/python.exe" $fixture --assets "$WorkerDirectory/assets.json" *> $log
    exit $LASTEXITCODE
}

$directory = Join-Path $env:RUNNER_TEMP ('tjnd-' + [guid]::NewGuid().ToString('N').Substring(0, 8))
$null = New-Item -ItemType Directory -Path $directory
uv run --locked python $fixture --stage $directory
if ($LASTEXITCODE) { exit $LASTEXITCODE }

# Local Service can read the prebuilt assets and owns only the disposable fixture home.
foreach ($inputPath in @("$repo/.venv", "$repo/target/debug", $PSScriptRoot)) {
    icacls $inputPath /grant '*S-1-5-19:(OI)(CI)RX' | Out-Null
    if ($LASTEXITCODE) { exit $LASTEXITCODE }
}
icacls $directory /grant '*S-1-5-19:(OI)(CI)M' | Out-Null
if ($LASTEXITCODE) { exit $LASTEXITCODE }
$name = Split-Path -Leaf $directory
$principal = New-ScheduledTaskPrincipal -UserId 'NT AUTHORITY\LOCAL SERVICE' -LogonType ServiceAccount -RunLevel Limited
$arguments = "-NoLogo -NoProfile -NonInteractive -File `"$PSCommandPath`" -WorkerDirectory `"$directory`""
$action = New-ScheduledTaskAction -Execute (Get-Command pwsh).Source -Argument $arguments -WorkingDirectory $repo
$null = Register-ScheduledTask -TaskName $name -Principal $principal -Action $action
try {
    Start-ScheduledTask -TaskName $name
    $deadline = (Get-Date).AddMinutes(8)
    do {
        Start-Sleep -Seconds 1
        $info = Get-ScheduledTaskInfo -TaskName $name
        $state = (Get-ScheduledTask -TaskName $name).State
        if ((Get-Date) -gt $deadline) { throw 'Native acceptance worker timed out' }
    } while ($info.LastRunTime.Year -lt 2000 -or $state -in @('Running', 'Queued'))
    if ($info.LastTaskResult -ne 0) { throw "Native acceptance worker exited $($info.LastTaskResult)" }
    if (-not (Test-Path "$directory/acceptance.log" -PathType Leaf)) { throw 'Native acceptance worker produced no log' }
} finally {
    if (Test-Path "$directory/acceptance.log" -PathType Leaf) { Get-Content "$directory/acceptance.log" }
    Stop-ScheduledTask -TaskName $name
    Unregister-ScheduledTask -TaskName $name -Confirm:$false
}
