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
    [string]$ScanId
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$resolvedAxeDirectory = (Resolve-Path -LiteralPath $AxeDirectory).Path
[Environment]::CurrentDirectory = $resolvedAxeDirectory
Add-Type -Path (Join-Path $resolvedAxeDirectory 'Axe.Windows.Automation.dll')

$config = [Axe.Windows.Automation.Config+Builder]::ForProcessId($ProcessId).
    WithOutputFileFormat([Axe.Windows.Automation.OutputFileFormat]::None).
    Build()
$scanner = [Axe.Windows.Automation.ScannerFactory]::CreateScanner($config)
$options = [Axe.Windows.Automation.Data.ScanOptions]::new($ScanId, $null)
$output = $scanner.Scan($options)
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
