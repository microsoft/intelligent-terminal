#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# Uses only uniquely named sessions on an explicitly configured SSH host.

BeforeDiscovery {
    $script:TmuxBrowserConfigured = -not [string]::IsNullOrWhiteSpace($env:ITE2E_TMUX_SSH_HOST)
}

Describe 'Feature: default tmux session browser' -Tag 'Feature' -Skip:(-not $script:TmuxBrowserConfigured) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        if (-not ('ItE2E.TmuxBrowserTestWindow' -as [type])) {
            Add-Type -Namespace ItE2E -Name TmuxBrowserTestWindow -MemberDefinition @'
                [DllImport("user32.dll", SetLastError = true)]
                public static extern bool PostMessage(IntPtr hwnd, uint message, IntPtr wparam, IntPtr lparam);
                [DllImport("user32.dll")]
                public static extern bool IsWindow(IntPtr hwnd);
'@
        }
        if (-not ('ItE2E.TmuxBrowserLauncher' -as [type])) {
            Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
namespace ItE2E {
    [ComImport, Guid("2E941141-7F97-4756-BA1D-9DECDE894A3D"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    interface IApplicationActivationManager {
        [PreserveSig] int ActivateApplication([MarshalAs(UnmanagedType.LPWStr)] string appId,
            [MarshalAs(UnmanagedType.LPWStr)] string arguments, uint options, out uint processId);
    }
    [ComImport, Guid("45BA127D-10A8-46EA-8AB7-56EA9078943C")]
    class ApplicationActivationManager { }
    public static class TmuxBrowserLauncher {
        public static uint Launch(string appId, string arguments) {
            var manager = (IApplicationActivationManager)new ApplicationActivationManager();
            try {
                uint processId;
                Marshal.ThrowExceptionForHR(manager.ActivateApplication(appId, arguments, 0, out processId));
                return processId;
            } finally { Marshal.ReleaseComObject(manager); }
        }
    }
}
'@
        }
        $script:sshHost = $env:ITE2E_TMUX_SSH_HOST
        if ($script:sshHost -notmatch '^[A-Za-z0-9][A-Za-z0-9_.-]*$') {
            throw 'ITE2E_TMUX_SSH_HOST must be an SSH config alias or hostname without options.'
        }
        $script:sshPath = (Get-Command ssh.exe -ErrorAction Stop).Source
        $script:app = Resolve-ItApp -Package (Get-ItTestPackage)
        $hash = (Get-FileHash -LiteralPath (Join-Path $script:app.InstallLocation 'TerminalApp.dll') -Algorithm SHA256).Hash
        if ($env:ITE2E_EXPECTED_TERMINALAPP_SHA256 -and $hash -ine $env:ITE2E_EXPECTED_TERMINALAPP_SHA256) {
            throw "Deployed TerminalApp.dll does not match the intended build: $hash"
        }
        Write-Host "Testing $($script:app.PackageFullName); TerminalApp.dll SHA256=$hash"
        Resolve-WtComClsid -App $script:app | Out-Null
        $script:prefix = 'it-browser-' + [guid]::NewGuid().ToString('N')
        $script:ownedWindows = [Collections.Generic.List[object]]::new()
        $script:ownedPanes = [Collections.Generic.List[string]]::new()
        $script:sessions = [Collections.Generic.List[object]]::new()

        function Quote-BrowserArgument([string]$Value) {
            "'" + $Value.Replace("'", "'\''") + "'"
        }

        function Invoke-BrowserTmux {
            param([Parameter(Mandatory)][string]$Command, [string]$Socket = 'default')
            $result = Invoke-Native -FilePath $script:sshPath -TimeoutSec 15 -Arguments @(
                '-T', '-o', 'BatchMode=yes', '-o', 'ConnectTimeout=5',
                $script:sshHost, "tmux -L $Socket $Command"
            )
            if ($result.ExitCode -ne 0) {
                throw "SSH tmux $Command failed ($($result.ExitCode)): $($result.StdErr.Trim())"
            }
            $result.StdOut.Trim()
        }

        function New-BrowserSession([string]$Name, [string]$Socket = 'default') {
            $id = Invoke-BrowserTmux -Socket $Socket -Command "new-session -d -P -F '#{session_id}' -s $(Quote-BrowserArgument $Name)"
            $session = [pscustomobject]@{ Id = $id; Name = $Name; Socket = $Socket }
            $script:sessions.Add($session)
            $session
        }

        function Wait-BrowserWindow([string]$Title, [object[]]$Previous) {
            $window = Wait-Until -TimeoutSec 30 -IntervalSec 0.5 -Quiet -Because "new tmux window $Title" -Condition {
                Get-WtWindowHwnds -App $script:app |
                    Where-Object { $_.title -ceq $Title -and $_.hwnd -notin $Previous } |
                    Select-Object -First 1
            }
            if (-not $window) {
                $live = @(Get-WtWindowHwnds -App $script:app)
                Write-Host ($live | ConvertTo-Json -Compress)
                Write-Host (Invoke-BrowserTmux "list-clients -F '#{client_pid} #{session_name}'")
                throw "No native window appeared with title '$Title'."
            }
            $context = $script:app.PSObject.Copy()
            $context.Hwnd = $window.hwnd
            $context.Pid = $window.pid
            $script:ownedWindows.Add($context)
            $context
        }

        function Close-BrowserWindow($App) {
            if ([ItE2E.TmuxBrowserTestWindow]::IsWindow([IntPtr]$App.Hwnd)) {
                if (-not [ItE2E.TmuxBrowserTestWindow]::PostMessage([IntPtr]$App.Hwnd, 0x10, [IntPtr]::Zero, [IntPtr]::Zero)) {
                    throw 'Could not close the owned tmux browser test window.'
                }
                $closed = Test-Until -TimeoutSec 10 -IntervalSec 0.2 -Condition {
                    if (-not [ItE2E.TmuxBrowserTestWindow]::IsWindow([IntPtr]$App.Hwnd)) { return $true }
                    # Respect confirmOnClose without rewriting the user's settings.
                    if (Get-UiElement -App $App -Selector PrimaryButton) {
                        Invoke-UiElement -App $App -Selector PrimaryButton | Out-Null
                    }
                    -not [ItE2E.TmuxBrowserTestWindow]::IsWindow([IntPtr]$App.Hwnd)
                }
                if (-not $closed) { throw "Owned test window $($App.Hwnd) did not close." }
            }
            [void]$script:ownedWindows.Remove($App)
        }

        function Open-BrowserMenu($App) {
            Invoke-UiElement -App $App -Selector TmuxSessionsButton | Out-Null
        }
    }

    AfterAll {
        if ($script:ownedWindows) {
            foreach ($window in @($script:ownedWindows.ToArray())) {
                Close-BrowserWindow $window
            }
            if ($script:ownedPanes) {
                foreach ($sessionId in $script:ownedPanes) {
                    Close-WtPane -App $script:app -SessionId $sessionId
                }
            }
        }
        if ($script:sessions) {
            foreach ($session in $script:sessions) {
                Invoke-BrowserTmux -Socket $session.Socket -Command "kill-session -t $(Quote-BrowserArgument $session.Id)" | Out-Null
            }
        }
    }

    It 'Ordinary SSH tabs expose the tmux session menu' {
        $target = New-BrowserSession "$script:prefix-ordinary"
        $title = "$script:prefix-ssh"
        $before = @(Get-WtWindowHwnds -App $script:app | ForEach-Object hwnd)
        [ItE2E.TmuxBrowserLauncher]::Launch($script:app.AppUserModelId, "-w new new-tab --title $title --suppressApplicationTitle ssh.exe $script:sshHost") | Out-Null
        $sourceWindow = Wait-BrowserWindow $title $before
        Set-WtWindowForeground -App $sourceWindow | Should -BeTrue
        $sourceTab = Wait-Until -TimeoutSec 10 -IntervalSec 0.25 -Quiet -Because 'exact test SSH tab identity' -Condition {
            foreach ($window in @(Get-WtWindows -App $sourceWindow)) {
                Get-WtTabs -App $sourceWindow -WindowId $window.window_id |
                    Where-Object title -CEQ $title |
                    Select-Object -First 1
            }
        }
        $sourceTab | Should -Not -BeNullOrEmpty
        $sourcePane = Get-WtPanes -App $sourceWindow -WindowId $sourceTab.window_id -TabId $sourceTab.tab_id |
            Where-Object { -not $_.is_agent_pane } |
            Select-Object -First 1
        $sourcePane | Should -Not -BeNullOrEmpty
        $script:ownedPanes.Add([string]$sourcePane.session_id)
        Wait-UiElement -App $sourceWindow -Selector TmuxSessionsButton -TimeoutSec 15 | Out-Null
        Invoke-BrowserTmux "list-clients -t $(Quote-BrowserArgument $target.Id)" | Should -BeNullOrEmpty

        Open-BrowserMenu $sourceWindow
        $selector = 'TmuxSession_' + $target.Id.TrimStart('$')
        Wait-UiElement -App $sourceWindow -Selector $selector -TimeoutSec 15 | Out-Null
        $previous = @(Get-WtWindowHwnds -App $script:app | ForEach-Object hwnd)
        Invoke-UiElement -App $sourceWindow -Selector $selector | Out-Null
        $attached = Wait-BrowserWindow $target.Name $previous
        Invoke-BrowserTmux "list-clients -t $(Quote-BrowserArgument $target.Id) -F '#{session_name}'" |
            Should -BeExactly $target.Name
        [ItE2E.TmuxBrowserTestWindow]::IsWindow([IntPtr]$sourceWindow.Hwnd) | Should -BeTrue
        Wait-UiElement -App $attached -Selector TmuxSessionsButton -TimeoutSec 15 | Out-Null
        Close-BrowserWindow $attached

        Set-WtPaneFocus -App $sourceWindow -SessionId $sourcePane.session_id
        Set-WtWindowForeground -App $sourceWindow | Should -BeTrue
        $local = New-WtTab -App $sourceWindow -Command 'cmd.exe /d' -Title "$script:prefix-local"
        $script:ownedPanes.Add([string]$local.session_id)
        Set-WtPaneFocus -App $sourceWindow -SessionId $local.session_id
        (Test-Until -TimeoutSec 10 -IntervalSec 0.25 -Condition {
            $button = Get-UiElement -App $sourceWindow -Selector TmuxSessionsButton
            -not $button -or $button.isOffscreen
        }) | Should -BeTrue -Because 'local tabs must hide the SSH-only menu'
        Set-WtPaneFocus -App $sourceWindow -SessionId $sourcePane.session_id
        Wait-UiElement -App $sourceWindow -Selector TmuxSessionsButton -TimeoutSec 10 | Out-Null
        Open-BrowserMenu $sourceWindow
        try {
            Wait-UiElement -App $sourceWindow -Selector $selector -TimeoutSec 15 | Out-Null
        } catch {
            Write-Host (Get-UiTree -App $sourceWindow -Depth 7 -Interactive)
            Write-Host (Invoke-BrowserTmux "list-sessions -F '#{session_id} #{session_name}'")
            throw
        }
        Send-WtWindowKey -App $sourceWindow -Vk 0x1B -RequireForeground | Out-Null
        Close-WtPane -App $sourceWindow -SessionId $local.session_id
        [void]$script:ownedPanes.Remove([string]$local.session_id)
        Close-WtPane -App $sourceWindow -SessionId $sourcePane.session_id
        [void]$script:ownedPanes.Remove([string]$sourcePane.session_id)
    }

    It 'Default tmux session menu opens an independent window' {
        $source = New-BrowserSession "$script:prefix-source"
        $target = New-BrowserSession "$script:prefix-target"
        $otherSocket = New-BrowserSession "$script:prefix-hidden" "$script:prefix-other"
        $previous = @(Get-WtWindowHwnds -App $script:app | ForEach-Object hwnd)
        $launch = Invoke-WtCli -App $script:app -Arguments @('tmux', '--ssh', $script:sshHost, '--session', $source.Name)
        $launch.state | Should -Be 'starting'
        $sourceWindow = Wait-BrowserWindow $source.Name $previous
        Wait-UiElement -App $sourceWindow -Selector TmuxSessionsButton -TimeoutSec 15 | Out-Null

        Open-BrowserMenu $sourceWindow
        $targetSelector = 'TmuxSession_' + $target.Id.TrimStart('$')
        Wait-UiElement -App $sourceWindow -Selector $targetSelector -TimeoutSec 15 | Out-Null
        (Get-UiElement -App $sourceWindow -Selector $targetSelector).name | Should -BeExactly $target.Name
        Get-UiElement -App $sourceWindow -Selector $otherSocket.Name | Should -BeNullOrEmpty

        # Rename after opening the menu: the row must attach by stable ID.
        $renamed = "$script:prefix 'work' $([char]0x4f1a)$([char]0x8bdd)"
        Invoke-BrowserTmux "rename-session -t $(Quote-BrowserArgument $target.Id) $(Quote-BrowserArgument $renamed)" | Out-Null
        $previous = @(Get-WtWindowHwnds -App $script:app | ForEach-Object hwnd)
        Invoke-UiElement -App $sourceWindow -Selector $targetSelector | Out-Null
        $targetWindow = Wait-BrowserWindow $renamed $previous
        $targetWindow.Hwnd | Should -Not -Be $sourceWindow.Hwnd
        $targetWindow.Pid | Should -Be $sourceWindow.Pid
        Invoke-BrowserTmux "list-clients -t $(Quote-BrowserArgument $source.Id) -F '#{session_name}'" |
            Should -BeExactly $source.Name
        Invoke-BrowserTmux "list-clients -t $(Quote-BrowserArgument $target.Id) -F '#{session_name}'" |
            Should -BeExactly $renamed

        Close-BrowserWindow $targetWindow
        Invoke-BrowserTmux "has-session -t $(Quote-BrowserArgument $target.Id)" | Out-Null
        Open-BrowserMenu $sourceWindow
        Wait-UiElement -App $sourceWindow -Selector $targetSelector -TimeoutSec 15 | Out-Null
        (Get-UiElement -App $sourceWindow -Selector $targetSelector).name | Should -BeExactly $renamed
        Send-WtWindowKey -App $sourceWindow -Vk 0x1B -RequireForeground | Out-Null

        $previous = @(Get-WtWindowHwnds -App $script:app | ForEach-Object hwnd)
        Invoke-WtCli -App $script:app -Arguments @(
            'tmux', "ssh.exe -T -o BatchMode=yes $script:sshHost tmux -L default -C attach-session -t $($source.Name)"
        ) | Out-Null
        $opaqueWindow = Wait-BrowserWindow $source.Name $previous
        Get-UiElement -App $opaqueWindow -Selector TmuxSessionsButton | Should -BeNullOrEmpty
        Close-BrowserWindow $opaqueWindow
        [ItE2E.TmuxBrowserTestWindow]::IsWindow([IntPtr]$sourceWindow.Hwnd) | Should -BeTrue
    }
}
