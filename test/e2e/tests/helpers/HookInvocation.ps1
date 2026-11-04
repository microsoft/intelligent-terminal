# Copyright (c) Microsoft Corporation.
# Licensed under the MIT license.

function Get-ShippedAgentHook {
    param([string]$BundleRoot, [string]$Cli, [string]$Event)
    $relative = @{
        copilot = 'copilot\wt-agent-hooks\hooks\hooks.json'
        claude = 'claude\wt-agent-hooks\hooks\hooks.json'
        gemini = 'gemini-extension\hooks\hooks.json'
        codex = 'codex\wt-agent-hooks\hooks\hooks.json'
        antigravity = 'antigravity\hooks.json'
    }
    if (-not $relative.ContainsKey($Cli)) { throw "No manifest hook integration for '$Cli'." }
    $path = Join-Path $BundleRoot $relative[$Cli]
    $manifest = Get-Content -LiteralPath $path -Raw | ConvertFrom-Json
    $topics = if ($Cli -eq 'antigravity') { $manifest.'wt-agent-hooks' } else { $manifest.hooks }
    $needle = '--event ' + [regex]::Escape($Event) + '(?=[\s;]|$)'
    foreach ($topic in $topics.PSObject.Properties) {
        foreach ($handler in $topic.Value) {
            $hooks = if ($handler.hooks) { $handler.hooks } else { @($handler) }
            foreach ($hook in $hooks) {
                if ($hook.exec -and ($hook.args -join ' ') -match $needle) { return $hook }
                foreach ($field in @('command', 'commandWindows', 'powershell', 'bash')) {
                    if ($hook.$field -and $hook.$field -match $needle) { return $hook }
                }
            }
        }
    }
    throw "No shipped hook for '$Cli' / '$Event'."
}

function New-AgentHookInvocation {
    param(
        [string]$BundleRoot,
        [string]$Cli,
        [string]$Event,
        [string]$PayloadFile,
        [string]$GitBash,
        [ValidateSet('auto', 'bash', 'powershell')][string]$Variant = 'auto'
    )
    $hook = Get-ShippedAgentHook -BundleRoot $BundleRoot -Cli $Cli -Event $Event
    $feed = "Get-Content -Raw -LiteralPath '$($PayloadFile.Replace("'", "''"))' | "
    $executable = $null
    $arguments = @()
    if ($hook.exec) {
        $executable = $hook.exec
        $arguments = @($hook.args)
    }
    elseif ($Cli -eq 'antigravity' -and $Variant -ne 'bash') {
        # Match the provider's native whitespace-split argv, preserving the
        # literal quotes it passes to PowerShell's -Command.
        $argv = @($hook.command -split '\s+')
        $executable = $argv[0]
        $arguments = @($argv | Select-Object -Skip 1)
    }
    if ($executable) {
        $quoted = @($arguments | ForEach-Object { "'" + $_.Replace("'", "''") + "'" })
        return $feed + "& '$($executable.Replace("'", "''"))' " + ($quoted -join ' ')
    }

    if ($Variant -eq 'bash' -or ($Variant -eq 'auto' -and $hook.shell -eq 'bash')) {
        if (-not $GitBash) { return $null }
        $command = if ($hook.bash) { $hook.bash } else { $hook.command }
        return $feed + "& '$($GitBash.Replace("'", "''"))' -c '$($command.Replace("'", "''"))'"
    }
    $command = if ($hook.commandWindows) { $hook.commandWindows } elseif ($hook.powershell) { $hook.powershell } else { $hook.command }
    $encoded = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($command))
    $feed + "& pwsh -NoLogo -NoProfile -EncodedCommand $encoded"
}
