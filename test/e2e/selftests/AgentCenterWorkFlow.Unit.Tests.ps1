#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }

BeforeAll {
    . (Join-Path $PSScriptRoot '..\tests\helpers\AgentCenterWorkFlow.ps1')
}

Describe 'Agent Center owned-window visual evidence' -Tag 'Unit' {
    BeforeAll {
        . (Join-Path $PSScriptRoot '..\ItE2E\Private\Core.ps1')
        . (Join-Path $PSScriptRoot '..\ItE2E\Public\Ui.ps1')
        $script:visualOwnershipGuard = ${function:Assert-WorkFlowVisualOwnership}
        $script:visualRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot ('..\artifacts\visual-unit-' + [guid]::NewGuid().ToString('N'))))
        New-Item -ItemType Directory -Path $script:visualRoot -Force | Out-Null
    }
    BeforeEach {
        $root = Join-Path $script:visualRoot ([guid]::NewGuid().ToString('N'))
        New-Item -ItemType Directory -Path $root | Out-Null
        $script:visualContext = @{
            Root = $root; App = @{ Hwnd = 71; Pid = 72 }
            UiIdentity = @{ pid = 73; created = [datetime]'2026-01-01T00:00:00Z' }
            Pane = @{ session_id = 'owned-pane' }; Runtime = @{ sha256 = 'owned-deployed-hash' }
        }
        $script:visualExitCode = 0
        $script:visualOutput = 'png'
        Mock Assert-WorkFlowVisualOwnership {}
        Mock Invoke-WinAppUi {
            if ($script:visualOutput -eq 'png') {
                $png = [Convert]::FromBase64String('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aK1cAAAAASUVORK5CYII=')
                [IO.File]::WriteAllBytes($UiArgs[2], $png)
            }
            elseif ($script:visualOutput -eq 'invalid') {
                'not an image' | Set-Content -LiteralPath $UiArgs[2]
            }
            return @{ ExitCode = $script:visualExitCode; StdErr = 'fixture capture error' }
        }
        Mock Write-ItLog {}
    }
    AfterAll { Remove-Item -LiteralPath $script:visualRoot -Recurse -Force }

    It 'Captures only the verified HWND and pairs the exact frame with image provenance' {
        $frame = "Message for: Work A · Harbor Reports`nactual α evidence"
        Save-WorkFlowVisualEvidence -Context $script:visualContext -Name current-work -Frame $frame
        $record = Get-Content -LiteralPath (Join-Path $script:visualContext.Root 'visual-current-work.json') -Raw | ConvertFrom-Json
        $record.screenshotStatus | Should -BeExactly 'Captured'
        $record.captureScreen | Should -BeFalse
        $record.hwnd | Should -Be 71
        $record.uiIdentity.pid | Should -Be 73
        $record.paneSessionId | Should -BeExactly 'owned-pane'
        $record.wtaSha256 | Should -BeExactly 'owned-deployed-hash'
        (Get-Content -LiteralPath $record.frame -Raw) | Should -BeExactly $frame
        $record.frameSha256 | Should -BeExactly (Get-FileHash -LiteralPath $record.frame).Hash
        $record.screenshotSha256 | Should -BeExactly (Get-FileHash -LiteralPath $record.screenshot).Hash
        Should -Invoke Assert-WorkFlowVisualOwnership -Times 1 -Exactly
        Should -Invoke Invoke-WinAppUi -Times 1 -Exactly -ParameterFilter {
            $App.Hwnd -eq 71 -and -not $NoTarget -and $UiArgs.Count -eq 3 -and
            $UiArgs[0] -ceq 'screenshot' -and $UiArgs[1] -ceq '--output' -and
            $UiArgs[2] -ceq (Join-Path $script:visualContext.Root 'visual-current-work.png')
        }
    }

    It 'Refuses capture after the original ownership check fails' {
        Mock Assert-WorkFlowVisualOwnership { throw 'original window or active pane changed' }
        { Save-WorkFlowVisualEvidence -Context $script:visualContext -Name overview -Frame 'owned' } |
            Should -Throw '*original window or active pane changed*'
        Should -Invoke Invoke-WinAppUi -Times 0 -Exactly
        @(Get-ChildItem -LiteralPath $script:visualContext.Root).Count | Should -Be 0
    }

    It 'Requires <Missing> identity before calling any native window API' -TestCases @(
        @{ Missing = 'HWND' }; @{ Missing = 'host PID' }; @{ Missing = 'UI creation time' }; @{ Missing = 'pane' }
    ) {
        param($Missing)
        switch ($Missing) {
            'HWND' { $script:visualContext.App.Hwnd = 0 }
            'host PID' { $script:visualContext.App.Pid = 0 }
            'UI creation time' { $script:visualContext.UiIdentity.created = $null }
            'pane' { $script:visualContext.Pane.session_id = '' }
        }
        { & $script:visualOwnershipGuard -Context $script:visualContext } | Should -Throw '*original owned HWND*'
        Should -Invoke Invoke-WinAppUi -Times 0 -Exactly
    }

    It 'Rejects relative output roots and boundary names that escape the fixture' {
        { Save-WorkFlowVisualEvidence -Context $script:visualContext -Name '..\outside' -Frame 'owned' } | Should -Throw
        $script:visualContext.Root = 'relative-artifacts'
        { Save-WorkFlowVisualEvidence -Context $script:visualContext -Name overview -Frame 'owned' } |
            Should -Throw '*existing absolute test-fixture directory*'
        Should -Invoke Invoke-WinAppUi -Times 0 -Exactly
    }

    It 'Never reuses an old screenshot as new evidence' {
        $image = Join-Path $script:visualContext.Root 'visual-overview.png'
        'previous artifact' | Set-Content -LiteralPath $image
        { Save-WorkFlowVisualEvidence -Context $script:visualContext -Name overview -Frame 'owned' } |
            Should -Throw '*must not reuse a previous artifact*'
        (Get-Content -LiteralPath $image) | Should -BeExactly 'previous artifact'
        Should -Invoke Invoke-WinAppUi -Times 0 -Exactly
    }

    It 'Records explicit failure and retains the frame when capture produces <Output>' -TestCases @(
        @{ Output = 'none'; Error = '*did not produce a new image*' }
        @{ Output = 'invalid'; Error = '*not a nonempty PNG*' }
    ) {
        param($Output, $Error)
        $script:visualOutput = $Output
        { Save-WorkFlowVisualEvidence -Context $script:visualContext -Name fixed-report -Frame 'actual fixed report' } |
            Should -Throw $Error
        $record = Get-Content -LiteralPath (Join-Path $script:visualContext.Root 'visual-fixed-report.json') -Raw | ConvertFrom-Json
        $record.screenshotStatus | Should -BeExactly 'Failed'
        $record.error | Should -BeLike $Error
        $record.PSObject.Properties.Name | Should -Not -Contain 'screenshotSha256'
        (Get-Content -LiteralPath $record.frame -Raw) | Should -BeExactly 'actual fixed report'
    }

    It 'Rejects a failed capture command even when a PNG exists' {
        $script:visualExitCode = 9
        { Save-WorkFlowVisualEvidence -Context $script:visualContext -Name typed-intake -Frame 'actual question' } |
            Should -Throw '*screenshot failed (exit 9)*'
        $record = Get-Content -LiteralPath (Join-Path $script:visualContext.Root 'visual-typed-intake.json') -Raw | ConvertFrom-Json
        $record.screenshotStatus | Should -BeExactly 'Failed'
        $record.PSObject.Properties.Name | Should -Not -Contain 'screenshotSha256'
    }

    It 'Preserves the existing optional screenshot warning behavior for other callers' {
        $script:visualExitCode = 9
        $script:visualOutput = 'none'
        $path = Join-Path $script:visualContext.Root 'optional.png'
        Save-UiScreenshot -App $script:visualContext.App -Path $path | Should -BeExactly $path
        Should -Invoke Write-ItLog -Times 1 -Exactly -ParameterFilter { $Level -ceq 'WARN' -and $Message -like '*fixture capture error*' }
    }
}

Describe 'Agent Center work-flow fixture contracts' -Tag 'Unit' {
    It 'Reads only the selected action from the activity pane' {
        $frame = @'
┌Agent Center──────────────────────────────────────────┐
│Current work: Review the harbor checklist              │
└───────────────────────────────────────────────────────┘
┌Works─────────┐┌F4 actions─────────────────────────────┐
│> unrelated   ││↑↓ choose · Enter open · Esc dismiss   │
│              ││                                      │
│              ││> Open: Review the harbor checklist   │
│              ││  · Harbor Reports · C:\a long path    │
│              ││                                      │
│              ││  Open: Review the orchard checklist  │
└──────────────┘└──────────────────────────────────────┘
┌Enter send─────────────────────────────────────────────┐
│> not a menu selection                                 │
└───────────────────────────────────────────────────────┘
'@
        Get-WorkFlowSelectedMenuLabel $frame | Should -BeExactly 'Open: Review the harbor checklist'
        $body = Get-WorkFlowBodyRows $frame
        $body.Count | Should -Be 6
        $body[0] | Should -BeExactly '↑↓ choose · Enter open · Esc dismiss'
        $body[3] | Should -BeExactly '  · Harbor Reports · C:\a long path'
    }

    It 'Reads the unbordered full-width F4 overlay while excluding the independent composer' {
        $frame = @'
Review the harbor checklist
Harbor Reports · Draft
Request succeeded.
F4 actions · Up/Down select · Enter open · Esc back · F12 diagnostics
  New work / Home
> Open: Review the orchard checklist · Orchard Notes
  · C:\a long test-owned path

Message for: Review the harbor checklist · Harbor Reports
┌Enter send · Shift+Enter newline────────────────────┐
│> this is draft text, not the selected menu action   │
└─────────────────────────────────────────────────────┘
Enter send · F4 actions
'@
        Get-WorkFlowSelectedMenuLabel $frame | Should -BeExactly 'Open: Review the orchard checklist · Orchard Notes'
        (Get-WorkFlowBodyRows $frame) -join "`n" | Should -Not -Match 'this is draft text'
    }

    It 'Reads a wrapped proposal summary without borrowing the next menu item' {
        $frame = @'
Global conversation
Context hint: Other work · Harbor Reports
F4 actions · Up/Down select · Enter open · Esc back · F12 diagnostics
> Review brief and approve start · Horizon Decisions · Start the human-approved
decision report
  Start an unrelated work

Global conversation
┌Enter send────────┐
│global draft      │
└──────────────────┘
'@
        $label = Get-WorkFlowSelectedMenuLabel -Frame $frame -IncludeWrapped
        $label | Should -BeExactly 'Review brief and approve start · Horizon Decisions · Start the human-approved decision report'
        $label | Should -Not -Match 'unrelated'
        (Get-WorkFlowSelectedMenuLabel -Frame $frame) | Should -Not -Match 'decision report$'
    }

    It 'Reads full-width modal JSON and form rows without borrowing header or composer content' {
        $request = [ordered]@{
            method = 'decision.answer'; params = @{ decisionId = 'decision-a'; value = @{ note = 'harbor α' } }
            ifMatch = @(@{ kind = 'DecisionRequest'; id = 'decision-a'; version = 4 }); commandId = 'captured-nonce'
        }
        $body = (@{ request = $request } | ConvertTo-Json -Depth 10) -split '\r?\n' | ForEach-Object { "│$_ │" }
        $frame = "Work A`nHarbor Reports · Active`n`n┌Diagnostic protocol details (read-only)────────┐`n" +
            ($body -join "`n") + "`n└───────────────────────────────────────────────┘`n" +
            "Message for: Work A · Harbor Reports`n┌Enter send──┐`n│draft       │`n└────────────┘"
        $captured = Get-WorkFlowCapturedRequest $frame
        $captured.commandId | Should -BeExactly 'captured-nonce'
        $captured.params.value.note | Should -BeExactly 'harbor α'
        $captured.ifMatch[0].version | Should -Be 4
        $form = "Work A`nHarbor Reports · Active`n`n┌Answer────────┐`n│> Note *: 港口  │`n│  Format *:   │`n└──────────────┘"
        Get-WorkFlowFormField -Frame $form -Label 'Note' | Should -BeExactly '港口'
        Get-WorkFlowFormField -Frame $form -Label 'Format' | Should -BeExactly ''
        { Get-WorkFlowBodyRows ($form -replace '└──────────────┘', 'missing border') } | Should -Throw '*malformed body rows*'
    }

    It 'Requires consistent main header and composer identity at <Width> columns' -TestCases @(
        @{ Width = 70 }; @{ Width = 90 }; @{ Width = 132 }; @{ Width = 140 }
    ) {
        param($Width)
        $rail = if ($Width -ge 90) { ('rail'.PadRight(25)) + '│' } else { '' }
        $frame = (@('Work A', 'Harbor Reports · Draft', '', 'Next responsibility', 'Review the brief',
            'F6 · Work actions', '> Cancel work', 'Message for: Work A · Harbor Reports',
            '┌Enter send─────────┐', '│/draft             │', '└───────────────────┘') |
            ForEach-Object { $rail + $_ }) -join "`n"
        Test-WorkFlowRenderedSelection -Frame $frame -Goal 'Work A' -ProjectName 'Harbor Reports' | Should -BeTrue
        Test-WorkFlowRenderedSelection -Frame $frame -Goal 'Work B' -ProjectName 'Harbor Reports' | Should -BeFalse
        Test-WorkFlowRenderedSelection -Frame $frame -Goal 'Work A' -ProjectName 'Orchard Notes' | Should -BeFalse
        Test-WorkFlowRenderedSelection -Frame ($frame -replace 'Message for: Work A', 'Message for: Work B') `
            -Goal 'Work A' -ProjectName 'Harbor Reports' | Should -BeFalse
        { Get-WorkFlowSelectedMenuLabel $frame } | Should -Throw '*not a focused card or draft*'
        $homeFrame = $frame -replace 'Work A', 'Overview' -replace 'Message for: Overview ·', 'New work ·'
        Test-WorkFlowRenderedSelection -Frame $homeFrame -ProjectName 'Harbor Reports' | Should -BeTrue
    }

    It 'Keeps foreign attention separate from the selected header draft and caret at <Width> columns' -TestCases @(
        @{ Width = 70 }; @{ Width = 90 }; @{ Width = 132 }; @{ Width = 140 }
    ) {
        param($Width)
        $rail = if ($Width -ge 90) { ('foreign work attention'.PadRight(25)) + '│' } else { '' }
        $attention = if ($Width -eq 90) {
            @('Needs attention: 1 · Review the orchard checklist · Orchard', 'Notes')
        } else {
            @('Needs attention: 1 · Review the orchard checklist · Orchard Notes')
        }
        $main = @('Work A', 'Harbor Reports · Draft') + $attention + @(
            'Request succeeded.', 'Next responsibility', 'Review the brief',
            'Message for: Work A · Harbor Reports', '┌Enter send────────────────────┐',
            '│/workflow-harbor-draft         │', '└──────────────────────────────┘')
        $lines = @($main | ForEach-Object { $rail + $_ })
        $frame = $lines -join "`n"
        Test-WorkFlowRenderedSelection -Frame $frame -Goal 'Work A' -ProjectName 'Harbor Reports' | Should -BeTrue
        Test-WorkFlowRenderedSelection -Frame $frame -Goal 'Review the orchard checklist' -ProjectName 'Orchard Notes' | Should -BeFalse
        $wrongScope = $frame.Replace('Message for: Work A · Harbor Reports', 'Message for: Review the orchard checklist · Orchard Notes')
        Test-WorkFlowRenderedSelection -Frame $wrongScope -Goal 'Review the orchard checklist' -ProjectName 'Orchard Notes' | Should -BeFalse
        Test-WorkFlowRenderedSelection -Frame $wrongScope -Goal 'Work A' -ProjectName 'Harbor Reports' | Should -BeFalse
        $box = Get-NativePasteInputBox $frame
        $box.rows | Should -Be @('/workflow-harbor-draft')
        $before = (($lines | Select-Object -First $box.firstRow) -join "`n") + "`n$rail" + '│/workflow-ha'
        Get-NativePasteCaretPrefix -InputBox $box -DocumentBeforeCaret $before | Should -BeExactly '/workflow-ha'
        { Get-NativePasteCaretPrefix -InputBox $box -DocumentBeforeCaret ($lines[0..2] -join "`n") } |
            Should -Throw '*outside the captured composer rows*'
    }

    It 'Rejects absent or ambiguous visible menu selections' -TestCases @(
        @{ Frame = "│> sidebar││  no selected action│" }
        @{ Frame = "│ ││> first│`n│ ││> second│" }
        @{ Frame = "┌Agent Center Enter┐`n│> draft only│" }
    ) {
        param($Frame)
        { Get-WorkFlowSelectedMenuLabel $Frame } | Should -Throw '*one visibly selected*'
    }

    It 'Preserves JSON indentation and Unicode in diagnostic body rows' {
        $frame = "│ ││{                                  │`n│ ││  `"method`": `"work.control`",           │`n│ ││  `"label`": `"港口`"                    │`n│ ││}                                  │"
        $rows = Get-WorkFlowBodyRows $frame
        $rows[1] | Should -BeExactly '  "method": "work.control",'
        (($rows -join "`n") | ConvertFrom-Json).label | Should -BeExactly '港口'
    }

    It 'Reads the complete captured request from <Envelope> diagnostics' -TestCases @(
        @{ Envelope = 'direct' }
        @{ Envelope = 'request with clipped snapshot' }
        @{ Envelope = 'operation' }
    ) {
        param($Envelope)
        $request = @{
            method = 'work.control'; params = @{ workId = 'harbor-work'; action = 'Cancel' }
            ifMatch = @(@{ kind = 'Work'; id = 'harbor-work'; version = 7 })
            commandId = '72e3c2d0-3f7c-4aa8-8444-b9fe0c46d15a'
        }
        $json = switch ($Envelope) {
            'direct' { $request | ConvertTo-Json -Depth 8 }
            'operation' { @{ operation = $request } | ConvertTo-Json -Depth 8 }
            default {
                $member = (($request | ConvertTo-Json -Depth 8) -split '\r?\n' | ForEach-Object { '  ' + $_ }) -join "`n"
                "{`n  `"request`": " + $member.Substring(2) + ",`n  `"work`": {`n    `"snapshotContinues`":"
            }
        }
        $frame = (($json -split '\r?\n') | ForEach-Object { "│sidebar││$_ │" }) -join "`n"
        $actual = Get-WorkFlowCapturedRequest $frame
        $actual.method | Should -BeExactly 'work.control'
        $actual.params.workId | Should -BeExactly 'harbor-work'
        $actual.ifMatch[0].version | Should -Be 7
        $actual.commandId | Should -BeExactly $request.commandId
    }

    It 'Rejects incomplete or unrelated diagnostic objects' -TestCases @(
        @{ Body = "{`n  `"request`": {`n    `"method`": `"work.control`"" }
        @{ Body = '{"work":{"method":"work.control"}}' }
    ) {
        param($Body)
        $frame = (($Body -split '\r?\n') | ForEach-Object { "│sidebar││$_ │" }) -join "`n"
        { Get-WorkFlowCapturedRequest $frame } | Should -Throw
    }

    It 'Rejects ambiguous quoted physical rows in <Envelope> diagnostics' -TestCases @(
        @{ Envelope = 'direct' }; @{ Envelope = 'request' }; @{ Envelope = 'operation' }
    ) {
        param($Envelope)
        $head = '"' + ('x' * 137)
        $tail = 'y"'
        $head.Length | Should -Be 138
        $withoutSeparator = ($head + $tail) | ConvertFrom-Json
        $withSeparator = ($head + ' ' + $tail) | ConvertFrom-Json
        $withoutSeparator | Should -Not -BeExactly $withSeparator
        $rows = @(
            '{'
            '  "method": "project.configure",'
            '  "params": {'
            '    "root":'
            $head
            $tail
            '  },'
            '  "ifMatch": [],'
            '  "commandId": "frozen"'
            '}'
        )
        $permissive = ($rows -join "`n") | ConvertFrom-Json
        $permissive.params.root | Should -BeExactly (('x' * 137) + "`n" + 'y')
        if ($Envelope -ne 'direct') {
            $rows = @('{', ('  "' + $Envelope + '": {')) + @($rows | Select-Object -Skip 1) + @('}')
        }
        $frame = ($rows | ForEach-Object { "││$_│" }) -join "`n"
        { Get-WorkFlowCapturedRequest $frame } | Should -Throw '*Ambiguous physical row break*'
    }

    It 'Preserves escaped line breaks and meaningful whitespace in captured values' {
        $note = "  before`r`n`t`"quoted`" \\ end  港口"
        $request = [ordered]@{
            method = 'conversation.answer_input'; params = @{ value = @{ note = $note; literal = '\n\u0020'; spaces = 'a  b ' } }
            ifMatch = @(); commandId = 'frozen'
        }
        $rows = ($request | ConvertTo-Json -Depth 10) -split '\r?\n'
        $frame = ($rows | ForEach-Object { "││$_│" }) -join "`n"
        $actual = Get-WorkFlowCapturedRequest $frame
        $actual.params.value.note | Should -BeExactly $note
        $actual.params.value.literal | Should -BeExactly '\n\u0020'
        $actual.params.value.spaces | Should -BeExactly 'a  b '
        Test-WorkFlowFrozenRequest $request $actual | Should -BeTrue
    }

    It 'Rejects corrupt or duplicate diagnostic JSON without coercion: <Body>' -TestCases @(
        @{ Body = '{"method":"work.control","params":{"root":"a","root":"b"},"ifMatch":[],"commandId":"frozen"}' }
        @{ Body = '{"method":"work.control","params":{"value":NaN},"ifMatch":[],"commandId":"frozen"}' }
        @{ Body = '{"method":"work.control","params":{},"ifMatch":[],"commandId":"frozen"} trailing' }
    ) {
        param($Body)
        { Get-WorkFlowCapturedRequest "││$Body│" } | Should -Throw
    }

    It 'Keeps incomplete direct requests distinguishable from invalid diagnostics for paging' {
        $frame = "││{│`n││  `"method`": `"work.control`",│"
        $caught = $null
        try { Get-WorkFlowCapturedRequest $frame }
        catch { $caught = $_.Exception }
        $caught | Should -BeOfType ([IO.EndOfStreamException])
    }

    It 'Persists expected and observed requests plus exact physical rows on mismatch or rejection' {
        $root = Join-Path $PSScriptRoot ('..\artifacts\diagnostic-unit-' + [guid]::NewGuid().ToString('N'))
        New-Item -ItemType Directory -Path $root | Out-Null
        try {
            $expected = @{ method = 'project.configure'; params = @{ root = 'C:\owned\approved' }; ifMatch = @(); commandId = 'frozen' }
            $actual = @{ method = 'project.configure'; params = @{ root = "C:\owned\ap`nproved" }; ifMatch = @(); commandId = 'frozen' }
            $rows = @('  "root": "C:\\owned\\ap', 'proved"')
            $frame = "││$($rows[0])│`r`n││$($rows[1])│"
            $file = Join-Path $root 'mismatch.json'
            Save-WorkFlowCapturedActionEvidence -Path $file -Expected $expected -Actual $actual -Frames @($frame) -Rows $rows
            $saved = Get-Content -LiteralPath $file -Raw | ConvertFrom-Json
            $saved.exactMatch | Should -BeFalse
            $saved.expected.params.root | Should -BeExactly $expected.params.root
            $saved.actual.params.root | Should -BeExactly $actual.params.root
            $saved.physicalFrames | Should -Be @($frame)
            $saved.physicalRows | Should -Be $rows
            $saved.physicalJson | Should -BeExactly ($rows -join "`n")
            $file = Join-Path $root 'rejected.json'
            Save-WorkFlowCapturedActionEvidence -Path $file -Expected $expected -Frames @($frame) -Rows $rows -CaptureError 'Ambiguous physical row break'
            $saved = Get-Content -LiteralPath $file -Raw | ConvertFrom-Json
            $saved.actual | Should -BeNullOrEmpty
            $saved.exactMatch | Should -BeNullOrEmpty
            $saved.error | Should -BeExactly 'Ambiguous physical row break'
            $saved.expected.params.root | Should -BeExactly $expected.params.root
            $saved.physicalFrames | Should -Be @($frame)
        }
        finally { Remove-Item -LiteralPath $root -Recurse -Force }
    }

    Context 'Encoded frozen diagnostic transport' {
        BeforeAll {
            function New-EncodedRows([string]$Json, [int]$Width = 37) {
                $hex = [Convert]::ToHexString([Text.Encoding]::UTF8.GetBytes($Json)).ToLowerInvariant()
                $value = '"' + $hex + '"'
                $rows = @('{', '  "requestUtf8Hex":')
                for ($i = 0; $i -lt $value.Length; $i += $Width) {
                    $rows += $value.Substring($i, [Math]::Min($Width, $value.Length - $i))
                }
                return ,@($rows + @('}'))
            }
            function New-DiagnosticFrame([string[]]$Rows) {
                ($Rows | ForEach-Object { "││$_   │" }) -join "`n"
            }
            function New-EncodedRequest {
                [ordered]@{
                    method = 'project.configure'
                    params = @{ root = 'C:\owned\' + ('long directory  ' * 20) + 'approved ';
                        note = "  before`r`n`t港口" + [char]::ConvertFromUtf32(0x1F469) + [char]0x200D + [char]::ConvertFromUtf32(0x1F4BB) + '  ';
                        literal = '\n\t\u0020'; date = '2026-01-01T00:00:00+00:00' }
                    ifMatch = @(@{ kind = 'Work'; id = 'owned'; version = 17 })
                    commandId = 'frozen'
                }
            }
        }

        It 'Decodes wrapped UTF8 hex without changing paths, whitespace, escapes or Unicode' {
            $expected = New-EncodedRequest
            $rows = New-EncodedRows ($expected | ConvertTo-Json -Depth 20 -Compress)
            $actual = Get-WorkFlowCapturedRequest (New-DiagnosticFrame $rows) -RequireEncoding
            Test-WorkFlowFrozenRequest $expected $actual | Should -BeTrue
            $actual.params.root | Should -BeExactly $expected.params.root
            $actual.params.note | Should -BeExactly $expected.params.note
            $actual.params.literal | Should -BeExactly $expected.params.literal
            $actual.params.date | Should -BeOfType ([string])
            $actual.params.date | Should -BeExactly $expected.params.date
        }

        It 'Uses only the root encoding despite nested decoys and lossy readable strings' {
            $expected = New-EncodedRequest
            $encoded = New-EncodedRows ($expected | ConvertTo-Json -Depth 20 -Compress)
            $rows = @('{', '  "preview": {', '    "requestUtf8Hex": "deadbeef",',
                '    "text": "physically broken', 'readable string"', '  },') +
                @($encoded | Select-Object -Skip 1)
            $actual = Get-WorkFlowCapturedRequest (New-DiagnosticFrame $rows)
            Test-WorkFlowFrozenRequest $expected $actual | Should -BeTrue
        }

        It 'Detects changed decoded <Field> without normalizing the frozen comparison' -TestCases @(
            @{ Field = 'method' }; @{ Field = 'commandId' }; @{ Field = 'guard' }; @{ Field = 'params' }; @{ Field = 'date string' }
        ) {
            param($Field)
            $expected = New-EncodedRequest
            $changed = New-EncodedRequest
            switch ($Field) {
                'method' { $changed.method = 'work.control' }
                'commandId' { $changed.commandId = 'different' }
                'guard' { $changed.ifMatch[0].version++ }
                'params' { $changed.params.root = $changed.params.root.TrimEnd() }
                'date string' { $changed.params.date = '2026-01-01T00:00:00Z' }
            }
            $actual = Get-WorkFlowCapturedRequest (New-DiagnosticFrame (New-EncodedRows ($changed | ConvertTo-Json -Depth 20 -Compress)))
            Test-WorkFlowFrozenRequest $expected $actual | Should -BeFalse
        }

        It 'Rejects noncanonical hex or UTF8: <Value>' -TestCases @(
            @{ Value = '""' }; @{ Value = '"a"' }; @{ Value = '"7B7D"' }; @{ Value = '"0g"' }
            @{ Value = '"c080"' }; @{ Value = '"ff"' }; @{ Value = '"eda080"' }
            @{ Value = '"\u0037b7d"' }; @{ Value = 'null' }; @{ Value = '123' }
        ) {
            param($Value)
            $rows = @('{', ('  "requestUtf8Hex": ' + $Value), '}')
            { Get-WorkFlowCapturedRequest (New-DiagnosticFrame $rows) } | Should -Throw
        }

        It 'Rejects invalid decoded JSON or request shape: <Json>' -TestCases @(
            @{ Json = '[]' }; @{ Json = '{}' }
            @{ Json = '{"method":"x","params":{"a":1,"a":2},"ifMatch":[],"commandId":"id"}' }
            @{ Json = '{"method":"x","params":{},"ifMatch":[],"commandId":"id","commandId":"other"}' }
            @{ Json = '{"method":"x","params":[],"ifMatch":[],"commandId":"id"}' }
            @{ Json = '{"method":"x","params":{},"ifMatch":{},"commandId":"id"}' }
            @{ Json = '{"method":"x","params":{},"ifMatch":[{"kind":"Work","id":"a","version":"1"}],"commandId":"id"}' }
            @{ Json = '{"method":"x","params":{},"ifMatch":[],"commandId":null}' }
            @{ Json = '{"method":"x","params":{},"ifMatch":[],"commandId":"id","extra":true}' }
            @{ Json = '{"method":"x","params":{},"ifMatch":[],"commandId":"id"} trailing' }
            @{ Json = ('{"method":"x","params":{"note":"raw' + "`n" + 'newline"},"ifMatch":[],"commandId":"id"}') }
        ) {
            param($Json)
            { Get-WorkFlowCapturedRequest (New-DiagnosticFrame (New-EncodedRows $Json)) } | Should -Throw
        }

        It 'Waits for the root ending and rejects a duplicate encoding on a later page' {
            $rows = New-EncodedRows ((New-EncodedRequest) | ConvertTo-Json -Depth 20 -Compress)
            $first = @($rows | Select-Object -First ($rows.Count - 1))
            $caught = $null
            try { Get-WorkFlowCapturedRequest (New-DiagnosticFrame $first) } catch { $caught = $_.Exception }
            $caught | Should -BeOfType ([IO.EndOfStreamException])
            $first[-1] += ','
            $later = @($first | Select-Object -Last 3) + @('  "requestUtf8Hex": "7b7d"', '}')
            $merged = Merge-WorkFlowBodyRows $first $later
            { Get-WorkFlowCapturedRequest (New-DiagnosticFrame $merged) } | Should -Throw '*Duplicate root requestUtf8Hex*'
        }

        It 'Retains incomplete encoded pages without falling back to a complete readable request' {
            $legacy = ((New-EncodedRequest) | ConvertTo-Json -Depth 20) -split '\r?\n'
            $prefix = @('{', '  "request": {') + @($legacy | Select-Object -Skip 1)
            $prefix[-1] += ','
            foreach ($ending in @('  "requestUtf8Hex":', '  "requestUtf8Hex": "7b', '  "requestUtf8He')) {
                $caught = $null
                try { Get-WorkFlowCapturedRequest (New-DiagnosticFrame @($prefix + @($ending))) -RequireEncoding }
                catch { $caught = $_.Exception }
                $caught | Should -BeOfType ([IO.EndOfStreamException])
            }
            { Get-WorkFlowCapturedRequest (New-DiagnosticFrame $legacy) -RequireEncoding } | Should -Throw '*missing requestUtf8Hex*'
        }

        It 'Does not use readable data when the authoritative encoding is invalid' {
            $legacy = ((New-EncodedRequest) | ConvertTo-Json -Depth 20) -split '\r?\n'
            $rows = @('{', '  "requestUtf8Hex": "xx",', '  "request": {') +
                @($legacy | Select-Object -Skip 1) + @('}')
            { Get-WorkFlowCapturedRequest (New-DiagnosticFrame $rows) } | Should -Throw '*lowercase hex*'
        }

        It 'Decodes only after verified overlapping encoded pages reach the root ending' {
            $expected = New-EncodedRequest
            $all = New-EncodedRows ($expected | ConvertTo-Json -Depth 20 -Compress) 17
            $rows = @($all | Select-Object -First 12)
            $actual = $null
            for ($offset = 8; $offset -lt $all.Count; $offset += 8) {
                $rows = Merge-WorkFlowBodyRows $rows @($all | Select-Object -Skip $offset -First 12)
                try { $actual = Get-WorkFlowCapturedRequest (New-DiagnosticFrame $rows) -RequireEncoding }
                catch [IO.EndOfStreamException] {}
                if ($rows[-1] -cne '}') { $actual | Should -BeNullOrEmpty }
            }
            Test-WorkFlowFrozenRequest $expected $actual | Should -BeTrue
        }

        It 'Rejects nested encoding or malformed root boundaries instead of trusting a decoy' {
            $encoded = New-EncodedRows ((New-EncodedRequest) | ConvertTo-Json -Depth 20 -Compress)
            $nested = @('{', '  "preview": {') + @($encoded | Select-Object -Skip 1 | ForEach-Object { '  ' + $_ }) + @('}')
            { Get-WorkFlowCapturedRequest (New-DiagnosticFrame $nested) -RequireEncoding } | Should -Throw '*missing requestUtf8Hex*'
            $encoded[-1] = ']'
            { Get-WorkFlowCapturedRequest (New-DiagnosticFrame $encoded) } | Should -Throw '*Malformed diagnostic root*'
        }

        It 'Requires encoded transport in the actual native capture call' {
            $tokens = $null
            $errors = $null
            $ast = [Management.Automation.Language.Parser]::ParseFile(
                (Join-Path $PSScriptRoot '..\tests\Feature.AgentCenterWorkFlow.Tests.ps1'), [ref]$tokens, [ref]$errors)
            $errors.Count | Should -Be 0
            $reader = $ast.Find({
                param($node)
                $node -is [Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -ceq 'Read-WorkFlowCapturedAction'
            }, $true)
            $calls = @($reader.Body.FindAll({
                param($node)
                $node -is [Management.Automation.Language.CommandAst] -and $node.GetCommandName() -ceq 'Get-WorkFlowCapturedRequest'
            }, $true))
            $calls.Count | Should -Be 1
            @($calls[0].CommandElements | Where-Object {
                $_ -is [Management.Automation.Language.CommandParameterAst] -and $_.ParameterName -ceq 'RequireEncoding'
            }).Count | Should -Be 1
        }
    }

    It 'Joins only diagnostic pages with verified ordinal overlap' {
        $merged = Merge-WorkFlowBodyRows @('a', 'b', 'c', 'd', 'e') @('c', 'd', 'e', 'f')
        $merged | Should -Be @('a', 'b', 'c', 'd', 'e', 'f')
        { Merge-WorkFlowBodyRows @('a', 'b', 'c') @('A', 'b', 'c', 'd') } | Should -Throw '*verifiable overlapping*'
    }

    It 'Rejects repeated page rows that admit different encoded byte counts' {
        { Merge-WorkFlowBodyRows @('a', 'b', 'c', 'a', 'b', 'c') @('a', 'b', 'c', 'a', 'b', 'c', 'tail') } |
            Should -Throw '*Ambiguous diagnostic page overlap*'
    }

    It 'Reconstructs a captured typed request spanning two diagnostic pages' {
        $request = @{
            method = 'conversation.answer_input'; commandId = '72e3c2d0-3f7c-4aa8-8444-b9fe0c46d15a'
            ifMatch = @(@{ kind = 'IntakeRequest'; id = 'intake-fixture'; version = 4 })
            params = @{ requestId = 'intake-fixture'; action = 'Answer'; value = @{ copies = 7; includeDetails = $true; note = '港口' } }
        }
        $rows = (@{ request = $request } | ConvertTo-Json -Depth 10) -split '\r?\n'
        $first = @($rows | Select-Object -First 14)
        $second = @($rows | Select-Object -Skip 10)
        $incomplete = ($first | ForEach-Object { "││$_│" }) -join "`n"
        { Get-WorkFlowCapturedRequest $incomplete } | Should -Throw
        $merged = Merge-WorkFlowBodyRows $first $second
        $frame = ($merged | ForEach-Object { "││$_│" }) -join "`n"
        $actual = Get-WorkFlowCapturedRequest $frame
        $actual.params.value.copies | Should -Be 7
        $actual.params.value.includeDetails | Should -BeTrue
        $actual.params.value.note | Should -BeExactly '港口'
        $actual.ifMatch[0].version | Should -Be 4
    }

    It 'Finds the frozen request after a long insertion-ordered work snapshot' {
        $request = [ordered]@{
            commandId = '72e3c2d0-3f7c-4aa8-8444-b9fe0c46d15a'; method = 'work.control'
            params = @{ workId = 'captured-work'; action = 'Cancel'; note = 'quoted "value" with } and {' }
            ifMatch = @(@{ kind = 'Work'; id = 'captured-work'; version = 17 })
        }
        $document = [ordered]@{
            work = [ordered]@{
                work = @{ id = 'captured-work'; goal = 'Review the harbor checklist' }
                obligations = @(1..20 | ForEach-Object { @{ reason = 'DraftApproval'; description = "Recorded obligation $_" } })
            }
            request = $request
        }
        $all = ($document | ConvertTo-Json -Depth 20) -split '\r?\n'
        $rows = @($all | Select-Object -First 30)
        $frame = ($rows | ForEach-Object { "│sidebar││$_ │" }) -join "`n"
        { Get-WorkFlowCapturedRequest $frame } | Should -Throw '*No complete captured request*'
        $pages = 0
        for ($offset = 10; $offset -lt $all.Count; $offset += 10) {
            $rows = Merge-WorkFlowBodyRows $rows @($all | Select-Object -Skip $offset -First 30)
            $pages++
            $frame = ($rows | ForEach-Object { "│sidebar││$_ │" }) -join "`n"
            try { $captured = Get-WorkFlowCapturedRequest $frame; break } catch {}
        }
        $pages | Should -BeGreaterThan 4
        $captured.commandId | Should -BeExactly $request.commandId
        $captured.params.note | Should -BeExactly $request.params.note
        $captured.ifMatch[0].version | Should -Be 17
    }

    It 'Distinguishes unselected form values from visible choice labels' {
        $frame = "│sidebar││> Format *:          │`n│sidebar││plain | table        │`n│sidebar││  Copies *: 7        │"
        Get-WorkFlowFormField -Frame $frame -Label 'Format' | Should -BeExactly ''
        Get-WorkFlowFormField -Frame $frame -Label 'Copies' | Should -BeExactly '7'
        { Get-WorkFlowFormField -Frame $frame -Label 'Missing' } | Should -Throw '*one rendered field*'
    }

    It 'Runs the scripted intake contracts without a service or network endpoint' {
        $tests = Join-Path $PSScriptRoot 'AgentCenterIntakeCoordinator.Unit.cjs'
        $output = & node.exe --test $tests 2>&1
        if ($LASTEXITCODE -ne 0) { throw ($output -join "`n") }
    }

    It 'Configures only a local fail-closed capability' {
        $config = Get-WorkFlowAdapterConfiguration
        $config.capabilities.Count | Should -Be 1
        $config.capabilities[0].adapter.executable | Should -BeExactly 'cmd.exe'
        $config.capabilities[0].adapter.args | Should -Be @('/d', '/c', 'exit 77')
        $config.capabilities[0].adapter.approvedModelDestination | Should -Match 'no model service'
    }

    It 'Adds only explicitly scoped local scripted capabilities' {
        $adapter = Join-Path $PSScriptRoot '..\fixtures\AgentCenterIntakeCoordinator.cjs'
        $config = Get-WorkFlowAdapterConfiguration -IntakeScriptPath $adapter -EvidenceDirectory $PSScriptRoot
        $config.capabilities.Count | Should -Be 5
        $config.capabilities[1].id | Should -BeExactly 'ite2e-local-intake'
        $config.capabilities[1].adapter.args | Should -Be @('--no-warnings', $adapter, $PSScriptRoot)
        $config.capabilities[2].id | Should -BeExactly 'ite2e-local-report'
        $config.capabilities[2].adapter.args | Should -Be @('--no-warnings', $adapter, $PSScriptRoot, 'report')
        $config.capabilities[3].id | Should -BeExactly 'ite2e-local-decision'
        $config.capabilities[3].adapter.args | Should -Be @('--no-warnings', $adapter, $PSScriptRoot, 'decision')
        $config.conversationCapabilityId | Should -BeExactly 'ite2e-local-global'
        $config.capabilities[4].adapter.args | Should -Be @('--no-warnings', $adapter, $PSScriptRoot, 'global')
        $config.conversationLimits.Keys.Count | Should -Be 7
        $config.conversationLimits.coordinationTurns | Should -Be 24
        { Get-WorkFlowAdapterConfiguration -IntakeScriptPath $adapter } | Should -Throw '*isolated evidence directory*'
    }

    It 'Runs the single-report adapter contracts without a live authority' {
        $tests = Join-Path $PSScriptRoot 'AgentCenterReportJourney.Unit.cjs'
        $output = & node.exe --test $tests 2>&1
        if ($LASTEXITCODE -ne 0) { throw ($output -join "`n") }
    }

    It 'Runs initial brief and bound decision continuation contracts without a live authority' {
        $tests = Join-Path $PSScriptRoot 'AgentCenterDecisionJourney.Unit.cjs'
        $output = & node.exe --test $tests 2>&1
        if ($LASTEXITCODE -ne 0) { throw ($output -join "`n") }
    }

    It 'Runs the isolated global adapter contracts without launching a service' {
        $output = & node.exe --test (Join-Path $PSScriptRoot 'AgentCenterGlobalJourney.Unit.cjs') 2>&1
        if ($LASTEXITCODE -ne 0) { throw ($output -join "`n") }
    }

    It 'Compares every frozen request field without depending on JSON property order' {
        $expected = @{ method = 'work.control'; commandId = 'frozen'; params = @{ workId = 'b'; action = 'Hold' }; ifMatch = @(@{ kind = 'Work'; id = 'b'; version = 7 }) }
        $actual = $expected | ConvertTo-Json -Depth 10 | ConvertFrom-Json
        Test-WorkFlowFrozenRequest $expected $actual | Should -BeTrue
        $actual.params.workId = 'a'
        Test-WorkFlowFrozenRequest $expected $actual | Should -BeFalse
        $actual.params.workId = 'b'
        $actual.ifMatch[0].version = 8
        Test-WorkFlowFrozenRequest $expected $actual | Should -BeFalse
    }

    It 'Recognizes wrapped literal preview paths without accepting omitted or altered characters' {
        $rows = @('', 'Project root: C:\owned\long-directory\', 'approved', '', 'Limits: bounded', '')
        Test-WorkFlowPreviewText -Rows $rows -Expected 'C:\owned\long-directory\approved' | Should -BeTrue
        Test-WorkFlowPreviewText -Rows $rows -Expected 'C:\owned\other-directory\approved' | Should -BeFalse
        Test-WorkFlowPreviewText -Rows @('Project root: C:\owned\...', 'approved') -Expected 'C:\owned\long-directory\approved' | Should -BeFalse
        Test-WorkFlowPreviewText -Rows @('Requested authority and', 'execution limits') -Expected 'Requested authority and execution limits' | Should -BeTrue
        Test-WorkFlowPreviewText -Rows @('') -Expected 'Required authority' | Should -BeFalse
        Test-WorkFlowPreviewText -Rows @() -Expected 'Required authority' | Should -BeFalse
    }

    It 'Reads the global composer and requires the exact optional context hint' {
        $frame = "Global conversation`nContext hint: Work A · Harbor Reports`nAssistant reply`nGlobal conversation`n┌Enter send──┐`n│global draft│`n└────────────┘"
        (Get-WorkFlowBodyRows $frame) | Should -Be @('Assistant reply')
        Test-WorkFlowRenderedSelection -Frame $frame -Goal 'Work A' -ProjectName 'Harbor Reports' | Should -BeTrue
        Test-WorkFlowRenderedSelection -Frame $frame -Goal 'Work B' -ProjectName 'Harbor Reports' | Should -BeFalse
        Test-WorkFlowRenderedSelection -Frame ($frame.Replace('Global conversation', 'Work A')) -Goal 'Work A' -ProjectName 'Harbor Reports' | Should -BeFalse
        { Get-WorkFlowSelectedMenuLabel $frame } | Should -Throw '*not a focused card or draft*'
    }

    It 'Returns from work hints through explicit global Home without clearing the selection anchor' {
        $tokens = $null
        $errors = $null
        $suite = Join-Path $PSScriptRoot '..\tests\Feature.AgentCenterWorkFlow.Tests.ps1'
        $ast = [Management.Automation.Language.Parser]::ParseFile($suite, [ref]$tokens, [ref]$errors)
        $errors.Count | Should -Be 0
        $definitions = @($ast.FindAll({
            param($node)
            $node -is [Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -ceq 'Open-WorkFlowWork'
        }, $true))
        $definitions.Count | Should -Be 1
        $commands = @($definitions[0].Body.FindAll({
            param($node)
            $node -is [Management.Automation.Language.CommandAst]
        }, $true))
        @($commands | ForEach-Object { $_.GetCommandName() }) |
            Should -Be @('Select-WorkFlowAction', 'Select-WorkFlowAction', 'Assert-WorkFlowSelection')
        $commands[0].CommandElements[1].Value | Should -BeExactly 'Global conversation'
    }

    Context 'Isolated command receipt oracle' {
        BeforeAll {
            $script:receiptRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot "..\artifacts\receipt-unit-$([guid]::NewGuid().ToString('N'))"))
            $script:reader = Join-Path $PSScriptRoot '..\fixtures\Read-AgentCenterCommandReceipt.cjs'
            $script:commandId = '72e3c2d0-3f7c-4aa8-8444-b9fe0c46d15a'
            New-Item -ItemType Directory -Path (Join-Path $script:receiptRoot 'isolated') -Force | Out-Null
            $script:databaseRoot = Join-Path $script:receiptRoot 'isolated'
            @{ stateRoot = $script:databaseRoot; scenario = 'WorkFlow' } | ConvertTo-Json |
                Set-Content -LiteralPath (Join-Path $script:receiptRoot 'runtime.json') -Encoding utf8NoBOM
            $seedCode = "const {DatabaseSync} = require('node:sqlite'); const db = new DatabaseSync(process.argv[1]);" +
                "db.exec('CREATE TABLE commands(principal TEXT, command_id TEXT, response TEXT)');" +
                "db.prepare('INSERT INTO commands VALUES(?,?,?)').run('human', process.argv[2], JSON.stringify({status:'ok',data:{version:7}}));" +
                "db.prepare('INSERT INTO commands VALUES(?,?,?)').run('coordinator', '00000000-0000-4000-8000-000000000000', JSON.stringify({status:'ok'})); db.close();"
            & node.exe --no-warnings -e $seedCode (Join-Path $script:databaseRoot 'work.db') $script:commandId
            if ($LASTEXITCODE -ne 0) { throw 'The offline receipt oracle requires node:sqlite.' }
        }
        AfterAll { if ($script:receiptRoot) { Remove-Item -LiteralPath $script:receiptRoot -Recurse -Force } }

        It 'Finds exactly the captured human nonce without modifying the database' {
            $before = (Get-FileHash -LiteralPath (Join-Path $script:databaseRoot 'work.db')).Hash
            $receipt = & node.exe --no-warnings $script:reader $script:receiptRoot $script:commandId | ConvertFrom-Json
            $LASTEXITCODE | Should -Be 0
            $receipt.receipt.data.version | Should -Be 7
            (Get-FileHash -LiteralPath (Join-Path $script:databaseRoot 'work.db')).Hash | Should -BeExactly $before
        }

        It 'Does not credit absent or nonhuman command receipts' -TestCases @(
            @{ Id = '00000000-0000-4000-8000-000000000000' }
            @{ Id = '00000000-0000-4000-8000-000000000001' }
        ) {
            param($Id)
            $receipt = & node.exe --no-warnings $script:reader $script:receiptRoot $Id | ConvertFrom-Json
            $LASTEXITCODE | Should -Be 0
            $receipt.receipt | Should -BeNullOrEmpty
        }

        It 'Refuses another fixture scenario before querying a receipt' {
            @{ stateRoot = $script:databaseRoot; scenario = 'NativePaste' } | ConvertTo-Json |
                Set-Content -LiteralPath (Join-Path $script:receiptRoot 'runtime.json') -Encoding utf8NoBOM
            try {
                $output = & node.exe --no-warnings $script:reader $script:receiptRoot $script:commandId 2>&1
                $LASTEXITCODE | Should -Not -Be 0
                ($output -join "`n") | Should -Match 'restricted to this isolated WorkFlow fixture'
            }
            finally {
                @{ stateRoot = $script:databaseRoot; scenario = 'WorkFlow' } | ConvertTo-Json |
                    Set-Content -LiteralPath (Join-Path $script:receiptRoot 'runtime.json') -Encoding utf8NoBOM
            }
        }

        It 'Refuses a database outside the exact evidence directory' {
            $child = Join-Path $script:receiptRoot 'different-evidence'
            New-Item -ItemType Directory -Path $child -Force | Out-Null
            @{ stateRoot = $script:databaseRoot; scenario = 'WorkFlow' } | ConvertTo-Json |
                Set-Content -LiteralPath (Join-Path $child 'runtime.json') -Encoding utf8NoBOM
            $output = & node.exe --no-warnings $script:reader $child $script:commandId 2>&1
            $LASTEXITCODE | Should -Not -Be 0
            ($output -join "`n") | Should -Match 'restricted to this isolated WorkFlow fixture'
        }
    }

    It 'Scopes the authority endpoint to its disposable state root' {
        $first = Get-WorkFlowPipeName 'C:\fixture\one'
        $first | Should -BeExactly (Get-WorkFlowPipeName 'c:\FIXTURE\one')
        $first | Should -Not -Be (Get-WorkFlowPipeName 'C:\fixture\two')
        $first | Should -Match '^IntelligentTerminal-AgentCenter-[0-9a-f]{64}$'
    }

    It 'Frames UTF-8 requests with a little-endian byte length' {
        $stream = [IO.MemoryStream]::new()
        try {
            Write-WorkFlowFrame $stream @{ type = 'request'; marker = '港口' }
            $bytes = $stream.ToArray()
            [BitConverter]::ToUInt32($bytes, 0) | Should -Be ($bytes.Length - 4)
            $stream.Position = 0
            (Read-WorkFlowFrame $stream).marker | Should -BeExactly '港口'
        }
        finally { $stream.Dispose() }
    }

    It 'Rejects malformed frame boundaries without reconnect or replay' -TestCases @(
        @{ Bytes = [byte[]]@(0, 0, 0, 0); Error = '*length is invalid*' }
        @{ Bytes = [byte[]]@(1, 0, 16, 0); Error = '*length is invalid*' }
        @{ Bytes = [byte[]]@(5, 0, 0, 0, 123); Error = '*closed mid-frame*' }
    ) {
        param($Bytes, $Error)
        $stream = [IO.MemoryStream]::new($Bytes)
        try { { Read-WorkFlowFrame $stream } | Should -Throw $Error }
        finally { $stream.Dispose() }
    }

    It 'Initializes global chat metadata without configuring any project or work' {
        Mock New-Item {}
        Mock Write-WorkFlowJsonSnapshot {}
        Mock Invoke-WorkFlowRequest { throw 'Project or work mutation before global chat is forbidden.' }
        $fixture = Initialize-WorkFlowGlobalFixture -Connection @{ Welcome = @{ storeId = 'fixture-store' } } -EvidenceDirectory $PSScriptRoot
        $fixture.storeId | Should -BeExactly 'fixture-store'
        $fixture.works.Count | Should -Be 0
        $fixture.seededThrough.Count | Should -Be 0
        $fixture.global.projectPrompt.Contains($fixture.global.projectRoot) | Should -BeTrue
        Should -Invoke Invoke-WorkFlowRequest -Times 0 -Exactly
    }

    It 'Seeds two distinct draft works through permitted public operations only' {
        $script:requests = [Collections.Generic.List[object]]::new()
        Mock New-Item {}
        Mock Invoke-WorkFlowRequest {
            param($Connection, $Method, $Params, $Mutation)
            $Mutation | Should -BeTrue
            $script:requests.Add(@{ method = $Method; params = $Params })
            @{ data = @{ projectId = [guid]::NewGuid().ToString(); workId = [guid]::NewGuid().ToString() } }
        }
        $fixture = Initialize-WorkFlowFixture -Connection @{ Welcome = @{ storeId = 'fixture-store' } } -EvidenceDirectory $PSScriptRoot
        $fixture.works.Count | Should -Be 2
        $fixture.works[0].projectName | Should -BeExactly 'Harbor Reports'
        $fixture.works[1].projectName | Should -BeExactly 'Orchard Notes'
        $fixture.works[0].id | Should -Not -Be $fixture.works[1].id
        $script:requests.method | Should -Be @('project.configure', 'work.create_draft', 'project.configure', 'work.create_draft')
        $script:requests[1].params.goal | Should -BeExactly $fixture.works[0].goal
        $script:requests[3].params.goal | Should -BeExactly $fixture.works[1].goal
        $fixture.safety | Should -Match 'no work.start'
        $script:requests[0].params.capabilityIds | Should -BeNullOrEmpty
        $script:requests[2].params.capabilityIds | Should -BeNullOrEmpty
    }

    It 'Requires a correlated response and never resends a mutation' {
        Mock Write-WorkFlowFrame {}
        Mock Read-WorkFlowFrame { @{ type = 'response'; requestId = 'foreign'; status = 'ok' } }
        { Invoke-WorkFlowRequest -Connection @{ Pipe = [IO.MemoryStream]::new() } -Method 'work.control' -Mutation } |
            Should -Throw '*uncorrelated response*'
        Should -Invoke Write-WorkFlowFrame -Times 1 -Exactly
    }

    It 'Binds the approved intake adapter only to the Harbor coordinator' {
        $script:requests = [Collections.Generic.List[object]]::new()
        Mock New-Item {}
        Mock Write-WorkFlowJsonSnapshot {}
        Mock Invoke-WorkFlowRequest {
            param($Connection, $Method, $Params)
            $script:requests.Add(@{ method = $Method; params = $Params })
            @{ data = @{ projectId = [guid]::NewGuid().ToString(); workId = [guid]::NewGuid().ToString() } }
        }
        Initialize-WorkFlowFixture -Connection @{ Welcome = @{ storeId = 'fixture-store' } } -EvidenceDirectory $PSScriptRoot -EnableIntake | Out-Null
        $script:requests[0].params.coordinatorCapabilityId | Should -BeExactly 'ite2e-local-intake'
        $script:requests[0].params.workerCapabilityId | Should -BeExactly 'ite2e-never-run'
        $script:requests[2].params.coordinatorCapabilityId | Should -BeExactly 'ite2e-never-run'
        $script:requests[0].params.limits.coordinationTurns | Should -Be 3
    }

    It 'Explicitly approves global conversation only on the owned seeded journey projects' {
        $script:requests = [Collections.Generic.List[object]]::new()
        Mock New-Item {}
        Mock Write-WorkFlowJsonSnapshot {}
        Mock Invoke-WorkFlowRequest {
            param($Connection, $Method, $Params, $Mutation)
            $Mutation | Should -BeTrue
            $script:requests.Add(@{ method = $Method; params = $Params })
            @{ data = @{ projectId = [guid]::NewGuid().ToString(); workId = [guid]::NewGuid().ToString() } }
        }
        $fixture = Initialize-WorkFlowFixture -Connection @{ Welcome = @{ storeId = 'fixture-store' } } -EvidenceDirectory $PSScriptRoot -EnableIntake
        $projects = @($script:requests | Where-Object method -CEQ 'project.configure')
        $projects.params.name | Should -Be @('Harbor Reports', 'Orchard Notes', 'Horizon Decisions')
        foreach ($project in $projects) {
            $project.params.capabilityIds | Should -Be @('ite2e-local-global')
            $project.params.root.StartsWith($PSScriptRoot + '\', [StringComparison]::Ordinal) | Should -BeTrue
        }
        $projects.params.coordinatorCapabilityId | Should -Be @('ite2e-local-intake', 'ite2e-never-run', 'ite2e-local-decision')
        $projects.params.workerCapabilityId | Should -Be @('ite2e-never-run', 'ite2e-never-run', 'ite2e-local-decision')
        $script:requests.method | Should -Not -Contain 'work.start'
        $fixture.works.Count | Should -Be 2
    }

    It 'Keeps query envelopes free of command identities' {
        $script:sent = $null
        Mock Write-WorkFlowFrame { param($Stream, $Frame) $script:sent = $Frame }
        Mock Read-WorkFlowFrame { @{ type = 'response'; requestId = $script:sent.requestId; status = 'ok' } }
        Invoke-WorkFlowRequest -Connection @{ Pipe = [IO.MemoryStream]::new() } -Method 'work.get' | Out-Null
        $script:sent.ContainsKey('commandId') | Should -BeFalse
        $script:sent.ifMatch.Count | Should -Be 0
    }

    It 'Preserves a recorded pending mutation without declaring its operation complete or replaying it' {
        $script:sent = $null
        Mock Write-WorkFlowFrame { param($Stream, $Frame) $script:sent = $Frame }
        Mock Read-WorkFlowFrame { @{ type = 'response'; requestId = $script:sent.requestId; status = 'pending'; operationId = 'recorded-operation'; data = @{ lifecycle = 'Active' } } }
        $response = Invoke-WorkFlowRequest -Connection @{ Pipe = [IO.MemoryStream]::new() } -Method 'work.start' -Mutation
        $response.status | Should -BeExactly 'pending'
        $response.operationId | Should -BeExactly 'recorded-operation'
        Should -Invoke Write-WorkFlowFrame -Times 1 -Exactly
    }

    It 'Rejects a pending mutation that has no recorded operation identity' {
        $script:sent = $null
        Mock Write-WorkFlowFrame { param($Stream, $Frame) $script:sent = $Frame }
        Mock Read-WorkFlowFrame { @{ type = 'response'; requestId = $script:sent.requestId; status = 'pending' } }
        { Invoke-WorkFlowRequest -Connection @{ Pipe = [IO.MemoryStream]::new() } -Method 'work.start' -Mutation } |
            Should -Throw '*identify its recorded operation*'
    }

    It 'Unwraps the exact operation record without confusing transport success with completion' -TestCases @(
        @{ State = 'Pending' }
        @{ State = 'Running' }
        @{ State = 'Succeeded' }
        @{ State = 'Failed' }
    ) {
        param($State)
        $response = @{ status = 'ok'; data = @{ operation = @{ id = 'recorded-operation'; kind = 'Operation'; status = $State }; subjects = @() } }
        $operation = Get-WorkFlowObservedOperation -Response $response -OperationId 'recorded-operation'
        $operation.status | Should -BeExactly $State
        $operation.id | Should -BeExactly 'recorded-operation'
    }

    It 'Rejects flattened or foreign operation views instead of treating null as a terminal status' {
        { Get-WorkFlowObservedOperation -Response @{ status = 'ok'; data = @{ status = 'Succeeded' } } -OperationId 'recorded-operation' } |
            Should -Throw '*exact recorded operation view*'
        $response = @{ status = 'ok'; data = @{ operation = @{ id = 'other-operation'; kind = 'Operation'; status = 'Succeeded' } } }
        { Get-WorkFlowObservedOperation -Response $response -OperationId 'recorded-operation' } | Should -Throw '*exact recorded operation view*'
    }

    It 'Round-trips Unicode observer JSON through an OEM-encoded redirected PowerShell process' {
        . (Join-Path $PSScriptRoot '..\ItE2E\Private\Core.ps1')
        $expected = 'harbor α · 港口 👩'
        $encoded = [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($expected))
        $helper = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..\tests\helpers\AgentCenterWorkFlow.ps1')).Replace("'", "''")
        $code = "[Console]::OutputEncoding=[Text.Encoding]::GetEncoding(437); . '$helper'; " +
            "`$value=[Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('$encoded')); " +
            'ConvertTo-WorkFlowObserverJson -Value @{note=$value; encoding=[Console]::OutputEncoding.CodePage}'
        $result = Invoke-Native -FilePath 'pwsh.exe' -Arguments @('-NoProfile', '-Command', $code) -TimeoutSec 10
        $result.ExitCode | Should -Be 0
        @($result.StdOut.ToCharArray() | Where-Object { [int]$_ -gt 127 }).Count | Should -Be 0
        $result.StdOut | Should -Match '\\u03b1'
        $observed = $result.StdOut | ConvertFrom-Json
        $observed.encoding | Should -Be 437
        $observed.note | Should -BeExactly $expected
    }
}

Describe 'Agent Center producer snapshot contracts' -Tag 'Unit' {
    BeforeAll {
        $script:progressRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot "..\artifacts\producer-unit-$([guid]::NewGuid().ToString('N'))"))
        New-Item -ItemType Directory -Path $script:progressRoot | Out-Null
    }
    BeforeEach {
        Get-ChildItem -LiteralPath $script:progressRoot -File | Remove-Item -Force
    }
    AfterAll { Remove-Item -LiteralPath $script:progressRoot -Recurse -Force }

    It 'Waits through an absent progress snapshot without inventing a zero receipt count' {
        Read-WorkFlowLoadProgress -Root $script:progressRoot -Process ([pscustomobject]@{ HasExited = $false }) |
            Should -BeNullOrEmpty
    }

    It 'Accepts only newly recorded global source files and rejects ambiguous arrivals' {
        $record = @{ conversationId = 'conversation'; source = @{ id = 'first'; role = 'human'; conversationId = 'conversation'
            context = @{ scope = 'Global'; consoleSessionId = 'console' } } }
        Write-WorkFlowJsonSnapshot -Path (Join-Path $script:progressRoot 'global-status-first.json') -Value $record
        Read-WorkFlowNewGlobalStatus -Root $script:progressRoot -ExcludedFiles @('global-status-first.json') | Should -BeNullOrEmpty
        $record.source.id = 'second'
        Write-WorkFlowJsonSnapshot -Path (Join-Path $script:progressRoot 'global-status-second.json') -Value $record
        (Read-WorkFlowNewGlobalStatus -Root $script:progressRoot -ExcludedFiles @('global-status-first.json')).source.id | Should -BeExactly 'second'
        { Read-WorkFlowNewGlobalStatus -Root $script:progressRoot } | Should -Throw '*More than one new global status source*'
    }

    It 'Rejects global status evidence with <Defect>' -TestCases @(
        @{ Defect = 'a different filename identity' }; @{ Defect = 'an assistant source' }
        @{ Defect = 'a project scope' }; @{ Defect = 'a foreign conversation' }; @{ Defect = 'no console identity' }
    ) {
        param($Defect)
        $record = @{ conversationId = 'conversation'; source = @{ id = 'source'; role = 'human'; conversationId = 'conversation'
            context = @{ scope = 'Global'; consoleSessionId = 'console' } } }
        switch ($Defect) {
            'a different filename identity' { $record.source.id = 'other' }
            'an assistant source' { $record.source.role = 'assistant' }
            'a project scope' { $record.source.context.scope = 'Project' }
            'a foreign conversation' { $record.source.conversationId = 'other' }
            'no console identity' { $record.source.context.consoleSessionId = $null }
        }
        Write-WorkFlowJsonSnapshot -Path (Join-Path $script:progressRoot 'global-status-source.json') -Value $record
        { Read-WorkFlowNewGlobalStatus -Root $script:progressRoot } | Should -Throw '*exact captured human source*'
    }

    It 'Returns the same successfully parsed snapshot instead of testing existence then reopening it' {
        @{ completed = 15; version = 10; storeId = 'fixture-store' } | ConvertTo-Json |
            Set-Content (Join-Path $script:progressRoot 'load-progress.json')
        $snapshot = Read-WorkFlowLoadProgress -Root $script:progressRoot -Process ([pscustomobject]@{ HasExited = $false })
        Remove-Item (Join-Path $script:progressRoot 'load-progress.json')
        $snapshot.completed | Should -Be 15
        $snapshot.version | Should -Be 10
    }

    It 'Replaces producer snapshots while an existing delete-shared reader remains valid' {
        $file = Join-Path $script:progressRoot 'load-progress.json'
        Write-WorkFlowJsonSnapshot -Path $file -Value @{ completed = 15; version = 10; storeId = 'fixture-store' }
        $stream = [IO.File]::Open($file, [IO.FileMode]::Open, [IO.FileAccess]::Read,
            [IO.FileShare]::ReadWrite -bor [IO.FileShare]::Delete)
        try {
            Write-WorkFlowJsonSnapshot -Path $file -Value @{ completed = 16; version = 11; storeId = 'fixture-store' }
            (Read-WorkFlowJsonSnapshot -Path $file).completed | Should -Be 16
            $reader = [IO.StreamReader]::new($stream)
            try { ($reader.ReadToEnd() | ConvertFrom-Json).completed | Should -Be 15 }
            finally { $reader.Dispose() }
        }
        finally { $stream.Dispose() }
    }

    It 'Surfaces an actual producer failure before accepting its previous progress' {
        @{ completed = 15; version = 10; storeId = 'fixture-store' } | ConvertTo-Json |
            Set-Content (Join-Path $script:progressRoot 'load-progress.json')
        @{ failed = $true; error = 'Actual STALE_VERSION receipt' } | ConvertTo-Json |
            Set-Content (Join-Path $script:progressRoot 'load-result.json')
        $status = Read-WorkFlowLoadProgress -Root $script:progressRoot -Process ([pscustomobject]@{ HasExited = $false })
        $status.failed | Should -BeTrue
        $status.error | Should -BeExactly 'Actual STALE_VERSION receipt'
    }

    It 'Reports an early process exit rather than accepting a missing startup record' {
        $status = Read-WorkFlowLoadProgress -Root $script:progressRoot -Process ([pscustomobject]@{ HasExited = $true })
        $status.failed | Should -BeTrue
        $status.error | Should -Match 'load.stderr.txt'
    }

    It 'Rejects an exited producer even when its last successful snapshot meets the sustained thresholds' {
        Write-WorkFlowJsonSnapshot -Path (Join-Path $script:progressRoot 'load-progress.json') -Value @{
            completed = 900; version = 901; storeId = 'fixture-store'; sustained = $true; receiptSpanSeconds = 181
        }
        Write-WorkFlowJsonSnapshot -Path (Join-Path $script:progressRoot 'load-result.json') -Value @{ completed = 900; failed = $false }
        $status = Read-WorkFlowLoadProgress -Root $script:progressRoot -Process ([pscustomobject]@{ HasExited = $true })
        $status.failed | Should -BeTrue
    }

    It 'Cooperatively settles the owned producer before another test can mutate its works' {
        $process = [pscustomobject]@{ HasExited = $false }
        $process | Add-Member -MemberType ScriptMethod -Name WaitForExit -Value {
            param($Timeout)
            $Timeout | Should -Be 20000
            $this.HasExited = $true
            return $true
        }
        Stop-WorkFlowLoadProducer -Context @{ Root = $script:progressRoot; LoadProcess = $process }
        $process.HasExited | Should -BeTrue
        Test-Path (Join-Path $script:progressRoot 'stop-load') | Should -BeTrue
    }
}

Describe 'Agent Center real snapshot sharing contracts' -Tag 'Unit' {
    BeforeAll {
        $script:sharingRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot "..\artifacts\sharing-unit-$([guid]::NewGuid().ToString('N'))"))
        New-Item -ItemType Directory -Path $script:sharingRoot | Out-Null
    }
    BeforeEach {
        Get-ChildItem -LiteralPath $script:sharingRoot -File | Remove-Item -Force
        $script:sharingPath = Join-Path $script:sharingRoot 'load-progress.json'
        Write-WorkFlowJsonSnapshot -Path $script:sharingPath -Value @{
            completed = 640; version = 641; storeId = 'fixture-store'; sustained = $true; receiptSpanSeconds = 113
        }
        $script:sharingHold = [IO.File]::Open($script:sharingPath, [IO.FileMode]::Open, [IO.FileAccess]::ReadWrite, [IO.FileShare]::None)
        $script:sharingClock = @{ Elapsed = @{ TotalSeconds = 112.0 } }
        $script:sharingMonitor = @{
            Clock = $script:sharingClock; LastCompleted = 636; LastAdvanceSeconds = 111
            LastReceiptSpanSeconds = 111; LastRecordedSeconds = 111
        }
        $script:sharingContext = @{
            Root = $script:sharingRoot; LoadProcess = @{ HasExited = $false }; Fixture = @{ storeId = 'fixture-store' }
        }
        $script:sharingReleaseAt = -1
        Mock Write-Warning {}
        Mock Start-Sleep {
            $script:sharingClock.Elapsed.TotalSeconds++
            if ($script:sharingClock.Elapsed.TotalSeconds -eq $script:sharingReleaseAt) {
                $script:sharingHold.Dispose()
                $script:sharingHold = $null
            }
        }
    }
    AfterEach {
        if ($script:sharingHold) { $script:sharingHold.Dispose(); $script:sharingHold = $null }
    }
    AfterAll { Remove-Item -LiteralPath $script:sharingRoot -Recurse -Force }

    It 'Recognizes the real wrapped sharing HRESULT and reads the same file after its handle is released' {
        $failure = $null
        try {
            $unexpected = [IO.File]::Open($script:sharingPath, [IO.FileMode]::Open, [IO.FileAccess]::Read,
                [IO.FileShare]::ReadWrite -bor [IO.FileShare]::Delete)
            $unexpected.Dispose()
        }
        catch [Management.Automation.MethodInvocationException] { $failure = $_ }
        $failure | Should -Not -BeNullOrEmpty
        $failure.Exception | Should -BeOfType ([Management.Automation.MethodInvocationException])
        $failure.Exception.GetBaseException() | Should -BeOfType ([IO.IOException])
        $failure.Exception.GetBaseException().HResult | Should -Be -2147024864
        Read-WorkFlowJsonSnapshot -Path $script:sharingPath | Should -BeNullOrEmpty
        Should -Invoke Write-Warning -Times 1 -Exactly -ParameterFilter { $Message -like '*ERROR_SHARING_VIOLATION*load-progress.json' }
        $script:sharingHold.Dispose()
        $script:sharingHold = $null
        (Read-WorkFlowJsonSnapshot -Path $script:sharingPath).completed | Should -Be 640
    }

    It 'Preserves established clocks and receipts across a real transient exclusive handle' {
        $originalClock = $script:sharingMonitor.Clock
        $script:sharingReleaseAt = 113
        $result = Wait-WorkFlowLoadCheckpoint -Context $script:sharingContext -Monitor $script:sharingMonitor -TargetSeconds 113 -MinimumUpdates 640
        $result.completed | Should -Be 640
        $script:sharingClock.Elapsed.TotalSeconds | Should -Be 113
        $script:sharingMonitor.LastAdvanceSeconds | Should -Be 113
        [object]::ReferenceEquals($originalClock, $script:sharingMonitor.Clock) | Should -BeTrue
        Should -Invoke Start-Sleep -Times 1 -Exactly
        Should -Invoke Write-Warning -Times 1 -Exactly
    }

    It 'Observes the first real receipt before the original startup deadline after a handle release' {
        $script:sharingClock.Elapsed.TotalSeconds = 13
        $script:sharingMonitor.LastCompleted = 0
        $script:sharingMonitor.LastAdvanceSeconds = 0
        $script:sharingMonitor.LastReceiptSpanSeconds = 0
        $script:sharingReleaseAt = 14
        (Wait-WorkFlowLoadCheckpoint -Context $script:sharingContext -Monitor $script:sharingMonitor).completed | Should -Be 640
        $script:sharingClock.Elapsed.TotalSeconds | Should -Be 14
        $script:sharingMonitor.LastAdvanceSeconds | Should -Be 14
        Should -Invoke Start-Sleep -Times 1 -Exactly
    }

    It 'Retains the fifteen-second first-receipt deadline while a real file remains exclusively held' {
        $script:sharingClock.Elapsed.TotalSeconds = 13
        $script:sharingMonitor.LastCompleted = 0
        $script:sharingMonitor.LastAdvanceSeconds = 0
        $script:sharingMonitor.LastReceiptSpanSeconds = 0
        { Wait-WorkFlowLoadCheckpoint -Context $script:sharingContext -Monitor $script:sharingMonitor } |
            Should -Throw '*first committed update within fifteen seconds*'
        $script:sharingClock.Elapsed.TotalSeconds | Should -Be 15
        $script:sharingMonitor.LastCompleted | Should -Be 0
        $script:sharingMonitor.LastAdvanceSeconds | Should -Be 0
        Should -Invoke Start-Sleep -Times 2 -Exactly
    }

    It 'Retains the exact established stall deadline without treating a held file as zero progress' {
        $script:sharingMonitor.LastAdvanceSeconds = 100
        { Wait-WorkFlowLoadCheckpoint -Context $script:sharingContext -Monitor $script:sharingMonitor -TargetSeconds 180 } |
            Should -Throw '*no committed progress for fifteen seconds*'
        $script:sharingClock.Elapsed.TotalSeconds | Should -Be 115
        $script:sharingMonitor.LastCompleted | Should -Be 636
        $script:sharingMonitor.LastReceiptSpanSeconds | Should -Be 111
        $script:sharingMonitor.LastAdvanceSeconds | Should -Be 100
        Should -Invoke Start-Sleep -Times 3 -Exactly
    }

    It 'Retains the total 240-second budget across a real sharing violation' {
        $script:sharingClock.Elapsed.TotalSeconds = 239
        $script:sharingMonitor.LastAdvanceSeconds = 238
        { Wait-WorkFlowLoadCheckpoint -Context $script:sharingContext -Monitor $script:sharingMonitor -TargetSeconds 180 } |
            Should -Throw '*240-second safety budget*'
        $script:sharingClock.Elapsed.TotalSeconds | Should -Be 240
        $script:sharingMonitor.LastAdvanceSeconds | Should -Be 238
        Should -Invoke Start-Sleep -Times 1 -Exactly
    }

    It 'Surfaces an actual producer error even when the progress file is exclusively held' {
        Write-WorkFlowJsonSnapshot -Path (Join-Path $script:sharingRoot 'load-result.json') -Value @{
            failed = $true; error = 'Actual guarded update rejected'
        }
        { Wait-WorkFlowLoadCheckpoint -Context $script:sharingContext -Monitor $script:sharingMonitor } |
            Should -Throw '*Actual guarded update rejected*'
        Should -Invoke Start-Sleep -Times 0 -Exactly
        Should -Invoke Write-Warning -Times 0 -Exactly
    }

    It 'Surfaces producer exit before retrying its held progress file' {
        $script:sharingContext.LoadProcess.HasExited = $true
        { Wait-WorkFlowLoadCheckpoint -Context $script:sharingContext -Monitor $script:sharingMonitor } |
            Should -Throw '*producer exited before loaded input*'
        Should -Invoke Start-Sleep -Times 0 -Exactly
        Should -Invoke Write-Warning -Times 0 -Exactly
    }

    It 'Propagates <Failure> rather than classifying every read error as transient' -TestCases @(
        @{ Failure = 'malformed JSON' }; @{ Failure = 'a missing parent directory' }; @{ Failure = 'a directory opened as a file' }
    ) {
        param($Failure)
        $path = switch ($Failure) {
            'malformed JSON' {
                $invalid = Join-Path $script:sharingRoot 'invalid.json'
                '{"completed":' | Set-Content -LiteralPath $invalid
                $invalid
            }
            'a missing parent directory' { Join-Path $script:sharingRoot 'absent\progress.json' }
            'a directory opened as a file' { $script:sharingRoot }
        }
        { Read-WorkFlowJsonSnapshot -Path $path } | Should -Throw
        Should -Invoke Write-Warning -Times 0 -Exactly
    }
}

Describe 'Agent Center sustained pressure contracts' -Tag 'Unit' {
    BeforeAll {
        $script:checkpointRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot "..\artifacts\checkpoint-unit-$([guid]::NewGuid().ToString('N'))"))
        New-Item -ItemType Directory -Path $script:checkpointRoot | Out-Null
    }
    BeforeEach {
        Get-ChildItem -LiteralPath $script:checkpointRoot -File | Remove-Item -Force
        $script:checkpointClock = @{ Elapsed = @{ TotalSeconds = 0.0 } }
        $script:checkpointMonitor = @{
            Clock = $script:checkpointClock; LastCompleted = 0; LastAdvanceSeconds = 0
            LastRecordedSeconds = -1; LastReceiptSpanSeconds = 0
        }
        $script:checkpointContext = @{
            Root = $script:checkpointRoot; LoadProcess = @{ HasExited = $false }; Fixture = @{ storeId = 'fixture-store' }
        }
        $script:checkpointProgress = @{
            completed = 1; version = 2; storeId = 'fixture-store'; sustained = $true; receiptSpanSeconds = 0.0
        }
        $script:advanceReceipts = $true
        Mock Read-WorkFlowLoadProgress { return $script:checkpointProgress }
        Mock Start-Sleep {
            $script:checkpointClock.Elapsed.TotalSeconds++
            if ($script:advanceReceipts -and $script:checkpointProgress) {
                $script:checkpointProgress.completed++
                $script:checkpointProgress.receiptSpanSeconds++
            }
        }
    }
    AfterAll { Remove-Item -LiteralPath $script:checkpointRoot -Recurse -Force }

    It 'Preserves four early human switches and even pairs at both sustained checkpoints and after producer exit' {
        $plan = @(Get-WorkFlowSustainedSwitchPlan)
        $plan.Count | Should -Be 4
        ($plan[0].indices -join ',') | Should -BeExactly '0,1,0,1'
        $plan[1].targetSeconds | Should -Be 60
        $plan[2].targetSeconds | Should -Be 130
        foreach ($stage in $plan[1..3]) { ($stage.indices -join ',') | Should -BeExactly '0,1' }
        @($plan | Where-Object afterStop).Count | Should -Be 1
        $plan[3].afterStop | Should -BeTrue
        $plan[3].targetSeconds | Should -BeGreaterOrEqual 180
        $plan[3].minimumUpdates | Should -Be 784
    }

    It 'Does not end sustained production at the old finite update cap or the minimum time' {
        Test-WorkFlowLoadContinues -Sustained $true -Completed 4096 -Count 1024 -ElapsedSeconds 181 |
            Should -BeTrue
        Test-WorkFlowLoadContinues -Sustained $false -Completed 1024 -Count 1024 -ElapsedSeconds 10 |
            Should -BeFalse
        Test-WorkFlowLoadContinues -Sustained $false -Completed 1023 -Count 1024 -ElapsedSeconds 10 |
            Should -BeTrue
    }

    It 'Honors the cooperative exit signal but fails an unattended producer at its hard safety deadline' {
        Test-WorkFlowLoadContinues -Sustained $true -Completed 900 -Count 1024 -ElapsedSeconds 181 -StopRequested $true |
            Should -BeFalse
        { Test-WorkFlowLoadContinues -Sustained $true -Completed 900 -Count 1024 -ElapsedSeconds 240 } |
            Should -Throw '*240-second safety budget*'
    }

    It 'Requires both receipt duration and successful count rather than just <Constraint>' -TestCases @(
        @{ Constraint = 'enough updates'; Completed = 900; Span = 179; Waits = 1 }
        @{ Constraint = 'enough elapsed receipt time'; Completed = 782; Span = 181; Waits = 2 }
        @{ Constraint = 'nearly complete thresholds'; Completed = 783; Span = 179; Waits = 1 }
    ) {
        param($Completed, $Span, $Waits)
        $script:checkpointProgress.completed = $Completed
        $script:checkpointProgress.receiptSpanSeconds = $Span
        $result = Wait-WorkFlowLoadCheckpoint -Context $script:checkpointContext -Monitor $script:checkpointMonitor `
            -TargetSeconds 180 -MinimumUpdates 784
        $result.completed | Should -BeGreaterOrEqual 784
        $result.receiptSpanSeconds | Should -BeGreaterOrEqual 180
        $script:checkpointClock.Elapsed.TotalSeconds | Should -Be $Waits
        Should -Invoke Read-WorkFlowLoadProgress -Times ($Waits + 1) -Exactly
        @(Get-Content (Join-Path $script:checkpointRoot 'load-observations.jsonl') | ConvertFrom-Json).Count |
            Should -Be ($Waits + 1)
    }

    It 'Cannot pad a frozen receipt span with idle waiting' {
        $script:checkpointProgress.completed = 900
        $script:checkpointProgress.receiptSpanSeconds = 179
        $script:advanceReceipts = $false
        { Wait-WorkFlowLoadCheckpoint -Context $script:checkpointContext -Monitor $script:checkpointMonitor -TargetSeconds 180 -MinimumUpdates 784 } |
            Should -Throw '*no committed progress for fifteen seconds*'
        $script:checkpointClock.Elapsed.TotalSeconds | Should -Be 15
    }

    It 'Fails explicitly when startup never publishes a real receipt' {
        $script:checkpointProgress = $null
        { Wait-WorkFlowLoadCheckpoint -Context $script:checkpointContext -Monitor $script:checkpointMonitor } |
            Should -Throw '*first committed update within fifteen seconds*'
    }

    It 'Retains established receipt history and clocks across a transient absent snapshot after one hundred seconds' {
        $script:checkpointClock.Elapsed.TotalSeconds = 112
        $script:checkpointMonitor.LastCompleted = 636
        $script:checkpointMonitor.LastReceiptSpanSeconds = 111
        $script:checkpointMonitor.LastAdvanceSeconds = 111
        $originalClock = $script:checkpointMonitor.Clock
        $script:checkpointProgress = $null
        Mock Start-Sleep {
            $script:checkpointMonitor.LastCompleted | Should -Be 636
            $script:checkpointMonitor.LastReceiptSpanSeconds | Should -Be 111
            $script:checkpointMonitor.LastAdvanceSeconds | Should -Be 111
            $script:checkpointClock.Elapsed.TotalSeconds++
            $script:checkpointProgress = @{
                completed = 640; version = 641; storeId = 'fixture-store'; sustained = $true; receiptSpanSeconds = 113
            }
        }
        $result = Wait-WorkFlowLoadCheckpoint -Context $script:checkpointContext -Monitor $script:checkpointMonitor -TargetSeconds 113
        $result.completed | Should -Be 640
        [object]::ReferenceEquals($originalClock, $script:checkpointMonitor.Clock) | Should -BeTrue
        $script:checkpointClock.Elapsed.TotalSeconds | Should -Be 113
        $script:checkpointMonitor.LastAdvanceSeconds | Should -Be 113
        Should -Invoke Start-Sleep -Times 1 -Exactly
    }

    It 'Uses the last committed advance for the exact stall deadline while established snapshots remain absent' {
        $script:checkpointClock.Elapsed.TotalSeconds = 112
        $script:checkpointMonitor.LastCompleted = 636
        $script:checkpointMonitor.LastReceiptSpanSeconds = 100
        $script:checkpointMonitor.LastAdvanceSeconds = 100
        $script:checkpointProgress = $null
        { Wait-WorkFlowLoadCheckpoint -Context $script:checkpointContext -Monitor $script:checkpointMonitor -TargetSeconds 180 } |
            Should -Throw '*no committed progress for fifteen seconds*'
        $script:checkpointClock.Elapsed.TotalSeconds | Should -Be 115
        $script:checkpointMonitor.LastCompleted | Should -Be 636
        $script:checkpointMonitor.LastAdvanceSeconds | Should -Be 100
        Should -Invoke Start-Sleep -Times 3 -Exactly
    }

    It 'Does not count the same recovered snapshot as a new advance after an absent read' {
        $script:checkpointClock.Elapsed.TotalSeconds = 112
        $script:checkpointMonitor.LastCompleted = 636
        $script:checkpointMonitor.LastReceiptSpanSeconds = 100
        $script:checkpointMonitor.LastAdvanceSeconds = 100
        $script:checkpointProgress = $null
        Mock Start-Sleep {
            $script:checkpointClock.Elapsed.TotalSeconds++
            $script:checkpointProgress = @{
                completed = 636; version = 637; storeId = 'fixture-store'; sustained = $true; receiptSpanSeconds = 100
            }
        }
        { Wait-WorkFlowLoadCheckpoint -Context $script:checkpointContext -Monitor $script:checkpointMonitor -TargetSeconds 180 } |
            Should -Throw '*no committed progress for fifteen seconds*'
        $script:checkpointClock.Elapsed.TotalSeconds | Should -Be 115
        $script:checkpointMonitor.LastAdvanceSeconds | Should -Be 100
    }

    It 'Accepts the first real receipt before the original startup deadline without resetting the total clock' {
        $script:checkpointClock.Elapsed.TotalSeconds = 13
        $script:checkpointProgress = $null
        Mock Start-Sleep {
            $script:checkpointClock.Elapsed.TotalSeconds++
            $script:checkpointProgress = @{
                completed = 1; version = 2; storeId = 'fixture-store'; sustained = $true; receiptSpanSeconds = 0
            }
        }
        $result = Wait-WorkFlowLoadCheckpoint -Context $script:checkpointContext -Monitor $script:checkpointMonitor
        $result.completed | Should -Be 1
        $script:checkpointClock.Elapsed.TotalSeconds | Should -Be 14
        $script:checkpointMonitor.LastAdvanceSeconds | Should -Be 14
    }

    It 'Preserves the global deadline when established snapshots disappear near the safety budget' {
        $script:checkpointClock.Elapsed.TotalSeconds = 239
        $script:checkpointMonitor.LastCompleted = 1000
        $script:checkpointMonitor.LastReceiptSpanSeconds = 238
        $script:checkpointMonitor.LastAdvanceSeconds = 238
        $script:checkpointProgress = $null
        { Wait-WorkFlowLoadCheckpoint -Context $script:checkpointContext -Monitor $script:checkpointMonitor -MinimumUpdates 1001 } |
            Should -Throw '*240-second safety budget*'
        $script:checkpointClock.Elapsed.TotalSeconds | Should -Be 240
        $script:checkpointMonitor.LastAdvanceSeconds | Should -Be 238
        Should -Invoke Start-Sleep -Times 1 -Exactly
    }

    It 'Propagates an established producer error after an absent snapshot without extending either deadline' {
        $script:checkpointClock.Elapsed.TotalSeconds = 112
        $script:checkpointMonitor.LastCompleted = 636
        $script:checkpointMonitor.LastAdvanceSeconds = 111
        $script:checkpointProgress = $null
        Mock Start-Sleep {
            $script:checkpointClock.Elapsed.TotalSeconds++
            $script:checkpointProgress = @{ failed = $true; error = 'Actual producer pipe closed after receipt 636' }
        }
        { Wait-WorkFlowLoadCheckpoint -Context $script:checkpointContext -Monitor $script:checkpointMonitor -TargetSeconds 180 } |
            Should -Throw '*Actual producer pipe closed after receipt 636*'
        $script:checkpointClock.Elapsed.TotalSeconds | Should -Be 113
        $script:checkpointMonitor.LastAdvanceSeconds | Should -Be 111
    }

    It 'Propagates the actual producer error without waiting through it' {
        $script:checkpointProgress = @{ failed = $true; error = 'Actual guarded update failed: STALE_VERSION' }
        { Wait-WorkFlowLoadCheckpoint -Context $script:checkpointContext -Monitor $script:checkpointMonitor } |
            Should -Throw '*Actual guarded update failed: STALE_VERSION*'
        Should -Invoke Start-Sleep -Times 0 -Exactly
    }

    It 'Rejects a changed sustained authority or regressed receipt history' {
        $script:checkpointProgress.storeId = 'foreign-store'
        { Wait-WorkFlowLoadCheckpoint -Context $script:checkpointContext -Monitor $script:checkpointMonitor } |
            Should -Throw '*changed authority, mode, or receipt history*'
        $script:checkpointProgress.storeId = 'fixture-store'
        $script:checkpointMonitor.LastCompleted = 2
        { Wait-WorkFlowLoadCheckpoint -Context $script:checkpointContext -Monitor $script:checkpointMonitor } |
            Should -Throw '*changed authority, mode, or receipt history*'
        $script:checkpointMonitor.LastCompleted = 0
        $script:checkpointMonitor.LastReceiptSpanSeconds = 1
        { Wait-WorkFlowLoadCheckpoint -Context $script:checkpointContext -Monitor $script:checkpointMonitor } |
            Should -Throw '*changed authority, mode, or receipt history*'
    }

    It 'Bounds the observer independently of a still-running child' {
        $script:checkpointClock.Elapsed.TotalSeconds = 240
        { Wait-WorkFlowLoadCheckpoint -Context $script:checkpointContext -Monitor $script:checkpointMonitor -TargetSeconds 180 -MinimumUpdates 784 } |
            Should -Throw '*240-second safety budget*'
        Should -Invoke Read-WorkFlowLoadProgress -Times 0 -Exactly
    }
}

Describe 'Agent Center diagnostic navigation evidence' -Tag 'Unit' {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        . (Join-Path $PSScriptRoot '..\ItE2E\Public\Ui.ps1')
        . (Join-Path $PSScriptRoot '..\ItE2E\Public\Wt.ps1')
        $script:navigationRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot ('..\artifacts\navigation-unit-' + [guid]::NewGuid().ToString('N'))))
        New-Item -ItemType Directory -Path $script:navigationRoot | Out-Null
        $script:priorNavigationLog = $env:WTA_LOG
        $script:navigationObserver = ${function:Get-WorkFlowNavigationObservation}
        function Invoke-OfflineNavigationUi { $script:observedUiLog = $env:WTA_LOG }
        $suite = Join-Path $PSScriptRoot '..\tests\Feature.AgentCenterWorkFlow.Tests.ps1'
        $tokens = $null; $parseErrors = $null
        $ast = [Management.Automation.Language.Parser]::ParseFile($suite, [ref]$tokens, [ref]$parseErrors)
        if ($parseErrors.Count) { throw ($parseErrors -join '; ') }
        foreach ($name in @('Send-WorkFlowKey', 'Read-WorkFlowUi')) {
            $definition = $ast.Find({
                param($node)
                $node -is [Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -ceq $name
            }, $true)
            . ([scriptblock]::Create($definition.Extent.Text))
        }
    }
    BeforeEach {
        $root = Join-Path $script:navigationRoot ([guid]::NewGuid().ToString('N'))
        $local = Join-Path $root 'local'
        New-Item -ItemType Directory -Path $local -Force | Out-Null
        $script:context = @{
            Root = $root; App = @{ Hwnd = 71; Pid = 72 }; Pane = @{ session_id = 'owned-pane' }
            UiIdentity = @{ pid = 73; created = [datetime]'2026-01-01T00:00:00Z' }
            Runtime = @{ navigationDiagnostics = $true; uiLogFilter = Get-WorkFlowNavigationLogFilter; localAppData = $local; sha256 = 'owned-hash' }
            Sequence = 389; CapturedActionEvidencePath = (Join-Path $root 'captured-action-owned.json')
            LastDecodedCommandId = 'previously-decoded-nonce'
        }
        Mock Get-WorkFlowNavigationObservation { @{
            foregroundHwnd = 81; foregroundPid = 82; foregroundThreadId = 83
            activePane = @{ sessionId = 'actually-observed-pane'; tabId = 'tab'; windowId = 'window' }
            focusedChildHwnd = $null; errors = @()
        } }
        Mock Send-WtWindowKey { @{ Hwnd = 91; Pid = 92 } }
        Mock Get-WtCapture { 'exact raw frame' }
    }
    AfterEach { $env:WTA_LOG = $script:priorNavigationLog }
    AfterAll { Remove-Item -LiteralPath $script:navigationRoot -Recurse -Force }

    It 'Scopes opted-in filtering to UI execution and restores <Prior> environment' -TestCases @(
        @{ Prior = $null }; @{ Prior = 'error,unrelated=info' }
    ) {
        param($Prior)
        $env:WTA_LOG = $Prior
        Invoke-WorkFlowUi -WtaPath 'Invoke-OfflineNavigationUi' -NavigationDiagnostics
        $script:observedUiLog | Should -BeExactly 'warn,agent_center::navigation=debug'
        $env:WTA_LOG | Should -BeExactly $Prior
    }

    It 'Leaves the existing filter alone without diagnostic opt-in' {
        $env:WTA_LOG = 'error,unrelated=info'
        Invoke-WorkFlowUi -WtaPath 'Invoke-OfflineNavigationUi'
        $script:observedUiLog | Should -BeExactly 'error,unrelated=info'
        $env:WTA_LOG | Should -BeExactly 'error,unrelated=info'
    }

    It 'Restores the prior filter when the UI invocation throws' {
        $env:WTA_LOG = 'error'
        Mock Invoke-OfflineNavigationUi { throw 'offline invocation failure' }
        { Invoke-WorkFlowUi -WtaPath 'Invoke-OfflineNavigationUi' -NavigationDiagnostics } |
            Should -Throw '*offline invocation failure*'
        $env:WTA_LOG | Should -BeExactly 'error'
    }

    It 'Explicitly opts the owned workflow into tracing without changing the service environment' {
        ${function:Start-WorkFlowTestContext}.ToString() | Should -Match '\-Scenario WorkFlow \-NavigationDiagnostics'
        $fixture = Get-Content -LiteralPath (Join-Path $PSScriptRoot '..\fixtures\Start-AgentCenterNativePaste.ps1') -Raw
        $fixture | Should -Match 'Invoke-WorkFlowUi -WtaPath \$WtaPath -NavigationDiagnostics:\$NavigationDiagnostics'
        $fixture | Should -Not -Match '\$env:WTA_LOG\s*='
        $fixture | Should -Match 'navigationDiagnostics = \[bool\]\$NavigationDiagnostics'
        $fixture | Should -Match 'uiLogFilter = '
    }

    It 'Records trustworthy request and return evidence for VK <Vk> without claiming delivery' -TestCases @(
        @{ Vk = 0x21 }; @{ Vk = 0x22 }; @{ Vk = 0x7B }
    ) {
        param($Vk)
        Mock Send-WtWindowKey {
            $prior = @(Get-Content -LiteralPath (Join-Path $script:context.Root 'navigation-input.jsonl') | ConvertFrom-Json)
            $prior.Count | Should -Be 1
            $prior[0].phase | Should -BeExactly 'Requested'
            @{ Hwnd = 91; Pid = 92 }
        }
        Send-WorkFlowKey -Vk $Vk -Ctrl -Shift
        $records = @(Get-Content -LiteralPath (Join-Path $script:context.Root 'navigation-input.jsonl') | ConvertFrom-Json)
        $records.Count | Should -Be 2
        $records[0].id | Should -BeExactly $records[1].id
        $record = $records[1]
        $record.phase | Should -BeExactly 'FrameworkReturned'
        $record.vk | Should -Be $Vk
        $record.ctrl | Should -BeTrue
        $record.shift | Should -BeTrue
        $record.alt | Should -BeFalse
        $record.requestedRepeat | Should -Be 1
        $record.injectionApi | Should -BeExactly 'keybd_event'
        $record.PSObject.Properties.Name | Should -Contain 'reportedSendCount'
        $record.reportedSendCount | Should -BeNullOrEmpty
        $record.deliveryEvidence | Should -Match '^Unavailable:'
        $record.expectedHwnd | Should -Be 71
        $record.expectedHostPid | Should -Be 72
        $record.expectedPaneId | Should -BeExactly 'owned-pane'
        $record.uiIdentity.pid | Should -Be 73
        $record.before.foregroundHwnd | Should -Be 81
        $record.after.foregroundThreadId | Should -Be 83
        $record.after.activePane.sessionId | Should -BeExactly 'actually-observed-pane'
        $record.returnedApp.hwnd | Should -Be 91
        $record.returnedApp.pid | Should -Be 92
        $record.capturePath | Should -BeExactly $script:context.CapturedActionEvidencePath
        $record.precedingFrameSequence | Should -Be 389
        $record.lastDecodedCommandId | Should -BeExactly 'previously-decoded-nonce'
        ([datetime]$record.sendStartedUtc -ge [datetime]$record.requestedUtc) | Should -BeTrue
        ([datetime]$record.sendFinishedUtc -ge [datetime]$record.sendStartedUtc) | Should -BeTrue
        ([datetime]$record.completedUtc -ge [datetime]$record.sendFinishedUtc) | Should -BeTrue
        $script:context.NativeInputSent | Should -BeTrue
        Should -Invoke Send-WtWindowKey -Times 1 -Exactly -ParameterFilter {
            $RequireForeground -and $Ctrl -and $Shift -and $App.Hwnd -eq 71
        }
        Should -Invoke Get-WorkFlowNavigationObservation -Times 2 -Exactly
    }

    It 'Retains sender failure and does not retry or claim a returned key' {
        Mock Send-WtWindowKey { throw 'foreground guard refused' }
        { Send-WorkFlowKey -Vk 0x22 } | Should -Throw '*foreground guard refused*'
        $records = @(Get-Content -LiteralPath (Join-Path $script:context.Root 'navigation-input.jsonl') | ConvertFrom-Json)
        $records.Count | Should -Be 2
        $records[1].phase | Should -BeExactly 'FrameworkThrew'
        $records[1].error | Should -BeExactly 'foreground guard refused'
        $records[1].returnedApp | Should -BeNullOrEmpty
        $records[1].reportedSendCount | Should -BeNullOrEmpty
        $script:context.NativeInputSent | Should -BeNullOrEmpty
        Should -Invoke Send-WtWindowKey -Times 1 -Exactly -ParameterFilter { $RequireForeground }
        Should -Invoke Get-WorkFlowNavigationObservation -Times 2 -Exactly
    }

    It 'Keeps the ordinary key path for <Label>' -TestCases @(
        @{ Label = 'Enter'; Vk = 0x0D; Enabled = $true }
        @{ Label = 'paste'; Vk = 0x56; Enabled = $true }
        @{ Label = 'disabled diagnostics'; Vk = 0x22; Enabled = $false }
    ) {
        param($Vk, $Enabled)
        $script:context.Runtime.navigationDiagnostics = $Enabled
        Send-WorkFlowKey -Vk $Vk -Ctrl -Shift
        Should -Invoke Send-WtWindowKey -Times 1 -Exactly -ParameterFilter { $RequireForeground -and $Ctrl -and $Shift }
        Should -Invoke Get-WorkFlowNavigationObservation -Times 0 -Exactly
        Test-Path -LiteralPath (Join-Path $script:context.Root 'navigation-input.jsonl') | Should -BeFalse
    }

    It 'Correlates repeated unchanged raw captures with the actual last navigation request' {
        Send-WorkFlowKey -Vk 0x22
        Read-WorkFlowUi | Should -BeExactly 'exact raw frame'
        Read-WorkFlowUi | Should -BeExactly 'exact raw frame'
        $captures = @(Get-Content -LiteralPath (Join-Path $script:context.Root 'navigation-captures.jsonl') | ConvertFrom-Json)
        $captures.Count | Should -Be 2
        $captures[0].sequence | Should -Be 390
        $captures[1].sequence | Should -Be 391
        foreach ($capture in $captures) {
            $capture.navigationId | Should -BeExactly $script:context.LastNavigationId
            $capture.capturePath | Should -BeExactly $script:context.CapturedActionEvidencePath
            $capture.uiPid | Should -Be 73
            $capture.paneSessionId | Should -BeExactly 'owned-pane'
            (Get-Content -LiteralPath (Join-Path $script:context.Root $capture.frame)) | Should -BeExactly 'exact raw frame'
            ([datetime]$capture.completedUtc -ge [datetime]$capture.startedUtc) | Should -BeTrue
        }
    }

    It 'Records observation errors explicitly without calling native APIs after initialization fails' {
        Mock Initialize-WtWin32Input { throw 'offline Win32 observation unavailable' } -ModuleName ItE2E
        Mock Get-ActivePane { throw 'offline active-pane observation unavailable' }
        Mock Write-Warning {}
        $observation = & $script:navigationObserver -Context $script:context
        $observation.errors.Count | Should -Be 2
        $observation.errors[0] | Should -Match 'Win32 observation.*unavailable'
        $observation.errors[1] | Should -Match 'Active-pane observation.*unavailable'
        $observation.focusedChildHwnd | Should -BeNullOrEmpty
        $observation.focusEvidence | Should -Match 'not exposed'
        Should -Invoke Write-Warning -Times 2 -Exactly
    }

    It 'Initializes Win32 observation inside the actual non-exporting module' {
        $module = Get-Module ItE2E
        $module.ExportedCommands.ContainsKey('Initialize-WtWin32Input') | Should -BeFalse
        & $module { Initialize-WtWin32Input; 'ItE2E.ItWtWin32Input' -as [type] } |
            Should -Not -BeNullOrEmpty
    }

    It 'Retains only discovered owned UI streams and their final byte hashes' {
        $logs = Join-Path $script:context.Runtime.localAppData 'logs\version'
        New-Item -ItemType Directory -Path $logs -Force | Out-Null
        $owned = Join-Path $logs 'wta-center-ui.2026-01-01.log'
        'owned navigation event' | Set-Content -LiteralPath $owned
        'service event' | Set-Content -LiteralPath (Join-Path $logs 'wta-center-service.2026-01-01.log')
        'outside runtime' | Set-Content -LiteralPath (Join-Path $script:context.Root 'wta-center-ui.2026-01-01.log')
        $script:context.NavigationKeyCount = 1
        Save-WorkFlowNavigationLogProvenance -Context $script:context -Final
        $record = Get-Content -LiteralPath (Join-Path $script:context.Root 'navigation-log-provenance.json') -Raw | ConvertFrom-Json
        $record.files.Count | Should -Be 1
        $record.files[0].path | Should -BeExactly $owned
        $record.files[0].sha256 | Should -BeExactly (Get-FileHash -LiteralPath $owned).Hash
        $record.uiIdentity.pid | Should -Be 73
        $record.wtaSha256 | Should -BeExactly 'owned-hash'
        $record.filter | Should -BeExactly 'warn,agent_center::navigation=debug'
        $record.status | Should -BeExactly 'Observed'
    }

    It 'Reports absent owned logs instead of substituting another process stream' {
        $script:context.NavigationKeyCount = 1
        { Save-WorkFlowNavigationLogProvenance -Context $script:context -Final } | Should -Throw '*No test-owned center-ui log*'
        $record = Get-Content -LiteralPath (Join-Path $script:context.Root 'navigation-log-provenance.json') -Raw | ConvertFrom-Json
        $record.status | Should -BeExactly 'NotObserved'
        $record.files.Count | Should -Be 0
    }
}

Describe 'Agent Center work-flow launch intent' -Tag 'Unit' {
    BeforeAll {
        function Get-ItTestPackage { return $script:selector }
        function Invoke-Native { @{ ExitCode = 1 } }
        function Stop-Terminal { param($App) }
        function Restore-ClipboardSnapshot { param($Snapshot) }
        $script:originalHash = $env:ITE2E_EXPECTED_WTA_SHA256
    }
    AfterAll { $env:ITE2E_EXPECTED_WTA_SHA256 = $script:originalHash }

    It 'Rejects <Label> before fixture state or launch can be created' -TestCases @(
        @{ Label = 'Store'; Selector = 'Store'; Hash = ('A' * 64) }
        @{ Label = 'implicit package'; Selector = ''; Hash = ('A' * 64) }
        @{ Label = 'missing provenance'; Selector = 'Dev'; Hash = '' }
        @{ Label = 'malformed provenance'; Selector = 'Dev'; Hash = 'not-a-sha256' }
    ) {
        param($Selector, $Hash)
        $script:selector = $Selector
        $env:ITE2E_EXPECTED_WTA_SHA256 = $Hash
        $context = @{}
        { Start-WorkFlowTestContext -Context $context } | Should -Throw '*requires ITE2E_PACKAGE=Dev*'
        $context.Count | Should -Be 0
    }

    It 'Checks the receipt reader prerequisite before preparing user settings' {
        $script:selector = 'Dev'
        $env:ITE2E_EXPECTED_WTA_SHA256 = 'A' * 64
        $context = @{}
        { Start-WorkFlowTestContext -Context $context } | Should -Throw '*require Node with node:sqlite*'
        $context.Count | Should -Be 0
    }

    It 'Restores the owned host and clipboard after pre-input setup failure' {
        Mock Stop-Terminal {}
        Mock Restore-ClipboardSnapshot {}
        $context = @{ App = @{ owned = $true }; ClipboardSaved = $true; Clipboard = 'in-memory-only'; SettingsHashes = @{} }
        Stop-WorkFlowTestContext $context
        Should -Invoke Stop-Terminal -Times 1 -Exactly
        Should -Invoke Restore-ClipboardSnapshot -Times 1 -Exactly -ParameterFilter { $Snapshot -ceq 'in-memory-only' }
        $context.NativeInputSent | Should -BeNullOrEmpty
    }

    It 'Still restores the clipboard when owned host teardown reports an error' {
        Mock Stop-Terminal { throw 'Owned teardown failed' }
        Mock Restore-ClipboardSnapshot {}
        $context = @{ App = @{ owned = $true }; ClipboardSaved = $true; Clipboard = 'in-memory-only'; SettingsHashes = @{} }
        { Stop-WorkFlowTestContext $context } | Should -Throw '*Owned teardown failed*'
        Should -Invoke Restore-ClipboardSnapshot -Times 1 -Exactly
    }
}
