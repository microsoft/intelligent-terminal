#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# Feature: agent pane image paste (Alt+V) — issue #211.
#
# Verifies the end-to-end Alt+V flow against a deployed package:
#   screenshot on clipboard -> focus agent pane -> simulate Alt+V -> image captured & queued.
#
# The key thing this proves that the Rust-side tests cannot: that Alt+V, injected through
# WT/ConPTY as a win32-input-mode key event, actually reaches the helper's crossterm reader
# as a KeyEvent carrying the ALT modifier (the path the unit tests stub out with a synthetic
# KeyEvent). If this assumption were wrong, Alt+V would type a literal 'v' or be dropped, and
# none of the handler's three outcome messages would appear.
#
#   Invoke-Pester test/e2e/tests/Feature.AgentImagePaste.Tests.ps1 -Tag Feature

BeforeDiscovery {
    $script:Ready = [bool](
        (Get-AppxPackage | Where-Object { $_.Name -like '*IntelligentTerminal*' }) -and
        (Get-Command copilot -ErrorAction SilentlyContinue) -and
        (Get-Command winapp -ErrorAction SilentlyContinue)
    )
}

Describe 'Feature: agent pane image paste (Alt+V)' -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $script:app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true -Settings @{ acpAgent = 'copilot' }
        Open-AgentPane -App $script:app | Out-Null
        Wait-AgentReady -App $script:app -TimeoutSec 60 | Out-Null
    }
    AfterAll { if ($script:app) { Stop-Terminal -App $script:app } }

    It 'Alt+V reaches the handler and captures the clipboard image (not typed as a literal key)' {
        # 1) Put a real image (CF_DIB) on the shared OS clipboard.
        Set-ClipboardImage

        # 2) Focus the agent pane input and dismiss any popup / stale char.
        Set-AgentPaneFocus -App $script:app | Out-Null
        Clear-AgentInput -App $script:app | Out-Null

        # 3) Simulate Alt+V via win32-input-mode raw key events.
        Send-AgentAltV -App $script:app | Out-Null

        # 4) The handler must have run. With an image on the clipboard it either ATTACHES it
        #    (📎 title / "Image attached") or — if copilot doesn't advertise the image prompt
        #    capability — reports it UNSUPPORTED. Either proves Alt+V arrived as a KeyEvent
        #    with the ALT modifier. It must NOT report an empty clipboard (we set one).
        Assert-AgentPaneText -App $script:app -Pattern '📎|Image attached|support image input' -TimeoutSec 15

        $after = Get-AgentPaneText -App $script:app -MaxLines 80
        $after | Should -Not -Match 'No image on the clipboard'
    }

    It 'Queued image is sent to the agent as an ACP image content block (when the agent supports images)' {
        $paneText = Get-AgentPaneText -App $script:app -MaxLines 80
        if ($paneText -match 'support image input') {
            Set-ItResult -Skipped -Because 'the configured agent does not advertise the image prompt capability'
            return
        }
        # The image is queued (the 📎 input-box title). Submit it, then confirm an image
        # content block reached the agent on the ACP wire. The wire trace only lands when
        # debug logging is on, so treat its absence as inconclusive rather than a failure.
        $acpLog = Get-ItLogText -App $script:app -Name 'wta-acp-debug.log' -SinceStart
        if ([string]::IsNullOrWhiteSpace($acpLog)) {
            Set-ItResult -Skipped -Because 'wta-acp-debug.log is empty (ACP wire trace requires WTA_LOG=debug)'
            return
        }

        Send-AgentKey -App $script:app -Key Enter | Out-Null
        Wait-AgentState -App $script:app -State Working -TimeoutSec 30 | Out-Null

        $sent = Test-Until -TimeoutSec 30 -IntervalSec 1 -Condition {
            $log = Get-ItLogText -App $script:app -Name 'wta-acp-debug.log' -SinceStart
            ($log -match '"type"\s*:\s*"image"') -or ($log -match 'image/png')
        }
        $sent | Should -BeTrue -Because 'the pasted image must reach the agent as an ACP ContentBlock::Image'
    }
}
