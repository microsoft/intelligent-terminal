#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# Real shell stdout -> ConPTY -> parser/Core -> Control event -> tab metadata/UIA.
# Negative controls: root completion cannot hide descendants; invalid/oversized
# reports cannot create a root task; focus is not acknowledgment.
# Existing protections: PaneProgress, ShellIntegration, AgentStatusSessionsOff,
# SessionHookRouting. Real authenticated provider adoption is a separate run.

Describe 'Feature: OSC 7501 program status' -Tag @('Feature', 'ProgramStatus') {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes
        $script:app = $null
        $package = Get-ItTestPackage
        if ($package -eq 'Store') { throw 'OSC 7501 requires a Dev build from this feature branch, not the shipped Store baseline.' }
        $target = Resolve-ItApp -Package $package
        foreach ($entry in @(
            @{ File = 'TerminalApp.dll'; Hash = $env:ITE2E_EXPECTED_APP_SHA256 },
            @{ File = 'Microsoft.Terminal.Control.dll'; Hash = $env:ITE2E_EXPECTED_CONTROL_SHA256 }
        )) {
            if (-not $entry.Hash) { throw "Supply the exact-source deployed SHA256 receipt for $($entry.File)." }
            (Get-FileHash -LiteralPath (Join-Path $target.InstallLocation $entry.File)).Hash | Should -Be $entry.Hash
        }
        $pwsh = (Get-Command pwsh.exe -ErrorAction Stop).Source
        $profile = '{' + [guid]::NewGuid().ToString() + '}'
        $script:app = Start-Terminal -Package $package -PassFre $true -Settings @{
            language = 'en-US'; tabLayout = 'vertical'; startupActions = ''
            firstWindowPreference = 'defaultProfile'; windowingBehavior = 'useNew'
            'warning.confirmOnClose' = 'never'; autoErrorDetectionEnabled = $false
            defaultProfile = $profile
            profiles = @{ list = @(@{
                guid = $profile; name = 'ItE2E program status'
                commandline = "`"$pwsh`" -NoLogo -NoProfile -NoExit"
                shellIntegrationEnabled = $false
            }) }
        }
        $script:app.Launched | Should -BeTrue
        $script:shell = "`"$pwsh`" -NoLogo -NoProfile -NoExit"

        function Write-ProgramWire {
            param([Parameter(Mandatory)][string]$Wire)
            $base64 = [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($Wire))
            $command = "[Console]::Write([Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('$base64')))"
            Send-WtInput -App $script:app -SessionId $script:pane.session_id -Text $command
            Send-WtKeys -App $script:app -SessionId $script:pane.session_id -Keys @('Enter')
        }
        function New-ProgramWire {
            param([string[]]$Reports)
            -join @($Reports | ForEach-Object { [char]27 + ']7501;' + $_ + [char]27 + '\' })
        }
        function Wait-ProgramSnapshot {
            param([Parameter(Mandatory)][scriptblock]$Match)
            $script:snapshotMatch = $Match
            (Wait-WtEvent -Listener $script:listener -TimeoutSec 15 -Predicate {
                $_.method -eq 'program_status_changed' -and
                $_.params.pane_id -eq $script:pane.session_id -and
                (& $script:snapshotMatch $_.params)
            }).params
        }
        function Assert-ProgramMetadata {
            param([Parameter(Mandatory)][string]$Label)
            Wait-Until -TimeoutSec 12 -Because 'the real tab accessibility metadata reflects program status' -Condition {
                $root = [Windows.Automation.AutomationElement]::FromHandle([IntPtr][long]$script:app.Hwnd)
                $elements = $root.FindAll([Windows.Automation.TreeScope]::Descendants,
                    [Windows.Automation.Condition]::TrueCondition)
                @($elements | Where-Object {
                    $_.Current.ProcessId -eq $script:app.Pid -and
                    $_.Current.Name.StartsWith($script:paneTitle, [StringComparison]::Ordinal) -and
                    $_.Current.Name.Contains($Label, [StringComparison]::Ordinal)
                }).Count -gt 0
            } | Should -BeTrue
        }
    }
    BeforeEach {
        $script:paneTitle = 'OSC7501-' + [guid]::NewGuid().ToString('N')
        $script:pane = New-WtTab -App $script:app -Command $script:shell -Title $script:paneTitle
        $script:listener = Start-WtEventListener -App $script:app -EventFilter program_status_changed -WaitForReady
    }
    AfterEach {
        if ($script:listener) { Stop-WtEventListener -Listener $script:listener; $script:listener = $null }
        if ($script:pane) { Close-WtPane -App $script:app -SessionId $script:pane.session_id; $script:pane = $null }
    }
    AfterAll {
        if ($script:app) { Stop-Terminal -App $script:app }
    }

    It 'OSC 7501 hierarchy reaches pane and tab metadata' {
        Write-ProgramWire (New-ProgramWire @(
            'state=done',
            'id=build/compile:state=working:progress=30',
            'id=build/authorize:state=blocked:kind=permission:msg=QXBwcm92ZSBkZXBsb3ltZW50'
        ))
        $snapshot = Wait-ProgramSnapshot { param($value) $value.state -eq 'blocked' -and $value.working -eq 1 -and $value.blocked -eq 1 }
        $snapshot.unseen_done | Should -Be 1
        $snapshot.message | Should -Be 'Approve deployment'
        $snapshot.progress | Should -BeNullOrEmpty -Because 'independent descendants cannot produce a single aggregate percentage'
        Assert-ProgramMetadata 'Waiting for input'
        Assert-ProgramMetadata 'Approve deployment'
        Assert-ProgramMetadata 'Active'
        Assert-ProgramMetadata 'Completed'
    }

    It 'OSC 7501 unread completion survives rapid idle and focus' {
        Write-ProgramWire (New-ProgramWire @('state=done', 'state=idle'))
        $snapshot = Wait-ProgramSnapshot { param($value) $value.state -eq 'idle' -and $value.unseen_done -eq 1 }
        Assert-ProgramMetadata 'Completed'
        Set-WtPaneFocus -App $script:app -SessionId $script:pane.session_id
        Start-Sleep -Milliseconds 600
        Assert-ProgramMetadata 'Completed'
        $revision = $snapshot.revision
        Send-WtKeys -App $script:app -SessionId $script:pane.session_id -Keys @('Escape')
        $script:acknowledgedRevision = $revision
        Wait-ProgramSnapshot { param($value) $value.revision -gt $script:acknowledgedRevision -and $value.unseen_done -eq 0 } | Out-Null
        Assert-ProgramMetadata 'Idle'
    }

    It 'OSC 7501 rejects malformed reports and recovers without shell false positives' {
        $wire = New-ProgramWire @(
            'id=../invalid:state=blocked:kind=auth',
            ('state=blocked:msg=' + ('A' * 4096)),
            'id=valid:state=working:progress=40'
        )
        Write-ProgramWire $wire
        $snapshot = Wait-ProgramSnapshot { param($value) $value.state -eq 'working' -and $value.working -eq 1 }
        $snapshot.blocked | Should -Be 0
        $snapshot.unseen_error | Should -Be 0
        Assert-ProgramMetadata 'Active'
        # An ordinary shell's reports must not fabricate a native agent identity.
        $rows = @(Get-WtPanes -App $script:app | Where-Object session_id -EQ $script:pane.session_id)
        $rows | Should -HaveCount 1
        $rows[0].PSObject.Properties.Name | Should -Contain native_agent_provider_id
        $rows[0].native_agent_provider_id | Should -BeNullOrEmpty
    }

    It 'OSC 7501 prompt cleanup retains unread results but removes live tasks' {
        $wire = New-ProgramWire @(
            'id=result:state=done',
            'id=running:state=working',
            'id=approval:state=blocked:kind=permission'
        )
        Write-ProgramWire ($wire + [char]27 + ']133;A' + [char]27 + '\')
        $snapshot = Wait-ProgramSnapshot { param($value) $value.working -eq 0 -and $value.blocked -eq 0 -and $value.unseen_done -eq 1 }
        $snapshot.done | Should -Be 1
        Assert-ProgramMetadata 'Completed'
    }
}
