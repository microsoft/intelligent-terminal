<#
.SYNOPSIS
    FRE Test Reset Tool for Intelligent Terminal.
    Helps testers reset the First Run Experience to test it repeatedly.

.DESCRIPTION
    Interactive menu to selectively:
      [1] Reset FRE flag (agentFreCompleted -> false)
      [2] Uninstall GitHub Copilot CLI (winget)
      [3] Clear GitHub Copilot credentials (Credential Manager + config)
      [4] Fake GitHub Copilot credential (bypass auth screen in FRE)
      [A] All of 1-3 (full reset for clean FRE test)

.NOTES
    Run from any PowerShell terminal:
      powershell -ExecutionPolicy Bypass -File tools\fre-test-reset.ps1
#>

$ErrorActionPreference = 'Continue'

# ── Helpers ──────────────────────────────────────────────────────────────────

function Find-StateJson {
    $pkgDir = "$env:LOCALAPPDATA\Packages"
    $candidates = Get-ChildItem -Path $pkgDir -Directory -Filter "IntelligentTerminal_*" -ErrorAction SilentlyContinue
    foreach ($dir in $candidates) {
        $state = Join-Path $dir.FullName "LocalState\state.json"
        if (Test-Path $state) { return $state }
    }
    return $null
}

function Reset-FreFlag {
    Write-Host "`n[1] Resetting FRE flag..." -ForegroundColor Cyan
    $stateFile = Find-StateJson
    if (-not $stateFile) {
        Write-Host "    state.json not found. Has Intelligent Terminal been launched at least once?" -ForegroundColor Yellow
        return
    }
    $json = Get-Content $stateFile -Raw | ConvertFrom-Json
    if ($json.agentFreCompleted -eq $false) {
        Write-Host "    Already false, nothing to do." -ForegroundColor Green
        return
    }
    $json.agentFreCompleted = $false
    $json | ConvertTo-Json -Depth 10 | Set-Content $stateFile -Encoding UTF8
    Write-Host "    Done: $stateFile" -ForegroundColor Green
    Write-Host "    agentFreCompleted = false" -ForegroundColor Green
}

function Uninstall-CopilotCli {
    Write-Host "`n[2] Uninstalling GitHub Copilot CLI..." -ForegroundColor Cyan

    # Try winget first
    $winget = Get-Command winget -ErrorAction SilentlyContinue
    if ($winget) {
        Write-Host "    Running: winget uninstall GitHub.Copilot" -ForegroundColor Gray
        $result = & winget uninstall --id GitHub.Copilot --silent 2>&1
        $exitCode = $LASTEXITCODE
        if ($exitCode -eq 0) {
            Write-Host "    Uninstalled via winget." -ForegroundColor Green
        } else {
            Write-Host "    winget uninstall exit code $exitCode (may not have been installed via winget)" -ForegroundColor Yellow
        }
    }

    # Also remove npm global install if present
    $npmGlobalDirs = @(
        "$env:APPDATA\npm",
        "C:\.tools\.npm-global"
    )
    foreach ($dir in $npmGlobalDirs) {
        foreach ($ext in @("", ".cmd", ".ps1")) {
            $f = Join-Path $dir "copilot$ext"
            if (Test-Path $f) {
                Remove-Item $f -Force
                Write-Host "    Removed: $f" -ForegroundColor Green
            }
        }
        $modDir = Join-Path $dir "node_modules\@anthropic-ai\copilot-cli"
        if (Test-Path $modDir) {
            Remove-Item $modDir -Recurse -Force
            Write-Host "    Removed: $modDir" -ForegroundColor Green
        }
    }

    # Verify
    $check = & cmd /c "where copilot 2>&1"
    if ($LASTEXITCODE -ne 0) {
        Write-Host "    Verified: copilot not on PATH." -ForegroundColor Green
    } else {
        Write-Host "    Warning: copilot still found at: $check" -ForegroundColor Yellow
    }
}

function Clear-CopilotCredentials {
    Write-Host "`n[3] Clearing GitHub Copilot credentials..." -ForegroundColor Cyan

    # Remove from Windows Credential Manager
    # cmdkey /list shows lines like:
    #     Target: LegacyGeneric:target=copilot-cli/https://github.com:user
    # cmdkey /delete needs the part after "target=" as the key
    $found = $false
    $creds = & cmd /c "cmdkey /list 2>&1"
    foreach ($line in $creds) {
        if ($line -match 'Target:\s*\S*target=(.+)$') {
            $target = $Matches[1].Trim()
            if ($target -match 'copilot') {
                & cmdkey /delete:$target 2>&1 | Out-Null
                Write-Host "    Removed credential: $target" -ForegroundColor Green
                $found = $true
            }
        }
    }
    if (-not $found) {
        Write-Host "    No copilot entries in Credential Manager." -ForegroundColor Gray
    }

    # Clear config.json loggedInUsers
    $configFile = "$env:USERPROFILE\.copilot\config.json"
    if (Test-Path $configFile) {
        Remove-Item $configFile -Force
        Write-Host "    Removed: $configFile" -ForegroundColor Green
    } else {
        Write-Host "    No .copilot/config.json found." -ForegroundColor Gray
    }
}

function Add-FakeCopilotCredential {
    Write-Host "`n[4] Adding fake Copilot credential..." -ForegroundColor Cyan

    # Add a fake credential that matches `cmdkey /list | findstr /i copilot-cli`
    & cmdkey /generic:copilot-cli-fake-token /user:fre-tester /pass:fake-token-for-fre-test 2>&1 | Out-Null
    if ($LASTEXITCODE -eq 0) {
        Write-Host "    Added credential: copilot-cli-fake-token" -ForegroundColor Green
        Write-Host "    has_credential('copilot') will now return true." -ForegroundColor Green
        Write-Host "    (FRE will skip auth screen and try to connect directly)" -ForegroundColor Gray
    } else {
        Write-Host "    Failed to add credential." -ForegroundColor Red
    }

    # Also create a fake config.json so config-based checks pass
    $copilotDir = "$env:USERPROFILE\.copilot"
    if (-not (Test-Path $copilotDir)) {
        New-Item -ItemType Directory -Path $copilotDir -Force | Out-Null
    }
    $fakeConfig = @{
        firstLaunchAt = (Get-Date).ToString("yyyy-MM-ddTHH:mm:ss.fffZ")
        lastLoggedInUser = @{ host = "https://github.com"; login = "fre-tester" }
        loggedInUsers = @(@{ host = "https://github.com"; login = "fre-tester" })
    } | ConvertTo-Json -Depth 5
    Set-Content "$copilotDir\config.json" $fakeConfig -Encoding UTF8
    Write-Host "    Created fake .copilot/config.json" -ForegroundColor Green
}

# ── Menu ─────────────────────────────────────────────────────────────────────

Write-Host ""
Write-Host "========================================" -ForegroundColor White
Write-Host "  FRE Test Reset - Intelligent Terminal" -ForegroundColor White
Write-Host "========================================" -ForegroundColor White
Write-Host ""
Write-Host "  [1] Reset FRE flag" -ForegroundColor White
Write-Host "  [2] Uninstall GitHub Copilot CLI" -ForegroundColor White
Write-Host "  [3] Clear Copilot credentials" -ForegroundColor White
Write-Host "  [4] Fake Copilot credential (skip auth)" -ForegroundColor White
Write-Host ""
Write-Host "  [A] Full reset (1 + 2 + 3)" -ForegroundColor Yellow
Write-Host "  [Q] Quit" -ForegroundColor Gray
Write-Host ""

$choice = Read-Host "Choose option(s) (e.g. 1, 13, A)"

if ($choice -match '[Qq]') {
    Write-Host "Bye!" -ForegroundColor Gray
    exit 0
}

if ($choice -match '[Aa]') {
    Reset-FreFlag
    Uninstall-CopilotCli
    Clear-CopilotCredentials
} else {
    if ($choice -match '1') { Reset-FreFlag }
    if ($choice -match '2') { Uninstall-CopilotCli }
    if ($choice -match '3') { Clear-CopilotCredentials }
    if ($choice -match '4') { Add-FakeCopilotCredential }
}

Write-Host "`nDone! Launch Intelligent Terminal (F5) to test FRE.`n" -ForegroundColor Cyan
