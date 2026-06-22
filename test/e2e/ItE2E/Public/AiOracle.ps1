# AiOracle.ps1 — the AI verification oracle. Wraps an agent CLI's non-interactive
# print mode (copilot -p / claude -p) DIRECTLY, so the oracle is independent of the
# product under test (wta). The agent acts as an LLM judge returning a JSON verdict.

# Agent -> argument builder for a one-shot "answer this prompt and exit" call.
$script:ItAgentJudges = @{
    copilot = { param($prompt, $model) $a = @('-p', $prompt); if ($model) { $a += @('--model', $model) }; $a }
    claude  = { param($prompt, $model) $a = @('-p', $prompt); if ($model) { $a += @('--model', $model) }; $a }
    codex   = { param($prompt, $model) $a = @('exec', $prompt); if ($model) { $a += @('--model', $model) }; $a }
    gemini  = { param($prompt, $model) $a = @('-p', $prompt); if ($model) { $a += @('--model', $model) }; $a }
}

function Get-JsonObjectFromText {
    <# Extract and parse the first balanced {...} JSON object from noisy CLI output. #>
    [CmdletBinding()] param([Parameter(Mandatory)][string]$Text)
    $m = [regex]::Match($Text, '\{(?:[^{}]|(?<o>\{)|(?<-o>\}))*\}')
    if (-not $m.Success) { return $null }
    try { $m.Value | ConvertFrom-Json -Depth 64 } catch { $null }
}

function Invoke-AgentJudge {
    <#
    .SYNOPSIS
        Send a single judge prompt to an agent CLI's non-interactive mode and return the
        parsed JSON verdict @{ pass; confidence; reason }. Independent of wta.
    .PARAMETER Agent   copilot (default) | claude | codex | gemini, or a custom exe name.
    .PARAMETER Model   Optional model override passed to the agent CLI.
    #>
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)][string]$Prompt,
        [string]$Agent = $(if ($env:ITE2E_AI_AGENT) { $env:ITE2E_AI_AGENT } else { 'copilot' }),
        [string]$Model = $env:ITE2E_AI_MODEL,
        [int]$TimeoutSec = 120
    )
    $exe = (Get-Command $Agent -ErrorAction SilentlyContinue | Select-Object -First 1).Source
    if (-not $exe) { throw "Invoke-AgentJudge: agent CLI '$Agent' not found on PATH." }

    $builder = $script:ItAgentJudges[$Agent]
    $cliArgs = if ($builder) { & $builder $Prompt $Model } else { @('-p', $Prompt) }

    Write-ItLog -Level INFO -Message "AI judge via '$Agent' ($exe, timeout ${TimeoutSec}s)."
    # Use a job so the call operator handles multi-line arg passing AND we get a timeout.
    # Pass the resolved executable path ($exe) so the job doesn't re-resolve via PATH.
    $job = Start-Job -ScriptBlock {
        param($e, $a)
        $ErrorActionPreference = 'Continue'
        & $e @a 2>&1 | Out-String
    } -ArgumentList $exe, $cliArgs
    try {
        if (-not (Wait-Job $job -Timeout $TimeoutSec)) {
            Stop-Job $job
            throw "Invoke-AgentJudge: '$Agent' timed out after ${TimeoutSec}s."
        }
        $out = (Receive-Job $job) -join "`n"
    }
    finally { Remove-Job $job -Force -ErrorAction SilentlyContinue }

    $verdict = Get-JsonObjectFromText -Text $out
    if ($null -eq $verdict) { throw "Invoke-AgentJudge: no JSON verdict in '$Agent' output. Raw:`n$out" }
    [pscustomobject]@{
        pass       = [bool]$verdict.pass
        confidence = $verdict.confidence
        reason     = [string]$verdict.reason
        raw        = $out
    }
}

function Test-AIClaim {
    <#
    .SYNOPSIS
        Ask an agent CLI to judge whether a natural-language CLAIM holds for the captured
        CONTEXT. Returns @{ pass; confidence; reason }. Wraps copilot/claude directly.
    .PARAMETER Context  A context string, a Get-ContextBundle object, or omitted (then a
                        bundle is captured from -App).
    #>
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)][string]$Claim,
        $Context,
        $App,
        [string]$Agent = $(if ($env:ITE2E_AI_AGENT) { $env:ITE2E_AI_AGENT } else { 'copilot' }),
        [string]$Model = $env:ITE2E_AI_MODEL,
        [int]$TimeoutSec = 120
    )
    $ctxText =
    if ($Context -is [string]) { $Context }
    elseif ($Context) { ConvertTo-ContextText -Bundle $Context }
    elseif ($App) { ConvertTo-ContextText -Bundle (Get-ContextBundle -App $App) }
    else { '' }

    $prompt = @"
You are a strict, deterministic test oracle. Given CONTEXT (captured terminal/UI state)
and a CLAIM, decide whether the CLAIM is clearly supported by the CONTEXT. Respond with
ONLY a compact JSON object: {"pass": true|false, "confidence": 0..1, "reason": "<short>"}.
Do not run any tools or commands. Do not output anything except the JSON.

CLAIM:
$Claim

CONTEXT:
$ctxText
"@
    Invoke-AgentJudge -Prompt $prompt -Agent $Agent -Model $Model -TimeoutSec $TimeoutSec
}

function Assert-AI {
    <# Assert that an AI claim holds for the captured context (throws on a 'false' verdict). #>
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)][string]$Claim,
        $Context,
        $App,
        [string]$Agent = $(if ($env:ITE2E_AI_AGENT) { $env:ITE2E_AI_AGENT } else { 'copilot' }),
        [string]$Model = $env:ITE2E_AI_MODEL,
        [int]$TimeoutSec = 120
    )
    $r = Test-AIClaim -Claim $Claim -Context $Context -App $App -Agent $Agent -Model $Model -TimeoutSec $TimeoutSec
    if (-not $r.pass) { throw "Assert-AI FAILED: '$Claim' -> $($r.reason) (confidence=$($r.confidence))" }
    Write-ItLog -Level INFO -Message "Assert-AI passed: '$Claim' ($($r.reason))"
    $r
}
