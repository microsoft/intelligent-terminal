# state-logger.ps1 - Log all CLI agent hook events
# Logs different hook points with their full state to a file
# Supports Claude Code, Copilot CLI, and Gemini CLI input formats
# Usage: state-logger.ps1 [-CliSource cli-name]
#   cli-name: "claude", "copilot", or "gemini" (default: "unknown")

param(
    [string]$CliSource = "unknown"
)

# Log file path and timestamp
$logFile = Join-Path $PSScriptRoot "agent-hooks.log"
$timestamp = Get-Date -Format "yyyy-MM-dd HH:mm:ss.fff"
$separator = "=" * 60

# Read the JSON input from stdin
$inputJson = [Console]::In.ReadToEnd()

# Parse the JSON
$data = $null
try {
    if ($inputJson -and $inputJson.Trim()) {
        $data = $inputJson | ConvertFrom-Json
    } else {
        $data = [PSCustomObject]@{ _no_input = $true; _raw = $inputJson }
    }
} catch {
    $data = [PSCustomObject]@{ parse_error = $_.ToString(); raw = $inputJson }
}

# Detect source format and normalize fields
if ($CliSource -eq "gemini") {
    # Gemini CLI format: fields may include hook_type, tool_name, tool_input, session_id, etc.
    # Also check environment variables as Gemini may pass data that way
    $hookType = if ($data.hook_type) { $data.hook_type }
                elseif ($data.event_type) { $data.event_type }
                elseif ($data.hookName) { $data.hookName }
                elseif ($env:GEMINI_HOOK_TYPE) { $env:GEMINI_HOOK_TYPE }
                elseif ($env:HOOK_TYPE) { $env:HOOK_TYPE }
                else { "unknown" }

    # Capture relevant env vars for debugging
    $geminiEnvVars = Get-ChildItem env: | Where-Object { $_.Name -match "GEMINI|HOOK|TOOL|SESSION" } | ForEach-Object { "$($_.Name)=$($_.Value)" }
    $envVarsStr = if ($geminiEnvVars) { $geminiEnvVars -join "`n" } else { "(none)" }

    $logEntry = @"
$separator
[$timestamp] CLI: $CliSource | HOOK: $hookType
$separator
Session ID: $($data.session_id)
Tool Name: $($data.tool_name)$($data.toolName)
Tool Input: $($data.tool_input | ConvertTo-Json -Compress -Depth 5 2>$null)$($data.toolInput | ConvertTo-Json -Compress -Depth 5 2>$null)
Tool Output: $($data.tool_output | ConvertTo-Json -Compress -Depth 3 2>$null)$($data.toolOutput | ConvertTo-Json -Compress -Depth 3 2>$null)
Agent: $($data.agent_name)$($data.agentName)
Model: $($data.model)
Notification: $($data.notification)$($data.message)
Env Vars:
$envVarsStr
Raw Data:
$($data | ConvertTo-Json -Depth 5 2>$null)

"@
} elseif ($data.toolName -or $data.initialPrompt -or $data.PSObject.Properties.Name -contains 'timestamp') {
    # Copilot CLI format: fields are timestamp, cwd, toolName, toolArgs, toolResult, prompt, source, etc.
    $hookType = if ($data.toolName) { "toolUse" }
                elseif ($data.prompt) { "userPromptSubmitted" }
                elseif ($data.source) { "sessionStart" }
                elseif ($data.reason) { "sessionEnd" }
                elseif ($data.error) { "errorOccurred" }
                else { "unknown" }

    $logEntry = @"
$separator
[$timestamp] CLI: $CliSource | HOOK: $hookType
$separator
CWD: $($data.cwd)
Tool Name: $($data.toolName)
Tool Args: $($data.toolArgs)
Tool Result Type: $($data.toolResult.resultType)
Tool Result: $($data.toolResult.textResultForLlm)
Prompt: $($data.prompt)
Session Source: $($data.source)
Initial Prompt: $($data.initialPrompt)
End Reason: $($data.reason)
Error: $($data.error | ConvertTo-Json -Compress -Depth 3 2>$null)
Raw Data:
$($data | ConvertTo-Json -Depth 5 2>$null)

"@
} else {
    # Claude Code format: fields are session_id, tool_name, tool_input, hook_event_name, etc.
    $hookType = if ($data.hook_event_name) { $data.hook_event_name } else { $env:CLAUDE_HOOK_TYPE }

    $logEntry = @"
$separator
[$timestamp] CLI: $CliSource | HOOK: $hookType
$separator
Session ID: $($data.session_id)
Tool Name: $($data.tool_name)
Tool Input: $($data.tool_input | ConvertTo-Json -Compress -Depth 5 2>$null)
Tool Output: $($data.tool_output | ConvertTo-Json -Compress -Depth 3 2>$null)
Transcript: $($data.transcript_path)
Raw Data:
$($data | ConvertTo-Json -Depth 5 2>$null)

"@
}

# Write to log file in script's directory (works from any cwd)
Add-Content -Path $logFile -Value $logEntry -Encoding UTF8

# Forward event to WTA via wtcli send-event (if available)
if ($env:WT_COM_CLSID -and (Get-Command wtcli -ErrorAction SilentlyContinue)) {
    $eventType = switch -Wildcard ($hookType) {
        "*toolUse*"   { "agent.tool.starting" }
        "*PreToolUse" { "agent.tool.starting" }
        "*PostToolUse"{ "agent.tool.finished" }
        "*Start*"     { "agent.session.start" }
        "*End*"       { "agent.session.end" }
        "*Stop*"      { "agent.stop" }
        "*Notif*"     { "agent.notification" }
        "*error*"     { "agent.error" }
        "*prompt*"    { "agent.prompt.submit" }
        default       { "agent.hook" }
    }
    $wtPayload = @{ cli_source = $CliSource; payload = $data } | ConvertTo-Json -Compress -Depth 5
    try {
        wtcli send-event -e $eventType $wtPayload 2>$null
    } catch { }
}

# Output nothing (hook succeeds silently)
