# Dedicated native Agent Console launch; never creates an ordinary shell tab.
# Dot-source for recording: $demo = @{}; Start-NativeWorkStoryDemo -Context $demo
# -Package Dev -StateDirectory <isolated-root> -ArtifactDirectory <evidence-root>
# -ExpectedWtaSha256 <feature-build-hash>. Close only with Stop-NativeWorkStoryDemo.

function Get-NativeWorkStoryPackageName {
    param([Parameter(Mandatory)][int]$ProcessId)
    if (-not ('ItE2E.NativeWorkStoryIdentity' -as [type])) {
        Add-Type @'
using System;
using System.Text;
using System.Diagnostics;
using System.ComponentModel;
using System.Runtime.InteropServices;
namespace ItE2E {
    public static class NativeWorkStoryIdentity {
        [DllImport("kernel32.dll", CharSet=CharSet.Unicode)]
        static extern int GetPackageFullName(IntPtr process, ref uint length, StringBuilder name);
        public static string Read(int pid) {
            using (var process = Process.GetProcessById(pid)) {
                uint length = 0;
                int result = GetPackageFullName(process.Handle, ref length, null);
                if (result == 15700) return "";
                if (result != 122) throw new Win32Exception(result);
                var name = new StringBuilder((int)length);
                result = GetPackageFullName(process.Handle, ref length, name);
                if (result != 0) throw new Win32Exception(result);
                return name.ToString();
            }
        }
    }
}
'@
    }
    [ItE2E.NativeWorkStoryIdentity]::Read($ProcessId)
}

function Get-NativeWorkStoryDocument {
    param([Parameter(Mandatory)][hashtable]$Context)
    Add-Type -AssemblyName UIAutomationClient
    Add-Type -AssemblyName UIAutomationTypes
    $root = [System.Windows.Automation.AutomationElement]::FromHandle([IntPtr][int64]$Context.App.Hwnd)
    $condition = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::IsTextPatternAvailableProperty, $true)
    $documents = @(foreach ($element in $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, $condition)) {
        if ($element.Current.IsOffscreen -or $element.Current.BoundingRectangle.Height -le 0 -or
            $element.Current.BoundingRectangle.Width -le 0) { continue }
        $element
    })
    if ($documents.Count -ne 1) { throw 'Expected exactly one visible native Console text document in the owned zero-tab window.' }
    $documents[0]
}

function Get-NativeWorkStoryHosts {
    param([Parameter(Mandatory)]$App)
    Get-WtProcessesForApp -App $App | Where-Object {
        try {
            $_.Path -eq $App.WindowsTerminal -and
                (Get-NativeWorkStoryPackageName -ProcessId $_.Id) -ceq $App.PackageFullName
        }
        catch { $false }
    }
}

function Get-NativeWorkStoryText {
    param([Parameter(Mandatory)][hashtable]$Context)
    $document = Get-NativeWorkStoryDocument -Context $Context
    $pattern = $document.GetCurrentPattern([System.Windows.Automation.TextPattern]::Pattern)
    $pattern.DocumentRange.GetText(-1)
}

function Get-NativeWorkStoryDraftText {
    param([Parameter(Mandatory)][AllowEmptyString()][string]$Frame)
    $lines = $Frame -split '\r?\n'
    $closing = @(for ($i = [Math]::Max(0, $lines.Count - 12); $i -lt $lines.Count; $i++) {
        if ($lines[$i] -cmatch '^[ ]*└─+┘[ ]*$') { $i }
    })
    if (-not $closing.Count) { throw 'Expected a composer closing border near the bottom of the native view.' }
    $bottom = $closing[-1]
    $rows = [Collections.Generic.List[string]]::new()
    for ($i = $bottom - 1; $i -ge [Math]::Max(0, $bottom - 7); $i--) {
        if ($lines[$i] -cmatch '^[ ]*┌─+┐[ ]*$') {
            if (-not $rows.Count -or $lines[$i + 1] -cnotmatch '^[ ]*│ > ') {
                throw 'The shared composer has no first-row prompt marker.'
            }
            $text = $rows -join "`n"
            # The shared renderer displays this English hint only for an empty draft.
            if ($text -ceq 'Ask anything, / for commands..') { return '' }
            return $text
        }
        if ($lines[$i] -cnotmatch '^[ ]*│ (?:> |  )(.*)│[ ]*$') {
            throw 'The shared composer has a malformed draft row.'
        }
        $rows.Insert(0, $Matches[1].TrimEnd([char]' '))
    }
    throw 'The shared composer has no opening border within its six-row input viewport.'
}

function Set-NativeWorkStoryWindowSize {
    param(
        [Parameter(Mandatory)][hashtable]$Context,
        [ValidateRange(800, 3840)][int]$Width = 1360,
        [ValidateRange(600, 2160)][int]$Height = 900
    )
    if (-not $Context.OwnedWindowId -or $Context.Closed -or
        $Context.OwnedWindowId -in @($Context.BeforeWindows.window_id) -or
        [ItE2E.ItWtWin32Input]::GetWindowProcessId([IntPtr][int64]$Context.App.Hwnd) -ne $Context.App.Pid -or
        @(Get-WtTabs -App $Context.App -WindowId $Context.OwnedWindowId).Count) {
        throw 'Resizing requires a uniquely owned native demo window with zero shell tabs.'
    }
    if (-not ('ItE2E.NativeWorkStorySizing' -as [type])) {
        Add-Type @'
using System;
using System.ComponentModel;
using System.Runtime.InteropServices;
namespace ItE2E {
    public static class NativeWorkStorySizing {
        [StructLayout(LayoutKind.Sequential)]
        public struct Rect { public int Left, Top, Right, Bottom; }
        [StructLayout(LayoutKind.Sequential)]
        struct MonitorInfo { public int Size; public Rect Monitor, Work; public uint Flags; }
        [DllImport("user32.dll")] static extern IntPtr SetThreadDpiAwarenessContext(IntPtr context);
        [DllImport("user32.dll")] static extern IntPtr MonitorFromWindow(IntPtr hwnd, uint flags);
        [DllImport("user32.dll", SetLastError=true)]
        static extern bool GetMonitorInfo(IntPtr monitor, ref MonitorInfo info);
        [DllImport("user32.dll")] static extern bool ShowWindow(IntPtr hwnd, int command);
        [DllImport("user32.dll", SetLastError=true)]
        static extern bool SetWindowPos(IntPtr hwnd, IntPtr after, int x, int y, int width, int height, uint flags);
        [DllImport("user32.dll", SetLastError=true)]
        static extern bool GetWindowRect(IntPtr hwnd, out Rect rect);
        public static Rect Read(IntPtr hwnd) {
            var previous = SetThreadDpiAwarenessContext(new IntPtr(-4));
            if (previous == IntPtr.Zero) throw new InvalidOperationException("Cannot set per-monitor DPI awareness.");
            try {
                Rect rect;
                if (!GetWindowRect(hwnd, out rect)) throw new Win32Exception(Marshal.GetLastWin32Error());
                return rect;
            } finally { SetThreadDpiAwarenessContext(previous); }
        }
        public static Rect Resize(IntPtr hwnd, int width, int height) {
            var previous = SetThreadDpiAwarenessContext(new IntPtr(-4));
            if (previous == IntPtr.Zero) throw new InvalidOperationException("Cannot set per-monitor DPI awareness.");
            try {
                var info = new MonitorInfo { Size = Marshal.SizeOf(typeof(MonitorInfo)) };
                if (!GetMonitorInfo(MonitorFromWindow(hwnd, 2), ref info))
                    throw new Win32Exception(Marshal.GetLastWin32Error());
                width = Math.Min(width, info.Work.Right - info.Work.Left - 32);
                height = Math.Min(height, info.Work.Bottom - info.Work.Top - 32);
                if (width < 800 || height < 600) throw new InvalidOperationException("Monitor work area is too small for the demo.");
                var x = info.Work.Left + (info.Work.Right - info.Work.Left - width) / 2;
                var y = info.Work.Top + (info.Work.Bottom - info.Work.Top - height) / 2;
                ShowWindow(hwnd, 9);
                if (!SetWindowPos(hwnd, IntPtr.Zero, x, y, width, height, 0x14))
                    throw new Win32Exception(Marshal.GetLastWin32Error());
                return new Rect { Left = x, Top = y, Right = x + width, Bottom = y + height };
            } finally { SetThreadDpiAwarenessContext(previous); }
        }
    }
}
'@
    }
    $expected = [ItE2E.NativeWorkStorySizing]::Resize([IntPtr][int64]$Context.App.Hwnd, $Width, $Height)
    Wait-Until -TimeoutSec 10 -Because 'the owned demo uses a compact, non-maximized window' -Condition {
        $actual = [ItE2E.NativeWorkStorySizing]::Read([IntPtr][int64]$Context.App.Hwnd)
        ($actual.Right - $actual.Left) -eq ($expected.Right - $expected.Left) -and
            ($actual.Bottom - $actual.Top) -eq ($expected.Bottom - $expected.Top)
    } | Out-Null
    [pscustomobject]@{ width = $expected.Right - $expected.Left; height = $expected.Bottom - $expected.Top; units = 'physical pixels' }
}

function Read-NativeWorkStoryState {
    param([Parameter(Mandatory)][hashtable]$Context)
    $result = Invoke-Native -FilePath $Context.App.WtaPath -Arguments @(
        '--language', 'en-US', 'demo', '--state-dir', $Context.StateDirectory, '--inspect'
    ) -TimeoutSec 15
    if ($result.ExitCode -ne 0) { throw "Native demo read-only inspection failed: $($result.StdErr)" }
    $result.StdOut | ConvertFrom-Json -Depth 64
}

function Get-NativeWorkStoryInputRoute {
    param([Parameter(Mandatory)][hashtable]$Context)
    if (-not $Context.InputRoute) {
        $Context.InputRoute = if ([ItE2E.ItWtWin32Input]::GetForegroundWindow() -eq [IntPtr]::Zero) {
            'ConsoleInput'
        } else { 'Keyboard' }
    }
    if ($Context.InputRoute -notin @('Keyboard', 'ConsoleInput')) { throw 'Unknown native demo input route.' }
    $Context.InputRoute
}

function Send-NativeWorkStoryKey {
    param(
        [Parameter(Mandatory)][hashtable]$Context,
        [Parameter(Mandatory)][int]$Vk,
        [switch]$Ctrl,
        [switch]$Shift
    )
    $route = Get-NativeWorkStoryInputRoute -Context $Context
    if ($route -eq 'ConsoleInput') {
        Send-NativeWorkStoryConsoleInput -Context $Context -Vk $Vk -Ctrl:$Ctrl -Shift:$Shift
    }
    else {
        if ($Context.Closed -or $Context.OwnedWindowId -in @($Context.BeforeWindows.window_id) -or
            [ItE2E.ItWtWin32Input]::GetWindowProcessId([IntPtr][int64]$Context.App.Hwnd) -ne $Context.App.Pid -or
            @(Get-WtTabs -App $Context.App -WindowId $Context.OwnedWindowId).Count -or
            -not (Get-NativeWorkStoryText -Context $Context).Contains('Work story demo', [StringComparison]::Ordinal)) {
            throw 'Owned native zero-tab demo identity changed; refusing physical input.'
        }
        if (-not (Get-NativeWorkStoryDocument -Context $Context).Current.HasKeyboardFocus) {
            Send-WtWindowKey -App $Context.App -Vk 0x47 -Ctrl -Shift -RequireForeground | Out-Null
        }
        Wait-Until -TimeoutSec 5 -Because 'the owned native Console document has keyboard focus' -Condition {
            (Get-NativeWorkStoryDocument -Context $Context).Current.HasKeyboardFocus
        } | Out-Null
        Send-WtWindowKey -App $Context.App -Vk $Vk -Ctrl:$Ctrl -Shift:$Shift -RequireForeground | Out-Null
    }
    @{ utc = [datetime]::UtcNow; hwnd = $Context.App.Hwnd; uiPid = $Context.UiPid
        route = $route; vk = $Vk; ctrl = [bool]$Ctrl; shift = [bool]$Shift } |
        ConvertTo-Json -Compress | Add-Content -LiteralPath (Join-Path $Context.ArtifactDirectory 'native-keys.jsonl') -Encoding utf8NoBOM
}

function Set-NativeWorkStoryDraft {
    param(
        [Parameter(Mandatory)][hashtable]$Context,
        [Parameter(Mandatory)][string]$Text,
        [switch]$Global
    )
    if ($Global) { Send-NativeWorkStoryKey -Context $Context -Vk 0x70 }
    Send-NativeWorkStoryKey -Context $Context -Vk 0x41 -Ctrl
    Send-NativeWorkStoryKey -Context $Context -Vk 0x08
    Wait-Until -TimeoutSec 10 -Because 'the shared native composer is empty before text entry' -Condition {
        (Get-NativeWorkStoryDraftText -Frame (Get-NativeWorkStoryText -Context $Context)) -ceq ''
    } | Out-Null
    $route = Get-NativeWorkStoryInputRoute -Context $Context
    $clipboard = $null
    try {
        if ($route -eq 'ConsoleInput') {
            Send-NativeWorkStoryConsoleInput -Context $Context -Text $Text
        }
        else {
            $clipboard = Get-ClipboardSnapshot
            Set-Clipboard -Value $Text
            if ((Get-Clipboard -Raw) -cne $Text) { throw 'The native clipboard did not retain the exact demo phrase.' }
            Send-NativeWorkStoryKey -Context $Context -Vk 0x56 -Ctrl -Shift
        }
        Wait-Until -TimeoutSec 10 -Because 'the exact phrase is in the shared composer, not history' -Condition {
            (Get-NativeWorkStoryDraftText -Frame (Get-NativeWorkStoryText -Context $Context)) -ceq $Text
        } | Out-Null
        @{ utc = [datetime]::UtcNow; text = $Text; route = $route; hwnd = $Context.App.Hwnd; submitted = $false } |
            ConvertTo-Json -Compress | Add-Content -LiteralPath (Join-Path $Context.ArtifactDirectory 'native-drafts.jsonl') -Encoding utf8NoBOM
    }
    finally { if ($null -ne $clipboard) { Restore-ClipboardSnapshot -Snapshot $clipboard } }
}

function Send-NativeWorkStoryPrompt {
    param(
        [Parameter(Mandatory)][hashtable]$Context,
        [Parameter(Mandatory)][string]$Text,
        [switch]$Global
    )
    Set-NativeWorkStoryDraft -Context $Context -Text $Text -Global:$Global
    @{ utc = [datetime]::UtcNow; text = $Text; route = Get-NativeWorkStoryInputRoute -Context $Context
        hwnd = $Context.App.Hwnd; uiPid = $Context.UiPid } |
        ConvertTo-Json -Compress | Add-Content -LiteralPath (Join-Path $Context.ArtifactDirectory 'native-inputs.jsonl') -Encoding utf8NoBOM
    Send-NativeWorkStoryKey -Context $Context -Vk 0x0D
}

function Wait-NativeWorkStoryCondition {
    param(
        [Parameter(Mandatory)][hashtable]$Context,
        [Parameter(Mandatory)][scriptblock]$StateCondition,
        [scriptblock]$FrameCondition = { param($state, $text) $true },
        [string[]]$Text = @(),
        [int]$TimeoutSec = 30,
        [string]$Because = 'native demo state and UI agree'
    )
    Wait-Until -TimeoutSec $TimeoutSec -IntervalSec 1 -Because $Because -Condition {
        $state = Read-NativeWorkStoryState -Context $Context
        $frame = Get-NativeWorkStoryText -Context $Context
        if ((& $StateCondition $state) -and (& $FrameCondition $state $frame) -and -not @($Text | Where-Object {
            -not $frame.Contains($_, [StringComparison]::Ordinal)
        }).Count) {
            return [pscustomobject]@{ State = $state; Text = $frame; CapturedUtc = [datetime]::UtcNow }
        }
        return $false
    }
}

function Save-NativeWorkStoryMarker {
    param(
        [Parameter(Mandatory)][hashtable]$Context,
        [Parameter(Mandatory)][ValidatePattern('^[a-zA-Z0-9][a-zA-Z0-9._-]*$')][string]$Name,
        [string]$ArtifactDirectory = $Context.ArtifactDirectory,
        [switch]$SkipScreenshot,
        $Observation
    )
    if (@(Get-WtTabs -App $Context.App -WindowId $Context.OwnedWindowId).Count) {
        throw 'Native walkthrough evidence cannot contain a shell-tab substitute.'
    }
    New-Item -ItemType Directory -Path $ArtifactDirectory -Force | Out-Null
    $state = if ($Observation) { $Observation.State } else { Read-NativeWorkStoryState -Context $Context }
    $text = if ($Observation) { $Observation.Text } else { Get-NativeWorkStoryText -Context $Context }
    $state | ConvertTo-Json -Depth 64 | Set-Content -LiteralPath (Join-Path $ArtifactDirectory "$Name-state.json") -Encoding utf8NoBOM
    $text | Set-Content -LiteralPath (Join-Path $ArtifactDirectory "$Name.txt") -Encoding utf8NoBOM
    if (-not $SkipScreenshot) {
        Save-UiScreenshot -App $Context.App -Path (Join-Path $ArtifactDirectory "$Name.png") -RequireSuccess | Out-Null
    }
    $marker = [pscustomobject]@{
        name = $Name; capturedUtc = [datetime]::UtcNow; scene = $state.scene; revision = $state.revision
        displayedScene = [regex]::Match((($text -split '\r?\n' | Select-Object -First 4) -join "`n"), 'Scene ([1-8])/8').Groups[1].Value
        hwnd = $Context.App.Hwnd; uiPid = $Context.UiPid; wtaSha256 = $Context.Sha256
        inputRoute = Get-NativeWorkStoryInputRoute -Context $Context
        statePath = Join-Path $ArtifactDirectory "$Name-state.json"
        textPath = Join-Path $ArtifactDirectory "$Name.txt"
        diagnosticImagePath = $(if ($SkipScreenshot) { $null } else { Join-Path $ArtifactDirectory "$Name.png" })
    }
    $marker | ConvertTo-Json -Compress | Add-Content -LiteralPath (Join-Path $ArtifactDirectory 'native-markers.jsonl') -Encoding utf8NoBOM
    $marker
}

function Wait-NativeWorkStoryPace {
    param([ValidateRange(0, 30)][double]$Seconds)
    if ($Seconds -eq 0) { return }
    $deadline = [datetime]::UtcNow.AddSeconds($Seconds)
    Wait-Until -TimeoutSec ([int][Math]::Ceiling($Seconds) + 2) -IntervalSec ([Math]::Min(1, $Seconds)) `
        -Because 'the requested recording dwell has elapsed' -Condition { [datetime]::UtcNow -ge $deadline } | Out-Null
}

function Get-NativeWorkStoryLegacyTabs {
    @(
        [pscustomobject]@{ Title = 'Fix bug'; Marker = 'the shell reports a failure' }
        [pscustomobject]@{ Title = 'Code review'; Marker = 'Ask that session' }
        [pscustomobject]@{ Title = 'Migration'; Marker = 'Keep the investigation separate' }
        [pscustomobject]@{ Title = 'Research'; Marker = 'No decision is recorded' }
    )
}

function Wait-NativeWorkStoryBudget {
    param(
        [Parameter(Mandatory)][hashtable]$Context,
        [string]$ArtifactDirectory = $Context.ArtifactDirectory,
        [int]$TimeoutSec = 40,
        $InitialObservation
    )
    $tracking = @{ Last = -1; Samples = [Collections.Generic.List[object]]::new(); Error = $null }
    if ($InitialObservation) {
        $used = [int]$InitialObservation.State.budget.consumed
        if (-not $InitialObservation.Text.Contains("Tokens: $used / 20000 |", [StringComparison]::Ordinal) -or
            -not $InitialObservation.Text.Contains('Investigate API v2', [StringComparison]::Ordinal)) {
            throw 'The initial budget observation does not pair persisted usage with its owning native banner.'
        }
        $marker = Save-NativeWorkStoryMarker -Context $Context -Name "natural-budget-$used" `
            -ArtifactDirectory $ArtifactDirectory -SkipScreenshot -Observation $InitialObservation
        $tracking.Samples.Add([pscustomobject]@{ Consumed = $used; CapturedUtc = $InitialObservation.CapturedUtc; Marker = $marker })
        $tracking.Last = $used
    }
    $final = Wait-Until -TimeoutSec $TimeoutSec -IntervalSec 1 -Because 'visible idle budget usage increases and pauses at 20000 tokens' -Condition {
        $state = Read-NativeWorkStoryState -Context $Context
        $text = Get-NativeWorkStoryText -Context $Context
        $used = [int]$state.budget.consumed
        $observation = [pscustomobject]@{ State = $state; Text = $text; CapturedUtc = [datetime]::UtcNow }
        $visible = $text.Contains("Tokens: $used / 20000 |", [StringComparison]::Ordinal) -and
            $text.Contains('Investigate API v2', [StringComparison]::Ordinal)
        if ($visible -and $used -ne $tracking.Last) {
            if ($used -lt $tracking.Last) {
                $tracking.Error = 'Simulated usage moved backwards during the idle budget observation.'
                return $observation
            }
            $marker = Save-NativeWorkStoryMarker -Context $Context -Name "natural-budget-$used" `
                -ArtifactDirectory $ArtifactDirectory -SkipScreenshot -Observation $observation
            $tracking.Samples.Add([pscustomobject]@{ Consumed = $used; CapturedUtc = $observation.CapturedUtc; Marker = $marker })
            $tracking.Last = $used
        }
        if ($visible -and $state.budget.paused -and $used -eq 20000 -and $text.Contains('Paused at cap')) {
            return $observation
        }
        return $false
    }
    if ($tracking.Error) { throw $tracking.Error }
    $intermediate = @($tracking.Samples | Where-Object { $_.Consumed -gt 0 -and $_.Consumed -lt 20000 })
    if ($intermediate.Count -lt 2) { throw 'Fewer than two increasing intermediate budget values were proven in both persisted state and native UI.' }
    [pscustomobject]@{ State = $final.State; Text = $final.Text; Samples = $tracking.Samples.ToArray() }
}

function Invoke-NativeWorkStoryWalkthrough {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)][hashtable]$Context,
        [ValidateRange(0, 15)][double]$PauseSeconds = 4,
        [scriptblock]$OnStep = {},
        [string]$ArtifactDirectory = (Join-Path $Context.ArtifactDirectory 'natural-walkthrough')
    )
    $initial = Read-NativeWorkStoryState -Context $Context
    if ($initial.scene -ne 1 -or $initial.resumed -or -not $initial.clock -or
        $initial.capConfigured -ne $false -or @($initial.works).Count -ne 1) {
        throw 'The natural walkthrough requires fresh legacy Scene 1 with the clock enabled and capConfigured=false; it never resets or upgrades an existing story.'
    }
    New-Item -ItemType Directory -Path $ArtifactDirectory -Force | Out-Null
    $markers = [Collections.Generic.List[object]]::new()
    function Enter-WalkthroughStep([int]$Scene, [string]$Title) {
        $step = [pscustomobject]@{ Scene = $Scene; Title = $Title; Utc = [datetime]::UtcNow }
        $step | ConvertTo-Json -Compress | Add-Content -LiteralPath (Join-Path $ArtifactDirectory 'natural-steps.jsonl') -Encoding utf8NoBOM
        & $OnStep $step | Out-Null
    }
    function Save-WalkthroughStep([string]$Name, $Observation) {
        $markers.Add((Save-NativeWorkStoryMarker -Context $Context -Name $Name -ArtifactDirectory $ArtifactDirectory `
            -SkipScreenshot -Observation $Observation))
    }
    try {
        Enter-WalkthroughStep 1 'The problem: work buried in four old agent sessions'
        $tabs = @(Get-NativeWorkStoryLegacyTabs)
        for ($i = 0; $i -lt $tabs.Count; $i++) {
            if ($i) { Send-NativeWorkStoryKey -Context $Context -Vk 0x27 }
            $tab = $tabs[$i]
            $seen = Wait-NativeWorkStoryCondition -Context $Context -StateCondition { param($s) $s.scene -eq 1 -and -not $s.resumed } `
                -Text @("[$($tab.Title)]", $tab.Marker, 'Work story demo | Simulated data')
            foreach ($other in $tabs | Where-Object Title -CNE $tab.Title) {
                if ($seen.Text.Contains($other.Marker, [StringComparison]::Ordinal)) { throw 'Legacy tab navigation retained another session history.' }
            }
            Save-WalkthroughStep "scene-01-legacy-$i" $seen
            Wait-NativeWorkStoryPace -Seconds $PauseSeconds
        }

        Send-NativeWorkStoryPrompt -Context $Context -Text 'Continue fixing issue #4821' -Global
        $seen = Wait-NativeWorkStoryCondition -Context $Context -StateCondition { param($s) $s.resumed -and $s.scene -eq 2 } `
            -Text @('Resumed Fix Issue #4821.', 'Completed: Reproduce issue', 'Blocked: Compatibility test', 'Pending: Documentation')
        Enter-WalkthroughStep 2 'Resume the Work, not an agent session'
        Save-WalkthroughStep 'scene-02-resume' $seen
        Wait-NativeWorkStoryPace -Seconds $PauseSeconds

        Send-NativeWorkStoryKey -Context $Context -Vk 0x74
        $seen = Wait-NativeWorkStoryCondition -Context $Context -StateCondition { param($s) $s.selectedWorkId -eq 'fix-4821' } `
            -Text @('Scene 3/8', 'Work ID: fix-4821', 'sample-fix-4821-compatibility', 'exitCode: 1')
        Enter-WalkthroughStep 3 'Inspect completed, blocked and pending evidence'
        Save-WalkthroughStep 'scene-03-evidence' $seen
        Wait-NativeWorkStoryPace -Seconds $PauseSeconds
        Send-NativeWorkStoryKey -Context $Context -Vk 0x74

        Send-NativeWorkStoryPrompt -Context $Context -Text 'Should we migrate to API v2?'
        $seen = Wait-NativeWorkStoryCondition -Context $Context -StateCondition { param($s) $s.branchConfirmationPending -and @($s.works).Count -eq 1 } `
            -Text @('Create Related Work: Investigate API v2?', 'The bug fix keeps its original goal.', 'F4: confirm or cancel.')
        Enter-WalkthroughStep 4 'Branch the Work with explicit confirmation'
        Save-WalkthroughStep 'scene-04-confirmation' $seen
        Wait-NativeWorkStoryPace -Seconds $PauseSeconds
        Send-NativeWorkStoryKey -Context $Context -Vk 0x73
        Wait-NativeWorkStoryCondition -Context $Context -StateCondition { param($s) $s.branchConfirmationPending } `
            -Text @('Create related work', 'Cancel related work') | Out-Null
        Send-NativeWorkStoryKey -Context $Context -Vk 0x0D
        $seen = Wait-NativeWorkStoryCondition -Context $Context -StateCondition { param($s) @($s.works).Count -eq 2 -and $null -ne $s.exchange } `
            -Text @('Related Work created: Investigate API v2', 'Schema 2.3 | 3 breaking changes')
        Save-WalkthroughStep 'scene-04-related-work' $seen

        Send-NativeWorkStoryKey -Context $Context -Vk 0x74
        $seen = Wait-NativeWorkStoryCondition -Context $Context -StateCondition {
            param($s) $s.exchange.response.version -eq '2.3' -and $s.exchange.response.breakingChanges -eq 3 -and -not $s.exchange.request.blocking
        } -Text @('Work ID: api-v2', 'Schema exchange | Request owner: api-v2 | Result owner: fix-4821',
            'Need: API v2 schema', 'Blocking: false', 'Schema 2.3 | Breaking changes: 3 | Source: scripted-fixture')
        Enter-WalkthroughStep 5 'Work exchanges structured schema without a human handoff'
        Save-WalkthroughStep 'scene-05-schema' $seen
        Wait-NativeWorkStoryPace -Seconds $PauseSeconds

        $seen = Wait-NativeWorkStoryCondition -Context $Context -TimeoutSec 25 -StateCondition {
            param($s) $s.clock.attentionRaised -and $null -eq $s.decision
        } -Text @('Work ID: api-v2', 'Fix Issue #4821 | Compatibility failed - human decision needed', 'F6: decide')
        Enter-WalkthroughStep 6 'The owning Work proactively requests a human decision'
        Save-WalkthroughStep 'scene-06-attention' $seen
        Wait-NativeWorkStoryPace -Seconds $PauseSeconds
        Send-NativeWorkStoryKey -Context $Context -Vk 0x75
        Wait-NativeWorkStoryCondition -Context $Context -StateCondition { param($s) $null -eq $s.decision } `
            -Text @('B. Fix compatibility (recommended) - Fix Issue #4821', 'A. Ignore', 'C. Roll back') | Out-Null
        Wait-NativeWorkStoryPace -Seconds $PauseSeconds
        Send-NativeWorkStoryKey -Context $Context -Vk 0x0D
        $seen = Wait-NativeWorkStoryCondition -Context $Context -StateCondition {
            param($s) $s.decision.resolved -and $s.decision.choice -eq 'B' -and $s.decision.workId -eq 'fix-4821'
        } -Text @('Decision B recorded.', 'Compatibility still failed', 'Set token cap to start')
        Save-WalkthroughStep 'scene-06-decision' $seen

        Send-NativeWorkStoryKey -Context $Context -Vk 0x75
        $capMenu = Wait-NativeWorkStoryCondition -Context $Context -StateCondition {
            param($s) $s.capConfigured -eq $false -and $s.budget.consumed -eq 0 -and $null -eq $s.clock.budgetDueMs
        } -Text @('Investigate API v2 | Token cap', 'Migration cap: 10K', '> Migration cap: 20K tokens (default)', 'Migration cap: 30K', 'Cancel')
        Enter-WalkthroughStep 7 'Set a 20K token cap, then let the Work pause automatically'
        Save-WalkthroughStep 'scene-07-set-token-cap' $capMenu
        Wait-NativeWorkStoryPace -Seconds $PauseSeconds
        Send-NativeWorkStoryKey -Context $Context -Vk 0x0D
        $firstBudget = Wait-NativeWorkStoryCondition -Context $Context -StateCondition {
            param($s) $s.capConfigured -eq $true -and $s.budget.limit -eq 20000 -and $s.budget.consumed -lt 20000 -and
                $null -ne $s.clock.budgetDueMs -and -not $s.budget.paused
        } -FrameCondition {
            param($s, $text) $text.Contains("Tokens: $($s.budget.consumed) / 20000 |", [StringComparison]::Ordinal)
        } -Text @('Scene 7/8', 'Investigate API v2', 'Token cap set: 20K')
        $budget = Wait-NativeWorkStoryBudget -Context $Context -ArtifactDirectory $ArtifactDirectory -InitialObservation $firstBudget
        if (@($budget.State.events | Where-Object { $_.kind -eq 'budgetConsumed' -and $null -eq $_.input }).Count -ne 3 -or
            @($budget.State.events | Where-Object { $_.kind -eq 'budgetReached' -and $null -eq $_.input }).Count -ne 1) {
            throw 'Budget progression was not recorded as three autonomous increments and one autonomous pause.'
        }
        Wait-NativeWorkStoryPace -Seconds $PauseSeconds

        Send-NativeWorkStoryKey -Context $Context -Vk 0x71
        $seen = Wait-NativeWorkStoryCondition -Context $Context -StateCondition { param($s) @($s.works).Count -eq 3 -and $s.budget.paused } `
            -Text @('Scene 8/8', 'Work Overview', 'Fix Issue #4821', 'Investigate API v2', 'Prepare Documentation')
        foreach ($field in @('Status', 'Progress', 'Blockers', 'Decisions', 'Deliverables', 'Acceptance')) {
            if ([regex]::Matches($seen.Text, [regex]::Escape("$($field):")).Count -ne 3) {
                throw "The final native view does not expose $field on all three actual Work cards."
            }
        }
        if (@($seen.State.events | Where-Object { $_.input -clike 'Show *' }).Count) {
            throw 'A manual scene-advancement input appeared in the natural walkthrough.'
        }
        Enter-WalkthroughStep 8 'Manage three Works, not agents or sessions'
        Save-WalkthroughStep 'scene-08-work-overview' $seen
        Wait-NativeWorkStoryPace -Seconds $PauseSeconds
        [pscustomobject]@{ Markers = $markers.ToArray(); BudgetSamples = $budget.Samples; FinalState = $seen.State; WindowLeftOpen = $true }
    }
    catch {
        $failure = $_
        try { Save-NativeWorkStoryMarker -Context $Context -Name 'natural-walkthrough-failure' -ArtifactDirectory $ArtifactDirectory -SkipScreenshot | Out-Null }
        catch { Write-Warning "Native walkthrough diagnostic capture failed: $($_.Exception.Message)" }
        throw $failure
    }
}

function Test-NativeWorkStoryFrame {
    param([Parameter(Mandatory)][string]$Frame)
    $Frame.Contains('Work story demo', [StringComparison]::Ordinal) -or (
        $Frame.Contains('Saved Work relationships', [StringComparison]::Ordinal) -and
        $Frame.Contains('Scripted prototype | Independent contexts and budgets | Simulated tokens, NOT provider quota',
            [StringComparison]::Ordinal)
    )
}

function Send-NativeWorkStoryConsoleInput {
    [CmdletBinding(DefaultParameterSetName = 'Key')]
    param(
        [Parameter(Mandatory)][hashtable]$Context,
        [Parameter(Mandatory, ParameterSetName = 'Key')][int]$Vk,
        [Parameter(ParameterSetName = 'Key')][switch]$Ctrl,
        [Parameter(ParameterSetName = 'Key')][switch]$Shift,
        [Parameter(Mandatory, ParameterSetName = 'Text')][string]$Text
    )
    if (-not $Context.UiPid -or $Context.UiPid -in $Context.BeforeUiProcessIds -or $Context.Closed -or
        $Context.OwnedWindowId -in @($Context.BeforeWindows.window_id)) {
        throw 'Console input requires a uniquely new, currently owned demo UI process and window.'
    }
    if ([ItE2E.ItWtWin32Input]::GetWindowProcessId([IntPtr][int64]$Context.App.Hwnd) -ne $Context.App.Pid -or
        (Get-Process -Id $Context.App.Pid -ErrorAction Stop).Path -ne $Context.App.WindowsTerminal -or
        @(Get-WtTabs -App $Context.App -WindowId $Context.OwnedWindowId).Count) {
        throw 'Owned native demo HWND/zero-tab identity changed; refusing ConsoleInput.'
    }
    if (-not (Test-NativeWorkStoryFrame -Frame (Get-NativeWorkStoryText -Context $Context))) {
        throw 'Owned native Console no longer exposes the isolated demo marker.'
    }
    $payload = @{
        pid = $Context.UiPid; path = $Context.App.WtaPath
        createdTicks = $Context.UiCreated.ToUniversalTime().Ticks
        text = $Text; isText = $PSCmdlet.ParameterSetName -eq 'Text'
        vk = $Vk; ctrl = [bool]$Ctrl; shift = [bool]$Shift
    } | ConvertTo-Json -Compress
    $encodedPayload = [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($payload))
    # Only this disposable child detaches/attaches a console. The test runner and
    # existing terminal processes retain their original console associations.
    $child = @'
$ErrorActionPreference = 'Stop'
$p = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('__PAYLOAD__')) | ConvertFrom-Json
$heldProcess = [Diagnostics.Process]::GetProcessById([int]$p.pid)
$null = $heldProcess.Handle
try {
$target = Get-CimInstance Win32_Process -Filter "ProcessId=$($p.pid)"
if (-not $target -or $target.ExecutablePath -ne $p.path -or
    $target.CommandLine -notmatch '(?:^|\s)ui\s*$' -or
    $target.CreationDate.ToUniversalTime().Ticks -ne $p.createdTicks) {
    throw 'New demo UI process identity changed before console attachment.'
}
Add-Type @"
using System;
using System.ComponentModel;
using System.Runtime.InteropServices;
public static class DemoConsoleInput {
    [StructLayout(LayoutKind.Explicit, Size=20)]
    public struct Record {
        [FieldOffset(0)] public ushort Type;
        [FieldOffset(4)] public int Down;
        [FieldOffset(8)] public ushort Repeat;
        [FieldOffset(10)] public ushort Vk;
        [FieldOffset(12)] public ushort Scan;
        [FieldOffset(14)] public ushort Character;
        [FieldOffset(16)] public uint Controls;
    }
    [DllImport("kernel32.dll", SetLastError=true)] public static extern bool FreeConsole();
    [DllImport("kernel32.dll", SetLastError=true)] public static extern bool AttachConsole(uint pid);
    [DllImport("kernel32.dll", CharSet=CharSet.Unicode, SetLastError=true)]
    public static extern IntPtr CreateFileW(string path, uint access, uint share, IntPtr security, uint disposition, uint flags, IntPtr template);
    [DllImport("kernel32.dll", SetLastError=true)]
    public static extern bool WriteConsoleInputW(IntPtr input, Record[] records, uint count, out uint written);
    [DllImport("kernel32.dll")] public static extern bool CloseHandle(IntPtr handle);
    public static void Key(IntPtr input, int vk, int character, uint controls) {
        var down = new Record { Type=1, Down=1, Repeat=1, Vk=(ushort)vk, Character=(ushort)character, Controls=controls };
        var up = down; up.Down=0;
        uint written;
        if (!WriteConsoleInputW(input, new[] { down, up }, 2, out written))
            throw new Win32Exception(Marshal.GetLastWin32Error());
        if (written != 2) throw new InvalidOperationException("ConsoleInput did not accept both key records.");
    }
}
"@
[DemoConsoleInput]::FreeConsole() | Out-Null
if (-not [DemoConsoleInput]::AttachConsole([uint32]$p.pid)) {
    throw [ComponentModel.Win32Exception]::new([Runtime.InteropServices.Marshal]::GetLastWin32Error())
}
$handle = [IntPtr]::Zero
try {
    $handle = [DemoConsoleInput]::CreateFileW('CONIN$', [uint32]3221225472, 3, [IntPtr]::Zero, 3, 0, [IntPtr]::Zero)
    if ($handle -eq [IntPtr](-1)) { throw [ComponentModel.Win32Exception]::new([Runtime.InteropServices.Marshal]::GetLastWin32Error()) }
    if ($p.isText) {
        foreach ($character in $p.text.ToCharArray()) { [DemoConsoleInput]::Key($handle, 0, [int]$character, 0) }
    } else {
        $controls = $(if ($p.ctrl) { 8 } else { 0 }) -bor $(if ($p.shift) { 16 } else { 0 })
        $character = if ($p.ctrl -and $p.vk -ge 0x41 -and $p.vk -le 0x5A) { $p.vk - 0x40 }
            elseif ($p.vk -in @(8, 9, 13, 27)) { $p.vk } else { 0 }
        [DemoConsoleInput]::Key($handle, $p.vk, $character, $controls)
    }
} finally {
    if ($handle -ne [IntPtr]::Zero -and $handle -ne [IntPtr](-1)) { [DemoConsoleInput]::CloseHandle($handle) | Out-Null }
    [DemoConsoleInput]::FreeConsole() | Out-Null
}
} finally { $heldProcess.Dispose() }
'@
    $child = $child.Replace('__PAYLOAD__', $encodedPayload)
    $encodedCommand = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($child))
    $result = Invoke-Native -FilePath (Get-Command pwsh -ErrorAction Stop).Source -Arguments @(
        '-NoLogo', '-NoProfile', '-NonInteractive', '-EncodedCommand', $encodedCommand
    ) -TimeoutSec 20
    if ($result.ExitCode -ne 0) { throw "Owned native ConsoleInput failed: $($result.StdErr)" }
}

function Stop-NativeWorkStoryDemo {
    param([Parameter(Mandatory)][hashtable]$Context)
    if (-not $Context.OwnedWindowId -or -not $Context.App.Hwnd) { return }
    if (-not @(Get-NativeWorkStoryHosts -App $Context.App).Count) {
        if ($Context.BeforeWindows.Count) { throw 'Preexisting Dev windows disappeared before native demo cleanup.' }
        $Context.Closed = $true
        return
    }
    $current = @(Get-WtWindows -App $Context.App | Where-Object { [string]$_.window_id -ceq $Context.OwnedWindowId })
    if (-not $current.Count) { return }
    if ($current.Count -ne 1 -or $Context.OwnedWindowId -in @($Context.BeforeWindows | ForEach-Object { [string]$_.window_id })) {
        throw 'Refusing to close a window without unique new-window ownership.'
    }
    if (@(Get-WtTabs -App $Context.App -WindowId $Context.OwnedWindowId).Count) {
        throw 'The owned demo window now contains user-added tabs; it is retained rather than closing their resources.'
    }
    if ([ItE2E.ItWtWin32Input]::GetWindowProcessId([IntPtr][int64]$Context.App.Hwnd) -ne $Context.App.Pid -or
        (Get-Process -Id $Context.App.Pid -ErrorAction Stop).Path -ne $Context.App.WindowsTerminal) {
        throw 'Owned HWND/process identity changed; refusing native close.'
    }
    if (-not ('ItE2E.NativeWorkStoryWindow' -as [type])) {
        Add-Type @'
using System;
using System.Runtime.InteropServices;
namespace ItE2E {
    public static class NativeWorkStoryWindow {
        [DllImport("user32.dll", SetLastError = true)]
        public static extern bool PostMessage(IntPtr hwnd, uint message, IntPtr wParam, IntPtr lParam);
    }
}
'@
    }
    if (-not [ItE2E.NativeWorkStoryWindow]::PostMessage([IntPtr][int64]$Context.App.Hwnd, 0x0010, [IntPtr]::Zero, [IntPtr]::Zero)) {
        throw 'Posting WM_CLOSE to the verified owned native window failed.'
    }
    Wait-Until -TimeoutSec 15 -Because 'only the owned native demo window closes' -Condition {
        if (-not @(Get-NativeWorkStoryHosts -App $Context.App).Count) { return $true }
        @((Get-WtWindows -App $Context.App).window_id | Where-Object { [string]$_ -ceq $Context.OwnedWindowId }).Count -eq 0
    } | Out-Null
    $remaining = if (@(Get-NativeWorkStoryHosts -App $Context.App).Count) { @(Get-WtWindows -App $Context.App) } else { @() }
    foreach ($window in $Context.BeforeWindows) {
        if ($window.window_id -notin @($remaining.window_id)) { throw "Preexisting window $($window.window_id) disappeared during demo cleanup." }
    }
    if ($Context.PreviousForeground -and $Context.PreviousForeground -ne [IntPtr]::Zero) {
        [ItE2E.ItWtWin32Input]::SetForegroundWindow($Context.PreviousForeground) | Out-Null
    }
    @{
        closedOwnedWindowId = $Context.OwnedWindowId
        preservedWindowIds = @($remaining.window_id)
        closedUtc = [datetime]::UtcNow
    } | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $Context.ArtifactDirectory 'native-cleanup.json') -Encoding utf8NoBOM
    $Context.Closed = $true
}

function Start-NativeWorkStoryDemo {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)][hashtable]$Context,
        [Parameter(Mandatory)][ValidateSet('Dev')][string]$Package,
        [Parameter(Mandatory)][string]$StateDirectory,
        [Parameter(Mandatory)][string]$ArtifactDirectory,
        [Parameter(Mandatory)][ValidatePattern('^[a-fA-F0-9]{64}$')][string]$ExpectedWtaSha256,
        [string]$ExpectedUiMarker = 'Work story demo',
        [ValidateRange(800, 3840)][int]$WindowWidth = 1360,
        [ValidateRange(600, 2160)][int]$WindowHeight = 900
    )
    if ($Context.OwnedWindowId -and -not $Context.Closed) { throw 'This context already owns a native demo window.' }
    Import-Module (Join-Path $PSScriptRoot '..\..\ItE2E\ItE2E.psd1') -Force
    $Context.App = Resolve-ItApp -Package $Package
    $Context.Sha256 = (Get-FileHash -LiteralPath $Context.App.WtaPath -Algorithm SHA256).Hash
    if ($Context.Sha256 -ne $ExpectedWtaSha256) { throw 'Packaged WTA does not match the independently pinned native feature build.' }
    $Context.StateDirectory = [IO.Path]::GetFullPath($StateDirectory)
    $Context.ArtifactDirectory = [IO.Path]::GetFullPath($ArtifactDirectory)
    New-Item -ItemType Directory -Path $Context.StateDirectory, $Context.ArtifactDirectory -Force | Out-Null
    & (Get-Module ItE2E) { Initialize-WtWin32Input }
    $Context.PreviousForeground = [ItE2E.ItWtWin32Input]::GetForegroundWindow()
    $hosts = @(Get-NativeWorkStoryHosts -App $Context.App)
    $Context.BeforeNativeWindows = @(Get-WtWindowHwnds -App $Context.App | Where-Object {
        $_.pid -in @($hosts.Id)
    })
    if ($hosts.Count -and -not $Context.BeforeNativeWindows.Count) {
        throw 'An existing packaged Dev host has no visible native window yet; refusing COM activation or a second launch during its startup/shutdown.'
    }
    $Context.BeforeWindows = @()
    if ($hosts.Count) { $Context.BeforeWindows = @(Get-WtWindows -App $Context.App) }
    $Context.BeforeUiProcessIds = @(Get-CimInstance Win32_Process -Filter "Name='wta.exe'" | Where-Object {
        $_.ExecutablePath -eq $Context.App.WtaPath -and $_.CommandLine -match '(?:^|\s)ui(?:\s|$)'
    } | ForEach-Object ProcessId)
    $packageAlias = Join-Path $env:LOCALAPPDATA "Microsoft\WindowsApps\$($Context.App.Package)\wtai.exe"
    if (-not (Test-Path -LiteralPath $packageAlias -PathType Leaf)) {
        throw "Selected Dev package execution alias is unavailable: $packageAlias. Direct-image and ambiguous global-alias fallbacks are not allowed."
    }
    $startInfo = [Diagnostics.ProcessStartInfo]::new()
    $startInfo.FileName = $packageAlias
    $startInfo.UseShellExecute = $false
    $startInfo.ArgumentList.Add('-w')
    $startInfo.ArgumentList.Add('new')
    $startInfo.Environment['INTELLIGENT_TERMINAL_AGENT_CENTER'] = '1'
    $startInfo.Environment['INTELLIGENT_TERMINAL_WORK_DEMO_STATE'] = $Context.StateDirectory
    $metadata = [ordered]@{
        boundary = 'Native Agent Console -> dedicated wta ui -> isolated demo provider; zero shell tabs'
        package = $Context.App.PackageFullName; executable = $startInfo.FileName
        activation = 'Package-scoped AppExecutionAlias; never direct WindowsTerminal image or global wtai'
        nativeImage = $Context.App.WindowsTerminal
        wtaPath = $Context.App.WtaPath; wtaSha256 = $Context.Sha256
        stateDirectory = $Context.StateDirectory
        requestedEnvironment = @{
            INTELLIGENT_TERMINAL_AGENT_CENTER = '1'
            INTELLIGENT_TERMINAL_WORK_DEMO_STATE = $Context.StateDirectory
        }
        language = 'Native UI uses its existing OS locale; scripted fixture text is English'
        arguments = @($startInfo.ArgumentList)
        processStart = @{
            windowStyle = $startInfo.WindowStyle.ToString()
            createNoWindow = $startInfo.CreateNoWindow
            useShellExecute = $startInfo.UseShellExecute
            redirectedInput = $startInfo.RedirectStandardInput
            redirectedOutput = $startInfo.RedirectStandardOutput
            redirectedError = $startInfo.RedirectStandardError
        }
        previousWindowIds = @($Context.BeforeWindows | ForEach-Object window_id)
        previousHwnds = @($Context.BeforeNativeWindows | ForEach-Object hwnd)
        previousHostIds = @($hosts | ForEach-Object Id)
        coldStart = $hosts.Count -eq 0
        comDiscovery = 'After the newly created package-owned HWND renders its demo UI; never race cold-start first layout'
        startedUtc = [datetime]::UtcNow
        expectedUiMarker = $ExpectedUiMarker
        nativeHostPrerequisite = 'A reused Dev host must already have Agent Center enabled in its process environment; launcher environment cannot toggle that host'
        physicalScreenRepaint = 'UNQUALIFIED; HWND PNGs are diagnostic until independently reviewed'
    }
    $metadata | ConvertTo-Json -Depth 8 |
        Set-Content -LiteralPath (Join-Path $Context.ArtifactDirectory 'native-launch.json') -Encoding utf8NoBOM
    $launcher = $null
    try {
        $launcher = [Diagnostics.Process]::Start($startInfo)
        $Context.LauncherPid = $launcher.Id
        $metadata.launcherPid = $Context.LauncherPid
        $metadata | ConvertTo-Json -Depth 8 |
            Set-Content -LiteralPath (Join-Path $Context.ArtifactDirectory 'native-launch.json') -Encoding utf8NoBOM
        $nativeWindow = Wait-Until -TimeoutSec 40 -Because 'one new package-owned HWND renders the demo before any COM discovery; a reused host must already have Agent Center enabled' -Condition {
            $launcher.Refresh()
            if ($launcher.HasExited -and $launcher.ExitCode -ne 0) {
                return @{ LaunchFailure = "Native launcher PID $($Context.LauncherPid) exited with code $($launcher.ExitCode) before window creation." }
            }
            $nativeNow = @(Get-WtWindowHwnds -App $Context.App)
            $runtimeErrors = @($nativeNow | Where-Object {
                $_.pid -eq $Context.LauncherPid -and $_.title -eq 'Microsoft Visual C++ Runtime Library'
            })
            if ($runtimeErrors.Count) {
                return @{ LaunchFailure = "Native launcher PID $($Context.LauncherPid) shows a C++ runtime error dialog (HWND $($runtimeErrors[0].hwnd)); no demo window ownership is assumed." }
            }
            $newHwnds = @($nativeNow | Where-Object {
                $_.hwnd -notin @($Context.BeforeNativeWindows.hwnd) -and
                (Get-Process -Id $_.pid -ErrorAction Stop).Path -eq $Context.App.WindowsTerminal -and
                (Get-NativeWorkStoryPackageName -ProcessId $_.pid) -ceq $Context.App.PackageFullName
            })
            if ($newHwnds.Count -eq 1) {
                $probe = @{ App = [pscustomobject]@{ Hwnd = $newHwnds[0].hwnd; Pid = $newHwnds[0].pid } }
                $frame = Get-NativeWorkStoryText -Context $probe
                if ($frame.Contains($ExpectedUiMarker, [StringComparison]::Ordinal)) { return $newHwnds[0] }
            }
            return $false
        }
        if ($nativeWindow -is [hashtable] -and $nativeWindow.ContainsKey('LaunchFailure')) { throw $nativeWindow.LaunchFailure }
        $metadata.nativeReadyUtc = [datetime]::UtcNow
        $metadata.nativeReadyHwnd = $nativeWindow.hwnd
        $metadata.nativeReadyPid = $nativeWindow.pid
        $metadata | ConvertTo-Json -Depth 8 |
            Set-Content -LiteralPath (Join-Path $Context.ArtifactDirectory 'native-launch.json') -Encoding utf8NoBOM
        $window = Wait-Until -TimeoutSec 20 -Because 'the initialized native demo has exactly one new COM window identity' -Condition {
            $newWindows = @(Get-WtWindows -App $Context.App |
                Where-Object { $_.window_id -notin @($Context.BeforeWindows.window_id) })
            if ($newWindows.Count -eq 1) { return $newWindows[0] }
            return $false
        }
        $Context.OwnedWindowId = [string]$window.window_id
        $Context.App | Add-Member -NotePropertyName WindowId -NotePropertyValue $Context.OwnedWindowId -Force
        $Context.App.Hwnd = [int64]$nativeWindow.hwnd
        $Context.App.Pid = [int]$nativeWindow.pid
        $Context.HostPackageFullName = Get-NativeWorkStoryPackageName -ProcessId $Context.App.Pid
        if ($Context.HostPackageFullName -cne $Context.App.PackageFullName) {
            throw "New native window host has unexpected package identity '$($Context.HostPackageFullName)'; expected '$($Context.App.PackageFullName)'."
        }
        if (@(Get-WtTabs -App $Context.App -WindowId $Context.OwnedWindowId).Count) {
            throw 'Native work-story proof requires the top Agent Console with zero shell tabs; an ordinary TermControl launch is not accepted. A reused Dev host must already have INTELLIGENT_TERMINAL_AGENT_CENTER=1 in its process environment. The launcher cannot toggle or restart that host.'
        }
        $state = Wait-Until -TimeoutSec 20 -Because 'native wta ui initializes the isolated demo store; a reused Dev host must already have process-level Agent Center enabled (launcher environment cannot toggle it)' -Condition {
            Read-NativeWorkStoryState -Context $Context
        }
        $text = Wait-Until -TimeoutSec 15 -Because 'the zero-tab native Console explicitly identifies the opted-in demo UI' -Condition {
            $frame = Get-NativeWorkStoryText -Context $Context
            if (-not [string]::IsNullOrWhiteSpace($frame) -and $frame.Contains($ExpectedUiMarker, [StringComparison]::Ordinal)) { return $frame }
            return $false
        }
        $uiProcess = Wait-Until -TimeoutSec 10 -Because 'one newly launched packaged wta ui belongs to the owned native window' -Condition {
            $newUi = @(Get-CimInstance Win32_Process -Filter "Name='wta.exe'" | Where-Object {
                $_.ExecutablePath -eq $Context.App.WtaPath -and $_.CommandLine -match '(?:^|\s)ui(?:\s|$)' -and
                $_.ProcessId -notin $Context.BeforeUiProcessIds
            })
            if ($newUi.Count -eq 1) { return $newUi[0] }
            return $false
        }
        $Context.UiPid = [int]$uiProcess.ProcessId
        $Context.UiCreated = $uiProcess.CreationDate
        $metadata.windowPresentation = Set-NativeWorkStoryWindowSize -Context $Context -Width $WindowWidth -Height $WindowHeight
        $text = Wait-Until -TimeoutSec 10 -Because 'the owned demo redraws after native window sizing' -Condition {
            $frame = Get-NativeWorkStoryText -Context $Context
            if ($frame.Contains($ExpectedUiMarker, [StringComparison]::Ordinal)) { return $frame }
            return $false
        }
        $metadata.initialUiaLineCount = @($text -split '\r?\n').Count
        $state | ConvertTo-Json -Depth 64 |
            Set-Content -LiteralPath (Join-Path $Context.ArtifactDirectory 'native-initial-state.json') -Encoding utf8NoBOM
        $text | Set-Content -LiteralPath (Join-Path $Context.ArtifactDirectory 'native-initial-ui.txt') -Encoding utf8NoBOM
        Save-UiScreenshot -App $Context.App -Path (Join-Path $Context.ArtifactDirectory 'native-initial.png') -RequireSuccess | Out-Null
        $metadata.windowId = $Context.OwnedWindowId
        $metadata.hwnd = $Context.App.Hwnd
        $metadata.hostPid = $Context.App.Pid
        $metadata.hostPackageFullName = $Context.HostPackageFullName
        $metadata.launcherPid = $Context.LauncherPid
        $metadata.uiPid = $Context.UiPid
        $metadata.uiCreated = $Context.UiCreated
        $metadata.uiCommand = $uiProcess.CommandLine
        $metadata.shellTabCount = 0
        $metadata | ConvertTo-Json -Depth 8 |
            Set-Content -LiteralPath (Join-Path $Context.ArtifactDirectory 'native-launch.json') -Encoding utf8NoBOM
        return $Context
    }
    catch {
        $failure = $_
        try {
            $nativeNow = @(Get-WtWindowHwnds -App $Context.App)
            $ownedLauncherDialogs = @(foreach ($item in $nativeNow | Where-Object {
                $_.pid -eq $Context.LauncherPid -and $_.title -eq 'Microsoft Visual C++ Runtime Library'
            }) {
                Add-Type -AssemblyName UIAutomationClient
                Add-Type -AssemblyName UIAutomationTypes
                $root = [Windows.Automation.AutomationElement]::FromHandle([IntPtr][int64]$item.hwnd)
                @{
                    hwnd = $item.hwnd; pid = $item.pid
                    text = @($root.FindAll([Windows.Automation.TreeScope]::Descendants,
                        [Windows.Automation.Condition]::TrueCondition) | ForEach-Object { $_.Current.Name })
                }
            })
            $failureInfo = @{
                error = $failure.Exception.Message; launcherPid = $Context.LauncherPid
                launcherDialogs = $ownedLauncherDialogs
                previousWindowIds = @($Context.BeforeWindows.window_id)
                newNativeCandidates = @($nativeNow | Where-Object {
                    $_.hwnd -notin @($Context.BeforeNativeWindows.hwnd) -and
                    (Get-Process -Id $_.pid -ErrorAction Stop).Path -eq $Context.App.WindowsTerminal
                } | Select-Object hwnd, pid)
                stateFiles = @(Get-ChildItem -LiteralPath $Context.StateDirectory -File | Select-Object Name, Length)
                retainedAmbiguousResources = $true
            }
            $failureInfo | ConvertTo-Json -Depth 8 |
                Set-Content -LiteralPath (Join-Path $Context.ArtifactDirectory 'native-launch-failure.json') -Encoding utf8NoBOM
        }
        catch { Write-Warning "Native launch diagnostics unavailable: $($_.Exception.Message)" }
        try { Stop-NativeWorkStoryDemo -Context $Context }
        catch { Write-Warning "Native demo cleanup retained unverified resources: $($_.Exception.Message)" }
        throw $failure
    }
    finally { if ($launcher) { $launcher.Dispose() } }
}
