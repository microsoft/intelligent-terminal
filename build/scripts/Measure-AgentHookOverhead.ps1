<#
.SYNOPSIS
    Compare agent-hook overhead across three Copilot hook-bundle generations.

.DESCRIPTION
    Configurations under test:

      A  PowerShell bridge   hooks.json -> powershell.exe -File send-event.ps1 -> wtcli send-event
                             subscribes PreToolUse + PostToolUse + PostToolUseFailure
      B  native bridge       hooks.json -> wtcli.exe agent-hook   (PR #571)
                             subscribes PreToolUse
      C  native, no per-tool hooks                                (PR #631)
                             subscribes neither

    Two measurements are reported because they answer different questions.

    Part 1 - hook replay (deterministic).
      Replays the exact command string each configuration ships, in the exact
      sequence and count a turn produces, without invoking the model. This is
      the only part that isolates the variable under test: model latency is
      identical across A/B/C by construction, so including it only adds noise.

    Part 2 - end-to-end (noisy).
      Runs the real CLI on the same two prompts under each configuration.
      A trivial turn was measured at ~28s wall with ~19s of model time, so
      run-to-run variance is on the order of the effect being measured. Medians
      are reported, and the spread is printed so a reader can judge whether the
      end-to-end ordering is meaningful or noise.

    Every hook path in all three configurations exits at its own
    WT_COM_CLSID gate when run outside an Intelligent Terminal pane, so the COM
    round trip is excluded from both parts. That is the same for A, B and C, so
    it does not bias the comparison -- but it means absolute numbers here are a
    lower bound on in-pane cost.

.PARAMETER Reps
    Repetitions per measurement. Part 1 is cheap; Part 2 costs a model turn each.

.PARAMETER SkipEndToEnd
    Run only the deterministic replay. No model turns are consumed.
#>
[CmdletBinding()]
param(
    [ValidateRange(1, 20)]
    [int]$Reps = 3,

    [switch]$SkipEndToEnd
)

$ErrorActionPreference = 'Stop'

$RepoRoot     = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
$InstalledDir = Join-Path $env:USERPROFILE '.copilot\installed-plugins\wt-local\wt-agent-hooks'
$HooksJson    = Join-Path $InstalledDir 'hooks\hooks.json'
$BridgePs1    = Join-Path $InstalledDir 'hooks\send-event.ps1'
# Both historical configurations are pinned to commits rather than branch names
# so the benchmark reproduces on any clone: branches get deleted after merge,
# and a shallow or offline clone may not have them at all.
#   A = the parent of the commit that deleted the PowerShell bridge.
#   B = the merge of the native-bridge work, which still subscribed PreToolUse.
$LegacyCommit       = '9a9a17b280ddc24209914ed83dd8ed00acb5e830'
$NativeBridgeCommit = 'e610bacc181c488d806fef1e84520761adbd2be4'

# Hook topics a single turn produces, in order. Session lifecycle is fixed;
# per-tool topics repeat once per tool call and are what the configurations
# actually differ on.
$SessionTopics = @('agent.session.start', 'agent.prompt.submit', 'agent.stop', 'agent.session.end')
$PerToolTopics = @{
    A = @('agent.tool.starting', 'agent.tool.finished')  # PreToolUse + PostToolUse
    B = @('agent.tool.starting')                         # PreToolUse
    C = @()                                              # none
}

$Prompts = @(
    [pscustomobject]@{
        Id     = 'P1-no-tools'
        Tools  = 0
        Text   = 'Reply with exactly the word READY. Do not use any tools.'
    }
    [pscustomobject]@{
        Id     = 'P2-three-tools'
        Tools  = 3
        Text   = 'Using your shell tool, run these three commands one at a time, ' +
                 'as three separate tool calls: first "echo alpha", then "echo beta", ' +
                 'then "echo gamma". Do not combine them into one command. ' +
                 'Then reply with exactly the word DONE.'
    }
)

function Get-RepoFileAt {
    param(
        [Parameter(Mandatory)][string]$Revision,
        [Parameter(Mandatory)][string]$RepoPath
    )
    Push-Location $RepoRoot
    try {
        $text = & git --no-pager show "${Revision}:$RepoPath" 2>$null
        if ($LASTEXITCODE -ne 0) { throw "cannot read $RepoPath at $Revision" }
        ($text | Out-String)
    }
    finally { Pop-Location }
}

# The command string a configuration runs for one topic, with placeholders
# already resolved so the replay measures dispatch rather than path expansion.
function Get-HookCommand {
    param(
        [Parameter(Mandatory)][ValidateSet('A', 'B', 'C')][string]$Config,
        [Parameter(Mandatory)][string]$Topic
    )
    switch ($Config) {
        'A' {
            # Nested shell: the CLI's own PowerShell dispatches a *second*
            # powershell.exe to run the 346-line bridge script.
            "powershell -NoProfile -NonInteractive -ExecutionPolicy Bypass -File `"$BridgePs1`" -CliSource copilot $Topic"
        }
        default {
            "try { wtcli.exe agent-hook --cli-source copilot --event $Topic } catch { }; exit 0"
        }
    }
}

# One hook dispatch, run the way Copilot dispatches it: pwsh -Command <string>.
function Measure-HookDispatch {
    param([Parameter(Mandatory)][string]$Command)
    $sw = [Diagnostics.Stopwatch]::StartNew()
    & pwsh.exe -NoProfile -NonInteractive -Command $Command 2>&1 | Out-Null
    $sw.Stop()
    $sw.Elapsed.TotalMilliseconds
}

function Get-TurnTopics {
    param(
        [Parameter(Mandatory)][ValidateSet('A', 'B', 'C')][string]$Config,
        [Parameter(Mandatory)][int]$ToolCalls
    )
    $topics = [System.Collections.Generic.List[string]]::new()
    $topics.Add('agent.session.start')
    $topics.Add('agent.prompt.submit')
    for ($i = 0; $i -lt $ToolCalls; $i++) {
        foreach ($t in $PerToolTopics[$Config]) { $topics.Add($t) }
    }
    $topics.Add('agent.stop')
    $topics.Add('agent.session.end')
    $topics
}

function Get-Median {
    param([double[]]$Values)
    $sorted = $Values | Sort-Object
    $n = $sorted.Count
    if ($n -eq 0) { return 0 }
    if ($n % 2 -eq 1) { return $sorted[[int](($n - 1) / 2)] }
    ($sorted[$n / 2 - 1] + $sorted[$n / 2]) / 2
}

# ---- configuration install / restore -------------------------------------

function Backup-InstalledBundle {
    $backup = Join-Path $env:TEMP ("wt-agent-hooks-backup-" + [guid]::NewGuid().ToString('N'))
    Copy-Item -LiteralPath $InstalledDir -Destination $backup -Recurse -Force
    $backup
}

function Restore-InstalledBundle {
    param([Parameter(Mandatory)][string]$Backup)
    if (Test-Path $InstalledDir) { Remove-Item -LiteralPath $InstalledDir -Recurse -Force }
    Copy-Item -LiteralPath $Backup -Destination $InstalledDir -Recurse -Force
}

function Install-Config {
    param([Parameter(Mandatory)][ValidateSet('A', 'B', 'C')][string]$Config)

    if ($Config -eq 'A') {
        Get-RepoFileAt -Revision "${LegacyCommit}^" -RepoPath 'tools/wta/wt-agent-hooks/copilot/wt-agent-hooks/hooks/send-event.ps1' |
            Set-Content -LiteralPath $BridgePs1 -Encoding utf8
        # Absolute path rather than the plugin-root placeholder: this writes the
        # *installed* copy directly, so no expansion step runs over it. The path
        # is JSON-escaped first -- a Windows path's single backslashes are
        # invalid escapes inside a JSON string.
        $json = Get-RepoFileAt -Revision "${LegacyCommit}^" -RepoPath 'tools/wta/wt-agent-hooks/copilot/wt-agent-hooks/hooks/hooks.json'
        $escaped = $BridgePs1 -replace '\\', '\\'
        $json = $json -replace '\$\{CLAUDE_PLUGIN_ROOT\}/hooks/send-event\.ps1', $escaped
        $json = $json -replace '\$\{COPILOT_PLUGIN_ROOT\}/hooks/send-event\.ps1', $escaped
        $null = $json | ConvertFrom-Json   # fail loudly rather than benchmarking a broken manifest
        $json | Set-Content -LiteralPath $HooksJson -Encoding utf8
        return
    }

    if (Test-Path $BridgePs1) { Remove-Item -LiteralPath $BridgePs1 -Force }
    $source = if ($Config -eq 'B') {
        Get-RepoFileAt -Revision $NativeBridgeCommit -RepoPath 'tools/wta/wt-agent-hooks/copilot/wt-agent-hooks/hooks/hooks.json'
    }
    else {
        Get-Content -LiteralPath (Join-Path $RepoRoot 'tools\wta\wt-agent-hooks\copilot\wt-agent-hooks\hooks\hooks.json') -Raw
    }
    $source | Set-Content -LiteralPath $HooksJson -Encoding utf8
}

function Get-SubscribedEvents {
    (Get-Content -LiteralPath $HooksJson -Raw | ConvertFrom-Json).hooks.PSObject.Properties.Name
}

# ---- part 1: deterministic hook replay ------------------------------------

function Invoke-ReplayBenchmark {
    Write-Host ''
    Write-Host '== Part 1: hook replay (no model, deterministic) ==' -ForegroundColor Cyan

    # Per-topic dispatch cost is identical across topics within a configuration,
    # so measure the shape once per configuration and multiply by the count.
    # Each configuration is installed first: A's command targets send-event.ps1
    # by path, and measuring it before that file exists would time a failed
    # lookup rather than the bridge it is supposed to represent.
    $unit = @{}
    foreach ($config in 'A', 'B', 'C') {
        Install-Config -Config $config
        if ($config -eq 'A' -and -not (Test-Path $BridgePs1)) {
            throw 'configuration A did not produce send-event.ps1; the replay would measure a missing-file error.'
        }
        $command = Get-HookCommand -Config $config -Topic 'agent.stop'
        Measure-HookDispatch -Command $command | Out-Null   # warm-up
        $samples = 1..$Reps | ForEach-Object { Measure-HookDispatch -Command $command }
        $unit[$config] = Get-Median -Values $samples
        '{0}  per-hook dispatch: {1,7:N0} ms  (n={2})' -f $config, $unit[$config], $Reps | Write-Host
    }

    Write-Host ''
    '{0,-16} {1,-6} {2,7} {3,12} {4,12}' -f 'prompt', 'config', 'hooks', 'total (ms)', 'vs C' | Write-Host
    '{0}' -f ('-' * 60) | Write-Host

    $results = @()
    foreach ($prompt in $Prompts) {
        $baseline = $null
        foreach ($config in 'A', 'B', 'C') {
            $topics = Get-TurnTopics -Config $config -ToolCalls $prompt.Tools
            $total  = $topics.Count * $unit[$config]
            if ($config -eq 'C') { $baseline = $total }
            $results += [pscustomobject]@{
                Prompt = $prompt.Id; Config = $config
                Hooks  = $topics.Count; TotalMs = [math]::Round($total)
            }
        }
        foreach ($r in $results | Where-Object Prompt -eq $prompt.Id) {
            $delta = if ($baseline -gt 0) { '{0:+#;-#;0} ms' -f [math]::Round($r.TotalMs - $baseline) } else { '-' }
            '{0,-16} {1,-6} {2,7} {3,12:N0} {4,12}' -f $r.Prompt, $r.Config, $r.Hooks, $r.TotalMs, $delta | Write-Host
        }
    }
    $results
}

# ---- part 2: end-to-end ---------------------------------------------------

function Invoke-EndToEndBenchmark {
    Write-Host ''
    Write-Host '== Part 2: end-to-end (real model turns; noisy) ==' -ForegroundColor Cyan
    '{0,-16} {1,-6} {2,10} {3,10} {4,10} {5,8} {6,12}' -f 'prompt', 'config', 'median s', 'min s', 'max s', 'events', 'markers' | Write-Host
    '{0}' -f ('-' * 80) | Write-Host

    $results = @()
    foreach ($prompt in $Prompts) {
        foreach ($config in 'A', 'B', 'C') {
            Install-Config -Config $config
            $events = @(Get-SubscribedEvents).Count
            $samples = @()
            $toolsSeen = @()
            for ($i = 0; $i -lt $Reps; $i++) {
                $sw = [Diagnostics.Stopwatch]::StartNew()
                $out = & copilot -p $prompt.Text --allow-all-tools 2>&1 | Out-String
                $sw.Stop()
                $samples += $sw.Elapsed.TotalSeconds
                # The turn is only comparable across configurations if it did the
                # same work. P2 asks for three separately-echoed markers, so their
                # presence is the cheapest available check that the tool count did
                # not drift between runs.
                $toolsSeen += @('alpha', 'beta', 'gamma' | Where-Object { $out -match $_ }).Count
            }
            $results += [pscustomobject]@{
                Prompt = $prompt.Id; Config = $config; Events = $events
                MedianS = [math]::Round((Get-Median -Values $samples), 1)
                MinS = [math]::Round(($samples | Measure-Object -Minimum).Minimum, 1)
                MaxS = [math]::Round(($samples | Measure-Object -Maximum).Maximum, 1)
                Markers = ($toolsSeen -join '/')
            }
            $r = $results[-1]
            '{0,-16} {1,-6} {2,10:N1} {3,10:N1} {4,10:N1} {5,8} {6,12}' -f `
                $r.Prompt, $r.Config, $r.MedianS, $r.MinS, $r.MaxS, $r.Events, $r.Markers | Write-Host
        }
    }
    $results
}

# ---- main -----------------------------------------------------------------

if (-not (Get-Command wtcli.exe -ErrorAction SilentlyContinue)) {
    throw 'wtcli.exe is not on PATH; configurations B and C would measure a failed lookup instead of the bridge.'
}
if (-not (Test-Path $InstalledDir)) {
    throw "no installed Copilot bundle at $InstalledDir; run 'wta hooks install --cli copilot' first."
}

$backup = Backup-InstalledBundle
Write-Host "Installed bundle backed up to $backup"
try {
    $replay = Invoke-ReplayBenchmark
    $e2e = if ($SkipEndToEnd) { @() } else { Invoke-EndToEndBenchmark }
    [pscustomobject]@{ Replay = $replay; EndToEnd = $e2e } | Out-Null
}
finally {
    Restore-InstalledBundle -Backup $backup
    Remove-Item -LiteralPath $backup -Recurse -Force -ErrorAction SilentlyContinue
    Write-Host ''
    Write-Host "Installed bundle restored; subscribed events: $((Get-SubscribedEvents) -join ', ')" -ForegroundColor Green
}
