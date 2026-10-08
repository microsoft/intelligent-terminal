param(
    [Parameter(Mandatory)][string]$FunnelCapture,
    [Parameter(Mandatory)][string]$SidebarCapture,
    [Parameter(Mandatory)][string]$OutputPath
)
$ErrorActionPreference = 'Stop'
Import-Module Pester -MinimumVersion 5.0
$funnel = @(Get-Content (Join-Path $FunnelCapture 'scoped-events.json') -Raw | ConvertFrom-Json)
$sidebar = @(Get-Content (Join-Path $SidebarCapture 'scoped-events.json') -Raw | ConvertFrom-Json)

function Select-Phase {
    param([string]$Capture, [object[]]$Events, [string]$Phase, [string]$Name)
    $phases = Get-Content (Join-Path $Capture 'phases.json') -Raw | ConvertFrom-Json -AsHashtable
    $bounds = $phases[$Phase]
    if (-not $bounds -or $bounds.Error -or $bounds.CleanupError) { throw "Unverified phase: $Phase" }
    $clock = Get-Content (Join-Path $Capture 'phase-clock.json') -Raw | ConvertFrom-Json
    $Events | Where-Object {
        $time = ([DateTimeOffset]$_.Timestamp).UtcDateTime.AddTicks($clock.correctionTicks)
        $_.Name -eq $Name -and $time -ge ([DateTimeOffset]$bounds.StartedUtc).UtcDateTime -and
            $time -le ([DateTimeOffset]$bounds.EndedUtc).UtcDateTime
    }
}

function Measure-Join {
    param([string]$Name, [object[]]$Left, [object[]]$Right, [string[]]$Keys)
    $cohort = @{}
    foreach ($event in $Left) {
        $parts = @($Keys | ForEach-Object {
            $value = [string]$event.Fields.$_
            if (-not $value) { throw "Missing cohort key $_ for $Name" }
            $value
        })
        $key = $parts -join '|'
        $time = [DateTimeOffset]$event.Timestamp
        if (-not $cohort.ContainsKey($key) -or $time -lt $cohort[$key]) { $cohort[$key] = $time }
    }
    $matched = [Collections.Generic.HashSet[string]]::new()
    foreach ($event in $Right) {
        $key = @($Keys | ForEach-Object { [string]$event.Fields.$_ }) -join '|'
        if ($cohort.ContainsKey($key)) {
            if ([DateTimeOffset]$event.Timestamp -lt $cohort[$key]) { throw "Outcome precedes entry for $Name" }
            [void]$matched.Add($key)
        }
    }
    [pscustomobject]@{
        Funnel = $Name
        Denominator = $cohort.Count
        Numerator = $matched.Count
        Unmatched = $cohort.Count - $matched.Count
        Ratio = if ($cohort.Count) { $matched.Count / $cohort.Count } else { $null }
    }
}

$prompts = @(Select-Phase $FunnelCapture $funnel conversation AgentPromptSent)
$completed = @(Select-Phase $FunnelCapture $funnel conversation AgentResponseComplete)
@($prompts | Where-Object { $_.Fields.IsAutofix -notin @('false', '0') }) | Should -HaveCount 0
@($completed | Where-Object { $_.Fields.Success -notin @('true', '1') }) | Should -HaveCount 0
$detections = @()
$offers = @()
$accepts = @()
foreach ($decision in @('Run', 'Insert', 'Reject')) {
    $detections += @(Select-Phase $FunnelCapture $funnel "offer-$decision" ErrorDetected |
        Where-Object { $_.Provider -eq '4cfcff80-4e6b-5bfd-8ea1-d38e1226f70b' })
    $offers += @(Select-Phase $FunnelCapture $funnel "offer-$decision" ErrorFixOffered)
    $accepts += @(Select-Phase $FunnelCapture $funnel "offer-$decision" ErrorFixAccepted)
}
$runs = @(Select-Phase $FunnelCapture $funnel offer-Run ErrorFixRunStarted)
$results = @(Select-Phase $FunnelCapture $funnel offer-Run ErrorFixRunResult)
@($results | Where-Object { $_.Fields.Outcome -ne 'unobservable' }) | Should -HaveCount 0
$entries = @(Select-Phase $FunnelCapture $funnel palette CommandPaletteAgentPromptEntered)
$submissions = @(Select-Phase $FunnelCapture $funnel palette CommandPaletteDispatchedAgentPrompt |
    Where-Object { $_.Fields.IsBackgroundMode -in @('false', '0') })
$marked = @(foreach ($phase in @('pin-first', 'pin-second-hidden-first', 'repin')) {
    Select-Phase $SidebarCapture $sidebar $phase KeepRunningMarked
})
$detached = @(Select-Phase $SidebarCapture $sidebar retain-restore KeepRunningDetached)
$started = @(Select-Phase $SidebarCapture $sidebar retain-restore KeepRunningReattachStarted)
$restored = @(Select-Phase $SidebarCapture $sidebar retain-restore KeepRunningReattached |
    Where-Object { $_.Fields.Outcome -eq 'live' })
$eligible = @($restored | Where-Object { $_.Fields.HasAgentSession -in @('true', '1') })
$continued = @(Select-Phase $SidebarCapture $sidebar reattach-prompt AgentPromptSent |
    Where-Object { $_.Fields.IsAutofix -in @('false', '0') -and $_.Fields.Reattached -in @('true', '1') })
$sidebarPrompts = @(foreach ($phase in @('session-id-privacy', 'pre-reattach-prompt', 'reattach-prompt')) {
    Select-Phase $SidebarCapture $sidebar $phase AgentPromptSent |
        Where-Object { $_.Fields.IsAutofix -in @('false', '0') }
})

$metrics = @(
    Measure-Join 'User sent turns -> successful RPC completion' $prompts $completed @('SessionId', 'TurnId', 'IsAutofix')
    Measure-Join 'First user prompt -> second prompt in observation session' @($sidebarPrompts | Where-Object { $_.Fields.UserPromptOrdinal -eq 'First' }) @($sidebarPrompts | Where-Object { $_.Fields.UserPromptOrdinal -eq 'Second' }) @('SessionId')
    Measure-Join 'Detected flows -> presented offers' $detections $offers @('OfferId', 'Source')
    Measure-Join 'Presented offers -> accepted Run' $offers $accepts @('OfferId', 'Source')
    Measure-Join 'Accepted offers -> executor Run dispatch' $accepts $runs @('OfferId')
    Measure-Join 'Run starts -> observed unknown execution result' $runs $results @('OfferId', 'RunId')
    Measure-Join 'Foreground palette visits -> submissions' $entries $submissions @('EntryId')
    Measure-Join 'Keep opt-ins -> observed retained close (not close success rate)' $marked $detached @('KeepId')
    Measure-Join 'Retained flows -> successful live restore' $detached $restored @('KeepId')
    Measure-Join 'Restore attempts -> live results' $started $restored @('AttemptId')
    Measure-Join 'Eligible restored attempts -> user prompt' $eligible $continued @('KeepId', 'AttemptId')
)
$expected = @(@(2, 2), @(2, 1), @(3, 3), @(3, 1), @(1, 1), @(1, 1), @(2, 1), @(3, 1), @(1, 1), @(1, 1), @(1, 1))
for ($i = 0; $i -lt $metrics.Count; $i++) {
    $metrics[$i].Denominator | Should -Be $expected[$i][0] -Because $metrics[$i].Funnel
    $metrics[$i].Numerator | Should -Be $expected[$i][1] -Because $metrics[$i].Funnel
}

# Robustness controls operate on copies of captured events, not new telemetry.
$duplicate = Measure-Join 'duplicate delivery' @($prompts + $prompts) @($completed + $completed) @('SessionId', 'TurnId')
$duplicate.Denominator | Should -Be 2
$duplicate.Numerator | Should -Be 2
$missing = Measure-Join 'missing completion' $prompts @($completed[0]) @('SessionId', 'TurnId')
$missing.Denominator | Should -Be 2
$missing.Numerator | Should -Be 1
$missing.Unmatched | Should -Be 1
$empty = Measure-Join 'empty cohort' @() $completed @('SessionId', 'TurnId')
$empty.Ratio | Should -BeNullOrEmpty
$other = $completed[0] | ConvertTo-Json -Depth 8 | ConvertFrom-Json
$other.Fields.SessionId = [guid]::NewGuid().ToString()
$outside = Measure-Join 'unrelated observation' $prompts @($other) @('SessionId', 'TurnId')
$outside.Numerator | Should -Be 0

[ordered]@{
    FunnelCapture = $FunnelCapture
    SidebarCapture = $SidebarCapture
    Scope = 'Local deterministic scenarios, not backend device cohorts or production conversion rates'
    Metrics = $metrics
    JoinControls = 'Duplicate delivery, missing result, unrelated session, and empty denominator passed'
    ExecutionSuccessRate = $null
    BackendIngestionVerified = $false
    D7D28Verified = $false
} | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $OutputPath
$metrics | Format-Table -AutoSize
