# Bootstrap.ps1 — install/verify everything the ItE2E framework needs, and import it.
#   pwsh -File test/e2e/bootstrap.ps1            # install deps + import
#   pwsh -File test/e2e/bootstrap.ps1 -Check     # verify only

[CmdletBinding()]
param([switch]$Check)

$ErrorActionPreference = 'Stop'
$here = Split-Path -Parent $MyInvocation.MyCommand.Path

function Test-Tool($name, $cmd) {
    $c = Get-Command $cmd -ErrorAction SilentlyContinue
    if ($c) { Write-Host "[ok]   $name -> $($c.Source)" -ForegroundColor Green; $true }
    else { Write-Host "[miss] $name ($cmd not found)" -ForegroundColor Yellow; $false }
}

Write-Host "== ItE2E bootstrap ==" -ForegroundColor Cyan
$psOk = $PSVersionTable.PSVersion.Major -ge 7
Write-Host ("[{0}] PowerShell {1}" -f ($(if ($psOk) { 'ok' } else { 'miss' }), $PSVersionTable.PSVersion))

$haveWinapp = Test-Tool 'Windows App CLI' 'winapp'
$pester = Get-Module -ListAvailable Pester | Where-Object Version -ge ([version]'5.0.0') | Select-Object -First 1
Write-Host ("[{0}] Pester 5 {1}" -f ($(if ($pester) { 'ok' } else { 'miss' }), $pester.Version))

if (-not $Check) {
    if (-not $haveWinapp) {
        Write-Host "Installing Microsoft.winappcli via winget..." -ForegroundColor Cyan
        winget install --id Microsoft.winappcli --source winget --accept-source-agreements --accept-package-agreements --disable-interactivity
        # winget is a native exe: $ErrorActionPreference='Stop' does NOT trap its non-zero
        # exit, so check it explicitly or later "winapp not found" errors are confusing.
        if ($LASTEXITCODE -ne 0) { throw "winget install Microsoft.winappcli failed (exit $LASTEXITCODE). Is the winget source reachable?" }
    }
    if (-not $pester) {
        Write-Host "Installing Pester 5..." -ForegroundColor Cyan
        Install-Module Pester -MinimumVersion 5.5.0 -Force -Scope CurrentUser -SkipPublisherCheck -AllowClobber
    }
}

# Confirm at least one Intelligent Terminal package is installed.
$pkgs = Get-AppxPackage | Where-Object { $_.Name -like '*IntelligentTerminal*' }
if ($pkgs) { $pkgs | ForEach-Object { Write-Host "[ok]   package $($_.PackageFamilyName) v$($_.Version)" -ForegroundColor Green } }
else { Write-Host "[miss] No Intelligent Terminal package installed (deploy one to run E2E tests)." -ForegroundColor Yellow }

# Import the module.
Import-Module (Join-Path $here 'ItE2E\ItE2E.psd1') -Force
Write-Host "[ok]   Imported ItE2E ($((Get-Command -Module ItE2E).Count) functions)" -ForegroundColor Green
Write-Host "Ready. Run tests with: Invoke-Pester $here\selftests" -ForegroundColor Cyan
