#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }

Describe 'Feature: Sidebar session status updates' -Tag @('Feature', 'SidebarSessionScroll') {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        Add-Type -AssemblyName UIAutomationClient
        Add-Type -AssemblyName UIAutomationTypes
        $script:app = $null
        $script:target = Resolve-ItApp -Package (Get-ItTestPackage)
        if ((Get-ItTestPackage) -ne 'Dev') { throw 'This regression suite requires explicitly selected Dev.' }
        if (@(Get-WtProcessesForApp -App $script:target -IncludePackageExecutables).Count) {
            throw 'Close the selected Dev package before running the Sidebar scroll regression.'
        }
        if (-not $env:ITE2E_EXPECTED_APP_SHA256 -or -not $env:ITE2E_EXPECTED_WTA_SHA256) {
            throw 'Supply TerminalApp and WTA hashes from the intended build receipt.'
        }
        (Get-FileHash -LiteralPath (Join-Path $script:target.InstallLocation 'TerminalApp.dll')).Hash |
            Should -Be $env:ITE2E_EXPECTED_APP_SHA256
        (Get-FileHash -LiteralPath $script:target.WtaPath).Hash | Should -Be $env:ITE2E_EXPECTED_WTA_SHA256
        (Get-WtSetting -App $script:target -Key tabLayout) | Should -Be 'vertical' -Because 'this configuration-free suite requires Sidebar mode'
        $script:settingsHash = (Get-FileHash -LiteralPath $script:target.SettingsPath).Hash
        $root = if ($env:ITE2E_ARTIFACT_ROOT) { $env:ITE2E_ARTIFACT_ROOT } else { Join-Path $PSScriptRoot '..\artifacts' }
        $script:marker = 'sidebar-scroll-' + [guid]::NewGuid().ToString('N').Substring(0, 8)
        $script:evidence = Join-Path ([IO.Path]::GetFullPath($root)) $script:marker
        New-Item -ItemType Directory -Path $script:evidence -Force | Out-Null

        if (-not ('ItE2E.SidebarScrollActivation' -as [type])) {
            Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
namespace ItE2E
{
    public static class SidebarScrollActivation
    {
        [ComImport, Guid("2E941141-7F97-4756-BA1D-9DECDE894A3D"),
         InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
        private interface IActivationManager
        {
            [PreserveSig]
            int ActivateApplication([MarshalAs(UnmanagedType.LPWStr)] string appId,
                [MarshalAs(UnmanagedType.LPWStr)] string arguments, uint options, out uint processId);
        }
        [ComImport, Guid("45BA127D-10A8-46EA-8AB7-56EA9078943C")]
        private class ActivationManager { }
        public static void Launch(string appId, string arguments)
        {
            var manager = (IActivationManager)new ActivationManager();
            try
            {
                uint processId;
                Marshal.ThrowExceptionForHR(manager.ActivateApplication(appId, arguments, 0, out processId));
            }
            finally { Marshal.ReleaseComObject(manager); }
        }
    }
}
'@
        }
        $title = "ItE2E $script:marker"
        $arguments = '-w new new-tab --title "' + $title + '" --startingDirectory "' +
            $script:evidence + '" pwsh -NoLogo -NoProfile -NoExit'
        if (@(Get-WtProcessesForApp -App $script:target -IncludePackageExecutables).Count) {
            throw 'Dev was opened during preparation; refusing to adopt it.'
        }
        [ItE2E.SidebarScrollActivation]::Launch($script:target.AppUserModelId, $arguments)
        $window = Wait-Until -TimeoutSec 40 -Because 'the explicitly activated owned Dev window appears' -Condition {
            Get-WtWindowHwnds -App $script:target | Where-Object {
                $_.title -eq $title -and (Get-Process -Id $_.pid).Path -eq $script:target.WindowsTerminal
            } | Select-Object -First 1
        }
        $script:app = $script:target.PSObject.Copy()
        $script:app.Hwnd = $window.hwnd
        $script:app.Pid = $window.pid
        $script:app | Add-Member -NotePropertyName Launched -NotePropertyValue $true
        Resolve-WtComClsid -App $script:app | Out-Null
        $active = Get-ActivePane -App $script:app
        $script:pane = $active.session_id
        $script:app | Add-Member -NotePropertyName WindowId -NotePropertyValue $active.window_id
        $script:pipe = (Get-Content -LiteralPath (Join-Path $script:app.LocalStateDir 'IntelligentTerminal\master-pipe.txt') -Raw).Trim()
        $script:liveId = "$script:marker-live"
        $script:liveTitle = "$script:marker-live"
        $script:fixture = (Resolve-Path (Join-Path $PSScriptRoot '..\fixtures\Emit-SidebarSessionHooks.ps1')).Path

        function Get-SidebarElement {
            param([string]$Id)
            $window = [Windows.Automation.AutomationElement]::FromHandle([IntPtr]([long]$script:app.Hwnd))
            $condition = [Windows.Automation.PropertyCondition]::new(
                [Windows.Automation.AutomationElement]::AutomationIdProperty, $Id)
            $window.FindFirst([Windows.Automation.TreeScope]::Descendants, $condition)
        }
        function Set-SessionQuery {
            param([AllowEmptyString()][string]$Text)
            (Get-SidebarElement HistorySearchTextBox).GetCurrentPattern(
                [Windows.Automation.ValuePattern]::Pattern).SetValue($Text)
        }
        function Get-SessionRows {
            $list = Get-SidebarElement HistoryList
            if (-not $list -or $list.Current.IsOffscreen) { return }
            $condition = [Windows.Automation.PropertyCondition]::new(
                [Windows.Automation.AutomationElement]::ControlTypeProperty, [Windows.Automation.ControlType]::ListItem)
            $textCondition = [Windows.Automation.PropertyCondition]::new(
                [Windows.Automation.AutomationElement]::ControlTypeProperty, [Windows.Automation.ControlType]::Text)
            foreach ($row in $list.FindAll([Windows.Automation.TreeScope]::Children, $condition)) {
                $texts = @($row.FindAll([Windows.Automation.TreeScope]::Descendants, $textCondition) |
                    ForEach-Object { $_.Current.Name })
                [pscustomobject]@{
                    Title = $texts[0]; Status = $texts[-1]; Offscreen = $row.Current.IsOffscreen
                    Top = $row.Current.BoundingRectangle.Top
                }
            }
        }
        function Read-Sessions {
            $result = Invoke-Wta -App $script:app -Arguments @(
                'sessions', 'list', '--master', $script:pipe, '--origin', 'shell', '--json', '--include-status') -Raw
            if ($result.ExitCode -ne 0) { throw $result.StdErr }
            @(($result.StdOut | ConvertFrom-Json -Depth 32).sessions)
        }
        function New-Hook {
            param([string]$Event, [string]$Id = $script:liveId, [hashtable]$Extra = @{})
            $payload = @{ session_id = $Id; cwd = Join-Path $script:evidence $Id }
            foreach ($key in $Extra.Keys) { $payload[$key] = $Extra[$key] }
            @{ event = $Event; payload = $payload }
        }
        function Send-Hooks {
            param([object[]]$Events, [string]$Pane = $script:pane)
            $token = [guid]::NewGuid().ToString('N')
            $inputPath = Join-Path $script:evidence "$token.json"
            $receiptPath = Join-Path $script:evidence "$token.receipt.json"
            ConvertTo-Json -InputObject $Events -Depth 8 | Set-Content -LiteralPath $inputPath
            $quote = { param([string]$Text) "'" + $Text.Replace("'", "''") + "'" }
            $command = "& $(& $quote $script:fixture) -InputPath $(& $quote $inputPath)" +
                " -ReceiptPath $(& $quote $receiptPath) -WtcliPath $(& $quote $script:app.WtcliPath)"
            Send-WtInput -App $script:app -SessionId $Pane -Text $command
            Send-WtKeys -App $script:app -SessionId $Pane -Keys Enter
            Wait-Until -TimeoutSec 45 -Because 'the shell hook fixture completes' -Condition {
                Test-Path -LiteralPath $receiptPath
            } | Out-Null
            $receipt = Get-Content -LiteralPath $receiptPath -Raw | ConvertFrom-Json
            $receipt.events | Should -Be $Events.Count
            $receipt.pane_session_id.Trim('{}').ToLowerInvariant() | Should -Be $Pane.Trim('{}').ToLowerInvariant()
        }
        function Wait-LiveStatus {
            param([string]$Status)
            Wait-Until -TimeoutSec 15 -IntervalSec 0.25 -Because "master records live fixture $Status" -Condition {
                @(Read-Sessions | Where-Object { $_.session_id -eq $script:liveId -and $_.status -eq $Status }).Count -eq 1
            } | Out-Null
        }
        $seed = @(
            foreach ($index in 1..48) {
                $id = '{0}-history-{1:d2}' -f $script:marker, $index
                New-Hook -Event agent.session.start -Id $id
                New-Hook -Event agent.session.end -Id $id -Extra @{ reason = 'user_exit' }
            }
            New-Hook -Event agent.session.start
        )
        Send-Hooks -Events $seed
        Wait-Until -TimeoutSec 20 -Because 'all deterministic fixtures cross COM into the real registry' -Condition {
            @(Read-Sessions | Where-Object session_id -Like "$script:marker*").Count -eq 49
        } | Out-Null
        Invoke-UiElement -App $script:app -Selector TabHistoryButton | Out-Null
        Wait-UiElement -App $script:app -Selector HistoryLoadingIndicator -Gone -TimeoutSec 60 | Out-Null
        Set-SessionQuery $script:marker
        Wait-Until -TimeoutSec 15 -Because 'the real Sidebar renders the live fixture' -Condition {
            @(Get-SessionRows | Where-Object Title -eq $script:liveTitle).Count -eq 1
        } | Out-Null
        @{
            package = $script:app.Package; version = $script:app.Version; source_commit = $env:ITE2E_SOURCE_COMMIT
            app_sha256 = $env:ITE2E_EXPECTED_APP_SHA256; wta_sha256 = $env:ITE2E_EXPECTED_WTA_SHA256
            fixture_id = $script:liveId; pane_id = $script:pane
        } | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $script:evidence 'package.json')
    }

    AfterAll {
        if ($script:app) {
            try {
                Save-UiScreenshot -App $script:app -Path (Join-Path $script:evidence 'final.png') | Out-Null
            }
            finally { Stop-Terminal -App $script:app -RestoreSettings $false }
        }
        if ($script:settingsHash) {
            (Get-FileHash -LiteralPath $script:target.SettingsPath).Hash |
                Should -Be $script:settingsHash -Because 'the suite must not edit user settings'
        }
    }

    It 'Sidebar Agents status updates preserve scroll' {
        foreach ($sample in 1..3) {
            Send-Hooks -Events @((New-Hook -Event agent.stop))
            Wait-LiveStatus Idle
            Set-SessionQuery $script:marker
            (Get-SidebarElement HistorySearchTextBox).SetFocus()
            $scroll = (Get-SidebarElement HistoryList).GetCurrentPattern([Windows.Automation.ScrollPattern]::Pattern)
            $scroll.SetScrollPercent(-1, 65)
            Start-Sleep -Seconds 1
            $before = @(Get-SessionRows | Where-Object { -not $_.Offscreen })[0]
            $scroll.Current.VerticalScrollPercent | Should -BeGreaterThan 50
            $beforeOrder = @(Read-Sessions | Where-Object session_id -Like "$script:marker*" | ForEach-Object session_id)
            Start-Sleep -Seconds 2
            @(Get-SessionRows | Where-Object { -not $_.Offscreen })[0].Title |
                Should -Be $before.Title -Because 'an idle control must keep the viewport stable'
            Save-UiScreenshot -App $script:app -Path (Join-Path $script:evidence "before-$sample.png") | Out-Null

            Send-Hooks -Events @((New-Hook -Event agent.tool.starting -Extra @{ tool_name = 'edit' }))
            Wait-LiveStatus Working
            Start-Sleep -Seconds 2
            $afterOrder = @(Read-Sessions | Where-Object session_id -Like "$script:marker*" | ForEach-Object session_id)
            ($afterOrder -join '|') | Should -Be ($beforeOrder -join '|') -Because 'the already-first live row must not change fixture ordering'
            $after = @(Get-SessionRows | Where-Object { -not $_.Offscreen })[0]
            @{ sample = $sample; before = $before; after = $after; order = $afterOrder } |
                ConvertTo-Json -Depth 6 | Set-Content -LiteralPath (Join-Path $script:evidence "viewport-$sample.json")
            Save-UiScreenshot -App $script:app -Path (Join-Path $script:evidence "after-$sample.png") | Out-Null
            $after.Title | Should -Be $before.Title
            [Math]::Abs($after.Top - $before.Top) | Should -BeLessOrEqual 2 -Because 'a status-only update must not scroll a stable history ordering'
        }
        Set-SessionQuery $script:liveTitle
        Wait-Until -TimeoutSec 10 -Because 'the final live status is actually rendered' -Condition {
            @(Get-SessionRows | Where-Object { $_.Title -eq $script:liveTitle -and $_.Status -eq 'Active' }).Count -eq 1
        } | Out-Null
    }

    It 'Status filtering still reacts to live updates' {
        Send-Hooks -Events @((New-Hook -Event agent.tool.starting -Extra @{ tool_name = 'ask_user' }))
        Wait-LiveStatus Attention
        Set-SessionQuery attention
        Wait-Until -TimeoutSec 10 -Because 'the attention query includes the actual waiting row' -Condition {
            @(Get-SessionRows | Where-Object { $_.Title -eq $script:liveTitle -and $_.Status -eq 'Waiting for input' }).Count -eq 1
        } | Out-Null
        Send-Hooks -Events @((New-Hook -Event agent.stop))
        Wait-LiveStatus Idle
        Wait-Until -TimeoutSec 10 -Because 'the now-idle row leaves the attention filter' -Condition {
            @(Get-SessionRows | Where-Object Title -eq $script:liveTitle).Count -eq 0
        } | Out-Null
        Set-SessionQuery $script:liveTitle
        Wait-Until -TimeoutSec 10 -Because 'clearing the status filter recovers the live row' -Condition {
            @(Get-SessionRows | Where-Object { $_.Title -eq $script:liveTitle -and $_.Status -eq 'Idle' }).Count -eq 1
        } | Out-Null
    }

    It 'Activity-time sorting still moves the newer live session first' {
        $beta = New-WtTab -App $script:app -Title "$script:marker-beta" -Command 'pwsh -NoLogo -NoProfile -NoExit'
        $betaId = "$script:marker-live-beta"
        Send-Hooks -Pane $beta.session_id -Events @((New-Hook -Event agent.session.start -Id $betaId))
        $search = Get-SidebarElement HistorySearchTextBox
        if (-not $search -or $search.Current.IsOffscreen) {
            Invoke-UiElement -App $script:app -Selector TabHistoryButton | Out-Null
        }
        Wait-UiElement -App $script:app -Selector HistoryLoadingIndicator -Gone -TimeoutSec 30 | Out-Null
        Set-SessionQuery "$script:marker-live"
        Wait-Until -TimeoutSec 15 -Because 'the newer beta session appears before the older live fixture' -Condition {
            $rows = @(Get-SessionRows)
            $rows.Count -eq 2 -and $rows[0].Title -eq $betaId -and $rows[1].Title -eq $script:liveTitle
        } | Out-Null
        Send-Hooks -Events @((New-Hook -Event agent.tool.starting -Extra @{ tool_name = 'edit' }))
        Wait-LiveStatus Working
        Wait-Until -TimeoutSec 15 -Because 'actual activity-time reordering still reaches the rendered list' -Condition {
            $rows = @(Get-SessionRows)
            $rows.Count -eq 2 -and $rows[0].Title -eq $script:liveTitle -and $rows[1].Title -eq $betaId
        } | Out-Null
        $ordered = @(Read-Sessions | Where-Object { $_.session_id -in @($script:liveId, $betaId) })
        $ordered[0].session_id | Should -Be $script:liveId
        $ordered[0].last_activity_at_ms | Should -BeGreaterThan $ordered[1].last_activity_at_ms
    }
}
