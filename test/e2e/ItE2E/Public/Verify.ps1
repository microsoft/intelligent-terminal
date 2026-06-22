# Verify.ps1 — assertion oracles. Each throws a descriptive error on failure (Pester
# turns the throw into a failed test). They are deliberately thin over the observe layer.

function Assert-Setting {
    <# Assert a settings.json top-level key equals the expected value. #>
    [CmdletBinding()]
    param([Parameter(Mandatory)]$App, [Parameter(Mandatory)][string]$Key, [Parameter(Mandatory)][AllowNull()]$Value)
    $actual = Get-WtSetting -App $App -Key $Key
    if ("$actual" -ne "$Value") { throw "Assert-Setting: '$Key' expected '$Value' but was '$actual'." }
}

function Assert-State {
    <# Assert a state.json key equals the expected value. #>
    [CmdletBinding()]
    param([Parameter(Mandatory)]$App, [Parameter(Mandatory)][string]$Key, [Parameter(Mandatory)][AllowNull()]$Value)
    $actual = (Get-WtStateObject -App $App).$Key
    if ("$actual" -ne "$Value") { throw "Assert-State: '$Key' expected '$Value' but was '$actual'." }
}

function Assert-Ui {
    <# Assert a UI element exists (default), is gone, or has a value. winapp ui wait-for. #>
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)]$App, [Parameter(Mandatory)][string]$Selector,
        [switch]$Gone, [string]$Value, [string]$Property, [switch]$Contains, [int]$TimeoutSec = 15
    )
    $splat = @{ App = $App; Selector = $Selector; TimeoutSec = $TimeoutSec; Quiet = $true }
    if ($Gone) { $splat.Gone = $true }
    if ($PSBoundParameters.ContainsKey('Value')) { $splat.Value = $Value }
    if ($Property) { $splat.Property = $Property }
    if ($Contains) { $splat.Contains = $true }
    if (-not (Wait-UiElement @splat)) {
        $shot = Save-UiScreenshot -App $App -Path (Join-Path $env:TEMP "ite2e-assertui-fail.png")
        throw "Assert-Ui: element '$Selector' did not satisfy condition within ${TimeoutSec}s. Screenshot: $shot"
    }
}
Set-Alias -Name Assert-Xaml -Value Assert-Ui

function Assert-Script {
    <# Run a PowerShell scriptblock; fail unless it exits 0 (and optionally matches output). #>
    [CmdletBinding()]
    param([Parameter(Mandatory)][scriptblock]$ScriptBlock, [string]$MatchOutput)
    # Reset so a stale exit code from an earlier native call can't cause a false failure
    # when the scriptblock contains no native command.
    $global:LASTEXITCODE = 0
    $out = & $ScriptBlock 2>&1 | Out-String
    if ($LASTEXITCODE -and $LASTEXITCODE -ne 0) { throw "Assert-Script: scriptblock exited $LASTEXITCODE. Output: $out" }
    if ($MatchOutput -and $out -notmatch $MatchOutput) { throw "Assert-Script: output did not match /$MatchOutput/. Output: $out" }
}

function Assert-Pane {
    <# Assert a pane's buffer matches a regex. #>
    [CmdletBinding()]
    param([Parameter(Mandatory)]$App, [Parameter(Mandatory)][string]$SessionId, [Parameter(Mandatory)][string]$Match, [int]$TimeoutSec = 15)
    $ok = Test-Until -TimeoutSec $TimeoutSec -Condition { (Get-WtCapture -App $App -SessionId $SessionId -MaxLines 200) -match $Match }
    if (-not $ok) { throw "Assert-Pane: pane $SessionId buffer never matched /$Match/ within ${TimeoutSec}s." }
}

function Assert-WtEvent {
    <# Assert a matching event was observed on a (pre-started) listener. #>
    [CmdletBinding()]
    param([Parameter(Mandatory)]$Listener, [Parameter(Mandatory)][scriptblock]$Predicate, [int]$TimeoutSec = 30)
    $ev = Wait-WtEvent -Listener $Listener -Predicate $Predicate -TimeoutSec $TimeoutSec
    if (-not $ev) { throw "Assert-WtEvent: no matching event within ${TimeoutSec}s." }
    $ev
}

function Assert-Log {
    <# Assert a log pattern appears in this test's log slice within the timeout. #>
    [CmdletBinding()]
    param([Parameter(Mandatory)]$App, [Parameter(Mandatory)][string]$Pattern, [string]$Name = '*', [int]$TimeoutSec = 20)
    $ok = Test-Until -TimeoutSec $TimeoutSec -Condition { (Get-ItLogText -App $App -Name $Name -SinceStart) -match $Pattern }
    if (-not $ok) { throw "Assert-Log: pattern /$Pattern/ not found in logs ($Name) within ${TimeoutSec}s." }
}

# ── AI oracle ────────────────────────────────────────────────────────────────
# Assert-AI / Test-AIClaim live in Public/AiOracle.ps1. They wrap an agent CLI's
# non-interactive print mode (copilot -p / claude -p) directly — independent of wta.
