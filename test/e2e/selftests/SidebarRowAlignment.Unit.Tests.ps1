BeforeAll {
    $script:suite = Join-Path $PSScriptRoot '..\tests\Feature.SidebarRowAlignment.Tests.ps1'
    $tokens = $null
    $errors = $null
    $script:ast = [Management.Automation.Language.Parser]::ParseFile($script:suite, [ref]$tokens, [ref]$errors)
    $script:parseErrors = $errors
    $script:title = 'Sidebar live and recent titles align'
    $script:root = Join-Path $PSScriptRoot ('..\artifacts\row-alignment-selftest-' + [guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $script:root | Out-Null
    $function = $script:ast.Find({
        param($node)
        $node -is [Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -eq 'Assert-AlignmentDelta'
    }, $true)
    if (-not $function) { throw 'The alignment suite is missing Assert-AlignmentDelta.' }
    . ([scriptblock]::Create($function.Extent.Text))
    $markerFunction = $script:ast.Find({
        param($node)
        $node -is [Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -eq 'Resolve-AlignmentMarker'
    }, $true)
    if (-not $markerFunction) { throw 'The alignment suite is missing Resolve-AlignmentMarker.' }
    . ([scriptblock]::Create($markerFunction.Extent.Text))
    function Get-AlignmentChecklistBaseline {
        param([string]$BaseRef = $env:ITE2E_CHECKLIST_BASE_REF)
        $useDefault = [string]::IsNullOrEmpty($BaseRef)
        if ($useDefault) { $BaseRef = 'origin/main' }
        $repo = Join-Path $PSScriptRoot '..\..\..'
        $commit = @(& git -C $repo rev-parse --verify --end-of-options "$BaseRef^{commit}" 2>$null)
        if ($LASTEXITCODE -ne 0 -or $commit.Count -ne 1) {
            throw "Cannot resolve checklist base '$BaseRef'; fetch the actual PR target or set ITE2E_CHECKLIST_BASE_REF."
        }
        $head = @(& git -C $repo rev-parse --verify HEAD 2>$null)
        if ($LASTEXITCODE -ne 0 -or $head.Count -ne 1) { throw 'Cannot resolve candidate HEAD; fetch the source history.' }
        $baselineKind = 'merge-base'
        if ($useDefault -and $commit[0] -eq $head[0]) {
            $mergeBase = @(& git -C $repo rev-parse --verify 'HEAD^1' 2>$null)
            if ($LASTEXITCODE -ne 0 -or $mergeBase.Count -ne 1) {
                throw 'Cannot resolve checklist first-parent baseline; fetch complete source history (including HEAD^1).'
            }
            $baselineKind = 'first parent'
        }
        else {
            $mergeBase = @(& git -C $repo merge-base --all HEAD $commit[0] 2>$null)
            if ($LASTEXITCODE -ne 0 -or $mergeBase.Count -ne 1) {
                throw "Cannot identify one checklist merge-base with '$BaseRef'; fetch the PR target history."
            }
        }
        if ($mergeBase[0] -eq $head[0]) {
            throw "Checklist baseline must precede candidate HEAD; verify ITE2E_CHECKLIST_BASE_REF ('$BaseRef')."
        }
        $content = @(& git -C $repo show "$($mergeBase[0]):doc/release-check-list.md" 2>$null)
        if ($LASTEXITCODE -ne 0) { throw "Cannot read checklist baseline at $($mergeBase[0]); fetch complete source history." }
        $rows = @($content | Where-Object { $_ -match '^- \[[ x]\] `C\d+`' })
        if (-not $rows.Count) { throw "Checklist baseline at $($mergeBase[0]) contains no stable-ID rows." }
        Write-Host "Checklist baseline: $BaseRef -> $baselineKind $($mergeBase[0]); $($rows.Count) prior rows."
        $rows
    }
    function Assert-AlignmentChecklistPreserved {
        param([string[]]$Candidate, [string[]]$Baseline)
        $baselineCounts = @{}
        $candidateCounts = @{}
        foreach ($pair in @(@($Baseline, $baselineCounts), @($Candidate, $candidateCounts))) {
            foreach ($line in $pair[0]) {
                if ($line -match '^- \[[ x]\] `(?<id>C\d+)`') {
                    $id = $Matches.id
                    $pair[1][$id] = [int]$pair[1][$id] + 1
                }
            }
        }
        if (-not $baselineCounts.Count) { throw 'An independent nonempty checklist baseline is required.' }
        foreach ($id in $baselineCounts.Keys) {
            if ([int]$candidateCounts[$id] -ne $baselineCounts[$id]) {
                throw "Prior checklist ID occurrence count changed: $id (expected $($baselineCounts[$id]), actual $([int]$candidateCounts[$id]))."
            }
        }
    }
    function New-AlignmentResult {
        param([string]$Result)
        $path = Join-Path $script:root "$Result.xml"
        $executed = if ($Result -eq 'Ignored') { 'False' } else { 'True' }
        '<test-results><test-suite><results><test-case name="Feature: Sidebar row alignment.' +
            $script:title + '" executed="' + $executed + '" result="' + $Result +
            '" /></results></test-suite></test-results>' | Set-Content -LiteralPath $path
        $path
    }
}
AfterAll { Remove-Item -LiteralPath $script:root -Recurse -Force }

Describe 'Sidebar row alignment nonlive contracts' -Tag Unit {
    It 'preserves the actual hook receipt read error at the assignment boundary' {
        $assignment = $script:ast.Find({
            param($node)
            $node -is [Management.Automation.Language.AssignmentStatementAst] -and
                $node.Extent.Text -match '^\$receipt = Get-Content'
        }, $true)
        if (-not $assignment) { throw 'The alignment suite is missing its hook receipt read.' }
        $receiptPath = Join-Path $script:root 'fictitious-receipt.json'
        $receipt = [pscustomobject]@{ unchanged = $true }
        $original = $receipt
        $script:receiptReadReturned = $false
        function Get-Content {
            [CmdletBinding()]
            param($LiteralPath, [switch]$Raw)
            Write-Error 'hook receipt read sentinel'
            $script:receiptReadReturned = $true
        }
        $read = [scriptblock]::Create($assignment.Extent.Text)
        {
            & {
                $ErrorActionPreference = 'Continue'
                . $read
            }
        } | Should -Throw '*hook receipt read sentinel*'
        $script:receiptReadReturned | Should -BeFalse
        [object]::ReferenceEquals($receipt, $original) | Should -BeTrue
    }
    It 'compares logical leading edges rather than intrinsic text widths (<Direction>)' -TestCases @(
        @{ Direction = 'LTR'; Expected = 100 },
        @{ Direction = 'RTL'; Expected = 300 }
    ) {
        param($Direction, $Expected)
        $function = $script:ast.Find({
            param($node)
            $node -is [Management.Automation.Language.FunctionDefinitionAst] -and
                $node.Name -eq 'Get-AlignmentLeadingEdge'
        }, $true)
        if (-not $function) { throw 'The alignment suite is missing Get-AlignmentLeadingEdge.' }
        . ([scriptblock]::Create($function.Extent.Text))
        $rectangle = [pscustomobject]@{ Left = 100; Right = 300 }
        Get-AlignmentLeadingEdge $rectangle $Direction | Should -Be $Expected
        if ($Direction -eq 'RTL') {
            $narrow = [pscustomobject]@{ Left = 180; Right = 300 }
            Get-AlignmentLeadingEdge $narrow $Direction | Should -Be (
                Get-AlignmentLeadingEdge $rectangle $Direction)
        }
    }
    It 'selects metadata nearest its logical leading side (<Direction>)' -TestCases @(
        @{ Direction = 'LTR'; Expected = 'left' },
        @{ Direction = 'RTL'; Expected = 'right' }
    ) {
        param($Direction, $Expected)
        $command = $script:ast.Find({
            param($node)
            $node -is [Management.Automation.Language.CommandAst] -and
                $node.GetCommandName() -eq 'Sort-Object' -and
                $node.Extent.Text.Contains('$script:alignmentFlowDirection')
        }, $true)
        if (-not $command) { throw 'The alignment suite is missing logical metadata ordering.' }
        $sort = $command.CommandElements[1].ScriptBlock.GetScriptBlock()
        $script:alignmentFlowDirection = $Direction
        $parts = @(
            [pscustomobject]@{ Id = 'left'; Current = [pscustomobject]@{ BoundingRectangle = [pscustomobject]@{ Left = 10; Right = 30 } } },
            [pscustomobject]@{ Id = 'right'; Current = [pscustomobject]@{ BoundingRectangle = [pscustomobject]@{ Left = 40; Right = 90 } } }
        )
        @($parts | Sort-Object $sort)[0].Id | Should -Be $Expected
    }
    It 'preserves the master-pipe read error before trimming its result' {
        $assignment = $script:ast.Find({
            param($node)
            $node -is [Management.Automation.Language.AssignmentStatementAst] -and
                $node.Left.Extent.Text -eq '$script:pipe'
        }, $true)
        if (-not $assignment) { throw 'The alignment suite is missing its master-pipe read.' }
        $script:app = [pscustomobject]@{ LocalStateDir = $script:root }
        $script:pipe = 'unmodified-pipe'
        $script:pipeReadReturned = $false
        function Get-Content {
            [CmdletBinding()]
            param($LiteralPath, [switch]$Raw)
            Write-Error 'master-pipe read sentinel'
            $script:pipeReadReturned = $true
        }
        $read = [scriptblock]::Create($assignment.Extent.Text)
        {
            & {
                $ErrorActionPreference = 'Continue'
                & $read
            }
        } | Should -Throw '*master-pipe read sentinel*'
        $script:pipeReadReturned | Should -BeFalse
        $script:pipe | Should -Be 'unmodified-pipe'
    }
    It 'requires successful evidence publication (<File>, <FailWrite>)' -TestCases @(
        @{ File = 'package.json'; FailWrite = $true },
        @{ File = 'geometry.json'; FailWrite = $true },
        @{ File = 'selected-bounds.json'; FailWrite = $true },
        @{ File = 'package.json'; FailWrite = $false },
        @{ File = 'geometry.json'; FailWrite = $false },
        @{ File = 'selected-bounds.json'; FailWrite = $false }
    ) {
        param($File, $FailWrite)
        $writes = @($script:ast.FindAll({
            param($node)
            $node -is [Management.Automation.Language.CommandAst] -and
                $node.GetCommandName() -eq 'Set-Content' -and $node.Extent.Text.Contains("'$File'")
        }, $true))
        $writes.Count | Should -Be 1
        $pipeline = $writes[0].Parent
        while ($pipeline -and $pipeline -isnot [Management.Automation.Language.PipelineAst]) {
            $pipeline = $pipeline.Parent
        }
        if (-not $pipeline) { throw "The alignment suite is missing the $File publication pipeline." }
        $operation = [scriptblock]::Create($pipeline.Extent.Text)
        $script:evidence = $script:root
        $script:app = [pscustomobject]@{ Package = 'Dev'; Version = 'fictitious-version' }
        $script:pane = 'fictitious-pane'
        $head = '0123456789abcdef0123456789abcdef01234567'
        $loadedApp = @([pscustomobject]@{ FileName = 'fictitious-TerminalApp.dll' })
        $receipt = @{ fixture_marker = 'fictitious-marker' }
        $recent = [pscustomobject]@{ Element = 'fictitious-row' }
        $script:evidenceWriteFails = $FailWrite
        $script:evidenceWriteReturned = $false
        $script:evidenceAccepted = 0
        $script:publishedEvidence = $null
        function Read-AlignmentSessions { [pscustomobject]@{ session_id = 'fictitious-session' } }
        function Get-AlignmentRect { param($Element) [pscustomobject]@{ left = 10; right = 20 } }
        function Set-Content {
            [CmdletBinding()]
            param($LiteralPath, [Parameter(ValueFromPipeline)]$Value)
            process {
                if ($script:evidenceWriteFails) { Write-Error 'evidence-write sentinel' }
                $script:publishedEvidence = $Value
                $script:evidenceWriteReturned = $true
            }
        }
        $invoke = {
            & {
                $ErrorActionPreference = 'Continue'
                & $operation
                $script:evidenceAccepted++
            }
        }
        if ($FailWrite) {
            $invoke | Should -Throw '*evidence-write sentinel*'
            $script:evidenceWriteReturned | Should -BeFalse
            $script:evidenceAccepted | Should -Be 0
            $script:publishedEvidence | Should -BeNullOrEmpty
        }
        else {
            & $invoke
            $script:evidenceWriteReturned | Should -BeTrue
            $script:evidenceAccepted | Should -Be 1
            ($script:publishedEvidence | ConvertFrom-Json) | Should -Not -BeNullOrEmpty
        }
    }
    It 'validates HEAD command results before receipt comparison (<Case>)' -TestCases @(
        @{ Case = 'empty'; Lines = @(); ExitCode = 0; Valid = $false },
        @{ Case = 'failed empty'; Lines = @(); ExitCode = 128; Valid = $false },
        @{ Case = 'multiple'; Lines = @('0123456789abcdef0123456789abcdef01234567', '0123456789abcdef0123456789abcdef01234567'); ExitCode = 0; Valid = $false },
        @{ Case = 'malformed'; Lines = @('not-a-hash'); ExitCode = 0; Valid = $false },
        @{ Case = 'failed with hash'; Lines = @('0123456789abcdef0123456789abcdef01234567'); ExitCode = 128; Valid = $false },
        @{ Case = 'valid'; Lines = @('0123456789abcdef0123456789abcdef01234567'); ExitCode = 0; Valid = $true }
    ) {
        param($Case, $Lines, $ExitCode, $Valid)
        $guard = $script:ast.Find({
            param($node)
            $node -is [Management.Automation.Language.IfStatementAst] -and
                $node.Extent.Text.Contains("throw 'Cannot resolve candidate HEAD for exact-source build provenance.'")
        }, $true)
        if (-not $guard) { throw 'The alignment suite is missing its HEAD result guard.' }
        $headLines = @($Lines | Where-Object { $null -ne $_ })
        $LASTEXITCODE = $ExitCode
        $check = [scriptblock]::Create($guard.Extent.Text)
        if ($Valid) {
            { & $check } | Should -Not -Throw
        }
        else {
            { & $check } | Should -Throw '*Cannot resolve candidate HEAD for exact-source build provenance.*'
        }
    }
    It 'submits hooks only after input publication succeeds (<WriteFails>, <StrictMode>)' -TestCases @(
        @{ WriteFails = $true; StrictMode = 'Off' },
        @{ WriteFails = $true; StrictMode = 'Latest' },
        @{ WriteFails = $false; StrictMode = 'Latest' }
    ) {
        param($WriteFails, $StrictMode)
        $helper = $script:ast.Find({
            param($node)
            $node -is [Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -eq 'Send-AlignmentHooks'
        }, $true)
        if (-not $helper) { throw 'The alignment suite is missing Send-AlignmentHooks.' }
        . ([scriptblock]::Create($helper.Extent.Text))
        $script:evidence = $script:root
        $script:hookFixture = 'fictitious-hook-fixture'
        $script:app = [pscustomobject]@{ WtcliPath = 'fictitious-wtcli' }
        $script:pane = '00000000-0000-0000-0000-000000000001'
        $script:inputWriteFails = $WriteFails
        $script:inputWriteReturned = 0
        $script:hookInputSent = 0
        $script:hookEnterSent = 0
        $script:hookWaitEntered = 0
        function Set-Content {
            [CmdletBinding()]
            param($LiteralPath, [Parameter(ValueFromPipeline)]$Value)
            process {
                if ($script:inputWriteFails) { Write-Error 'input-write sentinel' }
                $script:inputWriteReturned++
            }
        }
        function Send-WtInput { param($App, $SessionId, $Text) $script:hookInputSent++ }
        function Send-WtKeys { param($App, $SessionId, $Keys) $script:hookEnterSent++ }
        function Wait-Until { param($TimeoutSec, $Because, $Condition) $script:hookWaitEntered++ }
        function Get-Content {
            param($LiteralPath, [switch]$Raw)
            '{"events":1,"pane_session_id":"00000000-0000-0000-0000-000000000001"}'
        }
        $invoke = {
            & {
                if ($StrictMode -eq 'Off') { Set-StrictMode -Off } else { Set-StrictMode -Version Latest }
                $ErrorActionPreference = 'Continue'
                Send-AlignmentHooks -Events @(@{ event = 'fictitious-hook' })
            }
        }
        if ($WriteFails) {
            $invoke | Should -Throw '*input-write sentinel*'
            $script:inputWriteReturned | Should -Be 0
            $script:hookInputSent | Should -Be 0
            $script:hookEnterSent | Should -Be 0
            $script:hookWaitEntered | Should -Be 0
        }
        else {
            & $invoke
            $script:inputWriteReturned | Should -Be 1
            $script:hookInputSent | Should -Be 1
            $script:hookEnterSent | Should -Be 1
            $script:hookWaitEntered | Should -Be 1
        }
    }
    It 'reports missing extracted helpers with their actionable names' {
        $tokens = $null
        $errors = $null
        $startup = [Management.Automation.Language.Parser]::ParseFile(
            (Join-Path $PSScriptRoot 'SidebarRowAlignment.Unit.Tests.ps1'), [ref]$tokens, [ref]$errors)
        foreach ($name in @('Assert-AlignmentDelta', 'Resolve-AlignmentMarker')) {
            $guard = $startup.Find({
                param($node)
                $node -is [Management.Automation.Language.IfStatementAst] -and
                    $node.Extent.Text.Contains("throw 'The alignment suite is missing $name.'")
            }, $true)
            $guard | Should -Not -BeNullOrEmpty
            $function = $null
            $markerFunction = $null
            { & ([scriptblock]::Create($guard.Extent.Text)) } | Should -Throw "*missing $name*"
        }
    }
    It 'reuses a validated run-scoped marker for matched baseline and candidate titles' {
        $marker = 'row-align-20261009-a1b2c3'
        Resolve-AlignmentMarker $marker | Should -Be $marker
        Resolve-AlignmentMarker $marker | Should -Be (Resolve-AlignmentMarker $marker)
        Resolve-AlignmentMarker '' | Should -Match '^row-align-[a-f0-9]{8}$'
        foreach ($invalid in @('short', '../baseline', 'row align fixture', 'fixture_123', '-fixture-123',
            "fixture-123`n", ('a' * 65))) {
            { Resolve-AlignmentMarker $invalid } | Should -Throw
        }
        $script:ast.Extent.Text | Should -Match 'Resolve-AlignmentMarker \$env:ITE2E_ALIGNMENT_MARKER'
        $script:ast.Extent.Text | Should -Match '\$script:liveTitle = "\$script:marker-live"'
        $script:ast.Extent.Text | Should -Match '\$script:historyTitle = "\$script:marker-history"'
        $script:ast.Extent.Text | Should -Match 'if \(Test-Path -LiteralPath \$script:evidence -ErrorAction Stop\)'
        $script:ast.Extent.Text | Should -Match 'Alignment evidence already exists'
    }
    It 'persists actual geometry and screenshot before the unchanged baseline-failing oracle' {
        $case = $script:ast.Find({
            param($node)
            $node -is [Management.Automation.Language.CommandAst] -and $node.GetCommandName() -eq 'It'
        }, $true)
        $commands = @($case.FindAll({
            param($node)
            $node -is [Management.Automation.Language.CommandAst]
        }, $true))
        $geometry = @($commands | Where-Object {
            $_.GetCommandName() -eq 'Set-Content' -and $_.Extent.Text.Contains("'geometry.json'")
        })
        $screenshot = @($commands | Where-Object {
            $_.GetCommandName() -eq 'Save-UiScreenshot' -and $_.Extent.Text.Contains("'alignment.png'")
        })
        $firstOracle = @($commands | Where-Object { $_.GetCommandName() -eq 'Assert-AlignmentDelta' })[0]
        $geometry | Should -HaveCount 1
        $screenshot | Should -HaveCount 1
        $geometry[0].Extent.EndOffset | Should -BeLessThan $firstOracle.Extent.StartOffset
        $screenshot[0].Extent.EndOffset | Should -BeLessThan $firstOracle.Extent.StartOffset
        $case.Extent.Text | Should -Match 'title_delta_dip'
        $case.Extent.Text | Should -Match 'fixture_marker'
        $case.Extent.Text | Should -Match 'icon_center_delta_dip'
        $case.Extent.Text | Should -Not -Match 'if.*baseline|ExpectedFailure|Set-ItResult|Set-ItResult -Skipped'
        { Assert-AlignmentDelta 50 40 1 } | Should -Throw
    }
    It 'parses one exact-title real-package case without forbidden fixture setup' {
        $script:parseErrors | Should -BeNullOrEmpty
        $cases = @($script:ast.FindAll({
            param($node)
            $node -is [Management.Automation.Language.CommandAst] -and $node.GetCommandName() -eq 'It'
        }, $true))
        $cases | Should -HaveCount 1
        $cases[0].CommandElements[1].Value | Should -Be $script:title
        $script:ast.Extent.Text | Should -Not -Match 'Clear-WtConfig|Set-WtSetting|Start-Terminal|Stop-Terminal|HistoryList|HistorySearchTextBox|CombinedAgentsSidebar|Uninstall|Stop-Process'
        foreach ($required in @('ItemsList', 'SearchTextBox', 'Get-FreCompleted', 'ITE2E_SOURCE_COMMIT',
            'ITE2E_EXPECTED_APP_SHA256', 'ITE2E_EXPECTED_WTA_SHA256', 'Backup-WtConfig',
            'Invoke-SidebarSessionCleanup', 'OwnedPaneIds', 'original-hashes.json', 'runtime-cleanup.json',
            'Emit-SidebarSessionHooks.ps1', 'Read-AlignmentSessions', 'SelectionItemPattern',
            'icon_geometry_verified', 'pending independent review')) {
            $script:ast.Extent.Text | Should -Match ([regex]::Escape($required))
        }
    }
    It 'backs up and restores every hashed hidden and system runtime file' {
        $copies = @($script:ast.FindAll({
            param($node)
            $node -is [Management.Automation.Language.CommandAst] -and
                $node.GetCommandName() -eq 'Copy-Item' -and
                $node.Extent.Text.Contains('$script:runtimePath') -and
                $node.Extent.Text.Contains('$script:runtimeBackup')
        }, $true))
        $copies | Should -HaveCount 2
        $copies[0].CommandElements[2].Extent.Text | Should -Be '$script:runtimePath'
        $copies[1].CommandElements[2].Extent.Text | Should -Be '$script:runtimeBackup'
        $script:runtimePath = [IO.Path]::GetFullPath((Join-Path $script:root 'runtime-copy-source'))
        $script:runtimeBackup = [IO.Path]::GetFullPath((Join-Path $script:root 'runtime-copy-backup'))
        $nested = Join-Path $script:runtimePath 'hidden-directory'
        New-Item -ItemType Directory -Path $nested | Out-Null
        $hiddenSystem = [IO.FileAttributes]::Hidden -bor [IO.FileAttributes]::System
        $files = @(
            @{ Path = 'visible.json'; Attributes = [IO.FileAttributes]::Normal }
            @{ Path = 'hidden.json'; Attributes = $hiddenSystem -bor [IO.FileAttributes]::ReadOnly }
            @{ Path = 'hidden-directory\child.json'; Attributes = $hiddenSystem }
        )
        $hashes = @{}
        foreach ($file in $files) {
            $path = Join-Path $script:runtimePath $file.Path
            [IO.File]::WriteAllText($path, '{"text":"runtime snapshot"}', [Text.UTF8Encoding]::new($false))
            [IO.File]::SetAttributes($path, $file.Attributes)
            $hashes[$file.Path] = (Get-FileHash -LiteralPath $path).Hash
        }
        [IO.File]::SetAttributes($nested, $hiddenSystem -bor [IO.FileAttributes]::Directory)
        [IO.File]::SetAttributes($script:runtimePath, $hiddenSystem -bor [IO.FileAttributes]::Directory)
        @(Get-ChildItem -LiteralPath $script:runtimePath -File -Recurse -Force) | Should -HaveCount $files.Count
        & ([scriptblock]::Create($copies[0].Extent.Text))
        foreach ($file in $files) {
            (Get-FileHash -LiteralPath (Join-Path $script:runtimeBackup $file.Path)).Hash |
                Should -Be $hashes[$file.Path]
        }
        Remove-Item -LiteralPath $script:runtimePath -Recurse -Force
        & ([scriptblock]::Create($copies[1].Extent.Text))
        @(Get-ChildItem -LiteralPath $script:runtimePath -File -Recurse -Force) | Should -HaveCount $files.Count
        foreach ($file in $files) {
            $path = Join-Path $script:runtimePath $file.Path
            (Get-FileHash -LiteralPath $path).Hash | Should -Be $hashes[$file.Path]
            $preservedAttributes = $file.Attributes -band ($hiddenSystem -bor [IO.FileAttributes]::ReadOnly)
            ([IO.File]::GetAttributes($path) -band $preservedAttributes) | Should -Be $preservedAttributes
        }
    }
    It 'detects the pre-fix ten-DIP title and six-DIP icon deltas at multiple DPI scales' {
        foreach ($scale in @(1.0, 1.25, 1.5, 2.0)) {
            { Assert-AlignmentDelta (50 * $scale) (40 * $scale) $scale } | Should -Throw
            { Assert-AlignmentDelta (26 * $scale) (20 * $scale) $scale } | Should -Throw
            { Assert-AlignmentDelta (50 * $scale) (50 * $scale) $scale } | Should -Not -Throw
            { Assert-AlignmentDelta (50 * $scale) (49 * $scale) $scale } | Should -Not -Throw
            { Assert-AlignmentDelta (50 * $scale) (48.9 * $scale) $scale } | Should -Throw
        }
        { Assert-AlignmentDelta 50 50 0 } | Should -Throw
    }
    It 'uses one viewport origin and never substitutes slot geometry for missing icon peers' {
        $text = $script:ast.Extent.Text
        $text | Should -Match 'Get-AlignmentLeadingEdge \$liveTitle.Current.BoundingRectangle'
        $text | Should -Match 'Get-AlignmentLeadingEdge \$recentTitle.Current.BoundingRectangle'
        $text | Should -Match 'Assert-AlignmentDelta \$liveLeading \$recentLeading \$scale'
        $text | Should -Match 'RawViewWalker'
        $text | Should -Match 'if \(\$iconsVerified\)'
        $text | Should -Match 'else \{ Write-Warning \$receipt.icon_limitation \}'
        $text | Should -Not -Match 'icon.*=.*(?:26|18)\s*\*'
        $text | Should -Match 'Get-FileHash -LiteralPath \$script:target.SettingsPath'
    }
    It 'requires one measurable peer on both sides before recording icon geometry' {
        $assignments = @{}
        foreach ($name in @('historyIcon', 'liveIcon', 'iconsVerified')) {
            $assignment = $script:ast.Find({
                param($node)
                $node -is [Management.Automation.Language.AssignmentStatementAst] -and
                    $node.Left -is [Management.Automation.Language.VariableExpressionAst] -and
                    $node.Left.VariablePath.UserPath -eq $name
            }, $true)
            $assignment | Should -Not -BeNullOrEmpty
            $assignments[$name] = $assignment
        }
        $historyFilter = $assignments.historyIcon.Find({
            param($node)
            $node -is [Management.Automation.Language.ScriptBlockExpressionAst]
        }, $true).ScriptBlock.GetScriptBlock()
        $liveFilter = $assignments.liveIcon.Find({
            param($node)
            $node -is [Management.Automation.Language.ScriptBlockExpressionAst]
        }, $true).ScriptBlock.GetScriptBlock()
        $verified = [scriptblock]::Create($assignments.iconsVerified.Right.Extent.Text)
        foreach ($case in @(
            @{ HistoryWidth = 16; LiveWidth = 16; HistoryCount = 1; LiveCount = 1; Expected = $true }
            @{ HistoryWidth = 0; LiveWidth = 16; HistoryCount = 1; LiveCount = 1; Expected = $false }
            @{ HistoryWidth = 16; LiveWidth = 0; HistoryCount = 1; LiveCount = 1; Expected = $false }
            @{ HistoryWidth = 16; LiveWidth = 16; HistoryCount = 0; LiveCount = 1; Expected = $false }
            @{ HistoryWidth = 16; LiveWidth = 16; HistoryCount = 1; LiveCount = 0; Expected = $false }
            @{ HistoryWidth = 16; LiveWidth = 16; HistoryCount = 2; LiveCount = 1; Expected = $false }
            @{ HistoryWidth = 16; LiveWidth = 16; HistoryCount = 1; LiveCount = 2; Expected = $false }
        )) {
            $historyPeers = @(for ($i = 0; $i -lt $case.HistoryCount; $i++) {
                [pscustomobject]@{ Current = [pscustomobject]@{
                    AutomationId = 'HistoryProviderIcon'; IsOffscreen = $false
                    BoundingRectangle = [pscustomobject]@{ Width = $case.HistoryWidth }
                } }
            })
            $livePeers = @(for ($i = 0; $i -lt $case.LiveCount; $i++) {
                [pscustomobject]@{ Current = [pscustomobject]@{
                    AutomationId = 'TabIconPresenter'; IsOffscreen = $false
                    BoundingRectangle = [pscustomobject]@{ Width = $case.LiveWidth }
                } }
            })
            $historyIcon = @($historyPeers | Where-Object $historyFilter)
            $liveIcon = @($livePeers | Where-Object $liveFilter)
            (& $verified) | Should -Be $case.Expected
        }
    }
    It 'allocates one stable exact-title ID and preserves prior checklist ID occurrence counts' {
        $path = Join-Path $PSScriptRoot '..\..\..\doc\release-check-list.md'
        $lines = @(Get-Content -LiteralPath $path)
        $matched = @($lines | Where-Object { $_.Contains("**$($script:title):") })
        $matched | Should -HaveCount 1
        $matched[0] | Should -Match '^- \[ \] `C\d+`.*`\[E2E\]`'
        $matched[0] -match '`(?<id>C\d+)`' | Should -BeTrue
        $id = $Matches.id
        @($lines | Where-Object { $_.Contains('`' + $id + '`') }) | Should -HaveCount 1
        $baseline = @(Get-AlignmentChecklistBaseline)
        Assert-AlignmentChecklistPreserved -Candidate $lines -Baseline $baseline
    }
    It 'rejects in-memory committed removal and ID replacement against the independent PR base' {
        $baseline = @(Get-AlignmentChecklistBaseline)
        $candidate = @(Get-Content -LiteralPath (Join-Path $PSScriptRoot '..\..\..\doc\release-check-list.md'))
        $prior = $baseline[0]
        $prior | Should -Match '`C\d+`'
        $index = [Array]::IndexOf($candidate, $prior)
        $index | Should -BeGreaterOrEqual 0
        $removed = @(for ($i = 0; $i -lt $candidate.Count; $i++) { if ($i -ne $index) { $candidate[$i] } })
        { Assert-AlignmentChecklistPreserved -Candidate $removed -Baseline $baseline } |
            Should -Throw '*Prior checklist ID occurrence count changed*'
        $replacement = $prior -replace '`C\d+`', '`C999999`'
        $changed = @($candidate)
        $changed[$index] = $replacement
        { Assert-AlignmentChecklistPreserved -Candidate $changed -Baseline $baseline } |
            Should -Throw '*Prior checklist ID occurrence count changed*'
        $wording = @($candidate)
        $wording[$index] = ($prior -replace '^- \[[ x]\]', '- [x]') + ' Updated wording.'
        { Assert-AlignmentChecklistPreserved -Candidate $wording -Baseline $baseline } | Should -Not -Throw
        { Assert-AlignmentChecklistPreserved -Candidate $candidate -Baseline @() } |
            Should -Throw '*nonempty checklist baseline*'
    }
    It 'counts repeated prior IDs rather than treating checklist preservation as set membership' {
        $prior = @('- [ ] `C001` First item', '- [ ] `C001` Second item')
        { Assert-AlignmentChecklistPreserved -Candidate $prior[0] -Baseline $prior } |
            Should -Throw '*expected 2, actual 1*'
        { Assert-AlignmentChecklistPreserved -Candidate ($prior + $prior[0]) -Baseline $prior } |
            Should -Throw '*expected 2, actual 3*'
        { Assert-AlignmentChecklistPreserved -Candidate ($prior + '- [ ] `C002` New item') -Baseline $prior } |
            Should -Not -Throw
    }
    It 'refuses a missing or candidate-HEAD baseline rather than silently passing' {
        { Get-AlignmentChecklistBaseline -BaseRef 'refs/heads/ite2e-nonexistent-checklist-base' } |
            Should -Throw '*Cannot resolve checklist base*'
        { Get-AlignmentChecklistBaseline -BaseRef HEAD } |
            Should -Throw '*baseline must precede candidate HEAD*'
    }
    It 'uses an independent first parent on main after merge and still detects removed IDs' {
        Mock git {
            $global:LASTEXITCODE = 0
            if ($args -contains 'show') {
                ($args -join ' ') | Should -Match ('2{40}:doc/release-check-list.md')
                return @('- [ ] `C001` Original', '- [ ] `C002` Retained')
            }
            if ($args -contains 'HEAD^1') { return ('2' * 40) }
            return ('1' * 40)
        }
        $baseline = @(Get-AlignmentChecklistBaseline -BaseRef '')
        $baseline | Should -HaveCount 2
        { Assert-AlignmentChecklistPreserved -Candidate $baseline -Baseline $baseline } | Should -Not -Throw
        { Assert-AlignmentChecklistPreserved -Candidate $baseline[1] -Baseline $baseline } |
            Should -Throw '*C001*expected 1, actual 0*'
        Should -Invoke git -Times 1 -Exactly -ParameterFilter { $args -contains 'HEAD^1' }
        Should -Invoke git -Times 0 -Exactly -ParameterFilter { $args -contains 'merge-base' }
    }
    It 'fails with fetch guidance when the main first parent is unavailable' {
        Mock git {
            $global:LASTEXITCODE = 0
            if ($args -contains 'HEAD^1') { $global:LASTEXITCODE = 1; return }
            return ('1' * 40)
        }
        { Get-AlignmentChecklistBaseline -BaseRef '' } | Should -Throw '*fetch complete source history*'
    }
    It 'maps synthetic success to full and incremental reports without claiming live acceptance' {
        $xml = New-AlignmentResult Success
        $full = Join-Path $script:root 'full.md'
        $incremental = Join-Path $script:root 'incremental.md'
        & (Join-Path $PSScriptRoot '..\New-ReleaseReport.ps1') -ResultsXml $xml -OutFile $full
        & (Join-Path $PSScriptRoot '..\New-ReleaseReport.ps1') -ResultsXml @() -OutFile $incremental
        & (Join-Path $PSScriptRoot '..\Update-ReleaseReport.ps1') -Report $incremental -ResultsXml $xml -OutFile $incremental
        foreach ($path in @($full, $incremental)) {
            Get-Content -LiteralPath $path -Raw |
                Should -Match ('(?m)^- \[x\].*\*\*' + [regex]::Escape($script:title) + ':')
        }
    }
    It 'retains a genuine failed mapping through a skipped-only incremental overlay' {
        $failed = New-AlignmentResult Failure
        $skipped = New-AlignmentResult Ignored
        $report = Join-Path $script:root 'failed.md'
        & (Join-Path $PSScriptRoot '..\New-ReleaseReport.ps1') -ResultsXml $failed -OutFile $report
        $before = Get-Content -LiteralPath $report -Raw
        $before | Should -Match ('(?m)^- \[ \].*AUTOMATION FAILED.*\*\*' + [regex]::Escape($script:title) + ':')
        & (Join-Path $PSScriptRoot '..\Update-ReleaseReport.ps1') -Report $report -ResultsXml $skipped -OutFile $report
        $after = Get-Content -LiteralPath $report -Raw
        ($after -split "`n" | Where-Object { $_.Contains("**$($script:title):") }) |
            Should -Be ($before -split "`n" | Where-Object { $_.Contains("**$($script:title):") })
    }
}
