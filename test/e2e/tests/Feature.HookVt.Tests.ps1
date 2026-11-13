#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }

BeforeDiscovery {
    $script:Ready = [bool](Get-AppxPackage | Where-Object Name -Like '*IntelligentTerminal*')
}

Describe 'Feature: VT hook transport' -Tag @('Feature', 'HookVt') -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        . (Join-Path $PSScriptRoot 'helpers\HookInvocation.ps1')
        $script:app = $null
        $script:SkipReason = $null
        $target = Resolve-ItApp -Package (Get-ItTestPackage)
        foreach ($entry in @(
            @{ File = 'TerminalApp.dll'; Hash = $env:ITE2E_EXPECTED_APP_SHA256 },
            @{ File = 'Microsoft.Terminal.Control.dll'; Hash = $env:ITE2E_EXPECTED_CONTROL_SHA256 },
            @{ File = 'wta.exe'; Hash = $env:ITE2E_EXPECTED_WTA_SHA256 }
        )) {
            if (-not $entry.Hash) { throw "Exact-build hash is required for $($entry.File)." }
            (Get-FileHash -LiteralPath (Join-Path $target.InstallLocation $entry.File)).Hash | Should -Be $entry.Hash
        }
        & (Get-Module ItE2E) { Initialize-WtWin32Input }
        if (-not ('ItE2E.HookVtDesktopName' -as [type])) {
            Add-Type -Namespace ItE2E -Name HookVtDesktopName -MemberDefinition @'
    [System.Runtime.InteropServices.DllImport("user32.dll", CharSet=System.Runtime.InteropServices.CharSet.Unicode, SetLastError=true)]
    public static extern bool GetUserObjectInformation(System.IntPtr handle, int index, System.Text.StringBuilder value, uint length, out uint needed);
'@
        }
        $desktop = [ItE2E.ItWtWin32Input]::OpenInputDesktop(0, $false, 1)
        if ($desktop -eq [IntPtr]::Zero) {
            $script:SkipReason = 'Interactive desktop is not readable (Win32 error ' +
                [Runtime.InteropServices.Marshal]::GetLastWin32Error() + ').'
            return
        }
        try {
            $name = [Text.StringBuilder]::new(256)
            $needed = [uint32]0
            if (-not [ItE2E.HookVtDesktopName]::GetUserObjectInformation($desktop, 2, $name, 512, [ref]$needed) -or
                $name.ToString() -ne 'Default') {
                $script:SkipReason = 'Visible progress validation requires the Default interactive desktop.'
                return
            }
        }
        finally { [ItE2E.ItWtWin32Input]::CloseDesktop($desktop) | Out-Null }
        $root = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..\..\..'))
        $artifacts = if ($env:ITE2E_ARTIFACT_ROOT) { $env:ITE2E_ARTIFACT_ROOT } else { Join-Path $root 'Generated Files' }
        $script:caseDir = Join-Path $artifacts ('hook-vt-' + [guid]::NewGuid().ToString('N'))
        New-Item -ItemType Directory -Path $script:caseDir | Out-Null
        $script:bundle = Join-Path $target.InstallLocation 'wt-agent-hooks'
        foreach ($file in @('emit-hook.ps1', 'emit-hook.sh', 'copilot\wt-agent-hooks\hooks\hooks.json')) {
            (Get-FileHash -LiteralPath (Join-Path $script:bundle $file)).Hash |
                Should -Be (Get-FileHash -LiteralPath (Join-Path $root "tools\wta\wt-agent-hooks\$file")).Hash
        }

        $pwsh = [Environment]::ProcessPath
        $fixture = Join-Path $PSScriptRoot '..\fixtures\Mock-AcpInteractionAgent.ps1'
        $fixtureCode = "& '$($fixture.Replace("'", "''"))' -LogPath '$($script:caseDir.Replace("'", "''"))\acp.log'"
        $encoded = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($fixtureCode))
        $profile = '{' + [guid]::NewGuid().ToString() + '}'
        $script:app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true -State @{
            featureRolloutId = '{00000000-0000-0000-0000-000000000001}'
            sidebarRolloutVersion = 1
            sidebarRolloutAssignment = 'Sidebar'
            sidebarLayoutMigrationCompleted = $true
            sidebarIntroductionShown = $true
        } -Settings @{
            language = 'en-US'; tabLayout = 'vertical'; startupActions = ''
            firstWindowPreference = 'defaultProfile'; windowingBehavior = 'useNew'
            agentSessionManagementEnabled = $false
            autoErrorDetectionEnabled = $false; autoFixEnabled = $false
            'warning.confirmOnClose' = 'never'
            acpAgent = 'custom:hook-vt-fixture'
            acpCustomCommand = "`"$pwsh`" -NoProfile -EncodedCommand $encoded"
            defaultProfile = $profile
            profiles = @{ list = @(@{
                guid = $profile; name = 'ItE2E VT hook PowerShell'
                commandline = "`"$pwsh`" -NoLogo -NoProfile -NoExit"
                shellIntegrationEnabled = $false
            }) }
        }
        Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes

        function script:Get-HookProgressRings {
            param([switch]$Determinate)
            $window = [Windows.Automation.AutomationElement]::FromHandle([IntPtr][long]$script:app.Hwnd)
            if (-not $window -or $window.Current.ProcessId -ne $script:app.Pid) {
                throw 'Progress oracle lost the owned Terminal window.'
            }
            $headerId = if ($Determinate) { 'HeaderProgressRing' } else { 'HeaderIndeterminateProgressRing' }
            $paneId = if ($Determinate) { 'PaneProgressRing' } else { 'PaneIndeterminateProgressRing' }
            $tabId = if ($Determinate) { 'TabProgressRing' } else { 'TabIndeterminateProgressRing' }
            $conditions = foreach ($id in @($headerId, $paneId, $tabId)) {
                [Windows.Automation.PropertyCondition]::new(
                    [Windows.Automation.AutomationElement]::AutomationIdProperty, $id)
            }
            $condition = [Windows.Automation.OrCondition]::new([Windows.Automation.Condition[]]$conditions)
            @($window.FindAll([Windows.Automation.TreeScope]::Descendants, $condition) | Where-Object {
                $_.Current.ProcessId -eq $script:app.Pid -and -not $_.Current.IsOffscreen -and
                $_.Current.BoundingRectangle.Width -gt 0 -and $_.Current.BoundingRectangle.Height -gt 0
            })
        }
        function script:Test-HookNativePercentage {
            $rings = @(Get-HookProgressRings -Determinate)
            if ($rings.Count -ne 1) { return $false }
            $range = $null
            $rings[0].TryGetCurrentPattern([Windows.Automation.RangeValuePattern]::Pattern, [ref]$range) -and
                $range.Current.Value -eq 42
        }
        function script:Save-HookProgressEvidence {
            param([string]$Phase)
            $peers = @(@(Get-HookProgressRings) + @(Get-HookProgressRings -Determinate))
            $observations = foreach ($peer in $peers) {
                $range = $null
                $hasRange = $peer.TryGetCurrentPattern([Windows.Automation.RangeValuePattern]::Pattern, [ref]$range)
                @{
                    id = $peer.Current.AutomationId
                    name = $peer.Current.Name
                    value = if ($hasRange) { $range.Current.Value } else { $null }
                }
            }
            $window = [Windows.Automation.AutomationElement]::FromHandle([IntPtr][long]$script:app.Hwnd)
            @{ phase = $Phase; window_offscreen = $window.Current.IsOffscreen; progress = @($observations) } | ConvertTo-Json -Depth 5 |
                Set-Content -LiteralPath (Join-Path $script:caseDir "$Phase-progress.json") -Encoding utf8
        }
    }

    AfterAll {
        if ($script:app) { Stop-Terminal -App $script:app }
    }

    It 'VT hooks drive tab indicators without wtcli' {
        if ($script:SkipReason) { Set-ItResult -Skipped -Because $script:SkipReason; return }
        $pane = Get-ActivePane -App $script:app
        $session = 'hook-vt-' + [guid]::NewGuid().ToString('N')
        $payloadFile = Join-Path $script:caseDir "payload 'quoted'.json"
        [IO.File]::WriteAllText($payloadFile, (@{
            session_id = $session; cwd = $script:caseDir; prompt = 'private-hook-prompt'
        } | ConvertTo-Json -Compress))
        $listener = Start-WtEventListener -App $script:app -WaitForReady -ExistingOnly
        try {
            Wait-Until -TimeoutSec 15 -Because 'the owned tab starts without progress' -Condition {
                @(Get-HookProgressRings).Count -eq 0
            } | Out-Null
            Save-HookProgressEvidence before
            foreach ($topic in @('agent.prompt.submit', 'agent.stop', 'agent.tool.finished')) {
                $invoke = New-AgentHookInvocation -BundleRoot $script:bundle -Cli copilot -Event $topic -PayloadFile $payloadFile
                $missing = 'hook-vt-missing-' + [guid]::NewGuid().ToString('N') + '.exe'
                $command = "`$oldBridge=`$env:WTCLI_PATH; try { `$env:WTCLI_PATH='$missing'; $invoke } finally { `$env:WTCLI_PATH=`$oldBridge }"
                Invoke-RunCommand -App $script:app -SessionId $pane.session_id -Command $command -SettleSec 3 | Out-Null
                $event = Wait-WtEvent -Listener $listener -TimeoutSec 15 -Predicate {
                    $_.method -eq 'agent_event' -and $_.params.event -eq $topic -and
                    $_.params.agent_session_id -eq $session
                }
                $event.params.pane_id | Should -Be $pane.session_id
                $event.params.payload.PSObject.Properties.Name | Should -Not -Contain 'prompt'
                Save-HookProgressEvidence "after-$topic"
                if ($topic -eq 'agent.prompt.submit') {
                    Wait-Until -TimeoutSec 15 -Because 'the shipped hook activates the tab progress ring' -Condition {
                        @(Get-HookProgressRings).Count -eq 1
                    } | Out-Null
                }
                else {
                    Wait-Until -TimeoutSec 15 -Because 'Stop and late tool returns leave the tab idle' -Condition {
                        @(Get-HookProgressRings).Count -eq 0
                    } | Out-Null
                }
            }
            $until = [datetime]::UtcNow.AddSeconds(1)
            do {
                @(Get-HookProgressRings).Count | Should -Be 0
                Start-Sleep -Milliseconds 100
            } while ([datetime]::UtcNow -lt $until)

            foreach ($state in @(3, 0)) {
                Invoke-RunCommand -App $script:app -SessionId $pane.session_id -SettleSec 3 `
                    -Command "[Console]::Write(([char]27)+']9;4;$state'+([char]7))" | Out-Null
                $expected = if ($state -eq 3) { 1 } else { 0 }
                Wait-Until -TimeoutSec 15 -Because 'custom hooks can use the standard progress protocol directly' -Condition {
                    @(Get-HookProgressRings).Count -eq $expected
                } | Out-Null
            }

            foreach ($native in @($true, $false)) {
                $nativeSequence = if ($native) { '1;42' } else { '0' }
                Invoke-RunCommand -App $script:app -SessionId $pane.session_id -SettleSec 3 `
                    -Command "[Console]::Write(([char]27)+']9;4;$nativeSequence'+([char]7))" | Out-Null
                if ($native) {
                    Wait-Until -TimeoutSec 15 -Because 'native percentage progress is visible before hooks' -Condition {
                        Test-HookNativePercentage
                    } | Out-Null
                }
                $customSession = 'custom-vt-' + [guid]::NewGuid().ToString('N')
                foreach ($topic in @('agent.session.start', 'agent.prompt.submit', 'agent.stop')) {
                    $path = Join-Path $script:caseDir "$topic.json"
                    [IO.File]::WriteAllText($path, (@{
                        event = $topic; cli_source = 'custom:hook-vt-fixture'
                        agent_session_id = $customSession; payload = @{ cwd = $script:caseDir }
                    } | ConvertTo-Json -Compress))
                    $command = "[Console]::Write(([char]27)+']9001;AgentEvent;'+[IO.File]::ReadAllText('$($path.Replace("'", "''"))')+([char]7))"
                    Invoke-RunCommand -App $script:app -SessionId $pane.session_id -Command $command -SettleSec 3 | Out-Null
                    $event = Wait-WtEvent -Listener $listener -TimeoutSec 15 -Predicate {
                        $_.method -eq 'agent_event' -and $_.params.event -eq $topic -and
                        $_.params.agent_session_id -eq $customSession
                    }
                    $event.params.pane_id | Should -Be $pane.session_id
                    if ($native) {
                        Wait-Until -TimeoutSec 15 -Because 'hooks preserve a previously reported native percentage' -Condition {
                            Test-HookNativePercentage
                        } | Out-Null
                    }
                    else {
                        $expected = if ($topic -eq 'agent.prompt.submit') { 1 } else { 0 }
                        Wait-Until -TimeoutSec 15 -Because 'custom structured hooks update the same tab indicator' -Condition {
                            @(Get-HookProgressRings).Count -eq $expected
                        } | Out-Null
                    }
                }
            }
        }
        finally {
            Stop-WtEventListener -Listener $listener
        }
    }
}
