#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# No agent CLI or model is started. The real remote sender runs inside an owned
# tmux pane; COM events and the deployed master's source snapshot are the oracles.

BeforeDiscovery {
    $script:TmuxHooksConfigured = -not [string]::IsNullOrWhiteSpace($env:ITE2E_TMUX_SSH_HOST)
}

Describe 'Feature: native SSH tmux hook lifetime' -Tag 'Feature' -Skip:(-not $script:TmuxHooksConfigured) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $script:ownedWindows = [Collections.Generic.List[object]]::new()
        $script:fixtureCreated = $false
        $script:listener = $null
        $script:unavailable = $null
        $package = Get-ItTestPackage
        if ($package -notin @('Dev', 'IntelligentTerminal_rd9vj3e6a2mbr')) {
            throw 'This regression requires an explicitly selected, already running Dev feature build.'
        }
        $script:app = Resolve-ItApp -Package $package
        foreach ($binary in @(
            @{ Name = 'TerminalApp.dll'; Expected = $env:ITE2E_EXPECTED_TERMINALAPP_SHA256 },
            @{ Name = 'wta.exe'; Expected = $env:ITE2E_EXPECTED_WTA_SHA256 }
        )) {
            if ($binary.Expected -notmatch '^[0-9a-fA-F]{64}$') {
                throw "Set ITE2E_EXPECTED_TERMINALAPP_SHA256 and ITE2E_EXPECTED_WTA_SHA256 from the feature build before testing."
            }
            $actual = (Get-FileHash -LiteralPath (Join-Path $script:app.InstallLocation $binary.Name) -Algorithm SHA256).Hash
            if ($actual -ine $binary.Expected) { throw "Deployed $($binary.Name) differs from the feature build: $actual" }
            Write-Host "$($script:app.PackageFullName): $($binary.Name) SHA256=$actual"
        }
        # Do not stage an unpackaged copy or launch/reset Terminal. Use the
        # existing package's master pipe explicitly for every snapshot.
        $script:app | Add-Member -NotePropertyName WtaRunnable -NotePropertyValue $script:app.WtaPath -Force
        $terminals = @(Get-CimInstance Win32_Process -Filter "Name='WindowsTerminal.exe'" |
            Where-Object ExecutablePath -EQ $script:app.WindowsTerminal)
        $terminals.Count | Should -BeGreaterThan 0 -Because 'the operator must start the selected Dev app without resetting settings'
        $script:terminalPids = @($terminals.ProcessId)
        Resolve-WtComClsid -App $script:app | Out-Null
        $masters = @(Get-CimInstance Win32_Process -Filter "Name='wta.exe'" |
            Where-Object { $_.ExecutablePath -eq $script:app.WtaPath -and $_.CommandLine -match '\s--master\s' })
        $masters.Count | Should -Be 1 -Because 'snapshots must target the selected package master, not another installation'
        if ($masters[0].CommandLine -notmatch '--master\s+(?:"([^"]+)"|(\S+))') {
            throw 'Cannot resolve the selected Dev master pipe.'
        }
        $script:masterPipe = if ($Matches[1]) { $Matches[1] } else { $Matches[2] }
        $script:initialPane = Get-ActivePane -App $script:app
        $script:sshHost = $env:ITE2E_TMUX_SSH_HOST
        $script:sshUser = $env:ITE2E_TMUX_SSH_USER
        $script:sshPort = $env:ITE2E_TMUX_SSH_PORT
        if ($script:sshHost -notmatch '^[A-Za-z0-9][A-Za-z0-9_.-]*$' -or
            ($script:sshUser -and $script:sshUser -notmatch '^[A-Za-z0-9_][A-Za-z0-9_.-]*$') -or
            ($script:sshPort -and ($script:sshPort -notmatch '^\d{1,5}$' -or [int]$script:sshPort -notin 1..65535))) {
            throw 'Use a plain SSH alias/hostname, optional username, and optional numeric port (1-65535).'
        }
        $script:destination = if ($script:sshUser) { "$script:sshUser@$script:sshHost" } else { $script:sshHost }
        $script:sshSourceArgs = @()
        if ($script:sshUser) { $script:sshSourceArgs += @('-l', $script:sshUser) }
        if ($script:sshPort) { $script:sshSourceArgs += @('-p', $script:sshPort) }
        $script:sshPath = (Get-Command ssh.exe -ErrorAction Stop).Source
        $script:socket = 'it-e2e-hooks-' + [guid]::NewGuid().ToString('N')
        $script:remoteDirectory = ".$script:socket"
        if (-not ('ItE2E.TmuxHookWindow' -as [type])) {
            Add-Type -Namespace ItE2E -Name TmuxHookWindow -MemberDefinition @'
                [DllImport("user32.dll", SetLastError = true)]
                public static extern bool PostMessage(IntPtr hwnd, uint message, IntPtr wparam, IntPtr lparam);
                [DllImport("user32.dll")]
                public static extern bool IsWindow(IntPtr hwnd);
'@
        }

        function Quote-HookShell([string]$Value) { "'" + $Value.Replace("'", "'\''") + "'" }

        function Invoke-HookRemote([string]$Command, [switch]$AllowFailure) {
            $result = Invoke-Native -FilePath $script:sshPath -TimeoutSec 15 -Arguments (
                @('-T', '-o', 'BatchMode=yes', '-o', 'StrictHostKeyChecking=yes', '-o', 'ConnectTimeout=5',
                  '-o', 'ClearAllForwardings=yes', '-o', 'ForwardAgent=no', '-o', 'ForwardX11=no',
                  '-o', 'PermitLocalCommand=no', '-o', 'RemoteCommand=none') +
                $script:sshSourceArgs + @('--', $script:sshHost, $Command))
            if (-not $AllowFailure -and $result.ExitCode -ne 0) {
                throw "Owned remote fixture command failed ($($result.ExitCode)): $($result.StdErr)"
            }
            $result
        }

        function Invoke-HookTmux([string]$Command) {
            (Invoke-HookRemote "tmux -N -L $script:socket $Command").StdOut.Trim()
        }

        function Remove-HookSession([string]$Session) {
            $result = Invoke-HookRemote "tmux -N -L $script:socket kill-session -t $Session" -AllowFailure
            if ($result.ExitCode -ne 0 -and $result.StdErr -notmatch "can't find session|no server running|No such file or directory") {
                throw "Owned session cleanup failed: $($result.StdErr)"
            }
        }

        function Get-HookRows([string]$RawId, [switch]$HostSource, [switch]$OtherSource) {
            $arguments = @('sessions', 'list', '--master', $script:masterPipe, '--json')
            if (-not $HostSource) {
                $destination = if ($OtherSource) { "$script:destination-it-e2e-other" } else { $script:destination }
                $arguments += @('--ssh', $destination, '--cli', 'copilot')
                if ($script:sshPort) { $arguments += @('--port', $script:sshPort) }
            }
            $result = Invoke-Wta -App $script:app -Arguments $arguments -Raw -TimeoutSec 10
            if ($result.ExitCode -ne 0) { throw "Master snapshot failed: $($result.StdErr)" }
            @($result.StdOut -split '\r?\n' | Where-Object { $_.Trim() } |
                ForEach-Object { $_ | ConvertFrom-Json -Depth 32 } |
                Where-Object { $_.session_id -like "*$RawId*" })
        }

        function Wait-HookRow([string]$RawId, [string]$Status, [string]$NativePane) {
            Wait-Until -TimeoutSec 20 -IntervalSec 0.4 -Because "$RawId is $Status with expected native binding" -Condition {
                $rows = @(Get-HookRows $RawId)
                if ($rows.Count -eq 1 -and $rows[0].session_id -ceq $RawId -and
                    $rows[0].status -eq $Status -and [string]$rows[0].pane_session_id -eq $NativePane) {
                    $rows[0]
                }
            }
        }

        function Open-HookWindow([string]$Session) {
            $previous = @(Get-WtWindows -App $script:app | ForEach-Object window_id)
            $backend = (@('ssh.exe', '-T', '-o', 'BatchMode=yes') + $script:sshSourceArgs +
                @($script:sshHost, 'tmux', '-L', $script:socket, '-C', 'attach-session', '-t', $Session)) -join ' '
            (Invoke-WtCli -App $script:app -Arguments @('tmux', $backend)).state | Should -Be 'starting'
            $window = Wait-Until -TimeoutSec 25 -IntervalSec 0.4 -Because 'owned native tmux HWND' -Condition {
                Get-WtWindowHwnds -App $script:app |
                    Where-Object { $_.title -ceq "$script:socket/$Session" -and $_.pid -in $script:terminalPids } |
                    Select-Object -First 1
            }
            $owned = $script:app.PSObject.Copy()
            $owned.Hwnd = $window.hwnd
            $owned.Pid = $window.pid
            $script:ownedWindows.Add($owned)
            $native = Wait-Until -TimeoutSec 20 -IntervalSec 0.4 -Because 'owned native tmux pane inventory' -Condition {
                foreach ($candidate in @(Get-WtWindows -App $script:app | Where-Object window_id -NotIn $previous)) {
                    foreach ($tab in @(Get-WtTabs -App $script:app -WindowId $candidate.window_id | Where-Object title -EQ $Session)) {
                        Get-WtPanes -App $script:app -WindowId $candidate.window_id -TabId $tab.tab_id |
                            Where-Object { -not $_.is_agent_pane } | Select-Object -First 1
                    }
                }
            }
            $owned | Add-Member -NotePropertyName NativePane -NotePropertyValue ([string]$native.session_id)
            $owned
        }

        function Close-HookWindow($Window) {
            if ([ItE2E.TmuxHookWindow]::IsWindow([IntPtr]$Window.Hwnd)) {
                [ItE2E.TmuxHookWindow]::PostMessage([IntPtr]$Window.Hwnd, 0x10, [IntPtr]::Zero, [IntPtr]::Zero) |
                    Should -BeTrue -Because 'only the owned HWND receives WM_CLOSE; never kill-pane for detach'
                Wait-Until -TimeoutSec 10 -IntervalSec 0.2 -Because 'owned window closes' -Condition {
                    if (-not [ItE2E.TmuxHookWindow]::IsWindow([IntPtr]$Window.Hwnd)) { return $true }
                    if (Get-UiElement -App $Window -Selector PrimaryButton) {
                        Invoke-UiElement -App $Window -Selector PrimaryButton | Out-Null
                    }
                    -not [ItE2E.TmuxHookWindow]::IsWindow([IntPtr]$Window.Hwnd)
                } | Out-Null
            }
            [void]$script:ownedWindows.Remove($Window)
        }

        function Send-HookFromPane($Window, [string]$RawId, [string]$Event = 'agent.prompt.submit') {
            $payload = @{ session_id = $RawId; cwd = '.'; initial_prompt = $RawId; tool_name = 'it-e2e-tool' } |
                ConvertTo-Json -Compress
            # Unset the managed v3 route only inside our disposable shell.
            # The sender must discover real TMUX/TMUX_PANE metadata for v2.
            $command = 'unset IT_SSH_HOOK_ROUTE IT_SSH_HOOK_SOCKET IT_SSH_HOOK_SESSION; ' +
                'TMPDIR="$HOME/' + $script:remoteDirectory + '"; export TMPDIR; printf ''%s'' ' +
                (Quote-HookShell $payload) +
                ' | sh "$HOME/.intelligent-terminal/ssh-hooks/current/it-agent-hook.sh" --cli-source copilot --event ' + $Event
            Send-WtInput -App $script:app -SessionId $Window.NativePane -Text $command
            Send-WtKeys -App $script:app -SessionId $Window.NativePane -Keys Enter
            $nativePane = $Window.NativePane
            $hook = Wait-WtEvent -Listener $script:listener -TimeoutSec 15 -Predicate {
                $_.method -eq 'agent_event' -and $_.params.pane_id -eq $nativePane -and
                $_.params.event -eq $Event -and $_.params.agent_session_id -eq $RawId
            }
            $hook.params.tmux.pane_id | Should -Match '^%\d+$'
            $hook.params.tmux.session_id | Should -Match '^\$\d+$'
            $hook.params.tmux.socket_path | Should -Match ([regex]::Escape($script:socket) + '$')
            $hook.params.tmux.ssh_target.destination | Should -BeExactly $script:destination
            [string]$hook.params.tmux.ssh_target.port | Should -Be ([string]$script:sshPort)
        }

        function Assert-HookIsolation([string]$RawId) {
            @(Get-HookRows $RawId -HostSource).Count | Should -Be 0 -Because 'v2 must not fabricate a duplicate Host/tmux row'
            @(Get-HookRows $RawId -OtherSource).Count | Should -Be 0 -Because 'the snapshot must remain source scoped'
        }

        $probe = Invoke-HookRemote 'for utility in tmux sha256sum sh timeout mktemp rm rmdir head wc base64; do command -v "$utility" >/dev/null || exit 1; done; test -r "$HOME/.intelligent-terminal/ssh-hooks/current/it-agent-hook.sh" && tmux -V' -AllowFailure
        if ($probe.ExitCode -ne 0) {
            $script:unavailable = "SSH fixture unavailable (noninteractive SSH, tmux, sha256sum, preinstalled bundled sender): $($probe.StdErr)"
        } elseif ($probe.StdOut.Trim() -notmatch '^tmux (\d+)\.(\d+)' -or
            ([int]$Matches[1] -lt 3 -or ([int]$Matches[1] -eq 3 -and [int]$Matches[2] -lt 4))) {
            $script:unavailable = "The real v2 sender requires tmux >= 3.4: $($probe.StdOut.Trim())"
        }
        if (-not $script:unavailable) {
            $remoteHash = (Invoke-HookRemote 'sha256sum "$HOME/.intelligent-terminal/ssh-hooks/current/it-agent-hook.sh"').StdOut.Split(' ')[0]
            $bundledHash = (Get-FileHash -LiteralPath (Join-Path $script:app.InstallLocation 'wt-agent-hooks\tmux\it-agent-hook.sh')).Hash
            $remoteHash | Should -Be $bundledHash -Because 'preprovisioned remote sender must match the selected package; tests never install hooks'
            Invoke-HookRemote ('mkdir -m 700 "$HOME/' + $script:remoteDirectory + '"') | Out-Null
            $script:fixtureCreated = $true
            Invoke-HookRemote ('tmux -L ' + $script:socket +
                ' -f /dev/null new-session -d -s sentinel -n sentinel "sh"') | Out-Null
            Invoke-HookTmux 'set-option -g automatic-rename off' | Out-Null
            $script:listener = Start-WtEventListener -App $script:app -WaitForReady
        }
    }

    BeforeEach {
        if ($script:unavailable) { Set-ItResult -Skipped -Because $script:unavailable }
    }

    AfterAll {
        $cleanupErrors = [Collections.Generic.List[string]]::new()
        if ($script:fixtureCreated) {
            try {
                $result = Invoke-HookRemote "tmux -N -L $script:socket kill-server" -AllowFailure
                if ($result.ExitCode -ne 0 -and $result.StdErr -notmatch 'no server running|No such file or directory') {
                    throw "Owned socket cleanup failed: $($result.StdErr)"
                }
                Invoke-HookRemote ('rmdir -- "$HOME/' + $script:remoteDirectory + '"') | Out-Null
            } catch { $cleanupErrors.Add($_.ToString()) }
        }
        if ($script:ownedWindows) {
            foreach ($window in @($script:ownedWindows.ToArray())) {
                try { Close-HookWindow $window } catch { $cleanupErrors.Add($_.ToString()) }
            }
        }
        if ($script:listener) { Stop-WtEventListener -Listener $script:listener }
        if ($script:initialPane.session_id) {
            try { Set-WtPaneFocus -App $script:app -SessionId $script:initialPane.session_id } catch { Write-Warning $_ }
        }
        if ($cleanupErrors.Count) { throw ($cleanupErrors -join "`n") }
    }

    It 'Closing a native tmux window preserves remote activity and rebinds on attach' {
        $rawId = "$script:socket-detach"
        Invoke-HookTmux 'new-session -d -s detach -n detach "sh"' | Out-Null
        $originalProcess = Invoke-HookTmux "list-panes -t detach -F '#{pane_id}:#{pane_pid}:#{pane_start_command}'"
        $window = Open-HookWindow detach
        try {
            Send-HookFromPane $window $rawId
            Wait-HookRow $rawId Working $window.NativePane | Out-Null
            Send-HookFromPane $window $rawId 'agent.tool.starting'
            $before = Wait-Until -TimeoutSec 15 -IntervalSec 0.4 -Because 'nonempty current tool in shared registry' -Condition {
                Get-HookRows $rawId | Where-Object { $_.status -eq 'Working' -and $_.current_tool -eq 'it-e2e-tool' }
            }
            Assert-HookIsolation $rawId
            Set-WtPaneFocus -App $script:app -SessionId $window.NativePane
            (Get-ActivePane -App $script:app).session_id | Should -Be $window.NativePane
            $oldPane = $window.NativePane
            Close-HookWindow $window
            Wait-WtEvent -Listener $script:listener -TimeoutSec 10 -Predicate {
                $_.method -eq 'connection_state' -and $_.params.pane_id -eq $oldPane -and $_.params.state -eq 'detached'
            } | Out-Null
            Invoke-HookTmux 'has-session -t detach' | Out-Null
            Wait-Until -TimeoutSec 10 -IntervalSec 0.4 -Because 'no native receiver remains attached' -Condition {
                -not (Invoke-HookTmux 'list-clients -t detach')
            } | Out-Null
            $detached = Wait-HookRow $rawId Working ''
            foreach ($field in @('status', 'current_tool', 'last_error', 'title', 'last_activity_at_ms')) {
                $detached.$field | Should -Be $before.$field -Because "detach must preserve last-known $field"
            }
            # No observer is added: do not claim progress while no control
            # client exists. Reattachment itself must not start an agent.
            Assert-HookIsolation $rawId
            $reattached = Open-HookWindow detach
            $reattached.NativePane | Should -Not -Be $oldPane
            Invoke-HookTmux "list-panes -t detach -F '#{pane_id}:#{pane_pid}:#{pane_start_command}'" |
                Should -BeExactly $originalProcess -Because 'the observed remote pane must keep its original process across reattachment'
            Send-HookFromPane $reattached $rawId 'agent.stop'
            $refreshed = Wait-HookRow $rawId Idle $reattached.NativePane
            $refreshed.last_activity_at_ms | Should -BeGreaterThan $detached.last_activity_at_ms
            $refreshed.location.Ssh.target.destination | Should -BeExactly $script:destination
            [string]$refreshed.location.Ssh.target.port | Should -Be ([string]$script:sshPort)
            Assert-HookIsolation $rawId
            Set-WtPaneFocus -App $script:app -SessionId $refreshed.pane_session_id
            (Get-ActivePane -App $script:app).session_id | Should -Be $reattached.NativePane
            @(Get-WtEvents -Listener $script:listener -Predicate {
                $_.method -eq 'connection_state' -and $_.params.pane_id -eq $oldPane -and $_.params.state -eq 'closed'
            }).Count | Should -Be 0
            # The real control stream may also end with a bare %exit on
            # detach-client. Remote pane inventory must distinguish it from kill.
            Invoke-HookTmux 'detach-client -s detach' | Out-Null
            Wait-HookRow $rawId Idle '' | Out-Null
            Invoke-HookTmux 'has-session -t detach' | Out-Null
            $reattached = Open-HookWindow detach
            Send-HookFromPane $reattached $rawId
            Wait-HookRow $rawId Working $reattached.NativePane | Out-Null
            # End only the owned backend while it is observed, so cleanup does
            # not leave a synthetic live row behind in the user's master.
            Invoke-HookTmux 'kill-session -t detach' | Out-Null
            Wait-HookRow $rawId Ended '' | Out-Null
        } finally {
            try { Remove-HookSession detach } finally {
                foreach ($owned in @($script:ownedWindows.ToArray())) { Close-HookWindow $owned }
            }
        }
    }

    It 'Observed remote tmux exit ends the shared SSH session' {
        $rawId = "$script:socket-exit"
        Invoke-HookTmux 'new-session -d -s exiting -n exiting "sh"' | Out-Null
        $window = Open-HookWindow exiting
        try {
            Send-HookFromPane $window $rawId
            Wait-HookRow $rawId Working $window.NativePane | Out-Null
            $nativePane = $window.NativePane
            # The sentinel keeps the server alive. Destroying this session's
            # last pane can emit the same bare %exit as detach-client; only
            # the original server's pane inventory proves the pane is gone.
            Invoke-HookTmux 'kill-session -t exiting' | Out-Null
            Invoke-HookTmux 'has-session -t sentinel' | Out-Null
            $gone = Invoke-HookRemote "tmux -N -L $script:socket has-session -t exiting" -AllowFailure
            $gone.ExitCode | Should -Not -Be 0
            $gone.StdErr | Should -Match "can't find session"
            Wait-WtEvent -Listener $script:listener -TimeoutSec 15 -Predicate {
                $_.method -eq 'connection_state' -and $_.params.pane_id -eq $nativePane -and $_.params.state -eq 'closed'
            } | Out-Null
            Wait-HookRow $rawId Ended '' | Out-Null
            Assert-HookIsolation $rawId
            @(Get-WtEvents -Listener $script:listener -Predicate {
                $_.method -eq 'connection_state' -and $_.params.pane_id -eq $nativePane -and $_.params.state -eq 'detached'
            }).Count | Should -Be 0
        } finally {
            try { Remove-HookSession exiting } finally { Close-HookWindow $window }
        }
    }
}
