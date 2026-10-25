param([string] $TabId, [switch] $Json)
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes
$root = [System.Windows.Automation.AutomationElement]::RootElement
$windowType = New-Object System.Windows.Automation.PropertyCondition([System.Windows.Automation.AutomationElement]::ControlTypeProperty, [System.Windows.Automation.ControlType]::Window)
$tabs = @()
foreach ($window in @($root.FindAll([System.Windows.Automation.TreeScope]::Children, $windowType))) {
    $process = Get-Process -Id $window.Current.ProcessId -ErrorAction SilentlyContinue
    if (-not $process -or $process.ProcessName -notin @('WindowsTerminal', 'WindowsTerminalPreview')) { continue }
    $tabType = New-Object System.Windows.Automation.PropertyCondition([System.Windows.Automation.AutomationElement]::ControlTypeProperty, [System.Windows.Automation.ControlType]::TabItem)
    foreach ($tab in @($window.FindAll([System.Windows.Automation.TreeScope]::Descendants, $tabType))) {
        $tabs += [pscustomobject]@{ AutomationId = $tab.Current.AutomationId; Element = $tab; ProcessId = $window.Current.ProcessId }
    }
}
if ($Json) { Write-Output ('[' + (@($tabs | ForEach-Object { ConvertTo-Json -Compress ([pscustomobject]@{ automationId=$_.AutomationId; processId=$_.ProcessId }) }) -join ',') + ']'); return }
$matches = @($tabs | Where-Object { $TabId -and $_.AutomationId -eq $TabId })
if ($matches.Count -ne 1) { throw 'Windows Terminal target is absent or ambiguous.' }
$close = New-Object System.Windows.Automation.PropertyCondition([System.Windows.Automation.AutomationElement]::AutomationIdProperty, 'CloseButton')
$button = $matches[0].Element.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $close)
if (-not $button) { throw 'Windows Terminal tab has no close button.' }
$button.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke()
