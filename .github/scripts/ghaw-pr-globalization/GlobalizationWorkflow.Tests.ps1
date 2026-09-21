#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }

Describe 'PR globalization workflow' -Tag 'Unit' {
    BeforeAll {
        $script:root = Resolve-Path (Join-Path $PSScriptRoot '..\..\..')
        $script:classifier = Join-Path $PSScriptRoot 'Get-GlobalizationChangeContext.ps1'
        $script:validator = Join-Path $PSScriptRoot 'Test-GlobalizationFindings.ps1'
        $script:workflow = Join-Path $script:root '.github\workflows\ghaw-pr-globalization.md'
        $script:repairWorkflow = Join-Path $script:root '.github\workflows\ghaw-pr-globalization-repair.md'
        $script:controller = Join-Path $script:root '.github\workflows\ghaw-pr-globalization-controller.yml'

        function New-Report {
            param([object[]]$Findings = @(), [string]$Base = ('a' * 40), [string]$Head = ('b' * 40))
            return [ordered]@{ version = 1; baseSha = $Base; headSha = $Head; findings = $Findings }
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
            param($Report, [string]$ExpectedHead = ('b' * 40))
            $path = Join-Path $TestDrive (([guid]::NewGuid().ToString('N')) + '.json')
            [System.IO.File]::WriteAllText($path, ($Report | ConvertTo-Json -Depth 10), [System.Text.UTF8Encoding]::new($false))
            $null = & pwsh -NoProfile -File $script:validator -ReportPath $path -ExpectedBaseSha ('a' * 40) -ExpectedHeadSha $ExpectedHead 2>&1
            return $LASTEXITCODE
        }
    }

    It 'accepts no findings and a fully evidenced HIGH blocker' {
        (Invoke-Validator -Report (New-Report)) | Should -Be 0
        (Invoke-Validator -Report (New-Report -Findings @((New-Finding)))) | Should -Be 0
    }

    It 'rejects malformed output, stale SHA, unsafe paths, and invalid severity gating' {
        (Invoke-Validator -Report ([ordered]@{ version = 1 })) | Should -Not -Be 0
        (Invoke-Validator -Report (New-Report) -ExpectedHead ('c' * 40)) | Should -Not -Be 0

        $unsafe = New-Finding
        $unsafe.file = '../workflow.yml'
        (Invoke-Validator -Report (New-Report -Findings @($unsafe))) | Should -Not -Be 0

        $mediumBlocker = New-Finding -Severity 'MEDIUM' -Confidence 'moderate' -Disposition 'blocked'
        (Invoke-Validator -Report (New-Report -Findings @($mediumBlocker))) | Should -Not -Be 0
    }

    It 'allows fixed disposition only for strong HIGH repair findings' {
        $path = Join-Path $TestDrive 'repair-report.json'
        $fixed = New-Finding
        $fixed.disposition = 'fixed'
        $report = New-Report -Findings @($fixed)
        $report.patchFiles = @(
            [ordered]@{ path = $fixed.file; kind = 'fix'; findingIds = @($fixed.stableId) }
        )
        $report.executedValidation = @(
            [ordered]@{ command = 'focused-test'; exitCode = 0; result = 'passed' }
        )
        [System.IO.File]::WriteAllText($path, ($report | ConvertTo-Json -Depth 10))

        $null = & pwsh -NoProfile -File $script:validator -ReportPath $path `
            -ExpectedBaseSha ('a' * 40) -ExpectedHeadSha ('b' * 40) -Mode repair 2>&1
        $LASTEXITCODE | Should -Be 0
        $null = & pwsh -NoProfile -File $script:validator -ReportPath $path `
            -ExpectedBaseSha ('a' * 40) -ExpectedHeadSha ('b' * 40) -Mode guide 2>&1
        $LASTEXITCODE | Should -Not -Be 0
    }

    It 'rejects fixed findings without passing executed and resource validation' {
        $path = Join-Path $TestDrive 'invalid-fixed-report.json'
        $fixed = New-Finding
        $fixed.disposition = 'fixed'
        $report = New-Report -Findings @($fixed)
        [System.IO.File]::WriteAllText($path, ($report | ConvertTo-Json -Depth 10))
        $null = & pwsh -NoProfile -File $script:validator -ReportPath $path `
            -ExpectedBaseSha ('a' * 40) -ExpectedHeadSha ('b' * 40) -Mode repair 2>&1
        $LASTEXITCODE | Should -Not -Be 0

        $fixed.file = 'src/cascadia/TerminalApp/Resources/en-US/Resources.resw'
        $report = New-Report -Findings @($fixed)
        $report.patchFiles = @(
            [ordered]@{ path = $fixed.file; kind = 'fix'; findingIds = @($fixed.stableId) }
        )
        $report.executedValidation = @(
            [ordered]@{ command = 'resource-check'; exitCode = 0; result = 'passed' }
        )
        $report.resourceChecks = @(
            [ordered]@{ check = 'Test-ResourceSyntax'; status = 'FIXABLE'; exitCode = 20 }
        )
        [System.IO.File]::WriteAllText($path, ($report | ConvertTo-Json -Depth 10))
        $null = & pwsh -NoProfile -File $script:validator -ReportPath $path `
            -ExpectedBaseSha ('a' * 40) -ExpectedHeadSha ('b' * 40) -Mode repair 2>&1
        $LASTEXITCODE | Should -Not -Be 0
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
        (Get-Content -LiteralPath $script:repairWorkflow -Raw) | Should -Match 'review-globalization/SKILL\.md'
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
        $context.files.Count | Should -Be 4
        @($context.files | Where-Object path -eq 'src/cascadia/TerminalApp/Sample.xaml').surfaces | Should -Contain 'xaml-ui'
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
        $worker | Should -Match 'max: 1'
        $worker | Should -Match 'Reject stale worker output'
        $worker | Should -Not -Match 'push-to-pull-request-branch'
        $repair | Should -Match 'push-to-pull-request-branch'
        $repair | Should -Not -Match 'add-comment:'
        $repair | Should -Match 'HIGH finding with strong'
        $repair | Should -Match '\[globalization-review\]'
        $controller | Should -Match 'pull_request_target'
        $controller | Should -Match 'sameRepo'
        $controller | Should -Match 'ghaw-pr-globalization-repair\.lock\.yml'
        $controller | Should -Match 'persist-credentials: false'
        $controller | Should -Match 'cancel-in-progress: true'
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
