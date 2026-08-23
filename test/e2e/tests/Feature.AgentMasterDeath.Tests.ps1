#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# Release checklist §2 "Master death fails closed and a later pane open starts fresh" (#329):
#   If wta-master exits, its helper and pane exit without replaying the crashed session.
#   A later explicit pane open starts a fresh master/helper/ACP session.
#
# This suite exercises the REAL failure by killing wta-master out from under a live helper and
# asserting the fail-closed contract end-to-end:
#   1. the helper exits and closeOnExit closes the pane,
#   2. no master, helper, or ACP session is recreated automatically,
#   3. an explicit pane open creates a fresh master/helper/ACP session without session/load.
#
# Architecture facts this relies on (verified live + tools/wta/AGENTS.md):
#   * master  = `wta.exe --master <pipe>`         (spawned once by C++ SharedWta)
#   * helper  = `wta.exe --connect-master <pipe>` (one per agent pane)
#   * both are children of the WindowsTerminal.exe process (this app's Pid), so we kill ONLY
#     this app's master and never another instance's.
#   * each wta owner starts its own direct `wtcli.exe --json listen --parent-pid <owner>` child.
#   * the helper detects master death proactively and exits, so no prompt is needed to trigger it.
#
#   Invoke-Pester test/e2e/tests/Feature.AgentMasterDeath.Tests.ps1 -Tag Feature

BeforeDiscovery { $script:Ready = [bool]((Get-AppxPackage | Where-Object { $_.Name -like '*IntelligentTerminal*' }) -and (Get-Command copilot -ErrorAction SilentlyContinue) -and (Get-Command winapp -ErrorAction SilentlyContinue)) }

Describe 'Feature §2 master death fails closed (#329)' -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $script:app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true -Settings @{ acpAgent = 'copilot' }
        $script:shellPane = Get-ActivePane -App $script:app
        Open-AgentPane -App $script:app | Out-Null
        Wait-AgentReady -App $script:app -TimeoutSec 90 | Should -BeTrue -Because 'the agent pane must be connected before we can test losing the connection'
        $script:initialSession = Get-AgentPaneSession -App $script:app -OwnerPaneSessionId $script:shellPane.session_id
        $script:initialSession | Should -Not -BeNullOrEmpty

        # This app's wta-master(s): a wta.exe child of THIS WindowsTerminal.exe launched with
        # `--master` (not `--connect-master`). Scoping to the app's Pid guarantees we never touch
        # another instance's master.
        $script:GetMasters = {
            @(Get-CimInstance Win32_Process -Filter "Name='wta.exe'" -ErrorAction SilentlyContinue |
                Where-Object {
                    $_.ParentProcessId -eq $script:app.Pid -and
                    $_.CommandLine -match '--master(\s|$|")' -and
                    $_.CommandLine -notmatch '--connect-master'
                })
        }
        $script:GetOwnedListeners = {
            param([int[]]$OwnerPids)
            @(Get-CimInstance Win32_Process -Filter "Name='wtcli.exe'" -ErrorAction SilentlyContinue |
                Where-Object {
                    $ownerPid = [int]$_.ParentProcessId
                    $parentPattern = '(?i)(?:^|\s)"?[^"]*wtcli\.exe"?\s+--json\s+listen\s+--parent-pid\s+"?' +
                        [regex]::Escape([string]$ownerPid) + '(?:"|\s|$)'
                    $OwnerPids -contains $ownerPid -and
                    $_.CommandLine -match $parentPattern
                })
        }
    }
    AfterAll { if ($script:app) { Stop-Terminal -App $script:app } }

    It 'Master death fails closed and a later pane open starts fresh' {
        # --- there is exactly one live master while connected ---
        $masters = & $script:GetMasters
        # A connected agent pane runs EXACTLY ONE wta-master per WindowsTerminal process — the
        # C++ SharedWta singleton owns a single master and fans every pane onto it. Asserting
        # `-Be 1` (not just `> 0`) makes this gate itself catch a split-brain regression where a
        # second master already exists while connected.
        $masters.Count | Should -Be 1 -Because 'a connected agent pane implies exactly one shared wta-master (SharedWta is a singleton)'
        $killedPids = @($masters.ProcessId)
        $listenerOwnerPids = @($killedPids + [int]$script:initialSession.HelperProcessId)
        $ownedListeners = @(& $script:GetOwnedListeners $listenerOwnerPids)
        foreach ($ownerPid in $listenerOwnerPids) {
            @($ownedListeners | Where-Object { [int]$_.ParentProcessId -eq $ownerPid }).Count |
                Should -BeGreaterThan 0 -Because "wta owner $ownerPid must have a direct wtcli --json listen --parent-pid $ownerPid child"
        }
        $ownedListenerPids = @($ownedListeners.ProcessId)

        # --- kill the master out from under the live helper ---
        Initialize-LogOffsets -App $script:app | Out-Null
        foreach ($mp in $killedPids) { Stop-Process -Id $mp -Force -ErrorAction SilentlyContinue }
        Wait-Until -TimeoutSec 15 -Because 'the killed wta-master process(es) to be gone' -Condition {
            @(& $script:GetMasters | Where-Object { $killedPids -contains $_.ProcessId }).Count -eq 0
        } | Out-Null

        Wait-Until -TimeoutSec 20 -Because 'the helper to exit after its master dies' -Condition {
            -not (Get-CimInstance Win32_Process -Filter "ProcessId=$($script:initialSession.HelperProcessId)" -ErrorAction SilentlyContinue)
        } | Out-Null
        Wait-Until -TimeoutSec 20 -Because 'the master/helper-owned wtcli listener(s) to exit with their owners' -Condition {
            @($ownedListenerPids | Where-Object {
                    Get-CimInstance Win32_Process -Filter "ProcessId=$_" -ErrorAction SilentlyContinue
                }).Count -eq 0
        } | Out-Null
        Wait-Until -TimeoutSec 20 -Because 'the exited helper pane to close' -Condition {
            -not (Test-AgentPaneOpen -App $script:app)
        } | Out-Null
        (Get-AgentPaneSession -App $script:app -PaneSessionId $script:initialSession.PaneSessionId) |
            Should -BeNullOrEmpty -Because 'the crashed helper session must no longer be live'

        # Once process and pane exit establish completion, observe a bounded quiet period.
        # Nothing may recreate the stack or load the old conversation without a user action.
        for ($i = 0; $i -lt 6; $i++) {
            @(& $script:GetMasters).Count | Should -Be 0 -Because 'master death must not automatically respawn the stack'
            @(Get-AgentPaneSessions -App $script:app).Count | Should -Be 0 -Because 'master death must not automatically create or load a session'
            Start-Sleep -Milliseconds 500
        }
        (Get-ItLogText -App $script:app -Name 'wta-main_*.log' -SinceStart) |
            Should -Not -Match 'forwarding load_session|load_session requested' -Because 'crash handling must not invoke ACP session/load'

        Open-AgentPane -App $script:app | Out-Null
        $freshSession = Wait-NewAgentPaneSession -App $script:app `
            -OwnerPaneSessionId $script:shellPane.session_id `
            -ExcludePaneSessionId $script:initialSession.PaneSessionId `
            -TimeoutSec 30
        $freshSession | Should -Not -BeNullOrEmpty
        Wait-AgentReady -App $script:app -PaneSessionId $freshSession.PaneSessionId -TimeoutSec 90 |
            Should -BeTrue -Because 'an explicit pane open must create a usable fresh session'

        $recovered = & $script:GetMasters
        $recovered.Count | Should -Be 1 -Because 'the explicit pane open must start exactly one new shared master'
        ($killedPids -contains $recovered[0].ProcessId) | Should -BeFalse
        $freshSession.HelperProcessId | Should -Not -Be $script:initialSession.HelperProcessId
        $freshSession.AcpSessionId | Should -Not -Be $script:initialSession.AcpSessionId
        (Get-ItLogText -App $script:app -Name 'wta-main_*.log' -SinceStart) |
            Should -Not -Match 'forwarding load_session|load_session requested' -Because 'explicit reopen must create a fresh session rather than load the crashed one'
    }
}
