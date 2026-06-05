<#
.SYNOPSIS
    Compatibility wrapper that waits until Copilot's latest review reaches the PR HEAD.

.DESCRIPTION
    New workflows should prefer agent-owned waiting plus
    02-check-review-status.ps1. This wrapper remains for callers that
    still invoke the old blocking wait command: it performs an immediate
    status snapshot, then polls the status script until the latest
    Copilot review is at the expected HEAD, the PR head changes, or the
    timeout expires.
#>
[CmdletBinding()]
param(
    [string]$Owner,
    [string]$Repo,

    [Parameter(Mandatory = $true)]
    [int]$PrNumber,

    [string]$ExpectedHeadOid,
    [string]$SinceTimestamp,

    [int]$TimeoutMinutes = 35,
    [int]$PollSeconds = 60
)

$ErrorActionPreference = 'Stop'

if ($PollSeconds -lt 30) {
    Write-Warning "PollSeconds=$PollSeconds is below the 30s floor; using 30."
    $PollSeconds = 30
}

function Short {
    param([string]$Sha)
    if (-not $Sha) { return '(none)' }
    if ($Sha.Length -le 7) { return $Sha }
    return $Sha.Substring(0, 7)
}

function ToUtcDt {
    param($Value)
    if ($null -eq $Value -or $Value -eq '') { return $null }
    $s = if ($Value -is [datetime]) {
        if ($Value.Kind -eq [System.DateTimeKind]::Unspecified) {
            [System.DateTime]::SpecifyKind($Value, [System.DateTimeKind]::Utc).ToString('o')
        } else {
            $Value.ToUniversalTime().ToString('o')
        }
    } else {
        [string]$Value
    }
    return [datetime]::Parse(
        $s,
        [System.Globalization.CultureInfo]::InvariantCulture,
        [System.Globalization.DateTimeStyles]::AdjustToUniversal -bor [System.Globalization.DateTimeStyles]::AssumeUniversal
    )
}

function Get-ReviewStatus {
    $script = Join-Path $PSScriptRoot '02-check-review-status.ps1'
    $args = @($script, '-PrNumber', $PrNumber)
    if ($Owner) { $args += @('-Owner', $Owner) }
    if ($Repo) { $args += @('-Repo', $Repo) }
    $json = pwsh @args
    if ($LASTEXITCODE -ne 0) {
        throw "02-check-review-status.ps1 failed (exit $LASTEXITCODE): $json"
    }
    $json | ConvertFrom-Json
}

$start = Get-Date
$initial = Get-ReviewStatus
if (-not $ExpectedHeadOid) { $ExpectedHeadOid = $initial.HeadOid }
if (-not $SinceTimestamp) {
    $SinceTimestamp = if ($initial.LatestCopilotReview) {
        $initial.LatestCopilotReview.submittedAt
    } else {
        '1970-01-01T00:00:00Z'
    }
}
$sinceDt = ToUtcDt $SinceTimestamp

Write-Host "[baseline] expectedHead=$(Short $ExpectedHeadOid) since=$SinceTimestamp timeout=${TimeoutMinutes}min poll=${PollSeconds}s"

$deadline = $start.AddMinutes($TimeoutMinutes)
$last = $initial

while ($true) {
    $current = if ($last) { $last } else { Get-ReviewStatus }
    $last = $null

    if ($current.HeadOid -ne $ExpectedHeadOid) {
        $result = [ordered]@{
            Owner         = $current.Owner
            Repo          = $current.Repo
            PrNumber      = $PrNumber
            Status        = 'HeadAdvanced'
            ExpectedHead  = $ExpectedHeadOid
            LatestReview  = $current.LatestCopilotReview
            NoNewComments = [bool]$current.NoNewComments
            BodyHead      = if ($current.LatestCopilotReview) { $current.LatestCopilotReview.bodyHead } else { $null }
            ElapsedSec    = [int]((Get-Date) - $start).TotalSeconds
            Detail        = "PR head advanced from $(Short $ExpectedHeadOid) to $(Short $current.HeadOid) during wait."
        }
        Write-Host "[stop] $($result.Detail)"
        $result | ConvertTo-Json -Depth 5
        return
    }

    $latestDt = if ($current.LatestCopilotReview) { ToUtcDt $current.LatestCopilotReview.submittedAt } else { $null }
    $freshReviewAtHead = $current.ReviewAtHead -and $latestDt -and $latestDt -gt $sinceDt
    if ($freshReviewAtHead) {
        $result = [ordered]@{
            Owner              = $current.Owner
            Repo               = $current.Repo
            PrNumber           = $PrNumber
            Status             = 'ReviewCompleted'
            ExpectedHead       = $ExpectedHeadOid
            Since              = $SinceTimestamp
            LatestReview       = $current.LatestCopilotReview
            ReviewAtHead       = [bool]$current.ReviewAtHead
            NoNewComments      = [bool]$current.NoNewComments
            OpenThreadCount    = [int]$current.OpenThreadCount
            BodyHead           = if ($current.LatestCopilotReview) { $current.LatestCopilotReview.bodyHead } else { $null }
            ElapsedSec         = [int]((Get-Date) - $start).TotalSeconds
            Detail             = "Copilot latest review is at head $(Short $ExpectedHeadOid). NoNewComments=$($current.NoNewComments); OpenThreadCount=$($current.OpenThreadCount)."
        }
        Write-Host "[done] $($result.Detail)"
        $result | ConvertTo-Json -Depth 5
        return
    }

    if ((Get-Date) -ge $deadline) {
        $result = [ordered]@{
            Owner              = $current.Owner
            Repo               = $current.Repo
            PrNumber           = $PrNumber
            Status             = 'TimedOut'
            ExpectedHead       = $ExpectedHeadOid
            Since              = $SinceTimestamp
            LatestReview       = $current.LatestCopilotReview
            ReviewAtHead       = [bool]$current.ReviewAtHead
            NoNewComments      = [bool]$current.NoNewComments
            OpenThreadCount    = [int]$current.OpenThreadCount
            BodyHead           = if ($current.LatestCopilotReview) { $current.LatestCopilotReview.bodyHead } else { $null }
            ElapsedSec         = [int]((Get-Date) - $start).TotalSeconds
            Detail             = "No Copilot review at head $(Short $ExpectedHeadOid) landed within $TimeoutMinutes min."
        }
        $result | ConvertTo-Json -Depth 5
        return
    }

    $remaining = [int]($deadline - (Get-Date)).TotalSeconds
    $latestOid = if ($current.LatestCopilotReview) { Short $current.LatestCopilotReview.commitOid } else { '(none)' }
    Write-Host "[poll] no review at HEAD yet (latestHead=$latestOid); ${remaining}s left"
    Start-Sleep -Seconds ([Math]::Min($PollSeconds, [Math]::Max(1, $remaining)))
}
