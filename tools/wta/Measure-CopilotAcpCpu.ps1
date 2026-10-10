# Copyright (c) Microsoft Corporation.
# Licensed under the MIT license.

#Requires -Version 7.0
<#
.SYNOPSIS
    Compare idle, session/list, and restoration CPU in standalone Copilot ACP.
.DESCRIPTION
    Starts only its own processes and restores only a session it creates.
    Continuously drains both output pipes, records RPC latency and timeouts,
    and samples cumulative CPU time of the server and its observed descendants.
    Results contain no session-list contents, chat text, or stderr contents.
    Existing Terminal processes, settings, and sessions are not modified.
.PARAMETER CreateTranscript
    Make one synthetic, tool-denied model request, then test restoration.
    This consumes model credits and supplies a persisted conversation to replay.
    Without this switch, only idle/list/idle-after-polling are tested, with no model request.
    Custom instructions and built-in MCP servers are disabled in all scenarios
    when this is selected.
.PARAMETER PollPolicy
    Fixed sends a request every five seconds even after a timeout, testing the
    suspected aggressive polling. Backoff retains one request until its actual
    response, waits five seconds after success, and uses 5/10/20/40/60-second
    delays after error responses. It does not implement WTA's process recovery.
.EXAMPLE
    pwsh -File tools\wta\Measure-CopilotAcpCpu.ps1
.EXAMPLE
    pwsh -File tools\wta\Measure-CopilotAcpCpu.ps1 -CreateTranscript -DurationSeconds 60
#>
[CmdletBinding()]
param(
    [string]$CopilotPath = 'copilot',
    [ValidateRange(10, 3600)][int]$DurationSeconds = 60,
    [ValidateRange(0, 300)][int]$WarmupSeconds = 10,
    [ValidateSet('Fixed', 'Backoff')][string]$PollPolicy = 'Fixed',
    [ValidateRange(1, 100)][double]$BusyCorePercent = 20,
    [switch]$CreateTranscript,
    [string]$OutputDir
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
if (-not $IsWindows) { throw 'This probe requires Windows process CPU accounting.' }
$CopilotPath = (Get-Command $CopilotPath -CommandType Application -ErrorAction Stop | Select-Object -First 1).Source
if (-not $OutputDir) {
    $OutputDir = Join-Path $PSScriptRoot "target\acp-cpu-probe\$([DateTime]::UtcNow.ToString('yyyyMMdd-HHmmss-fff'))"
}
$OutputDir = [IO.Path]::GetFullPath($OutputDir)
if (Test-Path -LiteralPath $OutputDir) { throw "Output directory already exists: $OutputDir" }
New-Item -ItemType Directory -Path (Join-Path $OutputDir 'workspace') -Force | Out-Null
$Workspace = Join-Path $OutputDir 'workspace'
$Clock = [Diagnostics.Stopwatch]::StartNew()
$CpuCount = [int](Get-CimInstance Win32_ComputerSystem).NumberOfLogicalProcessors
$Samples = [Collections.Generic.List[object]]::new()
$Phases = [Collections.Generic.List[object]]::new()
$Clients = [Collections.Generic.List[object]]::new()
$ProbeError = $null
$SeedSessionId = $null
$TranscriptCreated = $false
$ReplayVerified = $false
$Version = (& $CopilotPath --no-auto-update --version | Out-String).Trim()
if ($LASTEXITCODE -ne 0) { throw 'Could not determine the bundled Copilot version.' }

function Send-Message($Client, [hashtable]$Message) {
    $Client.Process.StandardInput.WriteLine(($Message | ConvertTo-Json -Depth 30 -Compress))
    $Client.Process.StandardInput.Flush()
}

function Start-Request($Client, [string]$Method, [hashtable]$Params, [double]$TimeoutSeconds = 5) {
    $Client.NextId++
    $Request = [pscustomobject]@{
        id = $Client.NextId
        method = $Method
        sent_seconds = $Clock.Elapsed.TotalSeconds
        timeout_seconds = $TimeoutSeconds
        timed_out = $false
        completed_seconds = $null
        latency_ms = $null
        outcome = 'pending'
        error_code = $null
        session_count = $null
        has_next_cursor = $false
        response = $null
    }
    $Client.Requests.Add($Request)
    $Client.Pending[[string]$Request.id] = $Request
    Send-Message $Client @{ jsonrpc = '2.0'; id = $Request.id; method = $Method; params = $Params }
    return $Request
}

function Receive-Messages($Client) {
    while ($null -ne $Client.StderrTask -and $Client.StderrTask.IsCompleted) {
        $Line = $Client.StderrTask.GetAwaiter().GetResult()
        if ($null -eq $Line) { $Client.StderrTask = $null; break }
        $Client.StderrLines++
        $Client.StderrTask = $Client.Process.StandardError.ReadLineAsync()
    }
    while ($null -ne $Client.StdoutTask -and $Client.StdoutTask.IsCompleted) {
        $Line = $Client.StdoutTask.GetAwaiter().GetResult()
        if ($null -eq $Line) { throw "ACP stdout closed (server PID $($Client.Process.Id))." }
        $Client.StdoutTask = $Client.Process.StandardOutput.ReadLineAsync()
        if ([string]::IsNullOrWhiteSpace($Line)) { continue }
        $Message = ConvertFrom-Json -InputObject $Line -AsHashtable -Depth 100
        if ($Message.ContainsKey('method')) {
            $Client.Notifications++
            if ($Message.method -eq 'session/update') {
                $Client.SessionUpdates++
                $UpdateType = [string]$Message.params.update.sessionUpdate
                if (-not $Client.UpdateTypes.ContainsKey($UpdateType)) { $Client.UpdateTypes[$UpdateType] = 0 }
                $Client.UpdateTypes[$UpdateType]++
            }
            if ($Message.ContainsKey('id')) {
                $Client.ServerRequests++
                if ($Message.method -eq 'session/request_permission') {
                    Send-Message $Client @{
                        jsonrpc = '2.0'; id = $Message.id
                        result = @{ outcome = @{ outcome = 'cancelled' } }
                    }
                }
                else {
                    Send-Message $Client @{
                        jsonrpc = '2.0'; id = $Message.id
                        error = @{ code = -32601; message = 'CPU probe does not execute tools or access files.' }
                    }
                }
            }
            continue
        }
        if (-not $Message.ContainsKey('id') -or -not $Client.Pending.ContainsKey([string]$Message.id)) {
            throw 'ACP returned a response with an unknown request ID.'
        }
        $Request = $Client.Pending[[string]$Message.id]
        $Request.completed_seconds = $Clock.Elapsed.TotalSeconds
        $Request.latency_ms = 1000 * ($Request.completed_seconds - $Request.sent_seconds)
        # A response arriving between pump iterations can still miss its deadline.
        $Request.timed_out = $Request.timed_out -or ($Request.latency_ms -ge 1000 * $Request.timeout_seconds)
        $Request.outcome = if ($Message.ContainsKey('error')) { 'error' } else { 'ok' }
        if ($Message.ContainsKey('error')) { $Request.error_code = $Message.error.code }
        if ($Request.method -eq 'session/list' -and $Message.ContainsKey('result')) {
            $Request.session_count = @($Message.result.sessions).Count
            $Request.has_next_cursor = $Message.result.ContainsKey('nextCursor') -and [bool]$Message.result.nextCursor
        }
        if ($Request.method -in @('initialize', 'session/new', 'session/load', 'session/prompt')) {
            $Request.response = $Message
        }
        $Client.Pending.Remove([string]$Message.id)
    }
    foreach ($Request in $Client.Pending.Values) {
        if (-not $Request.timed_out -and $Clock.Elapsed.TotalSeconds - $Request.sent_seconds -ge $Request.timeout_seconds) {
            $Request.timed_out = $true
            $Request.outcome = 'timeout'
            Write-Warning "$($Client.Name): $($Request.method) #$($Request.id) timed out; server work may still be running."
        }
    }
    if ($Client.Process.HasExited) {
        throw "ACP server PID $($Client.Process.Id) exited with code $($Client.Process.ExitCode)."
    }
}

function Wait-Request($Client, $Request) {
    while ($null -eq $Request.completed_seconds -and -not $Request.timed_out) {
        Receive-Messages $Client
        Start-Sleep -Milliseconds 20
    }
    if ($Request.timed_out -or $Request.outcome -ne 'ok') {
        throw "$($Client.Name): $($Request.method) failed ($($Request.outcome), error code $($Request.error_code))."
    }
    return $Request.response.result
}

function Start-Server([string]$Name, [string]$ResumeSessionId) {
    $Info = [Diagnostics.ProcessStartInfo]::new()
    $Info.FileName = $CopilotPath
    $Info.WorkingDirectory = $Workspace
    $Info.UseShellExecute = $false
    $Info.CreateNoWindow = $true
    $Info.RedirectStandardInput = $true
    $Info.RedirectStandardOutput = $true
    $Info.RedirectStandardError = $true
    $Info.StandardInputEncoding = [Text.UTF8Encoding]::new($false)
    $Info.StandardOutputEncoding = [Text.Encoding]::UTF8
    $Info.StandardErrorEncoding = [Text.Encoding]::UTF8
    foreach ($Argument in @('--acp', '--stdio', '--no-auto-update')) { $Info.ArgumentList.Add($Argument) }
    if ($CreateTranscript) {
        foreach ($Argument in @('--no-custom-instructions', '--disable-builtin-mcps', '--available-tools=shell', '--deny-tool=shell', '--no-remote', '--no-remote-export')) {
            $Info.ArgumentList.Add($Argument)
        }
    }
    if ($ResumeSessionId) {
        $Info.ArgumentList.Add('--resume')
        $Info.ArgumentList.Add($ResumeSessionId)
    }
    $Process = [Diagnostics.Process]::new()
    $Process.StartInfo = $Info
    if (-not $Process.Start()) { throw 'Could not start Copilot ACP.' }
    $Client = [pscustomobject]@{
        Name = $Name; Process = $Process; NextId = 0
        StdoutTask = $Process.StandardOutput.ReadLineAsync()
        StderrTask = $Process.StandardError.ReadLineAsync()
        StderrLines = 0; Notifications = 0; SessionUpdates = 0; ServerRequests = 0
        Pending = @{}; Owned = @{ $Process.Id = $Process.StartTime.Ticks }; UpdateTypes = @{}
        BackoffPoll = [pscustomobject]@{ Request = $null; Failures = 0; NextPoll = 0.0 }
        Requests = [Collections.Generic.List[object]]::new()
    }
    $Clients.Add($Client)
    Write-Host "$Name started: PID $($Process.Id)"
    return $Client
}

function Initialize-Server($Client) {
    $Request = Start-Request $Client 'initialize' @{
        protocolVersion = 1
        clientInfo = @{ name = 'wta-master'; version = 'cpu-probe'; title = 'Standalone ACP CPU probe' }
        clientCapabilities = @{ terminal = $true }
    } 30
    $Result = Wait-Request $Client $Request
    if ($Result.protocolVersion -ne 1) { throw 'Server did not negotiate ACP version 1.' }
    if (-not $Result.agentCapabilities.sessionCapabilities.ContainsKey('list')) {
        throw 'Server does not advertise session/list support.'
    }
    return $Result
}

function Get-OwnedCpu($Client) {
    $Rows = @(Get-CimInstance Win32_Process -Property ProcessId, ParentProcessId)
    $Alive = @{}
    foreach ($Row in $Rows) {
        $ProcessId = [int]$Row.ProcessId
        if ($Client.Owned.ContainsKey($ProcessId)) {
            $Process = Get-Process -Id $ProcessId -ErrorAction SilentlyContinue
            if ($null -ne $Process -and $Process.StartTime.Ticks -eq $Client.Owned[$ProcessId]) {
                $Alive[$ProcessId] = $Process
            }
        }
    }
    do {
        $Added = $false
        foreach ($Row in $Rows) {
            $ProcessId = [int]$Row.ProcessId
            $ParentId = [int]$Row.ParentProcessId
            if (-not $Alive.ContainsKey($ProcessId) -and $Alive.ContainsKey($ParentId)) {
                $Process = Get-Process -Id $ProcessId -ErrorAction SilentlyContinue
                if ($null -ne $Process -and $Process.StartTime -ge $Alive[$ParentId].StartTime) {
                    $Client.Owned[$ProcessId] = $Process.StartTime.Ticks
                    $Alive[$ProcessId] = $Process
                    $Added = $true
                }
            }
        }
    } while ($Added)
    $Times = @{}
    foreach ($Process in $Alive.Values) {
        $Times["$($Process.Id):$($Process.StartTime.Ticks)"] = $Process.TotalProcessorTime.TotalSeconds
    }
    return $Times
}

function Test-BackoffPollDue($State, [double]$Now) {
    if ($null -ne $State.Request) {
        if ($null -eq $State.Request.completed_seconds) { return $false }
        if ($State.Request.outcome -eq 'ok') {
            $State.Failures = 0
            $Delay = 5
        }
        else {
            $State.Failures = [Math]::Min(5, $State.Failures + 1)
            $Delay = [Math]::Min(60, 5 * [Math]::Pow(2, $State.Failures - 1))
        }
        $State.NextPoll = $State.Request.completed_seconds + $Delay
        $State.Request = $null
    }
    return $Now -ge $State.NextPoll
}

function Observe-Phase($Client, [string]$Name, [switch]$Poll) {
    Write-Host "Observing $Name ($DurationSeconds seconds, warmup $WarmupSeconds seconds)"
    $EndWarmup = $Clock.Elapsed.TotalSeconds + $WarmupSeconds
    while ($Clock.Elapsed.TotalSeconds -lt $EndWarmup) {
        Receive-Messages $Client
        Start-Sleep -Milliseconds 50
    }
    $Before = Get-OwnedCpu $Client
    $Started = $Clock.Elapsed.TotalSeconds
    $Previous = $Started
    $NextSample = $Started + 1
    $NextPoll = $Started
    $FirstRequest = $Client.Requests.Count
    $FirstSample = $Samples.Count
    while ($Clock.Elapsed.TotalSeconds - $Started -lt $DurationSeconds) {
        Receive-Messages $Client
        $Now = $Clock.Elapsed.TotalSeconds
        if ($Poll) {
            if ($PollPolicy -eq 'Fixed' -and $Now -ge $NextPoll) {
                $null = Start-Request $Client 'session/list' @{}
                $NextPoll = $Now + 5
            }
            elseif ($PollPolicy -eq 'Backoff' -and (Test-BackoffPollDue $Client.BackoffPoll $Now)) {
                $Client.BackoffPoll.Request = Start-Request $Client 'session/list' @{}
            }
        }
        if ($Now -ge $NextSample) {
            $After = Get-OwnedCpu $Client
            $Now = $Clock.Elapsed.TotalSeconds
            $Delta = 0.0
            foreach ($Key in $After.Keys) {
                if ($Before.ContainsKey($Key)) { $Delta += [Math]::Max([double]0, $After[$Key] - $Before[$Key]) }
                else { $Delta += $After[$Key] }
            }
            $CorePercent = 100 * $Delta / ($Now - $Previous)
            $Samples.Add([pscustomobject]@{
                phase = $Name; server_pid = $Client.Process.Id
                elapsed_seconds = $Now - $Started; interval_seconds = $Now - $Previous
                cpu_seconds = $Delta; core_percent = $CorePercent
                machine_percent = $CorePercent / $CpuCount
                observed_processes = $After.Count; unresolved_requests = $Client.Pending.Count
            })
            $Before = $After
            $Previous = $Now
            $NextSample = $Now + 1
        }
        Start-Sleep -Milliseconds 20
    }
    Receive-Messages $Client
    $PhaseSamples = @($Samples | Select-Object -Skip $FirstSample)
    $PhaseRequests = @($Client.Requests | Select-Object -Skip $FirstRequest)
    $WallSeconds = ($PhaseSamples | Measure-Object interval_seconds -Sum).Sum
    $CpuSeconds = ($PhaseSamples | Measure-Object cpu_seconds -Sum).Sum
    $MeanCore = 100 * $CpuSeconds / $WallSeconds
    $BusyFraction = @($PhaseSamples | Where-Object core_percent -ge $BusyCorePercent).Count / $PhaseSamples.Count
    $Summary = [pscustomobject]@{
        phase = $Name; server_pid = $Client.Process.Id
        measured_seconds = $WallSeconds; cpu_seconds = $CpuSeconds
        mean_core_percent = $MeanCore; mean_machine_percent = $MeanCore / $CpuCount
        peak_core_percent = ($PhaseSamples | Measure-Object core_percent -Maximum).Maximum
        busy_sample_fraction = $BusyFraction
        sustained_busy = $MeanCore -ge $BusyCorePercent -and $BusyFraction -ge 0.8
        list_requests = $PhaseRequests.Count
        list_timeouts = @($PhaseRequests | Where-Object timed_out).Count
        list_errors = @($PhaseRequests | Where-Object outcome -eq 'error').Count
        unresolved_requests = $Client.Pending.Count
    }
    $Phases.Add($Summary)
    Write-Host ("{0}: {1:N2}% of one core, {2:N3}% of machine; {3} lists, {4} timeouts, {5} unresolved" -f
        $Name, $MeanCore, ($MeanCore / $CpuCount), $Summary.list_requests, $Summary.list_timeouts, $Client.Pending.Count)
    $Phases | ConvertTo-Json -Depth 10 | Set-Content -LiteralPath (Join-Path $OutputDir 'phases.json') -Encoding utf8
    $Samples | Export-Csv -LiteralPath (Join-Path $OutputDir 'cpu.csv') -NoTypeInformation
}

function Stop-Server($Client) {
    # Refresh ownership before closing stdin, since the launcher may exit first.
    $null = Get-OwnedCpu $Client
    if (-not $Client.Process.HasExited) {
        $Client.Process.StandardInput.Close()
        $null = $Client.Process.WaitForExit(1000)
    }
    foreach ($ProcessId in @($Client.Owned.Keys | Sort-Object -Descending)) {
        $Process = Get-Process -Id $ProcessId -ErrorAction SilentlyContinue
        if ($null -ne $Process -and $Process.StartTime.Ticks -eq $Client.Owned[$ProcessId]) {
            Stop-Process -Id $ProcessId -Force -ErrorAction Stop
        }
    }
}

try {
    Write-Host "$Version`nLogical processors: $CpuCount`nOutput: $OutputDir"
    $Client = Start-Server 'baseline' ''
    $Capabilities = Initialize-Server $Client
    Observe-Phase $Client 'initialized-idle'
    Observe-Phase $Client 'list-polling' -Poll
    Observe-Phase $Client 'after-list-idle'
    if ($CreateTranscript) {
        if (-not $Capabilities.agentCapabilities.loadSession) { throw 'Server does not support session/load.' }
        $New = Wait-Request $Client (Start-Request $Client 'session/new' @{ cwd = $Workspace; mcpServers = @() } 90)
        $SeedSessionId = $New.sessionId
        if (-not $SeedSessionId) { throw 'session/new returned no session ID.' }
        $Prompt = Start-Request $Client 'session/prompt' @{
            sessionId = $SeedSessionId
            prompt = @(@{ type = 'text'; text = 'Reply exactly ACP_CPU_PROBE_OK. Do not use tools, read files, or access any URLs.' })
        } 120
        $Result = Wait-Request $Client $Prompt
        if ($Result.stopReason -ne 'end_turn') { throw "Synthetic prompt ended with $($Result.stopReason)." }
        if (-not $Client.UpdateTypes.ContainsKey('agent_message_chunk')) {
            throw 'Synthetic prompt completed without an agent text response.'
        }
        $TranscriptCreated = $true
        Observe-Phase $Client 'new-session-idle'
        Stop-Server $Client
        $Client = Start-Server 'load' ''
        $null = Initialize-Server $Client
        $null = Wait-Request $Client (Start-Request $Client 'session/load' @{
            sessionId = $SeedSessionId; cwd = $Workspace; mcpServers = @()
        } 90)
        $ReplayVerified = $Client.UpdateTypes.ContainsKey('user_message_chunk') -and
            $Client.UpdateTypes.ContainsKey('agent_message_chunk')
        if (-not $ReplayVerified) { throw 'session/load completed without replaying both sides of the test conversation.' }
        Observe-Phase $Client 'loaded-session-idle'
        Observe-Phase $Client 'loaded-session-list-polling' -Poll
        Observe-Phase $Client 'loaded-session-after-list-idle'
        Stop-Server $Client
        $Client = Start-Server 'startup-resume' $SeedSessionId
        $null = Initialize-Server $Client
        Observe-Phase $Client 'startup-resume-idle'
    }
    else {
        Write-Host 'Restoration not requested; use -CreateTranscript to create a persisted test conversation (consumes model credits).'
    }
}
catch {
    $ProbeError = $_.Exception.Message
    Write-Warning "Probe incomplete: $ProbeError"
}
finally {
    foreach ($Client in $Clients) {
        try { Stop-Server $Client }
        catch {
            $CleanupError = "Cleanup failed for PID $($Client.Process.Id): $($_.Exception.Message)"
            Write-Warning $CleanupError
            if (-not $ProbeError) { $ProbeError = $CleanupError }
        }
    }
    $RpcRows = foreach ($Client in $Clients) {
        foreach ($Request in $Client.Requests) {
            $Request | Select-Object @{ n = 'server'; e = { $Client.Name } }, id, method, sent_seconds,
                timeout_seconds, timed_out, completed_seconds, latency_ms, outcome, error_code, session_count, has_next_cursor
        }
    }
    $RpcRows | Export-Csv -LiteralPath (Join-Path $OutputDir 'rpc.csv') -NoTypeInformation
    [ordered]@{
        version = $Version; executable = $CopilotPath; logical_processors = $CpuCount
        duration_seconds = $DurationSeconds; warmup_seconds = $WarmupSeconds
        poll_policy = $PollPolicy; busy_core_percent = $BusyCorePercent
        transcript_requested = [bool]$CreateTranscript; transcript_created = $TranscriptCreated
        conversation_replay_verified = $ReplayVerified; seed_session_id = $SeedSessionId
        error = $ProbeError; phases = $Phases.ToArray()
        servers = @($Clients.ToArray() | ForEach-Object {
            [ordered]@{
                name = $_.Name; pid = $_.Process.Id; stderr_lines = $_.StderrLines
                notifications = $_.Notifications; session_updates = $_.SessionUpdates
                update_types = $_.UpdateTypes
                server_requests = $_.ServerRequests; unresolved_requests = $_.Pending.Count
            }
        })
    } | ConvertTo-Json -Depth 20 | Set-Content -LiteralPath (Join-Path $OutputDir 'report.json') -Encoding utf8
    Write-Host "Report: $(Join-Path $OutputDir 'report.json')"
}
if ($ProbeError) { throw $ProbeError }
