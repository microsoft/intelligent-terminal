BeforeAll {
    $script:root = Join-Path $PSScriptRoot ('..\artifacts\sidebar-report-selftest-' + [guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $script:root | Out-Null
    $script:titles = @(
        'Sidebar plus always creates the default profile',
        'Tabs plus and split retain ordinary terminal behavior',
        'Custom native provider intent creates no conversation before its hook',
        'Combined sidebar mixed rows share one scroll viewport',
        'Recent Sessions collapses without hiding live agents',
        'Sidebar upgrade migrates Horizontal only once',
        'Sidebar introduction is independent of completed migration',
        'Sidebar migration persistence failure warns and retries without false completion',
        'Absent Sidebar upgrade flags migrate once without bypassing fresh FRE',
        'Collapsed Sidebar defers its pending introduction until visible',
        'Owned windows share the durable Sidebar introduction without duplicates',
        'Recent Sessions native expansion events match Content view',
        'Collapsing focused Recent Sessions preserves the active shell',
        'Agents live tab mutations reconcile rows without refreshing history',
        'Mixed Sidebar keyboard crosses the Recent Sessions boundary without activation',
        'Sidebar header search and options show their actual action tooltips'
    )
    $cases = $script:titles | ForEach-Object {
        '<test-case name="Synthetic report contract.' + [Security.SecurityElement]::Escape($_) +
            '" executed="True" result="Success" />'
    }
    # Synthetic XML exercises report matching only; it is not product acceptance.
    $script:xml = Join-Path $script:root 'synthetic.xml'
    '<test-results><test-suite><results>' + ($cases -join '') + '</results></test-suite></test-results>' |
        Set-Content -LiteralPath $script:xml
    $script:full = Join-Path $script:root 'full.md'
    $script:incremental = Join-Path $script:root 'incremental.md'
}
AfterAll { Remove-Item -LiteralPath $script:root -Recurse -Force }
Describe 'Sidebar release-report contracts without product activation' {
    It 'assigns new sidebar scope cases unique stable IDs' {
        $text = Get-Content (Join-Path $PSScriptRoot '..\..\..\doc\release-check-list.md') -Raw
        $ids = [regex]::Matches($text, '(?m)^- \[[ x!]\] `(?<id>C\d+)`') |
            ForEach-Object { $_.Groups['id'].Value }
        foreach ($id in 411..417) {
            @($ids | Where-Object { $_ -eq "C$id" }) | Should -HaveCount 1
        }
    }
    It 'retains upstream source-profile splits and fixed assistant panels instead of sidebar delegation overrides' {
        $tokens = $null
        $errors = $null
        $ast = [Management.Automation.Language.Parser]::ParseFile(
            (Join-Path $PSScriptRoot '..\tests\Feature.AgentsModeActions.Tests.ps1'),
            [ref]$tokens, [ref]$errors)
        $errors | Should -BeNullOrEmpty
        $ast.Extent.Text | Should -Not -Match 'Invoke-ActionRejectedSplit|Assert-ActionRetryClearsError|agent split rejected|split-session|same-provider split'
        $titles = @(
            'Native agent terminal splits use the source profile without delegation',
            'Sidebar splits use the source profile across filters search and layouts',
            'Assistant panes remain fixed during ordinary split actions'
        )
        foreach ($title in $titles) {
            $case = $ast.Find({
                param($node)
                $node -is [Management.Automation.Language.CommandAst] -and
                    $node.GetCommandName() -eq 'It' -and $node.CommandElements[1].Value -eq $title
            }, $true)
            $case | Should -Not -BeNullOrEmpty
            $case.Extent.Text | Should -Match 'Vk 0xBB -Alt -Shift'
            $case.Extent.Text | Should -Not -Match 'Invoke-Wta|retryError|HistoryMessage.*Should -Not -BeNullOrEmpty'
            if ($title -ne $titles[2]) {
                $case.Extent.Text | Should -Match 'SPLIT-PROFILE:\$script:otherProfile'
                $case.Extent.Text | Should -Match 'native_agent_provider_id \| Should -BeNullOrEmpty'
            }
            else {
                $case.Extent.Text | Should -Match 'SessionId \$helper.PaneSessionId'
                $case.Extent.Text | Should -Match 'AcpSessionId \| Should -Be \$helper.AcpSessionId'
            }
        }
    }
    It 'checks both real filter preferences around each plus and the layout round trip without resetting them' {
        $tokens = $null
        $errors = $null
        $ast = [Management.Automation.Language.Parser]::ParseFile(
            (Join-Path $PSScriptRoot '..\tests\Feature.AgentsModeActions.Tests.ps1'),
            [ref]$tokens, [ref]$errors)
        $errors | Should -BeNullOrEmpty
        $helper = $ast.Find({
            param($node)
            $node -is [Management.Automation.Language.FunctionDefinitionAst] -and
                $node.Name -eq 'Assert-ActionFilterState'
        }, $true)
        foreach ($pattern in @('AgentsOnlyFilterMenuItem', 'RecentAgentSessionsFilterMenuItem',
            'TogglePattern', 'Current.ToggleState', 'Should -Be \$expected', 'Current.ProcessId')) {
            $helper.Extent.Text | Should -Match $pattern
        }
        $helper.Extent.Text | Should -Not -Match 'Set-TestSidebarScope|Set-WtSetting|\.Toggle\(\)|\.Invoke\(\)'
        $case = $ast.Find({
            param($node)
            $node -is [Management.Automation.Language.CommandAst] -and
                $node.GetCommandName() -eq 'It' -and
                $node.CommandElements[1].Value -eq 'Sidebar plus always creates the default profile'
        }, $true)
        $commands = @($case.FindAll({
            param($node)
            $node -is [Management.Automation.Language.CommandAst]
        }, $true))
        $checks = @($commands | Where-Object { $_.GetCommandName() -eq 'Assert-ActionFilterState' })
        $checks | Should -HaveCount 3
        foreach ($check in $checks) {
            $check.Extent.Text | Should -Match '-AgentsOnly \$agentsOnly -Recent \$recent'
        }
        $plus = @($commands | Where-Object { $_.GetCommandName() -eq 'Invoke-ActionPlus' })[0]
        $checks[0].Extent.StartOffset | Should -BeLessThan $plus.Extent.StartOffset
        $checks[1].Extent.StartOffset | Should -BeGreaterThan $plus.Extent.EndOffset
        $checks[2].Extent.StartOffset | Should -BeGreaterThan $checks[1].Extent.EndOffset
        $setters = @($commands | Where-Object { $_.GetCommandName() -eq 'Set-TestSidebarScope' })
        $setters | Should -HaveCount 1
        $setters[0].Extent.EndOffset | Should -BeLessThan $checks[0].Extent.StartOffset
    }
    It 'keeps normal plus and explicit provider creation in separate real action cases' {
        $tokens = $null
        $errors = $null
        $ast = [Management.Automation.Language.Parser]::ParseFile(
            (Join-Path $PSScriptRoot '..\tests\Feature.AgentsModeActions.Tests.ps1'),
            [ref]$tokens, [ref]$errors)
        $errors | Should -BeNullOrEmpty
        $cases = @($ast.FindAll({
            param($node)
            $node -is [Management.Automation.Language.CommandAst] -and
                $node.GetCommandName() -eq 'It'
        }, $true))
        $cases | Should -HaveCount 7
        $normal = @($cases | Where-Object { $_.CommandElements[1].Value -in $script:titles[0..1] })
        $normal | Should -HaveCount 2
        foreach ($case in $normal) {
            $case.Extent.Text | Should -Match 'Invoke-ActionPlus'
            $case.Extent.Text | Should -Not -Match 'Vk 0x42|--agent-provider'
        }
        $canonical = @($cases | Where-Object { $_.CommandElements[1].Value -eq 'Native agent provider is visible before session startup' })[0]
        $canonical.Extent.Text | Should -Match 'Vk 0x42 -Alt -Shift'
        $canonical.Extent.Text | Should -Not -Match 'Invoke-ActionPlus'
        $custom = @($cases | Where-Object { $_.CommandElements[1].Value -eq $script:titles[2] })[0]
        $custom.Extent.Text | Should -Match "'--agent-provider', "
        $custom.Extent.Text | Should -Not -Match 'Invoke-ActionPlus'
        $plus = $ast.Find({
            param($node)
            $node -is [Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -eq 'Invoke-ActionPlus'
        }, $true)
        $plus.Extent.Text | Should -Match "-Selector 'New tab'"
        $plus.Extent.Text | Should -Not -Match 'actionAgentsOnly|Open background agent'
    }
    It 'allocates unique stable IDs and keeps the retired divider ID out of checkbox coverage' {
        $lines = Get-Content (Join-Path $PSScriptRoot '..\..\..\doc\release-check-list.md')
        $newLines = @($lines | Where-Object {
            $line = $_
            @($script:titles | Where-Object { $line.Contains("**${_}:") }).Count -gt 0
        })
        $newLines | Should -HaveCount $script:titles.Count
        $ids = @($newLines | Select-String '^- \[[ x]\] `C(\d+)`' | ForEach-Object { $_.Matches[0].Groups[1].Value })
        @($ids | Select-Object -Unique).Count | Should -Be $ids.Count
        foreach ($id in $ids) {
            @($lines | Where-Object { $_ -match ('`C' + $id + '`') }) | Should -HaveCount 1
        }
        @($lines | Where-Object { $_ -match '^- \[[ x]\] `C383`' }) | Should -HaveCount 0
        @($lines | Where-Object { $_ -match 'C383.*RETIRED' }) | Should -HaveCount 1
    }
    It 'credits exact new titles in full and incremental reports without reusing retired IDs' {
        & (Join-Path $PSScriptRoot '..\New-ReleaseReport.ps1') -ResultsXml $script:xml -OutFile $script:full
        & (Join-Path $PSScriptRoot '..\New-ReleaseReport.ps1') -ResultsXml @() -OutFile $script:incremental
        & (Join-Path $PSScriptRoot '..\Update-ReleaseReport.ps1') -Report $script:incremental `
            -ResultsXml $script:xml -OutFile $script:incremental
        foreach ($path in @($script:full, $script:incremental)) {
            $text = Get-Content -LiteralPath $path -Raw
            foreach ($title in $script:titles) {
                $text | Should -Match ('(?m)^- \[x\].*\*\*' + [regex]::Escape($title) + ':')
            }
            $text | Should -Not -Match '(?m)^- \[x\].*`C38[23]`'
        }
    }
    It 'marks a matching failed case as failed and does not erase that evidence with a skipped-only update' {
        $title = $script:titles[0]
        $failed = Join-Path $script:root 'failed.xml'
        $skipped = Join-Path $script:root 'skipped.xml'
        $failedReport = Join-Path $script:root 'failed.md'
        $skippedReport = Join-Path $script:root 'skipped.md'
        $name = [Security.SecurityElement]::Escape("Synthetic report contract.$title")
        "<test-results><test-case name=`"$name`" executed=`"True`" result=`"Failure`" /></test-results>" |
            Set-Content -LiteralPath $failed
        "<test-results><test-case name=`"$name`" executed=`"False`" result=`"Ignored`" /></test-results>" |
            Set-Content -LiteralPath $skipped
        & (Join-Path $PSScriptRoot '..\New-ReleaseReport.ps1') -ResultsXml $failed -OutFile $failedReport
        $before = Get-Content -LiteralPath $failedReport -Raw
        $before | Should -Match ('(?m)^- \[ \].*AUTOMATION FAILED.*\*\*' + [regex]::Escape($title) + ':')
        & (Join-Path $PSScriptRoot '..\Update-ReleaseReport.ps1') -Report $failedReport `
            -ResultsXml $skipped -OutFile $skippedReport
        $targetPattern = '\*\*' + [regex]::Escape($title) + ':'
        @(Get-Content -LiteralPath $skippedReport | Where-Object { $_ -match $targetPattern }) |
            Should -Be @($before -split '\r?\n' | Where-Object { $_ -match $targetPattern })
    }
}
