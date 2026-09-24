#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# Release checklist C318: a nested agent prompt must not replace the resumable
# session associated with its terminal pane.
#
# This crosses the native hook -> COM -> TerminalPage -> state.json boundary.
# The child ID is UUID-shaped on purpose: filtering only known prefixes would
# not reproduce the real Copilot behavior.

BeforeDiscovery { $script:Ready = [bool](Get-AppxPackage | Where-Object { $_.Name -like '*IntelligentTerminal*' }) }

Describe 'Feature §8 hook session ownership persistence' -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $script:app = $null
        $script:restoreApp = $null
        function script:Stop-SessionEndWta {
            param([int[]]$ProcessIds)
            foreach ($processId in $ProcessIds) {
                try {
                    Stop-Process -Id $processId -Force -ErrorAction Stop
                }
                catch {
                    if (Get-Process -Id $processId -ErrorAction SilentlyContinue) { throw }
                    Write-ItLog -Level INFO -Message "Owned WTA process $processId already exited during session-end cleanup."
                }
            }
        }
        if (-not ('ItE2E.SessionEndWindow' -as [type])) {
            Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
using System.Text;

namespace ItE2E
{
    public static class SessionEndWindow
    {
        private delegate bool EnumWindowsProc(IntPtr hwnd, IntPtr lparam);

        [DllImport("user32.dll")]
        private static extern bool EnumWindows(EnumWindowsProc callback, IntPtr lparam);
        [DllImport("user32.dll")]
        private static extern uint GetWindowThreadProcessId(IntPtr hwnd, out uint processId);
        [DllImport("user32.dll")]
        private static extern bool IsWindowVisible(IntPtr hwnd);
        [DllImport("user32.dll", CharSet = CharSet.Unicode)]
        private static extern int GetWindowText(IntPtr hwnd, StringBuilder text, int count);
        [DllImport("user32.dll")]
        public static extern IntPtr SendMessage(IntPtr hwnd, uint message, IntPtr wparam, IntPtr lparam);

        public static IntPtr FindEmperorWindow(uint targetProcessId)
        {
            var result = IntPtr.Zero;
            EnumWindows((hwnd, _) =>
            {
                GetWindowThreadProcessId(hwnd, out var processId);
                if (processId == targetProcessId && !IsWindowVisible(hwnd))
                {
                    var text = new StringBuilder(64);
                    GetWindowText(hwnd, text, text.Capacity);
                    if (text.ToString() == "Windows Terminal")
                    {
                        result = hwnd;
                        return false;
                    }
                }
                return true;
            }, IntPtr.Zero);
            return result;
        }
    }
}
'@
        }
    }

    AfterAll {
        if ($script:app) {
            Stop-Terminal -App $script:app -RestoreSettings:$false
        }
        if ($script:restoreApp) {
            Restore-WtConfig -App $script:restoreApp
        }
    }

    It 'Nested prompt keeps resumable pane owner' {
        $package = Get-ItTestPackage
        $targetApp = Resolve-ItApp -Package $package
        $script:restoreApp = $targetApp
        try {
            $script:app = Start-Terminal -Package $package -PassFre $true -Settings @{
                firstWindowPreference = 'persistedLayoutAndContent'
                agentSessionManagementEnabled = $false
                autoErrorDetectionEnabled = $false
                autoFixEnabled = $false
            }
            $script:restoreApp = $script:app
        }
        catch {
            Stop-AppInstances -App $targetApp
            Restore-WtConfig -App $targetApp
            $script:restoreApp = $null
            throw
        }

        $paneId = (Get-ActivePane -App $script:app).session_id
        $rootId = [guid]::NewGuid().ToString()
        $childId = [guid]::NewGuid().ToString()
        Initialize-LogOffsets -App $script:app | Out-Null
        $listener = Start-WtEventListener -App $script:app -WaitForReady
        try {
            foreach ($hook in @(
                    @{ Id = $rootId; Event = 'agent.session.start' }
                    @{ Id = $childId; Event = 'agent.prompt.submit' }
                )) {
                $payload = @{ session_id = $hook.Id; cwd = $TestDrive } | ConvertTo-Json -Compress
                $command = "'$payload' | wtcli.exe agent-hook --cli-source copilot --event $($hook.Event)"
                Invoke-RunCommand -App $script:app -SessionId $paneId -Command $command -SettleSec 2 | Out-Null
                Wait-WtEvent -Listener $listener -TimeoutSec 20 -Predicate {
                    $_.method -eq 'agent_event' -and
                    $_.params.event -eq $hook.Event -and
                    $_.params.agent_session_id -eq $hook.Id
                } | Should -Not -BeNullOrEmpty
            }
        }
        finally {
            Stop-WtEventListener -Listener $listener
        }

        Wait-Until -TimeoutSec 20 -Because 'TerminalPage to reject the nested prompt as pane ownership' -Condition {
            $log = Get-ItLogText -App $script:app -Name 'terminal-agent-pane.log' -SinceStart
            $log -match "ignored prompt session $([regex]::Escape($childId)) for already-bound pane"
        } | Should -BeTrue

        # Simulate the OS session-end path. It synchronously persists the window
        # layout before quitting, unlike a force-kill fallback in Stop-Terminal.
        $emperor = [ItE2E.SessionEndWindow]::FindEmperorWindow([uint32]$script:app.Pid)
        $emperor | Should -Not -Be ([IntPtr]::Zero)
        $queryResult = [ItE2E.SessionEndWindow]::SendMessage($emperor, 0x0011, [IntPtr]::Zero, [IntPtr]::Zero)
        $queryResult | Should -Not -Be ([IntPtr]::Zero)
        $wtaIds = @(Get-DescendantWtaIds -RootPid ([int]$script:app.Pid))
        [void][ItE2E.SessionEndWindow]::SendMessage($emperor, 0x0016, [IntPtr]::new(1), [IntPtr]::new(1))
        Test-Until -TimeoutSec 15 -IntervalSec 0.5 -Condition {
            $null -eq (Get-Process -Id $script:app.Pid -ErrorAction SilentlyContinue)
        } | Should -BeTrue
        script:Stop-SessionEndWta -ProcessIds $wtaIds
        $script:app = $null

        $state = Get-Content -Raw -LiteralPath $script:restoreApp.StatePath
        $state | Should -Match ([regex]::Escape($rootId)) -Because 'the pane must restore the resumable root session'
        $state | Should -Not -Match ([regex]::Escape($childId)) -Because 'a nested prompt ID is not a resumable pane owner'
    }

    It 'Antigravity WSL sessions persist an in-distro resume command' {
        $distro = $env:ITE2E_ANTIGRAVITY_WSL_DISTRO
        if ([string]::IsNullOrWhiteSpace($distro)) {
            Set-ItResult -Skipped -Because 'set ITE2E_ANTIGRAVITY_WSL_DISTRO to an installed distro with Windows interoperability'
            return
        }
        if ($distro -notmatch '^[A-Za-z0-9_.-]+$') {
            throw 'This fixture requires an unambiguous registered distro name.'
        }
        if ($script:restoreApp) {
            Restore-WtConfig -App $script:restoreApp
            $script:restoreApp = $null
        }
        $userArgument = ''
        if ($env:ITE2E_ANTIGRAVITY_WSL_USER) {
            if ($env:ITE2E_ANTIGRAVITY_WSL_USER -notmatch '^[A-Za-z0-9_.-]+$') {
                throw 'This fixture requires an unambiguous WSL user name.'
            }
            $userArgument = ' -u ' + $env:ITE2E_ANTIGRAVITY_WSL_USER
        }
        $command = 'wsl.exe -d ' + $distro + $userArgument + ' --cd /tmp --exec bash -l'
        $profileId = '{' + [guid]::NewGuid().ToString() + '}'
        $script:app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true -Settings @{
            firstWindowPreference = 'persistedLayoutAndContent'
            agentSessionManagementEnabled = $false
            autoErrorDetectionEnabled = $false
            autoFixEnabled = $false
            defaultProfile = $profileId
            profiles = @{
                defaults = @{}
                list = @(@{ guid = $profileId; name = 'Antigravity source regression'; commandline = $command })
            }
        }
        $script:restoreApp = $script:app
        $paneId = (Get-ActivePane -App $script:app).session_id

        $sessionId = "antigravity-source-$([guid]::NewGuid())"
        $payload = @{
            conversationId = $sessionId
            workspacePaths = @()
            transcriptPath = '/test/.gemini/antigravity-cli/brain/session/transcript.jsonl'
        } | ConvertTo-Json -Compress
        $encoded = [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($payload))
        $listener = Start-WtEventListener -App $script:app -WaitForReady
        try {
            $hook = "printf '%s' '$encoded' | base64 -d | WTA_HOOK_CWD=`"`$PWD`" WSLENV=`"`$WSLENV`:WSL_DISTRO_NAME/w:WTA_HOOK_CWD/w`" powershell.exe -NoLogo -NoProfile -NonInteractive -Command 'wtcli.exe agent-hook --cli-source AnTiGrAvItY --event agent.prompt.submit'"
            Invoke-RunCommand -App $script:app -SessionId $paneId -Command $hook -SettleSec 3 | Out-Null
            $event = Wait-WtEvent -Listener $listener -TimeoutSec 20 -Predicate {
                $_.method -eq 'agent_event' -and $_.params.agent_session_id -eq $sessionId
            }
            $event.params.wsl_distro | Should -BeExactly $distro
            $event.params.pane_id | Should -Be $paneId
            Wait-Until -TimeoutSec 15 -Because 'the native persistence map to bind this exact shell session' -Condition {
                (Get-ItLogText -App $script:app -Name 'terminal-agent-pane.log' -SinceStart) -match
                    "bound pane .* to session $([regex]::Escape($sessionId))"
            } | Should -BeTrue
        }
        catch {
            $artifactDirectory = if ($env:ITE2E_ARTIFACT_ROOT) { $env:ITE2E_ARTIFACT_ROOT } else { $TestDrive }
            Get-WtCapture -App $script:app -SessionId $paneId -MaxLines 30 |
                Set-Content -LiteralPath (Join-Path $artifactDirectory 'antigravity-wsl-source-failure.txt') -Encoding utf8
            throw
        }
        finally {
            Stop-WtEventListener -Listener $listener
        }

        $emperor = [ItE2E.SessionEndWindow]::FindEmperorWindow([uint32]$script:app.Pid)
        $emperor | Should -Not -Be ([IntPtr]::Zero)
        [void][ItE2E.SessionEndWindow]::SendMessage($emperor, 0x0011, [IntPtr]::Zero, [IntPtr]::Zero)
        $ownedWta = @(Get-DescendantWtaIds -RootPid ([int]$script:app.Pid))
        [void][ItE2E.SessionEndWindow]::SendMessage($emperor, 0x0016, [IntPtr]::new(1), [IntPtr]::new(1))
        Test-Until -TimeoutSec 15 -IntervalSec 0.5 -Condition {
            $null -eq (Get-Process -Id $script:app.Pid -ErrorAction SilentlyContinue)
        } | Should -BeTrue
        script:Stop-SessionEndWta -ProcessIds $ownedWta
        $script:app = $null
        $state = Get-Content -LiteralPath $script:restoreApp.StatePath -Raw | ConvertFrom-Json -AsHashtable -Depth 100
        function Find-ResumeCommand($value) {
            if ($value -is [System.Collections.IDictionary]) {
                if ($value.commandline -and $value.commandline.Contains($sessionId)) { $value.commandline }
                foreach ($child in $value.Values) { Find-ResumeCommand $child }
            }
            elseif ($value -is [array]) {
                foreach ($child in $value) { Find-ResumeCommand $child }
            }
        }
        $commands = @(Find-ResumeCommand $state)
        $commands.Count | Should -Be 1
        $commands[0] | Should -BeExactly ('wsl.exe -d ' + $distro + ' --cd "/tmp" --exec bash -lc "exec agy --conversation ' + $sessionId + '"')
    }
}
