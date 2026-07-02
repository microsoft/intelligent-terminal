#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# Release checklist §10 (C193) — the "Report a bug (collect logs)" command palette entry
# (bugReport / Terminal.BugReport action) bundles the IntelligentTerminal logs directory into a
# timestamped zip on the Desktop (AppActionHandlers.cpp _CreateBugReportZipAsync). This drives that
# command via the command palette and asserts the produced zip actually contains agent logs
# (wta-*.log / terminal-agent-pane.log), so a bug report is useful for diagnosing agent issues.
#
# The command palette is a WT window surface reached by the Ctrl+Shift+P accelerator, so this needs
# the WT window to hold foreground (Send-WtWindowKey). In an agent-driven session the controlling
# terminal can steal foreground; when it can't be taken the case SKIPS (a foreground precondition)
# rather than failing flakily.

BeforeDiscovery { $script:Ready = [bool]((Get-AppxPackage | Where-Object { $_.Name -like '*IntelligentTerminal*' }) -and (Get-Command copilot -ErrorAction SilentlyContinue) -and (Get-Command winapp -ErrorAction SilentlyContinue)) }

Describe 'Feature §10 bug report zip (collect logs)' -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $script:app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true -Settings @{ acpAgent = 'copilot' }
        # Open the agent pane so the per-tab helper is running and its logs exist on disk.
        Open-AgentPane -App $script:app | Out-Null
        Start-Sleep -Seconds 2
        $script:desktop = [Environment]::GetFolderPath('Desktop')
    }
    AfterAll { if ($script:app) { Stop-Terminal -App $script:app } }

    It 'Bug report zip includes agent logs (Report a bug collects wta/agent-pane logs into a Desktop zip)' {
        if (-not (Test-WtWindowKeyFocusable -App $script:app)) { Set-ItResult -Skipped -Because 'WT window cannot take foreground for the Ctrl+Shift+P command palette (competing foreground app)'; return }
        $before = @(Get-ChildItem $script:desktop -Filter 'intelligent-terminal-logs-*.zip' -ErrorAction SilentlyContinue | ForEach-Object FullName)

        # Open the command palette (Ctrl+Shift+P), run "Report a bug (collect logs)".
        $zip = $null
        for ($attempt = 0; $attempt -lt 3 -and -not $zip; $attempt++) {
            Set-WtWindowForeground -App $script:app | Out-Null
            Send-WtWindowKey -App $script:app -Vk 0x50 -Ctrl -Shift | Out-Null   # Ctrl+Shift+P
            if (-not (Test-Until -TimeoutSec 6 -IntervalSec 0.5 -Condition { Test-CommandPaletteOpen -App $script:app })) { continue }
            Set-UiValue -App $script:app -Selector '_searchBox' -Value 'Report a bug' | Out-Null
            Start-Sleep -Milliseconds 800
            Send-WtWindowKey -App $script:app -Vk 0x0D | Out-Null   # Enter -> run the command
            $zip = $null
            for ($i = 0; $i -lt 20 -and -not $zip; $i++) {
                Start-Sleep -Seconds 1
                $zip = Get-ChildItem $script:desktop -Filter 'intelligent-terminal-logs-*.zip' -ErrorAction SilentlyContinue |
                    Where-Object { $_.FullName -notin $before } | Select-Object -First 1 -ExpandProperty FullName
            }
        }
        if (-not $zip -and -not (Test-WtWindowKeyFocusable -App $script:app)) { Set-ItResult -Skipped -Because 'WT window could not take foreground for the command palette'; return }
        $zip | Should -Not -BeNullOrEmpty -Because 'the Report-a-bug command must create a logs zip on the Desktop'

        # Wait until the archive is fully written (the C++ side reveals it in Explorer once done),
        # then assert it contains agent logs so a bug report is actually useful for agent issues.
        Add-Type -AssemblyName System.IO.Compression.FileSystem
        $entries = @()
        $ok = Test-Until -TimeoutSec 20 -IntervalSec 1 -Condition {
            try { $z = [System.IO.Compression.ZipFile]::OpenRead($zip); $n = @($z.Entries.FullName).Count; $z.Dispose(); $n -gt 0 }
            catch { $false }
        }
        $ok | Should -BeTrue -Because 'the zip must become readable (and non-empty) once fully written'
        # Read entries in THIS scope (assignment inside a Test-Until scriptblock would not propagate).
        $z = [System.IO.Compression.ZipFile]::OpenRead($zip)
        $entries = @($z.Entries.FullName)
        $z.Dispose()
        ($entries -join "`n") | Should -Match '(?i)(wta-[^/\\]*\.log|terminal-agent-pane\.log)' -Because 'the bug-report zip must include the agent (wta / terminal-agent-pane) logs'

        # Cleanup: remove the zip this test created.
        Remove-Item -LiteralPath $zip -Force -ErrorAction SilentlyContinue
    }
}
