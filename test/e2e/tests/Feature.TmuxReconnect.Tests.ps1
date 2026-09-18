#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# Opt in with ITE2E_TMUX_SSH_HOST, an SSH config alias with noninteractive
# authentication and tmux installed. Only this suite's windows and sessions close.

BeforeDiscovery {
    $script:TmuxSshConfigured = -not [string]::IsNullOrWhiteSpace($env:ITE2E_TMUX_SSH_HOST)
}

Describe 'Feature: native tmux reconnect' -Tag 'Feature' -Skip:(-not $script:TmuxSshConfigured) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        Add-Type -AssemblyName UIAutomationClient
        Add-Type -AssemblyName UIAutomationTypes
        if (-not ('ItE2E.TmuxTestWindow' -as [type])) {
            Add-Type -Namespace ItE2E -Name TmuxTestWindow -MemberDefinition @'
                [DllImport("user32.dll", SetLastError = true)]
                public static extern bool PostMessage(IntPtr hwnd, uint message, IntPtr wparam, IntPtr lparam);
                [DllImport("user32.dll")]
                public static extern bool IsWindow(IntPtr hwnd);
'@
        }
        $script:sshHost = $env:ITE2E_TMUX_SSH_HOST
        if ($script:sshHost -notmatch '^[A-Za-z0-9][A-Za-z0-9_.-]*$') {
            throw 'ITE2E_TMUX_SSH_HOST must be an SSH config alias or hostname without whitespace or options.'
        }
        $script:sshPath = (Get-Command ssh.exe -ErrorAction Stop).Source
        $script:app = Resolve-ItApp -Package (Get-ItTestPackage)
        $hash = (Get-FileHash -LiteralPath (Join-Path $script:app.InstallLocation 'TerminalApp.dll') -Algorithm SHA256).Hash
        if ($env:ITE2E_EXPECTED_TERMINALAPP_SHA256 -and $hash -ine $env:ITE2E_EXPECTED_TERMINALAPP_SHA256) {
            throw "The deployed TerminalApp.dll does not match the requested build: $hash"
        }
        Write-Host "Testing $($script:app.PackageFullName) at $($script:app.InstallLocation); TerminalApp.dll SHA256=$hash"
        Resolve-WtComClsid -App $script:app | Out-Null
        $script:socket = 'it-e2e-mouse-' + [guid]::NewGuid().ToString('N')
        $script:ownedWindows = [Collections.Generic.List[object]]::new()
        $script:createdSessions = [Collections.Generic.List[string]]::new()

        function Invoke-TmuxRemote {
            param([Parameter(Mandatory)][string]$Command)
            $result = Invoke-Native -FilePath $script:sshPath -TimeoutSec 15 -Arguments @(
                '-T', '-o', 'BatchMode=yes', '-o', 'ConnectTimeout=5',
                $script:sshHost, "tmux -L $script:socket $Command"
            )
            if ($result.ExitCode -ne 0) {
                throw "SSH tmux command failed ($($result.ExitCode)): $($result.StdErr.Trim())"
            }
            $result.StdOut.Trim()
        }

        function Open-TmuxTestWindow {
            param([Parameter(Mandatory)][string]$Session)
            $result = Invoke-WtCli -App $script:app -Arguments @(
                'tmux', "ssh.exe -T -o BatchMode=yes $script:sshHost tmux -L $script:socket -C attach-session -t $Session"
            )
            $result.state | Should -Be 'starting'
            $window = Wait-Until -TimeoutSec 30 -IntervalSec 0.5 -Quiet -Because 'test tmux HWND' -Condition {
                Get-WtWindowHwnds -App $script:app |
                    Where-Object title -eq "$script:socket/$Session" |
                    Select-Object -First 1
            }
            $context = $script:app.PSObject.Copy()
            $context.Hwnd = $window.hwnd
            $context.Pid = $window.pid
            $script:ownedWindows.Add($context)
            $context
        }

        function Close-TmuxTestWindow {
            param([Parameter(Mandatory)]$App)
            if ([ItE2E.TmuxTestWindow]::IsWindow([IntPtr]$App.Hwnd)) {
                if (-not [ItE2E.TmuxTestWindow]::PostMessage([IntPtr]$App.Hwnd, 0x10, [IntPtr]::Zero, [IntPtr]::Zero)) {
                    throw 'Could not close the owned tmux test window.'
                }
                Wait-Until -TimeoutSec 10 -IntervalSec 0.2 -Quiet -Because 'owned tmux window closes' -Condition {
                    -not [ItE2E.TmuxTestWindow]::IsWindow([IntPtr]$App.Hwnd)
                } | Out-Null
            }
            [void]$script:ownedWindows.Remove($App)
        }

        function Get-TmuxSelectedNativeTab {
            param([Parameter(Mandatory)]$App)
            $root = [System.Windows.Automation.AutomationElement]::FromHandle([IntPtr]$App.Hwnd)
            $tabs = $root.FindAll(
                [System.Windows.Automation.TreeScope]::Descendants,
                [System.Windows.Automation.PropertyCondition]::new(
                    [System.Windows.Automation.AutomationElement]::ControlTypeProperty,
                    [System.Windows.Automation.ControlType]::TabItem
                )
            )
            foreach ($tab in $tabs) {
                if ($tab.GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern).Current.IsSelected) {
                    $tab.Current.Name
                }
            }
        }

        function Assert-TmuxStableSelection {
            param([Parameter(Mandatory)]$App, [Parameter(Mandatory)][string]$Name)
            Wait-Until -TimeoutSec 10 -IntervalSec 0.2 -Quiet -Because "native tab selects $Name" -Condition {
                (Get-TmuxSelectedNativeTab -App $App) -ceq $Name
            } | Out-Null
            foreach ($observation in 1..4) {
                Start-Sleep -Milliseconds 250
                (Invoke-TmuxRemote "list-windows -t regression -F '#{window_name}:#{window_active}'") -split '\r?\n' |
                    Should -Contain "${Name}:1" -Because 'a selected backend window must not be reverted by a stale inventory'
                Get-TmuxSelectedNativeTab -App $App | Should -BeExactly $Name
            }
        }
    }

    AfterAll {
        if ($script:ownedWindows) {
            foreach ($window in @($script:ownedWindows.ToArray())) {
                Close-TmuxTestWindow -App $window
            }
        }
        if ($script:createdSessions) {
            foreach ($session in $script:createdSessions) {
                Invoke-TmuxRemote "kill-session -t $session" | Out-Null
            }
        }
    }

    It 'Tmux reconnect preserves mouse tab selection' {
        Invoke-TmuxRemote 'new-session -d -s regression -n one' | Out-Null
        $script:createdSessions.Add('regression')
        Invoke-TmuxRemote 'set-option -t regression automatic-rename off' | Out-Null
        Invoke-TmuxRemote 'new-window -t regression -n two' | Out-Null
        Invoke-TmuxRemote 'split-window -h -t regression:two' | Out-Null
        Invoke-TmuxRemote 'new-window -d -t regression -n three' | Out-Null
        Invoke-TmuxRemote 'new-session -d -s keepalive -n sentinel' | Out-Null
        $script:createdSessions.Add('keepalive')
        Invoke-TmuxRemote 'set-option -t keepalive automatic-rename off' | Out-Null
        $keepalive = Open-TmuxTestWindow -Session keepalive

        foreach ($round in 1..3) {
            Write-Host "Checking tmux attach round $round"
            Invoke-TmuxRemote 'select-window -t regression:two' | Out-Null
            $window = Open-TmuxTestWindow -Session regression
            $window.Pid | Should -Be $keepalive.Pid -Because 'reconnect must exercise another island in the same process'
            Wait-UiElement -App $window -Selector 'two' -TimeoutSec 15 | Out-Null
            Assert-TmuxStableSelection -App $window -Name two

            Invoke-TmuxRemote 'select-window -t regression:three' | Out-Null
            Assert-TmuxStableSelection -App $window -Name three
            Set-WtWindowForeground -App $window | Should -BeTrue -Because 'physical mouse input needs the target foreground window'
            foreach ($name in @('one', 'two', 'three')) {
                Get-TmuxSelectedNativeTab -App $window | Should -Not -BeExactly $name
                Invoke-UiClick -App $window -Selector $name | Out-Null
                Assert-TmuxStableSelection -App $window -Name $name
                Invoke-TmuxRemote "list-windows -t keepalive -F '#{window_name}:#{window_active}'" |
                    Should -BeExactly 'sentinel:1'
            }

            Close-TmuxTestWindow -App $window
            Invoke-TmuxRemote 'has-session -t regression' | Out-Null
            Invoke-TmuxRemote 'list-clients -t regression' | Should -BeNullOrEmpty
            [ItE2E.TmuxTestWindow]::IsWindow([IntPtr]$keepalive.Hwnd) | Should -BeTrue
        }
    }
}
