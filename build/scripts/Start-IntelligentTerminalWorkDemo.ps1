<#
.SYNOPSIS
Launch the isolated work-story provider in a new native Agent Center window.
.DESCRIPTION
Uses the installed Dev package without settings changes, shell tabs, service
configuration or process termination. Activation uses the package-scoped wtai
execution alias, never a direct WindowsTerminal image or ambiguous global alias.
Environment changes apply only to the
new-window launch. The owned window opens at a compact 1360 x 900 physical
pixels by default, centered within its monitor. Closing the window preserves
its isolated demo state.
A reused Dev host must already have Agent Center enabled in its process
environment. Launch-scoped environment cannot toggle that existing host; this
script never restarts it to force the mode and rejects a shell-tab substitute.
The returned context can be passed to Stop-NativeWorkStoryDemo after dot-sourcing
test\e2e\tests\helpers\NativeWorkStoryDemo.ps1.
#>
[CmdletBinding()]
param(
    [ValidateSet('Dev')][string]$Package = 'Dev',
    [string]$StateDir,
    [string]$ArtifactDirectory,
    [string]$ExpectedWtaSha256 = $env:ITE2E_EXPECTED_WTA_SHA256,
    [string]$ExpectedUiMarker = 'Work story demo',
    [ValidateRange(800, 3840)][int]$WindowWidth = 1360,
    [ValidateRange(600, 2160)][int]$WindowHeight = 900
)
$ErrorActionPreference = 'Stop'
$repositoryRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..\..'))
Import-Module (Join-Path $repositoryRoot 'test\e2e\ItE2E\ItE2E.psd1') -Force
. (Join-Path $repositoryRoot 'test\e2e\tests\helpers\NativeWorkStoryDemo.ps1')
$app = Resolve-ItApp -Package $Package
if (-not $StateDir) {
    $StateDir = Join-Path $app.LocalStateDir 'IntelligentTerminal\work-story-demo-native'
}
if (-not $ArtifactDirectory) {
    $ArtifactDirectory = Join-Path $repositoryRoot ('test\e2e\artifacts\native-work-demo-' + [guid]::NewGuid().ToString('N'))
}
if (-not $ExpectedWtaSha256) {
    $ExpectedWtaSha256 = (Get-FileHash -LiteralPath $app.WtaPath -Algorithm SHA256).Hash
    Write-Warning 'No independent build hash supplied: recording the installed binary only, not qualifying a feature revision.'
}
$context = @{}
Start-NativeWorkStoryDemo -Context $context -Package $Package -StateDirectory $StateDir `
    -ArtifactDirectory $ArtifactDirectory -ExpectedWtaSha256 $ExpectedWtaSha256 -ExpectedUiMarker $ExpectedUiMarker `
    -WindowWidth $WindowWidth -WindowHeight $WindowHeight
