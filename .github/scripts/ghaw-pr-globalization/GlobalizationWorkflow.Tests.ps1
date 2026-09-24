#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }

Describe 'PR globalization workflow' -Tag 'Unit' {
    BeforeAll {
        $script:root = Resolve-Path (Join-Path $PSScriptRoot '..\..\..')
        $script:classifier = Join-Path $PSScriptRoot 'Get-GlobalizationChangeContext.ps1'
        $script:validator = Join-Path $PSScriptRoot 'Test-GlobalizationFindings.ps1'
        $script:workflow = Join-Path $script:root '.github\workflows\ghaw-pr-globalization.md'
        $script:repairWorkflow = Join-Path $script:root '.github\workflows\ghaw-pr-globalization-repair.md'
        $script:workflowLock = Join-Path $script:root '.github\workflows\ghaw-pr-globalization.lock.yml'
        $script:repairWorkflowLock = Join-Path $script:root '.github\workflows\ghaw-pr-globalization-repair.lock.yml'
        $script:localizationWorkflow = Join-Path $script:root '.github\workflows\ensure-localization.md'
        $script:controller = Join-Path $script:root '.github\workflows\ghaw-pr-globalization-controller.yml'

        function New-Report {
            param([object[]]$Findings = @(), [string]$Base = ('a' * 40), [string]$Head = ('b' * 40))
            return [ordered]@{
                version = 1
                baseSha = $Base
                headSha = $Head
                findings = $Findings
                patchFiles = @()
                executedValidation = @()
                resourceChecks = @()
            }
        }

        function New-Finding {
            param([string]$Severity = 'HIGH', [string]$Confidence = 'strong', [string]$Disposition = 'blocked')
            return [ordered]@{
                stableId = 'GLOB-RTL-001'
                severity = $Severity
                confidence = $Confidence
                sourceSha = 'a' * 40
                headSha = 'b' * 40
                file = 'src/cascadia/TerminalApp/Sample.xaml'
                line = 12
                scenario = 'Open the settings page under qps-plocm.'
                localeOrScript = 'Arabic / qps-plocm'
                observed = 'The directional icon and focus order remain left-to-right.'
                expected = 'Layout mirrors while the semantic icon remains unmirrored.'
                impact = 'Keyboard users encounter a visually reversed navigation order.'
                evidence = @('Sample.xaml:12 sets an explicit LeftToRight flow.')
                proposedFix = 'Inherit FlowDirection and opt the semantic icon out of mirroring.'
                validation = @('Run the RTL UI test with qps-plocm and verify focus order.')
                disposition = $Disposition
            }
        }

        function Invoke-Validator {
            param(
                $Report,
                [string]$ExpectedHead = ('b' * 40),
                [ValidateSet('guide', 'repair')][string]$Mode = 'guide',
                [switch]$Trusted,
                [switch]$NoHunks,
                [string]$ChangedPath = 'src/cascadia/TerminalApp/Sample.xaml'
            )
            $directory = Join-Path $TestDrive ([guid]::NewGuid().ToString('N'))
            [System.IO.Directory]::CreateDirectory($directory) | Out-Null
            $path = Join-Path $directory 'findings.json'
            $contextPath = Join-Path $directory 'context.json'
            $trustedPath = Join-Path $directory 'trusted-validation.json'
            [System.IO.File]::WriteAllText($path, ($Report | ConvertTo-Json -Depth 10), [System.Text.UTF8Encoding]::new($false))
            $hunks = if ($NoHunks) { @() } else {
                @([ordered]@{ oldStart = 1; oldCount = 100; newStart = 1; newCount = 100 })
            }
            $context = [ordered]@{
                version = 1
                baseSha = 'a' * 40
                headSha = $ExpectedHead
                files = @([ordered]@{
                    path = $ChangedPath
                    status = 'M'
                    hunks = @($hunks)
                })
            }
            [System.IO.File]::WriteAllText($contextPath, ($context | ConvertTo-Json -Depth 10), [System.Text.UTF8Encoding]::new($false))
            $arguments = @(
                '-NoProfile', '-File', $script:validator,
                '-ReportPath', $path,
                '-ContextPath', $contextPath,
                '-ExpectedBaseSha', ('a' * 40),
                '-ExpectedHeadSha', $ExpectedHead,
                '-Mode', $Mode
            )
            if ($Trusted) {
                $trustedValidation = [ordered]@{
                    version = 1
                    checks = @(
                        [ordered]@{ name = 'git-diff-check'; status = 'PASS'; exitCode = 0 },
                        [ordered]@{ name = 'patch-manifest'; status = 'PASS'; exitCode = 0 },
                        [ordered]@{ name = 'patch-shape'; status = 'PASS'; exitCode = 0 },
                        [ordered]@{ name = 'hunk-scope'; status = 'PASS'; exitCode = 0 }
                    )
                }
                [System.IO.File]::WriteAllText($trustedPath, ($trustedValidation | ConvertTo-Json -Depth 10), [System.Text.UTF8Encoding]::new($false))
                $arguments += @('-TrustedValidationPath', $trustedPath)
            }
            $null = & pwsh @arguments 2>&1
            return $LASTEXITCODE
        }
    }

    It 'accepts no findings and a fully evidenced HIGH blocker' {
        (Invoke-Validator -Report (New-Report)) | Should -Be 0
        (Invoke-Validator -Report (New-Report) -NoHunks) | Should -Be 0
        (Invoke-Validator -Report (New-Report -Findings @((New-Finding)))) | Should -Be 0
        (Invoke-Validator -Report (New-Report -Findings @((New-Finding))) -NoHunks) | Should -Not -Be 0
    }

    It 'rejects malformed output, stale SHA, unsafe paths, and invalid severity gating' {
        (Invoke-Validator -Report ([ordered]@{ version = 1 })) | Should -Not -Be 0
        (Invoke-Validator -Report (New-Report) -ExpectedHead ('c' * 40)) | Should -Not -Be 0

        $unsafe = New-Finding
        $unsafe.file = '../workflow.yml'
        (Invoke-Validator -Report (New-Report -Findings @($unsafe))) | Should -Not -Be 0

        $unchangedLine = New-Finding
        $unchangedLine.line = 101
        (Invoke-Validator -Report (New-Report -Findings @($unchangedLine))) | Should -Not -Be 0

        $scalarEvidence = New-Finding
        $scalarEvidence.evidence = 'not-an-array'
        (Invoke-Validator -Report (New-Report -Findings @($scalarEvidence))) | Should -Not -Be 0

        $blankValidation = New-Finding
        $blankValidation.validation = @(' ')
        (Invoke-Validator -Report (New-Report -Findings @($blankValidation))) | Should -Not -Be 0

        $mediumBlocker = New-Finding -Severity 'MEDIUM' -Confidence 'moderate' -Disposition 'blocked'
        (Invoke-Validator -Report (New-Report -Findings @($mediumBlocker))) | Should -Not -Be 0

        $lowRemaining = New-Finding -Severity 'LOW' -Confidence 'moderate' -Disposition 'remaining'
        (Invoke-Validator -Report (New-Report -Findings @($lowRemaining))) | Should -Not -Be 0

        $highSuggestion = New-Finding -Severity 'HIGH' -Confidence 'strong' -Disposition 'suggestion'
        (Invoke-Validator -Report (New-Report -Findings @($highSuggestion))) | Should -Not -Be 0
    }

    It 'allows fixed disposition only for strong HIGH repair findings' {
        $fixed = New-Finding
        $fixed.disposition = 'fixed'
        $report = New-Report -Findings @($fixed)
        $report.patchFiles = @(
            [ordered]@{ path = $fixed.file; kind = 'fix'; findingIds = @($fixed.stableId) }
        )
        $report.executedValidation = @(
            [ordered]@{ command = 'focused-test'; exitCode = 0; result = 'passed' }
        )
        (Invoke-Validator -Report $report -Mode repair -Trusted) | Should -Be 0
        (Invoke-Validator -Report $report -Mode repair) | Should -Not -Be 0
        (Invoke-Validator -Report $report -Mode guide -Trusted) | Should -Not -Be 0

        $testFinding = New-Finding
        $testFinding.file = 'src/cascadia/TerminalApp/LocalTests/Sample.xaml'
        $testFinding.disposition = 'fixed'
        $testReport = New-Report -Findings @($testFinding)
        $testReport.patchFiles = @(
            [ordered]@{ path = $testFinding.file; kind = 'fix'; findingIds = @($testFinding.stableId) }
        )
        $testReport.executedValidation = @(
            [ordered]@{ command = 'focused-test'; exitCode = 0; result = 'passed' }
        )
        (Invoke-Validator -Report $testReport -Mode repair -Trusted -ChangedPath $testFinding.file) | Should -Not -Be 0
    }

    It 'rejects fixed findings without validation or with paths outside exact finding scope' {
        $fixed = New-Finding
        $fixed.disposition = 'fixed'
        $report = New-Report -Findings @($fixed)
        (Invoke-Validator -Report $report -Mode repair -Trusted) | Should -Not -Be 0

        $report.patchFiles = @(
            [ordered]@{ path = 'src/cascadia/TerminalApp/Other.xaml'; kind = 'fix'; findingIds = @($fixed.stableId) }
        )
        $report.executedValidation = @(
            [ordered]@{ command = 'focused-test'; exitCode = 0; result = 'passed' }
        )
        (Invoke-Validator -Report $report -Mode repair -Trusted) | Should -Not -Be 0

        $remaining = New-Finding -Disposition 'remaining'
        $report = New-Report -Findings @($remaining)
        $report.patchFiles = @(
            [ordered]@{ path = $remaining.file; kind = 'fix'; findingIds = @($remaining.stableId) }
        )
        (Invoke-Validator -Report $report -Mode repair -Trusted) | Should -Not -Be 0
    }

    It 'rejects symlinked reports and agent-authored resource checker bundles' {
        $directory = Join-Path $TestDrive 'symlink'
        [System.IO.Directory]::CreateDirectory($directory) | Out-Null
        $target = Join-Path $directory 'target.json'
        $link = Join-Path $directory 'report.json'
        $context = Join-Path $directory 'context.json'
        [System.IO.File]::WriteAllText($target, ((New-Report) | ConvertTo-Json -Depth 10))
        [System.IO.File]::WriteAllText($context, ([ordered]@{
            version = 1
            baseSha = 'a' * 40
            headSha = 'b' * 40
            files = @([ordered]@{
                path = 'src/cascadia/TerminalApp/Sample.xaml'
                status = 'M'
                hunks = @([ordered]@{ oldStart = 1; oldCount = 100; newStart = 1; newCount = 100 })
            })
        } | ConvertTo-Json -Depth 10))
        [System.IO.File]::CreateSymbolicLink($link, $target) | Out-Null
        $null = & pwsh -NoProfile -File $script:validator -ReportPath $link -ContextPath $context `
            -ExpectedBaseSha ('a' * 40) -ExpectedHeadSha ('b' * 40) 2>&1
        $LASTEXITCODE | Should -Not -Be 0

        $untrusted = New-Report
        $untrusted.resourceChecks = @([ordered]@{
            check = 'Test-ResourceSyntax'
            status = 'PASS'
            exitCode = 0
            results = @([ordered]@{ status = 'PASS'; path = 'sample' })
        })
        (Invoke-Validator -Report $untrusted) | Should -Not -Be 0
    }

    It 'carries every required domain rule and false-positive boundary in the prompt' {
        $skill = Get-Content -LiteralPath (Join-Path $script:root '.github\skills\review-globalization\SKILL.md') -Raw
        foreach ($needle in @(
            'FlowDirection', 'directional icons', 'focus order', 'mixed paths',
            'UTF-16 surrogate pairs', 'UTF-8 boundaries', 'grapheme',
            'emoji/CJK width', 'invariant persistence', 'placeholder reordering',
            'concatenated sentence fragments', 'ACP/COM/VT tokens',
            'debug logs', 'lock_locale()', 'qps-plocm',
            'אבג C:\src\报告.txt', 'A😀é', '1,5', '{0} opened {1}',
            'Test-ResourceSyntax', 'Test-ResourceEncoding',
            'Test-RequiredKeys', 'Test-PlaceholderParity',
            'Test-LockedContent', 'Test-PseudoLocale'
        )) {
            $skill | Should -Match ([regex]::Escape($needle))
        }
        (Get-Content -LiteralPath $script:workflow -Raw) | Should -Match 'review-globalization/SKILL\.md'
        (Get-Content -LiteralPath $script:repairWorkflow -Raw) | Should -Match 'Do not load instructions, agents, skills'
    }

    It 'classifies UI, protocol, terminal, and Rust locale surfaces deterministically' {
        $repo = Join-Path $TestDrive 'classifier-repo'
        [System.IO.Directory]::CreateDirectory($repo) | Out-Null
        git -C $repo init --quiet --initial-branch=main
        git -C $repo config user.name 'Globalization Tests'
        git -C $repo config user.email 'globalization@example.test'
        [System.IO.File]::WriteAllText((Join-Path $repo 'baseline.txt'), 'baseline')
        git -C $repo add .
        git -C $repo commit --quiet -m baseline
        $base = (git -C $repo rev-parse HEAD).Trim()

        foreach ($relative in @(
            'src/cascadia/TerminalApp/Sample.xaml',
            'src/cascadia/TerminalApp/Resources/en-US/Resources.resw',
            'src/cascadia/TerminalProtocol/Sample.cpp',
            'src/buffer/out/Sample.cpp',
            'tools/wta/locales/ar-SA.yml'
        )) {
            $path = Join-Path $repo $relative
            [System.IO.Directory]::CreateDirectory((Split-Path $path -Parent)) | Out-Null
            [System.IO.File]::WriteAllText($path, 'sample')
        }
        git -C $repo add .
        git -C $repo commit --quiet -m samples
        $head = (git -C $repo rev-parse HEAD).Trim()
        $output = Join-Path $TestDrive 'context.json'

        Push-Location $repo
        try {
            & pwsh -NoProfile -File $script:classifier -BaseSha $base -HeadSha $head -OutputPath $output
            $LASTEXITCODE | Should -Be 0
        } finally {
            Pop-Location
        }

        $context = Get-Content -LiteralPath $output -Raw | ConvertFrom-Json
        $context.files.Count | Should -Be 5
        @($context.files | Where-Object path -eq 'src/cascadia/TerminalApp/Sample.xaml').surfaces | Should -Contain 'xaml-ui'
        $resource = @($context.files | Where-Object path -eq 'src/cascadia/TerminalApp/Resources/en-US/Resources.resw')
        $resource.surfaces | Should -Contain 'resw'
        $resource.surfaces | Should -Not -Contain 'xaml-ui'
        $resource.checks | Should -Not -Contain 'rtl-layout'
        @($context.files | Where-Object path -eq 'src/cascadia/TerminalProtocol/Sample.cpp').checks | Should -Contain 'machine-token-exclusion'
        @($context.files | Where-Object path -eq 'src/buffer/out/Sample.cpp').checks | Should -Contain 'grapheme-and-cell-width'
        @($context.files | Where-Object path -eq 'tools/wta/locales/ar-SA.yml').checks | Should -Contain 'placeholder-reordering'
    }

    It 'uses immutable fork-safe read-only execution and bounded publication' {
        $worker = Get-Content -LiteralPath $script:workflow -Raw
        $repair = Get-Content -LiteralPath $script:repairWorkflow -Raw
        $controller = Get-Content -LiteralPath $script:controller -Raw
        $worker | Should -Match 'checkout:\s*\r?\n\s*ref: \$\{\{ github\.workflow_sha \}\}'
        $worker | Should -Match 'edit: false'
        $worker | Should -Not -Match "'pwsh:\*'"
        $worker | Should -Not -Match "'git show:\*'"
        $worker | Should -Not -Match "'git diff:\*'"
        $worker | Should -Match 'get_pull_request_diff'
        $worker | Should -Match '(?m)^steps:'
        $worker | Should -Not -Match '(?m)^\s{2}prepare:'
        $worker | Should -Match 'Validate findings and publication shape'
        $worker | Should -Match 'Checkout trusted repository state'
        $worker | Should -Match 'Fetch immutable pull request head'
        $worker | Should -Not -Match '(?m)^post-steps:'
        $worker | Should -Not -Match 'pwsh -NoProfile -File \.github/scripts/ghaw-pr-globalization/'
        $worker | Should -Match 'max: 1'
        $worker | Should -Match 'Reject stale globalization comment'
        $worker | Should -Not -Match '(?m)^\s{8}/tmp/gh-aw/globalization-context\.json$'
        $worker | Should -Not -Match 'push-to-pull-request-branch'
        $repair | Should -Match 'push-to-pull-request-branch'
        $repair | Should -Match 'patch-format: am'
        $repair | Should -Match 'Validate isolated repair patch and live head'
        $repair | Should -Match 'GIT_CONFIG_NOSYSTEM'
        $repair | Should -Not -Match "'pwsh:\*'"
        $repair | Should -Match 'Only added or modified regular text files'
        $repair | Should -Match '40-line automatic repair limit'
        $repair | Should -Match 'within 20 lines of a linked fixed finding'
        $repair | Should -Match 'excluded-files:'
        $repair | Should -Match 'LocalTests'
        $repair | Should -Match 'Validate repair output selection'
        $repair | Should -Not -Match 'add-comment:'
        $repair | Should -Match '(?m)^steps:'
        $repair | Should -Not -Match '(?m)^\s{2}prepare:'
        $repair | Should -Match 'cancel-in-progress: false'
        $repair | Should -Match 'git --no-replace-objects diff'
        $repair | Should -Not -Match '(?m)^\s{8}/tmp/gh-aw/agent/globalization-context\.json$'
        (Get-Content -LiteralPath $script:classifier -Raw) | Should -Match 'git --no-replace-objects diff'
        (Get-Content -LiteralPath $script:localizationWorkflow -Raw) | Should -Match 'cancel-in-progress: false'
        $repair | Should -Match 'HIGH finding with strong'
        $repair | Should -Match '\[globalization-review\]'
        $controller | Should -Match 'pull_request_target'
        $controller | Should -Match 'sameRepo'
        $controller | Should -Match 'ghaw-pr-globalization-repair\.lock\.yml'
        $controller | Should -Match 'aw_context: awContext'
        $controller | Should -Match "item_type: 'pull_request'"
        $controller | Should -Match 'head_ref: process\.env\.HEAD_REF'
        $controller | Should -Match 'persist-credentials: false'
        $controller | Should -Match 'cancel-in-progress: true'
        $controller | Should -Match "'src/inc/\*\*'"
    }

    It 'compiles same-runner context, dispatch environment, and trusted gates' {
        $guideLock = Get-Content -LiteralPath $script:workflowLock -Raw
        $repairLock = Get-Content -LiteralPath $script:repairWorkflowLock -Raw
        foreach ($compiled in @($guideLock, $repairLock)) {
            $compiled | Should -Match '(?:Verify|Validate) immutable'
            $compiled | Should -Match 'HEAD_SHA:'
            $compiled | Should -Match 'BASE_SHA:'
            $compiled | Should -Match 'PR_NUMBER:'
            $compiled | Should -Match 'GH_TOKEN:'
        }
        $guideLock | Should -Match 'globalization-context-safe\.json'
        $guideLock | Should -Not -Match '# --allow-tool shell\(git show'
        $guideLock | Should -Not -Match '--allow-all-tools'
        $guideLock | Should -Match 'Checkout trusted repository state'
        $guideLock | Should -Match 'Fetch immutable pull request head'
        $repairLock | Should -Match 'trusted-validation\.json'
        $repairLock | Should -Match 'git-diff-check'
        $repairLock | Should -Match 'GH_AW_PR_HEAD_BASE_SHA'
        $repairLock | Should -Match 'Validate isolated repair patch and live head'
    }

    It 'pins the custom review agent to the native findings schema' {
        $agent = Get-Content -LiteralPath (Join-Path $script:root '.github\agents\ghaw-pr-globalization.agent.md') -Raw
        foreach ($field in @(
            'stableId', 'confidence', 'sourceSha', 'localeOrScript',
            'proposedFix', 'patchFiles', 'executedValidation', 'resourceChecks'
        )) {
            $agent | Should -Match ([regex]::Escape($field))
        }
        $agent | Should -Match 'strong\|moderate\|weak'
        $agent | Should -Match 'Never emit aliases'
    }
}
