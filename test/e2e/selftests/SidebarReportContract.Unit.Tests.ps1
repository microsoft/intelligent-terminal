BeforeAll {
    $script:root = Join-Path $PSScriptRoot ('..\artifacts\sidebar-report-selftest-' + [guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $script:root | Out-Null
    $script:titles = @(
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
        'Sidebar header search and options show their actual action tooltips',
        'Agents split falls back to an ordinary terminal',
        'Agents split falls back for an unsupported custom provider',
        'Agents split excludes the assistant from its MRU source',
        'Agents split reuses native provider intent before session startup'
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
    $script:fallbackTitle = 'Agents split falls back to an ordinary terminal'
    $script:fallbackContract = "When the PowerShell source terminal has no reusable agent identity, duplicate-split creates exactly one ordinary shell in the same tab with the source's ordinary profile and working directory. Both shells remain responsive, Agents remains selected, and no default agent or new tab is launched."
    $script:fallbackUnitTests = @(
        'AgentsSplitFallbackPreservesWslDirectory',
        'AgentsSplitFallbackPreservesWindowsDirectory',
        'AgentsSplitFallbackKeepsProfileDirectoryWhenUnusable',
        'AgentsSplitFallbackClearsEffectiveAgentLaunchIntent'
    )
}
AfterAll { Remove-Item -LiteralPath $script:root -Recurse -Force }
Describe 'Sidebar release-report contracts without product activation' {
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
            $fallback = @(Get-Content -LiteralPath $path | Where-Object { $_ -match '^- \[x\] `C379` ' })
            $fallback | Should -HaveCount 1
            $fallback[0] | Should -Be ('- [x] `C379` **{0}:** {1}' -f $script:fallbackTitle, $script:fallbackContract)
            $text | Should -Match '(?m)^\*\*Agents split fallback unit-only coverage \(passed\):\*\*'
        }
    }
    It 'keeps passed fallback unit-only cases outside C379 in full and incremental reports' {
        $unitXml = Join-Path $script:root 'fallback-unit-only.xml'
        $unitFull = Join-Path $script:root 'fallback-unit-full.md'
        $unitIncremental = Join-Path $script:root 'fallback-unit-incremental.md'
        $cases = $script:fallbackUnitTests | ForEach-Object {
            '<test-case name="Synthetic unit report.TabTests.' + $_ + '" executed="True" result="Success" />'
        }
        '<test-results><test-suite><results>' + ($cases -join '') + '</results></test-suite></test-results>' |
            Set-Content -LiteralPath $unitXml
        & (Join-Path $PSScriptRoot '..\New-ReleaseReport.ps1') -ResultsXml $unitXml -OutFile $unitFull
        & (Join-Path $PSScriptRoot '..\New-ReleaseReport.ps1') -ResultsXml @() -OutFile $unitIncremental
        & (Join-Path $PSScriptRoot '..\Update-ReleaseReport.ps1') -Report $unitIncremental `
            -ResultsXml $unitXml -OutFile $unitIncremental
        foreach ($path in @($unitFull, $unitIncremental)) {
            $lines = Get-Content -LiteralPath $path
            $fallback = @($lines | Where-Object { $_ -match '^- \[[ x]\] `C379` ' })
            $fallback | Should -HaveCount 1
            $fallback[0] | Should -Be ('- [ ] `C379` **{0}:** {1}' -f $script:fallbackTitle, $script:fallbackContract)
            $unitNote = @($lines | Where-Object { $_.StartsWith('**Agents split fallback unit-only coverage (passed):**') })
            $unitNote | Should -HaveCount 1
            $unitNote[0] | Should -Match 'WSL/Linux.*unusable directories.*--cd.*effective agent commands.*provider intent.*resume/session'
            $unitNote[0] | Should -Match "not part of C379's checkbox E2E acceptance"
            $unitNote[0] | Should -Match 'Live WSL/edge-case integration remains unvalidated'
            foreach ($name in $script:fallbackUnitTests) {
                $unitNote[0] | Should -Match ([regex]::Escape($name))
            }
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
