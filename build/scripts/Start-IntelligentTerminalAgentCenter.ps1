# Copyright (c) Microsoft Corporation.
# Licensed under the MIT license.

#Requires -Version 7.4
[CmdletBinding()]
param(
    [Parameter(Mandatory)][ValidateSet('Dev')][string]$Package,
    [string]$StateDirectory,
    [string]$AdapterConfiguration,
    [ValidatePattern('^[a-fA-F0-9]{64}$')][string]$ExpectedWtaSha256,
    [switch]$ConfigureOnly
)

$ErrorActionPreference = 'Stop'
$repo = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..\..'))
Import-Module (Join-Path $repo 'test\e2e\ItE2E\ItE2E.psd1') -Force
$app = Resolve-ItApp -Package $Package
if (-not $ExpectedWtaSha256) {
    $build = Join-Path $repo 'tools\wta\target\x86_64-pc-windows-msvc\debug\wta.exe'
    $ExpectedWtaSha256 = (Get-FileHash -LiteralPath $build -Algorithm SHA256).Hash
}
$actualHash = (Get-FileHash -LiteralPath $app.WtaPath -Algorithm SHA256).Hash
if ($actualHash -ne $ExpectedWtaSha256) {
    throw 'The Dev package does not contain the expected WTA build. Deploy the matching binary before launching; existing processes and settings will not be changed.'
}
if (-not $StateDirectory) {
    $StateDirectory = Join-Path $app.LocalStateDir 'IntelligentTerminal\agent-center-live'
}
if (-not [IO.Path]::IsPathFullyQualified($StateDirectory)) {
    throw 'StateDirectory must be an absolute directory.'
}
$StateDirectory = [IO.Path]::GetFullPath($StateDirectory)
if (-not $AdapterConfiguration) {
    $AdapterConfiguration = Join-Path $app.LocalStateDir 'IntelligentTerminal\agent-center\adapters.json'
}
$AdapterConfiguration = (Resolve-Path -LiteralPath $AdapterConfiguration).Path
$installedConfiguration = Join-Path $StateDirectory 'adapters.json'
if (Test-Path -LiteralPath $installedConfiguration) {
    if ((Get-FileHash -LiteralPath $installedConfiguration).Hash -ne
        (Get-FileHash -LiteralPath $AdapterConfiguration).Hash) {
        throw 'This state directory uses a different adapter configuration. Choose a new directory or explicitly reconfigure its stopped authority; live configuration will not be overwritten.'
    }
}
else {
    $previousState = $env:INTELLIGENT_TERMINAL_AGENT_CENTER_STATE
    try {
        $env:INTELLIGENT_TERMINAL_AGENT_CENTER_STATE = $StateDirectory
        $configured = & $app.WtaPath center configure --input-json $AdapterConfiguration
        if ($LASTEXITCODE -ne 0) { throw "Adapter configuration failed with exit code $LASTEXITCODE." }
        if (($configured | ConvertFrom-Json).status -ne 'ok' -or
            -not (Test-Path -LiteralPath $installedConfiguration -PathType Leaf)) {
            throw 'The binary did not configure the requested isolated state directory.'
        }
    }
    finally { $env:INTELLIGENT_TERMINAL_AGENT_CENTER_STATE = $previousState }
}

$launch = [ordered]@{
    package = $app.PackageFullName
    wtaPath = $app.WtaPath
    wtaSha256 = $actualHash
    stateDirectory = $StateDirectory
    mode = 'RealACP'
    configuredOnly = [bool]$ConfigureOnly
}
if (-not $ConfigureOnly) {
    $alias = Join-Path $env:LOCALAPPDATA "Microsoft\WindowsApps\$($app.Package)\wtai.exe"
    if (-not (Test-Path -LiteralPath $alias -PathType Leaf)) {
        throw "The Dev package execution alias is unavailable: $alias"
    }
    $start = [Diagnostics.ProcessStartInfo]::new()
    $start.FileName = $alias
    $start.UseShellExecute = $false
    $start.ArgumentList.Add('-w')
    $start.ArgumentList.Add('new')
    $start.Environment['INTELLIGENT_TERMINAL_AGENT_CENTER'] = '1'
    $start.Environment['INTELLIGENT_TERMINAL_AGENT_CENTER_STATE'] = $StateDirectory
    [void]$start.Environment.Remove('INTELLIGENT_TERMINAL_WORK_DEMO_STATE')
    $process = [Diagnostics.Process]::Start($start)
    $launch.launcherPid = $process.Id
    $process.Dispose()
    Write-Host 'Requested a real Agent Center window. A reused Dev host must already have Agent Center enabled. No Work or model response is preloaded.'
}
[pscustomobject]$launch
