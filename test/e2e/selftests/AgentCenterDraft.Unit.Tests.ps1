#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }

BeforeAll {
    . (Join-Path $PSScriptRoot '..\tests\helpers\AgentCenterDraft.ps1')
}

Describe 'Agent Center captured draft parser' -Tag 'Unit' {
    It 'Ignores the initial Agent Center header and reads the later empty input frame' {
        $frame = @'
┌Agent Center──────────────────────────────────────────┐
│Current work: — · Project: —                          │
└──────────────────────────────────────────────────────┘
┌Works─────────┐┌Conversation and activity──────────────┐
│              ││Enter appears in conversation history │
└──────────────┘└───────────────────────────────────────┘
┌Enter send · Shift+Enter newline · Tab complete────────┐
│                                                      │
│                                                      │
└──────────────────────────────────────────────────────┘
'@
        $rows = Get-NativePasteDraftRows -Frame $frame
        $rows.Count | Should -Be 2
        $rows[0] | Should -BeExactly ''
        $rows[1] | Should -BeExactly ''
    }

    It 'Reads the draft under <Label> help without requiring the full help string' -TestCases @(
        @{ Label = 'clipped English'; Title = 'Enter send · Shift+Enter new' }
        @{ Label = 'minimal clipped'; Title = 'Enter' }
        @{ Label = 'Chinese'; Title = 'Enter: 发送 · Shift+Enter: 换行' }
        @{ Label = 'Arabic'; Title = 'Enter: إرسال · Shift+Enter: سطر جديد' }
        @{ Label = 'accent pseudo-locale'; Title = '[Enter śéñď · Shift+Enter ñéŵĺïñé' }
        @{ Label = 'expanded pseudo-locale'; Title = '[!!_Enter ｓｅｎｄ · Shift+Enter' }
        @{ Label = 'RTL pseudo-locale'; Title = "[!! Enter$([char]0x202E) send" }
    ) {
        param($Title)
        $frame = "┌Agent Center─────┐`r`n│Current work: — │`r`n└────────────────┘`r`n" +
            "┌$Title─┐`r`n│/round10-draft  │`r`n└────────────────┘"
        $rows = Get-NativePasteDraftRows -Frame $frame
        $rows.Count | Should -Be 1
        $rows[0] | Should -BeExactly '/round10-draft'
    }

    It 'Rejects <Label> rather than returning unrelated rows' -TestCases @(
        @{ Label = 'the product header alone'; Frame = "┌Agent Center──┐`n│Current work │`n└─────────────┘" }
        @{ Label = 'an unanchored Enter token'; Frame = "┌History Enter──┐`n│wrong row     │`n└──────────────┘" }
        @{ Label = 'a lowercase token'; Frame = "┌enter send──┐`n│wrong row  │`n└───────────┘" }
        @{ Label = 'a token prefix'; Frame = "┌Entertain──┐`n│wrong row │`n└──────────┘" }
        @{ Label = 'a modified shortcut title'; Frame = "┌Shift+Enter──┐`n│wrong row   │`n└────────────┘" }
        @{ Label = 'an empty capture'; Frame = '' }
    ) {
        param($Frame)
        { Get-NativePasteDraftRows -Frame $Frame } | Should -Throw '*input box is not rendered*'
    }

    It 'Rejects <Label> input frames rather than accepting partial evidence' -TestCases @(
        @{ Label = 'zero-row'; Frame = "┌Enter send──┐`n└────────────┘" }
        @{ Label = 'malformed-row'; Frame = "┌Enter send──┐`nmissing borders`n└────────────┘" }
        @{ Label = 'unterminated'; Frame = "┌Enter send──┐`n│draft       │" }
    ) {
        param($Frame)
        { Get-NativePasteDraftRows -Frame $Frame } | Should -Throw '*input box*'
    }

    It 'Preserves ordinal multiline Unicode and whitespace, removing only terminal cell padding' {
        $emoji = [char]::ConvertFromUtf32(0x1F469) + [char]0x200D + [char]::ConvertFromUtf32(0x1F4BB)
        $first = "  /round10-$emoji  中"
        $second = "`t/round10-│-second$([char]0x00A0)`t"
        $frame = "  ┌Enter send──┐  `n  │$first   │  `n  │$second  │  `n  │    │  `n  └────────────┘  "
        $rows = Get-NativePasteDraftRows -Frame $frame
        $rows.Count | Should -Be 3
        $rows[0] | Should -BeExactly $first
        $rows[1] | Should -BeExactly $second
        $rows[2] | Should -BeExactly ''
    }

    It 'Reads the scoped composer beside a populated Unicode work rail without counting UTF16 units as cells' {
        $frame = @'
工作列表                  │港口报告
Overview                  │Horizon Decisions · Draft
Needs your attention      │
> other work              │Message for: 港口报告 · Horizon Decisions
另一个工作                │┌Enter send · Shift+Enter newline────────────────┐
未开始                    ││  a👩‍💻z  │ literal bar                           │
                          ││/second                                         │
                          │└────────────────────────────────────────────────┘
                          │Enter send · F4 actions
'@
        $box = Get-NativePasteInputBox $frame
        $box.hasRail | Should -BeTrue
        $box.scope | Should -BeExactly 'Message for: 港口报告 · Horizon Decisions'
        $box.rows | Should -Be @('  a👩‍💻z  │ literal bar', "/second$([char]0x00A0)")
        $before = ((($frame -split '\r?\n') | Select-Object -First $box.firstRow) -join "`n") + "`n另一个工作                ││  a👩‍💻"
        Get-NativePasteCaretPrefix -InputBox $box -DocumentBeforeCaret $before |
            Should -BeExactly '  a👩‍💻'
    }

    It 'Reads the full-width modal composer without accepting its scoped heading as draft text' {
        $frame = "Overview`nHarbor Reports ·`n`nNew work · Harbor Reports`n┌Enter send──────┐`n│draft          │`n└────────────────┘"
        $box = Get-NativePasteInputBox $frame
        $box.hasRail | Should -BeFalse
        $box.rows | Should -Be @('draft')
        $before = ((($frame -split '\r?\n') | Select-Object -First $box.firstRow) -join "`n") + "`n│dr"
        Get-NativePasteCaretPrefix -InputBox $box -DocumentBeforeCaret $before | Should -BeExactly 'dr'
        { Get-NativePasteCaretPrefix -InputBox $box -DocumentBeforeCaret "header`n│dr" } | Should -Throw '*outside the captured composer rows*'
        { Get-NativePasteCaretPrefix -InputBox $box -DocumentBeforeCaret 'not a composer row' } | Should -Throw '*outside*'
    }

    It 'Bounds editable rows before the external completion hint in a <Layout> frame' -TestCases @(
        @{ Layout = 'compact'; Railed = $false; InnerWidth = 78 }
        @{ Layout = 'wide railed'; Railed = $true; InnerWidth = 112 }
    ) {
        param($Railed, $InnerWidth)
        $rail = if ($Railed) { 'rail'.PadRight(25) + '│' } else { '' }
        $main = @('Work A', 'Harbor Reports · Draft', 'Message for: Work A · Harbor Reports',
            "┌Enter send$('─' * ($InnerWidth - 10))┐",
            "│$('/wo'.PadRight($InnerWidth))│", "└$('─' * $InnerWidth)┘",
            ' /work use', 'Enter send · F4 actions')
        $lines = @($main | ForEach-Object { $rail + $_ })
        $frame = $lines -join "`n"
        $box = Get-NativePasteInputBox $frame
        $box.rows | Should -Be @('/wo')
        $box.firstRow | Should -Be 4
        $box.endRow | Should -Be 5
        Get-NativePasteDraftRows $frame | Should -Be @('/wo')
        $before = ($lines[0..3] -join "`n") + "`n$rail" + '│/w'
        Get-NativePasteCaretPrefix -InputBox $box -DocumentBeforeCaret $before | Should -BeExactly '/w'
        foreach ($outsideRow in @($box.endRow, ($box.endRow + 1))) {
            { Get-NativePasteCaretPrefix -InputBox $box -DocumentBeforeCaret ($lines[0..$outsideRow] -join "`n") } |
                Should -Throw '*outside the captured composer rows*'
        }
    }

    It 'Preserves slash-like final draft text inside the border in a <Layout> frame' -TestCases @(
        @{ Layout = 'compact'; Railed = $false }
        @{ Layout = 'wide railed'; Railed = $true }
    ) {
        param($Railed)
        $rail = if ($Railed) { 'rail'.PadRight(25) + '│' } else { '' }
        $innerWidth = if ($Railed) { 112 } else { 78 }
        $main = @('Work A', 'Harbor Reports · Draft', 'Message for: Work A · Harbor Reports',
            "┌Enter send$('─' * ($innerWidth - 10))┐",
            "│$('/wo'.PadRight($innerWidth))│", "│$('/work use'.PadRight($innerWidth))│",
            "└$('─' * $innerWidth)┘", 'Enter send · F4 actions')
        $lines = @($main | ForEach-Object { $rail + $_ })
        $box = Get-NativePasteInputBox ($lines -join "`n")
        $box.rows | Should -Be @('/wo', '/work use')
        $box.endRow | Should -Be 6
        $before = ($lines[0..4] -join "`n") + "`n$rail" + '│/work'
        Get-NativePasteCaretPrefix -InputBox $box -DocumentBeforeCaret $before | Should -BeExactly '/work'
    }

    It 'Rejects a <Defect> composer instead of deriving editable bounds from its completion text' -TestCases @(
        @{ Defect = 'missing border' }; @{ Defect = 'broken closing corner' }
        @{ Defect = 'clipped border' }; @{ Defect = 'duplicate' }
    ) {
        param($Defect)
        foreach ($rail in @('', ('rail'.PadRight(25) + '│'))) {
            $lines = @('Message for: Work A · Harbor Reports', '┌Enter send────┐', '│/wo           │',
                '└──────────────┘', ' /work use') | ForEach-Object { $rail + $_ }
            $frame = $lines -join "`n"
            $broken = switch ($Defect) {
                'missing border' { $frame.Replace($lines[3] + "`n", '') }
                'broken closing corner' { $frame.Replace('└──────────────┘', '└──────────────│') }
                'clipped border' { $frame.Replace('└──────────────┘', '└────') }
                'duplicate' { "$frame`n$frame" }
            }
            { Get-NativePasteInputBox $broken } | Should -Throw '*input box*'
        }
    }

    It 'Keeps global Unicode draft and caret bounds in a <Mode> frame' -TestCases @(
        @{ Mode = 'chat'; Rail = '' }
        @{ Mode = 'dashboard'; Rail = '另一个工作                │' }
    ) {
        param($Rail)
        $heading = if ($Rail) { 'Other work' } else { 'Global conversation' }
        $rows = @($heading, 'Context hint: Other work · Another project',
            'Global conversation', '┌Enter send────────────────────────┐',
            '│/round9-a👩z                        │', '│/round9-two                         │',
            '└───────────────────────────────────┘')
        $frame = ($rows | ForEach-Object { $Rail + $_ }) -join "`n"
        $box = Get-NativePasteInputBox $frame
        $box.scope | Should -BeExactly 'Global conversation'
        $box.rows | Should -Be @('/round9-a👩z', '/round9-two')
        $prefix = (($rows[0..3] | ForEach-Object { $Rail + $_ }) -join "`n") + "`n$Rail" + '│/round9-a👩'
        Get-NativePasteCaretPrefix -InputBox $box -DocumentBeforeCaret $prefix | Should -BeExactly '/round9-a👩'
    }

    It 'Rejects ambiguous or broken rail composers rather than reading another region' {
        $box = "New work · Harbor Reports`n┌Enter send──┐`n│draft       │`n└────────────┘"
        { Get-NativePasteDraftRows "$box`n$box" } | Should -Throw '*ambiguous input boxes*'
        $broken = "rail│New work · Harbor Reports`nrail│┌Enter send──┐`n│draft       │`nrail│└────────────┘"
        { Get-NativePasteDraftRows $broken } | Should -Throw '*rail boundary*'
    }
}
