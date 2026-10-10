# Copyright (c) Microsoft Corporation.
# Licensed under the MIT license.

[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [int]$ProcessId,

    [Parameter(Mandatory)]
    [string]$AxeDirectory,

    [Parameter(Mandatory)]
    [string]$OutputPath,

    [Parameter(Mandatory)]
    [ValidateSet('fre', 'fre-settings', 'agents')]
    [string]$ScanId
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$resolvedAxeDirectory = (Resolve-Path -LiteralPath $AxeDirectory).Path
[Environment]::CurrentDirectory = $resolvedAxeDirectory
Add-Type -Path (Join-Path $resolvedAxeDirectory 'Axe.Windows.Automation.dll')
Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes

$processCondition = [Windows.Automation.PropertyCondition]::new(
    [Windows.Automation.AutomationElement]::ProcessIdProperty, $ProcessId)
$deadline = [DateTimeOffset]::UtcNow.AddSeconds(30)
do
{
    $root = [Windows.Automation.AutomationElement]::RootElement.FindFirst(
        [Windows.Automation.TreeScope]::Descendants, $processCondition)
    if ($root) { break }
    Start-Sleep -Milliseconds 250
} while ([DateTimeOffset]::UtcNow -lt $deadline)
if (-not $root) { throw 'The test host has no UI Automation root.' }

function Wait-VisibleElement
{
    param([string]$AutomationId)
    $condition = [Windows.Automation.PropertyCondition]::new(
        [Windows.Automation.AutomationElement]::AutomationIdProperty, $AutomationId)
    $deadline = [DateTimeOffset]::UtcNow.AddSeconds(30)
    do
    {
        $element = $root.FindFirst([Windows.Automation.TreeScope]::Descendants, $condition)
        if ($element -and -not $element.Current.IsOffscreen) { return $element }
        Start-Sleep -Milliseconds 250
    } while ([DateTimeOffset]::UtcNow -lt $deadline)
    throw "Expected visible accessibility state was not reached: $AutomationId."
}

if ($ScanId -eq 'fre-settings')
{
    $next = Wait-VisibleElement -AutomationId 'NextButton'
    $next.GetCurrentPattern([Windows.Automation.InvokePattern]::Pattern).Invoke()
}
$markerId = switch ($ScanId)
{
    'fre' { 'NextButton' }
    'fre-settings' { 'TabModeComboBox' }
    'agents' { 'AcpAgentComboBox' }
}
$marker = Wait-VisibleElement -AutomationId $markerId
$elements = $root.FindAll(
    [Windows.Automation.TreeScope]::Subtree, [Windows.Automation.Condition]::TrueCondition)
$hierarchy = @(
    foreach ($element in $elements)
    {
        $parent = [Windows.Automation.TreeWalker]::ControlViewWalker.GetParent($element)
        [ordered]@{
            runtime_id = @($element.GetRuntimeId())
            parent_runtime_id = if ($parent) { @($parent.GetRuntimeId()) } else { @() }
            automation_id = $element.Current.AutomationId
            name = $element.Current.Name
            control_type = $element.Current.ControlType.ProgrammaticName
            is_offscreen = $element.Current.IsOffscreen
            is_keyboard_focusable = $element.Current.IsKeyboardFocusable
            process_id = $element.Current.ProcessId
        }
    }
)
$treePath = Join-Path (Split-Path -Parent $OutputPath) 'uia-tree.json'
$hierarchy | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath $treePath -Encoding utf8NoBOM

$config = [Axe.Windows.Automation.Config+Builder]::ForProcessId($ProcessId).
    WithOutputFileFormat([Axe.Windows.Automation.OutputFileFormat]::None).
    Build()
$scanner = [Axe.Windows.Automation.ScannerFactory]::CreateScanner($config)
$options = [Axe.Windows.Automation.Data.ScanOptions]::new($ScanId, $null)
$output = $scanner.Scan($options)
if (@($output.WindowScanOutputs).Count -eq 0)
{
    throw 'Axe.Windows returned no windows; there is no runtime accessibility evidence.'
}
$findings = @(
    foreach ($window in $output.WindowScanOutputs)
    {
        foreach ($finding in $window.Errors)
        {
            [ordered]@{
                rule_id = [string]$finding.Rule.ID
                description = $finding.Rule.Description
                how_to_fix = $finding.Rule.HowToFix
                condition = $finding.Rule.Condition
                element_properties = $finding.Element.Properties
                element_patterns = @($finding.Element.Patterns)
            }
        }
    }
)

[ordered]@{
    version = 1
    scan_id = $ScanId
    visible_state_marker = $marker.Current.AutomationId
    uia_tree = $treePath
    process_id = $ProcessId
    window_count = @($output.WindowScanOutputs).Count
    error_count = $findings.Count
    errors = $findings
} |
    ConvertTo-Json -Depth 8 |
    Set-Content -LiteralPath $OutputPath -Encoding utf8NoBOM

if ($findings.Count -gt 0)
{
    exit 1
}
exit 0
