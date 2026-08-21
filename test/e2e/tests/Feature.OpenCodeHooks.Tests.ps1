#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# PR #476: install the packaged OpenCode plugin and prove a real shell-hosted
# OpenCode session reaches WTA through the hook -> PowerShell -> WT protocol path.

BeforeDiscovery {
    Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
    $script:Ready = $false
    try {
        $null = Resolve-ItApp -Package Dev -ErrorAction Stop
        $script:Ready = [bool]((Get-Command opencode -ErrorAction SilentlyContinue) -and (Test-WinAppAvailable))
    }
    catch {
        $script:Ready = $false
    }
}

Describe 'Feature: OpenCode session tracking hooks' -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $script:opencode = (Get-Command opencode -ErrorAction Stop).Source
        $script:openCodeStatus = Get-AgentCliStatus -Agent opencode -TimeoutSec 60
        $script:app = $null
        $script:fixtureSessionId = $null
        $script:fixtureDir = $null

        $configRoot = if ($env:XDG_CONFIG_HOME) { $env:XDG_CONFIG_HOME } else { Join-Path $HOME '.config' }
        $script:pluginsDir = Join-Path $configRoot 'opencode\plugins'
        $script:supportDir = Join-Path $script:pluginsDir 'wt-agent-hooks'
        $script:managedPaths = @(
            (Join-Path $script:pluginsDir 'wt-agent-hooks.js'),
            (Join-Path $script:supportDir 'send-event.ps1'),
            (Join-Path $script:supportDir 'plugin.json')
        )
        $script:pluginsDirExisted = Test-Path $script:pluginsDir
        $script:supportDirExisted = Test-Path $script:supportDir
        $script:managedSnapshots = @{}
        foreach ($path in $script:managedPaths) {
            $script:managedSnapshots[$path] = if (Test-Path $path) { [IO.File]::ReadAllBytes($path) } else { $null }
        }
    }

    AfterAll {
        if ($script:app) {
            Stop-Terminal -App $script:app
        }
        if ($script:fixtureSessionId) {
            & $script:opencode session delete $script:fixtureSessionId 2>&1 | Out-Null
        }
        if ($script:fixtureDir -and (Test-Path $script:fixtureDir)) {
            Remove-Item -LiteralPath $script:fixtureDir -Recurse -Force
        }

        foreach ($path in $script:managedPaths) {
            $bytes = $script:managedSnapshots[$path]
            if ($null -ne $bytes) {
                $parent = Split-Path $path
                if (-not (Test-Path $parent)) {
                    New-Item -ItemType Directory -Path $parent -Force | Out-Null
                }
                [IO.File]::WriteAllBytes($path, $bytes)
            }
            elseif (Test-Path $path) {
                Remove-Item -LiteralPath $path -Force
            }
        }
        if (-not $script:supportDirExisted -and (Test-Path $script:supportDir) -and -not (Get-ChildItem $script:supportDir -Force)) {
            Remove-Item -LiteralPath $script:supportDir
        }
        if (-not $script:pluginsDirExisted -and (Test-Path $script:pluginsDir) -and -not (Get-ChildItem $script:pluginsDir -Force)) {
            Remove-Item -LiteralPath $script:pluginsDir
        }
    }

    It 'OpenCode shell sessions are tracked by installed hooks' {
        if ($script:openCodeStatus -ne 'authed') {
            Set-ItResult -Skipped -Because "OpenCode cannot answer non-interactively ($script:openCodeStatus)"
            return
        }

        $script:app = Start-Terminal -Package Dev -PassFre $true -Settings @{ acpAgent = 'opencode' }
        $install = Invoke-Wta -App $script:app -Arguments @('hooks', 'install', '--cli', 'opencode') -TimeoutSec 45 -Raw
        $install.ExitCode | Should -Be 0

        $statusRaw = (Invoke-Wta -App $script:app -Arguments @('hooks', 'status', '--json') -TimeoutSec 45 -Raw).StdOut
        $status = $statusRaw | ConvertFrom-Json
        $openCodeHook = @($status.clis | Where-Object name -eq 'opencode')
        $openCodeHook | Should -HaveCount 1
        $openCodeHook[0].plugin_installed | Should -BeTrue -Because 'the packaged installer must report the complete OpenCode plugin'
        foreach ($path in $script:managedPaths) {
            Test-Path $path | Should -BeTrue -Because "the OpenCode hook install must include $path"
        }

        $shellPane = Get-ActivePane -App $script:app
        Initialize-LogOffsets -App $script:app | Out-Null
        Open-AgentPane -App $script:app | Out-Null
        $agentPane = (Wait-NewAgentPaneSession -App $script:app -OwnerPaneSessionId $shellPane.session_id -TimeoutSec 30).PaneSessionId
        Wait-AgentReady -App $script:app -PaneSessionId $agentPane -TimeoutSec 90 |
            Should -BeTrue -Because 'the OpenCode ACP session must be active for the duplicate-suppression control'
        Start-Sleep -Seconds 2
        (Get-ItLogText -App $script:app -Name 'hook-trace.log' -SinceStart) |
            Should -Not -Match 'cli=opencode' -Because 'OpenCode ACP mode must suppress plugin events that would duplicate the agent-pane session'

        $suffix = [guid]::NewGuid().ToString('N').Substring(0, 8)
        $title = "ITE2E OpenCode Hook $suffix"
        $token = "HOOKTOKEN$suffix"
        $done = "HOOKDONE$suffix"
        $script:fixtureDir = Join-Path $env:TEMP "ite2e-opencode-hook-$suffix"
        New-Item -ItemType Directory -Path $script:fixtureDir | Out-Null

        Initialize-LogOffsets -App $script:app | Out-Null
        $escapedDir = $script:fixtureDir.Replace("'", "''")
        $escapedExe = $script:opencode.Replace("'", "''")
        $command = "Set-Location -LiteralPath '$escapedDir'; & '$escapedExe' run --title '$title' 'Reply with only the token $token'; Write-Output '$done'"
        Send-WtInput -App $script:app -SessionId $shellPane.session_id -Text $command
        Send-WtKeys -App $script:app -SessionId $shellPane.session_id -Keys @('Enter')
        Assert-Pane -App $script:app -SessionId $shellPane.session_id -Match ([regex]::Escape($done)) -TimeoutSec 120

        $traceReady = Test-Until -TimeoutSec 30 -IntervalSec 0.5 -Condition {
            $trace = Get-ItLogText -App $script:app -Name 'hook-trace.log' -SinceStart
            $trace -match 'DISPATCHED cli=opencode event=agent\.session\.start' -and
            $trace -match 'DISPATCHED cli=opencode event=agent\.prompt\.submit'
        }
        $traceReady | Should -BeTrue -Because 'the external OpenCode process must dispatch its lifecycle through the installed bridge'

        Push-Location $script:fixtureDir
        try {
            $sessions = ((& $script:opencode session list --format json 2>&1) -join "`n") | ConvertFrom-Json
        }
        finally {
            Pop-Location
        }
        $fixture = @($sessions | Where-Object title -eq $title)
        $fixture | Should -HaveCount 1
        $script:fixtureSessionId = $fixture[0].id

        (Test-Until -TimeoutSec 15 -IntervalSec 0.5 -Condition {
                $helperLog = Get-ItLogText -App $script:app -Name 'wta-main_helper-*.log' -SinceStart
                $helperLog -match [regex]::Escape("routing event=agent.session.start asid=$script:fixtureSessionId") -and
                $helperLog -match [regex]::Escape("pane_session_id=$($shellPane.session_id.ToLowerInvariant()) cli_source=OpenCode")
            }) | Should -BeTrue -Because 'the hook event must route the real OpenCode session ID through the originating shell pane'

        $sessionPane = (Open-SessionList -App $script:app -TimeoutSec 30).PaneSessionId
        $pickerTitle = Split-Path $script:fixtureDir -Leaf
        $rowVisible = Test-Until -TimeoutSec 30 -IntervalSec 0.5 -Condition {
            $text = Get-AgentPaneText -App $script:app -PaneSessionId $sessionPane -MaxLines 100
            $text -match [regex]::Escape($pickerTitle) -or $text -match [regex]::Escape($title)
        }
        $pickerText = Get-AgentPaneText -App $script:app -PaneSessionId $sessionPane -MaxLines 100
        $rowVisible | Should -BeTrue -Because "the hook-routed shell session must appear in the OpenCode session picker; picker:`n$pickerText"
    }
}
