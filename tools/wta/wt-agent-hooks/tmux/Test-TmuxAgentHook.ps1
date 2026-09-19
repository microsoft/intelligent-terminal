# Copyright (c) Microsoft Corporation.
# Licensed under the MIT license.

[CmdletBinding()]
param(
    [string]$Distribution = 'Ubuntu',
    [string]$CaptureDirectory,
    [switch]$InstallerOnly
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$privateDirectory = $null
if ($InstallerOnly -and $CaptureDirectory) {
    throw 'Capture export requires the transport tests.'
}
if ($CaptureDirectory -and (Test-Path -LiteralPath $CaptureDirectory)) {
    throw 'Capture directory already exists; refusing to overwrite it.'
}

$readme = [IO.File]::ReadAllText((Join-Path $PSScriptRoot 'README.md'))
$shared = @{
    SessionStart = 'agent.session.start'; SessionEnd = 'agent.session.end'
    Notification = 'agent.notification'; UserPromptSubmit = 'agent.prompt.submit'
    StopFailure = 'agent.error'; Stop = 'agent.stop'
}
$catalog = @{
    claude = $shared; copilot = $shared
    codex = @{
        SessionStart = 'agent.session.start'; PermissionRequest = 'agent.notification'
        UserPromptSubmit = 'agent.prompt.submit'; Stop = 'agent.stop'
    }
    gemini = @{
        SessionStart = 'agent.session.start'; SessionEnd = 'agent.session.end'
        BeforeAgent = 'agent.prompt.submit'; BeforeTool = 'agent.tool.starting'
        Notification = 'agent.notification'; AfterAgent = 'agent.stop'
    }
}
$configuredSources = @{}
foreach ($block in [regex]::Matches($readme, "(?ms)^cat [^\r\n]*<<'JSON'\r?\n(.*?)^JSON\r?$")) {
    $config = $block.Groups[1].Value | ConvertFrom-Json -ErrorAction Stop
    $hooks = $config.PSObject.Properties['hooks']
    if ($null -eq $hooks -or $hooks.Value -is [string]) {
        continue
    }
    $source = $null
    foreach ($entry in $hooks.Value.PSObject.Properties) {
        $action = $entry.Value[0].hooks[0]
        $commandProperty = $action.PSObject.Properties['bash']
        if ($null -eq $commandProperty) {
            $commandProperty = $action.PSObject.Properties['command']
        }
        $command = $commandProperty.Value
        if ($command -notmatch '--cli-source (claude|copilot|codex|gemini) --event ([a-z.]+); exit 0$') {
            throw 'Invalid documented shell hook command.'
        }
        $source = $Matches[1]
        if ($catalog[$source][$entry.Name] -ne $Matches[2] -or $command -notlike '*it-agent-hook.sh*') {
            throw 'Documented hook does not match the existing event catalog.'
        }
    }
    if (@($hooks.Value.PSObject.Properties).Count -ne $catalog[$source].Count) {
        throw 'Documented hook catalog is incomplete.'
    }
    $configuredSources[$source] = $true
}
if ($configuredSources.Count -ne 4 -or $readme -notmatch 'export const ItTmuxHooks') {
    throw 'README must include all five CLI configurations.'
}
Write-Host 'PASS: static JSON setup examples and all five CLI configurations'

try {
    $created = & wsl.exe -d $Distribution --exec sh -c 'umask 077; mktemp -d /tmp/it-tmux-tests.XXXXXXXXXXXX'
    if ($LASTEXITCODE -ne 0) {
        throw 'Could not create the private Linux-native test directory.'
    }
    $privateDirectory = ($created -join "`n").Trim()
    if ($privateDirectory -cnotmatch '^/tmp/it-tmux-tests\.[A-Za-z0-9]{12}$') {
        $privateDirectory = $null
        throw 'Unexpected private test directory response.'
    }

    foreach ($name in @('it-agent-hook.sh', 'install-remote-hooks.sh', 'test-tmux-hooks.sh', 'test-install-remote-hooks.sh')) {
        $text = [IO.File]::ReadAllText((Join-Path $PSScriptRoot $name)).Replace("`r`n", "`n")
        $encoded = [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($text))
        $encoded | & wsl.exe -d $Distribution --exec sh -c 'umask 077; base64 -d > "$1"' sh "$privateDirectory/$name"
        if ($LASTEXITCODE -ne 0) {
            throw "Could not copy required test file: $name"
        }
    }

    if (-not $InstallerOnly) {
        & wsl.exe -d $Distribution --exec bash "$privateDirectory/test-tmux-hooks.sh" "$privateDirectory"
        if ($LASTEXITCODE -ne 0) {
            throw 'Linux tmux hook tests failed.'
        }
    }
    & wsl.exe -d $Distribution --exec bash "$privateDirectory/test-install-remote-hooks.sh" "$privateDirectory"
    if ($LASTEXITCODE -ne 0) {
        throw 'Managed remote hook installer tests failed.'
    }
    $evidence = & wsl.exe -d $Distribution --exec cat "$privateDirectory/installer-evidence"
    if ($LASTEXITCODE -ne 0) {
        throw 'Managed installer configuration evidence is unavailable.'
    }
    foreach ($line in $evidence) {
        $fields = $line.Split("`t")
        if ($fields.Count -ne 4) {
            throw 'Invalid managed installer configuration evidence.'
        }
        $provider = $fields[0]
        $scriptPath = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String($fields[1]))
        $config = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String($fields[2])) | ConvertFrom-Json
        $manifest = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String($fields[3])) | ConvertFrom-Json
        if ($manifest.name -ne 'it-ssh-hooks' -or @($config.hooks.PSObject.Properties).Count -ne $catalog[$provider].Count) {
            throw 'Managed plugin catalog is incomplete.'
        }
        foreach ($entry in $config.hooks.PSObject.Properties) {
            $action = $entry.Value[0].hooks[0]
            $property = $action.PSObject.Properties['bash']
            if ($null -eq $property) {
                $property = $action.PSObject.Properties['command']
            }
            $command = $property.Value
            if ($command.Contains('$HOME') -or $command -notlike "*--event $($catalog[$provider][$entry.Name]); exit 0") {
                throw 'Managed hook command does not use the expected absolute path and topic.'
            }
            $output = 'null' | & wsl.exe -d $Distribution --exec env -u TMUX -u TMUX_PANE -u IT_SSH_HOOK_ROUTE `
                HOME=/deliberately-different-runtime-home sh -c $command 2>&1
            if ($LASTEXITCODE -ne 0 -or $output) {
                throw 'Managed absolute hook path did not execute cleanly under a different HOME.'
            }
        }
        if ($provider -eq 'gemini') {
            foreach ($variable in @('IT_SSH_HOOK_ROUTE', 'IT_SSH_HOOK_SOCKET', 'IT_SSH_HOOK_SESSION', 'WTA_TMUX_HOOKS_DISABLED')) {
                if ($variable -notin $manifest.settings.envVar) {
                    throw 'Gemini route environment allowlist is incomplete.'
                }
            }
        }
    }
    Write-Host 'PASS: generated managed configs parse and absolute commands survive quotes and runtime HOME changes'
    if ($CaptureDirectory) {
        $capture = New-Item -ItemType Directory -Path $CaptureDirectory -ErrorAction Stop
        foreach ($name in @('real-shell-v2.messages', 'real-shell-v2.payload')) {
            $encoded = & wsl.exe -d $Distribution --exec base64 --wrap=0 "$privateDirectory/$name"
            if ($LASTEXITCODE -ne 0) {
                throw 'Could not export the real shell capture.'
            }
            $bytes = [Convert]::FromBase64String(($encoded -join ''))
            $path = Join-Path $capture.FullName $name
            $stream = [IO.File]::Open($path, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
            try {
                $stream.Write($bytes, 0, $bytes.Length)
            }
            finally {
                $stream.Dispose()
            }
        }
        $lines = [IO.File]::ReadAllLines((Join-Path $capture.FullName 'real-shell-v2.messages'))
        $data = ($lines | ForEach-Object { $_.Substring($_.LastIndexOf(' ') + 1) }) -join ''
        $body = [IO.File]::ReadAllBytes((Join-Path $capture.FullName 'real-shell-v2.payload'))
        if ($lines.Count -ne 2 -or $data -cne [Convert]::ToBase64String($body)) {
            throw 'Exported capture does not match its exact raw body.'
        }
        Write-Host "CAPTURE: $($capture.FullName)"
    }
}
finally {
    if ($null -ne $privateDirectory) {
        & wsl.exe -d $Distribution --exec sh -c 'if [ -S "$1/s,1" ]; then error=$(tmux -N -S "$1/s,1" kill-server 2>&1) || case $error in *"no server running"*|*"No such file or directory"*) :;; *) printf "%s\n" "Owned tmux test server cleanup failed" >&2; exit 1;; esac; fi; rm -rf -- "$1"' sh $privateDirectory
        if ($LASTEXITCODE -ne 0) {
            throw 'Private Linux test directory cleanup failed.'
        }
    }
}
