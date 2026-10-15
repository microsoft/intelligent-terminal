#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }

Describe 'Simplified Markdown fixture isolation' -Tag 'Unit' {
    BeforeAll {
        $ast = [Management.Automation.Language.Parser]::ParseFile(
            (Join-Path $PSScriptRoot '..\tests\Feature.SimplifiedMarkdown.Tests.ps1'), [ref]$null, [ref]$null)
        foreach ($phase in @('BeforeEach', 'AfterEach')) {
            $command = $ast.Find({
                param($node)
                $node -is [Management.Automation.Language.CommandAst] -and $node.GetCommandName() -eq $phase
            }, $true)
            $body = $command.CommandElements |
                Where-Object { $_ -is [Management.Automation.Language.ScriptBlockExpressionAst] } |
                Select-Object -First 1
            Set-Variable -Scope Script -Name "fixture$phase" -Value $body.ScriptBlock.GetScriptBlock()
        }

        function Get-WtProcessesForApp {
            param($App, [switch]$IncludePackageExecutables)
            $App | Should -Be $script:target
            $IncludePackageExecutables | Should -BeTrue
            if ($script:packageActive) { [pscustomobject]@{ Id = 12345 } }
        }
    }

    BeforeEach {
        $script:priorArtifactRoot = $env:ITE2E_ARTIFACT_ROOT
        $root = Join-Path $TestDrive ([guid]::NewGuid().ToString('N'))
        $env:ITE2E_ARTIFACT_ROOT = Join-Path $root 'evidence'
        $script:packageActive = $false
        $script:target = [pscustomobject]@{
            LocalStateDir = Join-Path $root 'state'
            SettingsPath = Join-Path $root 'state\settings.json'
            StatePath = Join-Path $root 'state\state.json'
            PackageFullName = 'fixture-package'
            InstallLocation = Join-Path $root 'install'
        }
        $script:originalRuntimeFile = Join-Path $script:target.LocalStateDir 'IntelligentTerminal\nested\original.bin'
    }

    AfterEach {
        $env:ITE2E_ARTIFACT_ROOT = $script:priorArtifactRoot
    }

    It 'preserves the original runtime and retains only test output as evidence (existing=<Existing>)' -ForEach @(
        @{ Existing = $false }, @{ Existing = $true }
    ) {
        if ($Existing) {
            New-Item -ItemType Directory -Path (Split-Path $script:originalRuntimeFile) -ErrorAction Stop | Out-Null
            [IO.File]::WriteAllBytes($script:originalRuntimeFile, [byte[]]@(0, 1, 127, 255))
            $hash = (Get-FileHash -LiteralPath $script:originalRuntimeFile).Hash
        }
        . $script:fixtureBeforeEach
        Test-Path -LiteralPath $script:runtimePath | Should -BeFalse
        if ($Existing) {
            (Get-FileHash -LiteralPath (Join-Path $script:runtimeBackup 'nested\original.bin')).Hash |
                Should -Be $hash
        }
        New-Item -ItemType Directory -Path $script:runtimePath -ErrorAction Stop | Out-Null
        [IO.File]::WriteAllText((Join-Path $script:runtimePath 'generated.json'), '{}')

        . $script:fixtureAfterEach

        Test-Path -LiteralPath (Join-Path $script:evidenceDirectory 'test-runtime\generated.json') |
            Should -BeTrue
        Test-Path -LiteralPath (Join-Path $script:runtimePath 'generated.json') | Should -BeFalse
        Test-Path -LiteralPath $script:runtimePath | Should -Be $Existing
        if ($Existing) { (Get-FileHash -LiteralPath $script:originalRuntimeFile).Hash | Should -Be $hash }
    }

    It 'refuses setup before touching an active package' {
        New-Item -ItemType Directory -Path (Split-Path $script:originalRuntimeFile) -ErrorAction Stop | Out-Null
        [IO.File]::WriteAllText($script:originalRuntimeFile, 'keep')
        $script:packageActive = $true
        { . $script:fixtureBeforeEach } | Should -Throw '*package must be inactive*'
        [IO.File]::ReadAllText($script:originalRuntimeFile) | Should -Be 'keep'
        $script:runtimeBackedUp | Should -BeFalse
    }

    It 'retains the backup and current output when package inactivity cannot be proved' {
        New-Item -ItemType Directory -Path (Split-Path $script:originalRuntimeFile) -ErrorAction Stop | Out-Null
        [IO.File]::WriteAllText($script:originalRuntimeFile, 'keep')
        . $script:fixtureBeforeEach
        New-Item -ItemType Directory -Path $script:runtimePath -ErrorAction Stop | Out-Null
        [IO.File]::WriteAllText((Join-Path $script:runtimePath 'generated.json'), '{}')
        $script:packageActive = $true

        { . $script:fixtureAfterEach } | Should -Throw '*runtime restoration requires package inactivity*'

        [IO.File]::ReadAllText((Join-Path $script:runtimeBackup 'nested\original.bin')) | Should -Be 'keep'
        Test-Path -LiteralPath (Join-Path $script:runtimePath 'generated.json') | Should -BeTrue
    }
}

Describe 'Simplified Markdown restored-pane binding' -Tag 'Unit' {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $ast = [Management.Automation.Language.Parser]::ParseFile(
            (Join-Path $PSScriptRoot '..\tests\Feature.SimplifiedMarkdown.Tests.ps1'), [ref]$null, [ref]$null)
        foreach ($name in @('Get-MarkdownPaneText', 'Bind-MarkdownPane')) {
            $definition = $ast.Find({
                param($node)
                $node -is [Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -eq $name
            }, $true)
            . ([scriptblock]::Create($definition.Extent.Text))
        }
    }

    BeforeEach {
        $script:app = [pscustomobject]@{
            Pid = 12345
            OwnedProcess = [pscustomobject]@{ StartTime = [datetime]'2026-01-01' }
        }
        $script:target = [pscustomobject]@{ WtaPath = 'C:\fixture\wta.exe' }
        $script:sessions = @([pscustomobject]@{
            session_id = 'markdown-saved-session'
            pane_session_id = 'restored-pane'
        })
        Mock Open-AgentPane {}
        Mock Get-WtSessions { $script:sessions }
        Mock Get-WtPaneStatus { [pscustomobject]@{ state = 'running'; pid = 12346 } }
        Mock Get-DescendantWtaIds { [pscustomobject]@{ Id = 12346; Path = $script:target.WtaPath } }
        Mock Get-AgentPaneSession { throw 'Saved-history binding must not depend on the legacy session file.' }
        Mock Get-WtCapture { 'Ask anything, / for commands' }
        Mock Get-CimInstance { [pscustomobject]@{ CommandLine = 'wta.exe --owner-tab-id restored-tab' } }
        Mock Wait-Until {
            param($Condition)
            $result = & $Condition
            if (-not $result) { throw 'Expected condition not satisfied.' }
            $result
        }
    }

    It 'observes readiness through the public module API and the verified live pane' {
        Bind-MarkdownPane -SavedHistory

        $script:agent.PaneSessionId | Should -Be 'restored-pane'
        $script:agent.AcpSessionId | Should -Be 'markdown-saved-session'
        $script:agent.HelperProcessId | Should -Be 12346
        $script:ownerTab | Should -Be 'restored-tab'
        Should -Invoke Get-WtCapture -Times 1 -Exactly -ParameterFilter { $SessionId -eq 'restored-pane' }
        Should -Invoke Get-AgentPaneSession -Times 0 -Exactly
    }

    It 'rejects ambiguous restored conversation ownership' {
        $script:sessions += $script:sessions[0]
        { Bind-MarkdownPane -SavedHistory } | Should -Throw '*ambiguous pane ownership*'
        Should -Invoke Get-WtCapture -Times 0 -Exactly
    }

    It 'does not bind an unrelated conversation' {
        $script:sessions[0].session_id = 'another-session'
        { Bind-MarkdownPane -SavedHistory } | Should -Throw '*Expected condition not satisfied*'
        Should -Invoke Get-WtCapture -Times 0 -Exactly
    }
}

Describe 'Simplified Markdown release-report mapping' -Tag 'Unit' {
    BeforeAll {
        $script:markdownCases = [ordered]@{
            C437 = 'Completed agent replies switch from raw text to Markdown'
            C438 = 'Cancelled agent replies format their received text'
            C439 = 'Raw Markdown preference bypasses formatting from helper startup'
            C440 = 'Selected raw replies survive completion without blocking queued requests'
            C433 = 'Saved agent replies use Markdown without changing raw history'
            C434 = 'FRE and Settings share a live Markdown preference without restarting the session'
            C435 = 'Failed agent replies format their received text'
            C436 = 'Disconnected agent replies format their received text'
        }
        function Write-MarkdownReportFixture {
            param([string]$Path, [string[]]$Passed)
            $cases = foreach ($title in $script:markdownCases.Values) {
                $name = [Security.SecurityElement]::Escape("Synthetic report contract.$title")
                $executed = $title -in $Passed
                $result = if ($executed) { 'Success' } else { 'Ignored' }
                "<test-case name=`"$name`" executed=`"$executed`" result=`"$result`" />"
            }
            ('<test-results>' + ($cases -join '') + '</test-results>') |
                Set-Content -LiteralPath $Path -Encoding utf8NoBOM -ErrorAction Stop
        }
    }

    BeforeEach {
        $script:reportRoot = Join-Path $TestDrive ([guid]::NewGuid().ToString('N'))
        New-Item -ItemType Directory -Path $script:reportRoot -ErrorAction Stop | Out-Null
    }

    It 'keeps Markdown checklist IDs unique across unrelated cases' {
        $text = Get-Content -LiteralPath (Join-Path $PSScriptRoot '..\..\..\doc\release-check-list.md') -Raw
        $ids = @([regex]::Matches($text, '(?m)^\s*- \[[ xX]\] `(?<id>C\d+)`') |
            ForEach-Object { $_.Groups['id'].Value })
        foreach ($id in $script:markdownCases.Keys) {
            @($ids | Where-Object { $_ -eq $id }) | Should -HaveCount 1
        }
    }

    It 'maps exactly the literal live scenario titles used by focused filters' {
        $ast = [Management.Automation.Language.Parser]::ParseFile(
            (Join-Path $PSScriptRoot '..\tests\Feature.SimplifiedMarkdown.Tests.ps1'), [ref]$null, [ref]$null)
        $cases = @($ast.FindAll({
            param($node)
            $node -is [Management.Automation.Language.CommandAst] -and $node.GetCommandName() -eq 'It'
        }, $true))
        $names = @($cases | ForEach-Object { $_.CommandElements[1].Value } | Sort-Object)
        $names | Should -HaveCount $script:markdownCases.Count
        @(Compare-Object @($script:markdownCases.Values | Sort-Object) $names) | Should -HaveCount 0
    }

    It 'credits all eight distinct cases in full and incremental reports' {
        $xml = Join-Path $script:reportRoot 'synthetic.xml'
        Write-MarkdownReportFixture -Path $xml -Passed @($script:markdownCases.Values)
        $full = Join-Path $script:reportRoot 'full.md'
        $incremental = Join-Path $script:reportRoot 'incremental.md'
        & (Join-Path $PSScriptRoot '..\New-ReleaseReport.ps1') -ResultsXml $xml -OutFile $full
        & (Join-Path $PSScriptRoot '..\New-ReleaseReport.ps1') -ResultsXml @() -OutFile $incremental
        & (Join-Path $PSScriptRoot '..\Update-ReleaseReport.ps1') -Report $incremental -ResultsXml $xml -OutFile $incremental
        foreach ($path in @($full, $incremental)) {
            $text = Get-Content -LiteralPath $path -Raw
            foreach ($entry in $script:markdownCases.GetEnumerator()) {
                $pattern = '(?m)^- \[x\] `' + $entry.Key + '` .*\*\*' + [regex]::Escape($entry.Value) + ':'
                $text | Should -Match $pattern
            }
            foreach ($id in 429..432) {
                $text | Should -Match ('(?m)^- \[ \] `C' + $id + '` .*\*\*OpenCode ')
                $text | Should -Not -Match ('(?m)^- \[x\] `C' + $id + '`')
            }
        }
    }

    It 'does not credit prompt errors or transport exits when only cancellation ran' {
        $xml = Join-Path $script:reportRoot 'partial-synthetic.xml'
        Write-MarkdownReportFixture -Path $xml -Passed $script:markdownCases.C438
        $report = Join-Path $script:reportRoot 'partial.md'
        & (Join-Path $PSScriptRoot '..\New-ReleaseReport.ps1') -ResultsXml $xml -OutFile $report
        $text = Get-Content -LiteralPath $report -Raw
        $text | Should -Match '(?m)^- \[x\] `C438`'
        foreach ($id in @('C435', 'C436')) {
            $text | Should -Match ('(?m)^- \[ \] `' + $id + '`')
            $text | Should -Not -Match ('(?m)^- \[x\] `' + $id + '`')
        }
    }
}
