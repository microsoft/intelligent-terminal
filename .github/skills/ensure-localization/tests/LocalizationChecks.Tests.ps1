#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }

Describe 'File-based localization checks' -Tag 'Unit' {
    BeforeAll {
        $script:checkerScript = Join-Path $PSScriptRoot '..\scripts\localization_checks.ps1'
        . $script:checkerScript

        function Write-Utf8TextFile {
            param(
                [Parameter(Mandatory)][string]$Path,
                [Parameter(Mandatory)][string]$Content,
                [bool]$WithBom = $false
            )

            $directory = Split-Path -Path $Path -Parent
            if (-not [string]::IsNullOrWhiteSpace($directory)) {
                [System.IO.Directory]::CreateDirectory($directory) | Out-Null
            }

            $bytes = ([System.Text.UTF8Encoding]::new($false)).GetBytes($Content)
            if ($WithBom) {
                $bytes = [byte[]](@(0xEF, 0xBB, 0xBF) + $bytes)
            }

            [System.IO.File]::WriteAllBytes($Path, $bytes)
        }

        function Write-BytesFile {
            param(
                [Parameter(Mandatory)][string]$Path,
                [Parameter(Mandatory)][byte[]]$Bytes
            )

            $directory = Split-Path -Path $Path -Parent
            if (-not [string]::IsNullOrWhiteSpace($directory)) {
                [System.IO.Directory]::CreateDirectory($directory) | Out-Null
            }

            [System.IO.File]::WriteAllBytes($Path, $Bytes)
        }

        function Write-ReswFixtureFile {
            param(
                [Parameter(Mandatory)][string]$Path,
                [AllowEmptyCollection()][object[]]$Resources,
                [bool]$WithBom = $true
            )

            $builder = [System.Text.StringBuilder]::new()
            [void]$builder.AppendLine('<?xml version="1.0" encoding="utf-8"?>')
            [void]$builder.AppendLine('<root>')
            foreach ($resource in $Resources) {
                $name = [System.Security.SecurityElement]::Escape([string]$resource.Name)
                $value = [System.Security.SecurityElement]::Escape([string]$resource.Value)
                [void]$builder.AppendLine("  <data name=""$name"" xml:space=""preserve"">")
                [void]$builder.AppendLine("    <value>$value</value>")
                if ($resource.ContainsKey('Comment') -and $null -ne $resource.Comment) {
                    $comment = [System.Security.SecurityElement]::Escape([string]$resource.Comment)
                    [void]$builder.AppendLine("    <comment>$comment</comment>")
                }
                [void]$builder.AppendLine('  </data>')
            }
            [void]$builder.AppendLine('</root>')

            Write-Utf8TextFile -Path $Path -Content $builder.ToString() -WithBom $WithBom
        }

        function Write-WtaFixtureFile {
            param(
                [Parameter(Mandatory)][string]$Path,
                [Parameter(Mandatory)][object[]]$Entries,
                [bool]$WithBom = $false
            )

            $lines = [System.Collections.Generic.List[string]]::new()
            foreach ($entry in $Entries) {
                foreach ($comment in @($entry.Comments)) {
                    $lines.Add("# $comment")
                }

                $value = ConvertTo-Json -InputObject ([string]$entry.Value) -Compress
                $line = '{0}: {1}' -f $entry.Name, $value
                if ($entry.ContainsKey('InlineComment') -and $null -ne $entry.InlineComment) {
                    $line = '{0}  # {1}' -f $line, $entry.InlineComment
                }
                $lines.Add($line)
            }

            Write-Utf8TextFile -Path $Path -Content ($lines -join "`n") -WithBom $WithBom
        }

        function Invoke-CheckerCli {
            param([Parameter(Mandatory)][string[]]$Arguments)

            $stdoutPath = Join-Path $TestDrive ("{0}.stdout.json" -f ([guid]::NewGuid().ToString('N')))
            $stderrPath = Join-Path $TestDrive ("{0}.stderr.txt" -f ([guid]::NewGuid().ToString('N')))

            & pwsh -NoLogo -NoProfile -File $script:checkerScript @Arguments 1> $stdoutPath 2> $stderrPath
            $exitCode = $LASTEXITCODE

            return [pscustomobject]@{
                ExitCode = $exitCode
                StdOut = if (Test-Path -LiteralPath $stdoutPath) { Get-Content -LiteralPath $stdoutPath -Raw } else { '' }
                Stderr = if (Test-Path -LiteralPath $stderrPath) { Get-Content -LiteralPath $stderrPath -Raw } else { '' }
            }
        }

    }

    It 'keeps caller preferences unchanged and CLI emits JSON with native exit codes' {
        & {
            $before = $ErrorActionPreference
            Set-StrictMode -Version 3.0
            . $script:checkerScript
            $ErrorActionPreference | Should -Be $before
            { $null = $undefinedVariableFromStrictModeProbe } | Should -Throw
        }

        $sourcePath = Join-Path $TestDrive 'cli\source.resw'
        $targetPath = Join-Path $TestDrive 'cli\target.resw'
        Write-ReswFixtureFile -Path $sourcePath -Resources @(
            @{ Name = 'hello'; Value = 'Hello' }
        )
        Write-ReswFixtureFile -Path $targetPath -Resources @()

        $fixable = Invoke-CheckerCli -Arguments @(
            '-Check', 'Test-RequiredKeys',
            '-SourceFile', $sourcePath,
            '-TargetFile', $targetPath
        )
        $fixable.ExitCode | Should -Be 20
        [string]::IsNullOrEmpty($fixable.Stderr) | Should -BeTrue
        ($fixable.StdOut | ConvertFrom-Json).status | Should -Be 'FIXABLE'

        $removed = Invoke-CheckerCli -Arguments @('-Check', 'Test-SourceUnchanged')
        $removed.ExitCode | Should -Be 64
        [string]::IsNullOrEmpty($removed.Stderr) | Should -BeTrue
        ($removed.StdOut | ConvertFrom-Json).status | Should -Be 'INVALID_INPUT'

        $invalid = Invoke-CheckerCli -Arguments @('-Check', 'Not-A-Check')
        $invalid.ExitCode | Should -Be 64
        [string]::IsNullOrEmpty($invalid.Stderr) | Should -BeTrue
        ($invalid.StdOut | ConvertFrom-Json).status | Should -Be 'INVALID_INPUT'
    }

    It 'accepts single and multi-key KeysJson CLI scope and rejects ambiguous CLI payloads' {
        $sourcePath = Join-Path $TestDrive 'cli-scope\source.resw'
        $targetPath = Join-Path $TestDrive 'cli-scope\target.resw'

        Write-ReswFixtureFile -Path $sourcePath -Resources @(
            @{ Name = 'plain'; Value = 'Plain' }
            @{ Name = 'comma,key'; Value = 'Comma key' }
            @{ Name = '日本語'; Value = 'Japanese key' }
        )
        Write-ReswFixtureFile -Path $targetPath -Resources @(
            @{ Name = 'plain'; Value = 'Translated plain' }
        )

        $single = Invoke-CheckerCli -Arguments @(
            '-Check', 'Test-RequiredKeys',
            '-SourceFile', $sourcePath,
            '-TargetFile', $targetPath,
            '-KeysJson', '["plain"]'
        )
        $single.ExitCode | Should -Be 0
        ($single.StdOut | ConvertFrom-Json).status | Should -Be 'PASS'

        $secondFailure = Invoke-CheckerCli -Arguments @(
            '-Check', 'Test-RequiredKeys',
            '-SourceFile', $sourcePath,
            '-TargetFile', $targetPath,
            '-KeysJson', '["plain","comma,key"]'
        )
        $secondFailure.ExitCode | Should -Be 20
        @($secondFailure.StdOut | ConvertFrom-Json).results.resource | Should -Be @('comma,key')

        $scoped = Invoke-CheckerCli -Arguments @(
            '-Check', 'Test-RequiredKeys',
            '-SourceFile', $sourcePath,
            '-TargetFile', $targetPath,
            '-KeysJson', '["plain","comma,key","日本語"]'
        )
        $scoped.ExitCode | Should -Be 20
        $scopedBundle = $scoped.StdOut | ConvertFrom-Json
        @($scopedBundle.results.resource | Sort-Object) | Should -Be @('comma,key', '日本語')

        $empty = Invoke-CheckerCli -Arguments @(
            '-Check', 'Test-RequiredKeys',
            '-SourceFile', $sourcePath,
            '-TargetFile', $targetPath,
            '-KeysJson', '[]'
        )
        $empty.ExitCode | Should -Be 64
        ($empty.StdOut | ConvertFrom-Json).status | Should -Be 'INVALID_INPUT'

        $invalidJson = Invoke-CheckerCli -Arguments @(
            '-Check', 'Test-RequiredKeys',
            '-SourceFile', $sourcePath,
            '-TargetFile', $targetPath,
            '-KeysJson', '{not-json}'
        )
        $invalidJson.ExitCode | Should -Be 64
        ($invalidJson.StdOut | ConvertFrom-Json).status | Should -Be 'INVALID_INPUT'

        foreach ($scalarJson in @('"plain"', '42', 'null')) {
            $invalidScalar = Invoke-CheckerCli -Arguments @(
                '-Check', 'Test-RequiredKeys',
                '-SourceFile', $sourcePath,
                '-TargetFile', $targetPath,
                '-KeysJson', $scalarJson
            )
            $invalidScalar.ExitCode | Should -Be 64
            ($invalidScalar.StdOut | ConvertFrom-Json).message | Should -Match 'JSON array of strings'
        }

        $extraArg = Invoke-CheckerCli -Arguments @(
            '-Check', 'Test-RequiredKeys',
            '-SourceFile', $sourcePath,
            '-TargetFile', $targetPath,
            '-KeysJson', '["plain"]',
            'unexpected-positional'
        )
        $extraArg.ExitCode | Should -Be 64
        ($extraArg.StdOut | ConvertFrom-Json).message | Should -Match 'Unexpected positional argument'
    }

    It 'rejects invalid PowerShell -Keys arrays instead of fabricating scoped PASS results' {
        $sourcePath = Join-Path $TestDrive 'powershell-scope\source.resw'
        $targetPath = Join-Path $TestDrive 'powershell-scope\target.resw'

        Write-ReswFixtureFile -Path $sourcePath -Resources @(
            @{ Name = 'alpha'; Value = 'Alpha' }
        )
        Write-ReswFixtureFile -Path $targetPath -Resources @(
            @{ Name = 'alpha'; Value = 'Alpha translated' }
        )

        foreach ($badKeys in @(
            [string[]]@(),
            [string[]]@($null),
            [string[]]@(''),
            [string[]]@('   '),
            [string[]]@('alpha', $null)
        )) {
            foreach ($check in @(
                { param($keys) Test-RequiredKeys -SourceFile $sourcePath -TargetFile $targetPath -Keys $keys },
                { param($keys) Test-PlaceholderParity -SourceFile $sourcePath -TargetFile $targetPath -Keys $keys },
                { param($keys) Test-LockedContent -SourceFile $sourcePath -TargetFile $targetPath -Locale 'fr-FR' -Keys $keys },
                { param($keys) Test-PseudoLocale -SourceFile $sourcePath -TargetFile $targetPath -Locale 'qps-ploc' -Keys $keys }
            )) {
                $bundle = & $check $badKeys
                $bundle.status | Should -Be 'INVALID_INPUT'
                $bundle.exitCode | Should -Be 64
                $bundle.message | Should -Match 'non-empty array of non-blank strings'
            }
        }
    }

    It 'blocks malformed or non-resource XML and unsupported YAML structures' {
        $badXml = Join-Path $TestDrive 'syntax\bad.resw'
        $wrongRootXml = Join-Path $TestDrive 'syntax\wrong-root.resw'
        $wrongStructureXml = Join-Path $TestDrive 'syntax\wrong-structure.resw'
        $emptyRootXml = Join-Path $TestDrive 'syntax\empty.resw'
        $badYaml = Join-Path $TestDrive 'syntax\bad.yml'
        $nestedAfterScalarYaml = Join-Path $TestDrive 'syntax\nested-after-scalar.yml'
        $mappingLikeScalarYaml = Join-Path $TestDrive 'syntax\mapping-like-scalar.yml'
        $nullScalarYaml = Join-Path $TestDrive 'syntax\null-scalar.yml'
        $booleanScalarYaml = Join-Path $TestDrive 'syntax\boolean-scalar.yml'
        $numericScalarYaml = Join-Path $TestDrive 'syntax\numeric-scalar.yml'
        $validPlainYaml = Join-Path $TestDrive 'syntax\valid-plain.yml'
        $validQuotedYaml = Join-Path $TestDrive 'syntax\valid-quoted.yml'

        Write-Utf8TextFile -Path $badXml -Content '<root><data name="x"><value>oops</data></root>' -WithBom $true
        Write-Utf8TextFile -Path $wrongRootXml -Content '<foo><bar>oops</bar></foo>' -WithBom $true
        Write-Utf8TextFile -Path $wrongStructureXml -Content '<root><foo><bar>oops</bar></foo></root>' -WithBom $true
        Write-Utf8TextFile -Path $emptyRootXml -Content "<?xml version=""1.0"" encoding=""utf-8""?><root />" -WithBom $true
        Write-Utf8TextFile -Path $badYaml -Content "title:`n  nested: ""nope""" -WithBom $false
        Write-Utf8TextFile -Path $nestedAfterScalarYaml -Content "parent: value`n  child: nested" -WithBom $false
        Write-Utf8TextFile -Path $mappingLikeScalarYaml -Content 'title: value: nope' -WithBom $false
        Write-Utf8TextFile -Path $nullScalarYaml -Content 'title: null' -WithBom $false
        Write-Utf8TextFile -Path $booleanScalarYaml -Content 'title: false' -WithBom $false
        Write-Utf8TextFile -Path $numericScalarYaml -Content 'title: 42' -WithBom $false
        Write-Utf8TextFile -Path $validPlainYaml -Content 'title: Common plain text https://example.test/a:b' -WithBom $false
        Write-Utf8TextFile -Path $validQuotedYaml -Content "title: ""value: nope""`nnullText: 'null'" -WithBom $false

        (Test-ResourceSyntax -File $badXml).status | Should -Be 'BLOCKED'
        (Test-ResourceSyntax -File $wrongRootXml).status | Should -Be 'BLOCKED'
        (Test-ResourceSyntax -File $wrongStructureXml).status | Should -Be 'BLOCKED'
        (Test-ResourceSyntax -File $emptyRootXml).status | Should -Be 'PASS'
        (Test-ResourceSyntax -File $badYaml).status | Should -Be 'BLOCKED'
        (Test-ResourceSyntax -File $nestedAfterScalarYaml).status | Should -Be 'BLOCKED'
        (Test-ResourceSyntax -File $mappingLikeScalarYaml).status | Should -Be 'BLOCKED'
        (Test-ResourceSyntax -File $nullScalarYaml).status | Should -Be 'BLOCKED'
        (Test-ResourceSyntax -File $booleanScalarYaml).status | Should -Be 'BLOCKED'
        (Test-ResourceSyntax -File $numericScalarYaml).status | Should -Be 'BLOCKED'
        (Test-ResourceSyntax -File $validPlainYaml).status | Should -Be 'PASS'
        (Test-ResourceSyntax -File $validQuotedYaml).status | Should -Be 'PASS'
    }

    It 'reports missing and stale keys, but scoped keys avoid unrelated debt' {
        $sourcePath = Join-Path $TestDrive 'keys\source.resw'
        $targetPath = Join-Path $TestDrive 'keys\target.resw'
        $caseSourcePath = Join-Path $TestDrive 'keys\case-source.yml'
        $caseTargetPath = Join-Path $TestDrive 'keys\case-target.yml'
        $distinctReswPath = Join-Path $TestDrive 'keys\distinct.resw'
        $distinctWtaPath = Join-Path $TestDrive 'keys\distinct.yml'

        Write-ReswFixtureFile -Path $sourcePath -Resources @(
            @{ Name = 'alpha'; Value = 'Alpha' }
            @{ Name = 'beta'; Value = 'Beta' }
        )
        Write-ReswFixtureFile -Path $targetPath -Resources @(
            @{ Name = 'alpha'; Value = 'Alpha translated' }
            @{ Name = 'gamma'; Value = 'Stale' }
        )

        $full = Test-RequiredKeys -SourceFile $sourcePath -TargetFile $targetPath
        $full.status | Should -Be 'FIXABLE'
        @($full.results.resource | Sort-Object) | Should -Be @('beta', 'gamma')

        $scoped = Test-RequiredKeys -SourceFile $sourcePath -TargetFile $targetPath -Keys 'alpha'
        $scoped.status | Should -Be 'PASS'

        Write-WtaFixtureFile -Path $caseSourcePath -Entries @(
            @{ Name = 'Title'; Value = 'Title'; Comments = @() }
        )
        Write-WtaFixtureFile -Path $caseTargetPath -Entries @(
            @{ Name = 'title'; Value = 'Translated title' }
        )

        $caseMismatch = Test-RequiredKeys -SourceFile $caseSourcePath -TargetFile $caseTargetPath
        $caseMismatch.status | Should -Be 'FIXABLE'
        @($caseMismatch.results).Count | Should -Be 2
        @($caseMismatch.results | Where-Object { $_.resource -ceq 'Title' }).Count | Should -Be 1
        @($caseMismatch.results | Where-Object { $_.resource -ceq 'title' }).Count | Should -Be 1

        $scopedCaseMismatch = Invoke-CheckerCli -Arguments @(
            '-Check', 'Test-RequiredKeys',
            '-SourceFile', $caseSourcePath,
            '-TargetFile', $caseTargetPath,
            '-KeysJson', '["Title"]'
        )
        $scopedCaseMismatch.ExitCode | Should -Be 20
        $scopedCaseBundle = $scopedCaseMismatch.StdOut | ConvertFrom-Json
        @($scopedCaseBundle.results.resource) | Should -Be @('Title')

        Write-ReswFixtureFile -Path $distinctReswPath -Resources @(
            @{ Name = 'Title'; Value = 'Upper' }
            @{ Name = 'title'; Value = 'Lower' }
        )
        Write-WtaFixtureFile -Path $distinctWtaPath -Entries @(
            @{ Name = 'Title'; Value = 'Upper'; Comments = @() }
            @{ Name = 'title'; Value = 'Lower'; Comments = @() }
        )

        (Test-ResourceSyntax -File $distinctReswPath).status | Should -Be 'PASS'
        (Test-ResourceSyntax -File $distinctWtaPath).status | Should -Be 'PASS'
        (Test-RequiredKeys -SourceFile $distinctReswPath -TargetFile $distinctReswPath).status | Should -Be 'PASS'
        (Test-RequiredKeys -SourceFile $distinctWtaPath -TargetFile $distinctWtaPath).status | Should -Be 'PASS'
    }

    It 'tracks placeholder counts and ignores escaped literal braces' {
        $sourcePath = Join-Path $TestDrive 'placeholders\source.resw'
        $targetPath = Join-Path $TestDrive 'placeholders\target.resw'
        $caseMismatchTargetPath = Join-Path $TestDrive 'placeholders\case-mismatch-target.resw'
        $reorderedTargetPath = Join-Path $TestDrive 'placeholders\reordered-target.resw'

        Write-ReswFixtureFile -Path $sourcePath -Resources @(
            @{ Name = 'message'; Value = 'Hello {{user}} {0} {0} {} %{Name} %{name} %{path} {name} %s %%s %%%s' }
        )
        Write-ReswFixtureFile -Path $targetPath -Resources @(
            @{ Name = 'message'; Value = 'Bonjour {{user}} {0} {} %{Name} %{name} %%s' }
        )
        Write-ReswFixtureFile -Path $caseMismatchTargetPath -Resources @(
            @{ Name = 'message'; Value = 'Bonjour {{user}} {0} {0} {} %{Name} %{Name} %{path} {name} %s %%s' }
        )
        Write-ReswFixtureFile -Path $reorderedTargetPath -Resources @(
            @{ Name = 'message'; Value = 'Bonjour {} %{path} %{name} {0} %s %{Name} {0} {name} {{user}} %%%s %%s' }
        )

        $result = Test-PlaceholderParity -SourceFile $sourcePath -TargetFile $targetPath
        $result.status | Should -Be 'FIXABLE'
        $result.results[0].expected | Should -Be '%s × 2, %{Name}, %{name}, %{path}, {0} × 2, {}'
        $result.results[0].observed | Should -Be '%{Name}, %{name}, {0}, {}'

        $caseMismatch = Test-PlaceholderParity -SourceFile $sourcePath -TargetFile $caseMismatchTargetPath
        $caseMismatch.status | Should -Be 'FIXABLE'
        $caseMismatch.results[0].expected | Should -Be '%s × 2, %{Name}, %{name}, %{path}, {0} × 2, {}'
        $caseMismatch.results[0].observed | Should -Be '%s, %{Name} × 2, %{path}, {0} × 2, {}'

        (Test-PlaceholderParity -SourceFile $sourcePath -TargetFile $reorderedTargetPath).status | Should -Be 'PASS'
    }

    It 'enforces full and token locks from source annotations without inventing absent tokens' {
        $sourcePath = Join-Path $TestDrive 'locks\source.yml'
        $targetPath = Join-Path $TestDrive 'locks\target.yml'
        $literalSourcePath = Join-Path $TestDrive 'locks\literal-source.resw'
        $literalTargetPath = Join-Path $TestDrive 'locks\literal-target.resw'
        $legacyTokenSourcePath = Join-Path $TestDrive 'locks\legacy-token-source.resw'
        $legacyTokenTargetPath = Join-Path $TestDrive 'locks\legacy-token-target.resw'
        $localeScopedSourcePath = Join-Path $TestDrive 'locks\locale-scoped-source.resw'
        $localeScopedTargetPath = Join-Path $TestDrive 'locks\locale-scoped-target.resw'
        $ambiguousSourcePath = Join-Path $TestDrive 'locks\ambiguous-source.resw'
        $ambiguousTargetPath = Join-Path $TestDrive 'locks\ambiguous-target.resw'
        $fileSourcePath = Join-Path $TestDrive 'locks\file-level-source.yml'
        $fileTargetPath = Join-Path $TestDrive 'locks\file-level-target.yml'
        $leadingScopedSourcePath = Join-Path $TestDrive 'locks\leading-scoped-source.yml'
        $leadingScopedTargetPath = Join-Path $TestDrive 'locks\leading-scoped-target.yml'

        Write-WtaFixtureFile -Path $sourcePath -Entries @(
            @{ Name = 'intro'; Value = 'Intro'; Comments = @() }
            @{ Name = 'title'; Value = 'Intelligent Terminal'; Comments = @('{Locked=qps-ploc}') }
            @{ Name = 'action'; Value = 'Press Enter to continue'; Comments = @('{Locked="Enter"}') }
            @{ Name = 'note'; Value = 'Hello there'; Comments = @('{Locked="NotInSource"}') }
        )
        Write-WtaFixtureFile -Path $targetPath -Entries @(
            @{ Name = 'intro'; Value = 'Intro' }
            @{ Name = 'title'; Value = '[Translated title]' }
            @{ Name = 'action'; Value = 'Appuyez pour continuer' }
            @{ Name = 'note'; Value = 'Bonjour là-bas' }
        )

        $result = Test-LockedContent -SourceFile $sourcePath -TargetFile $targetPath -Locale 'qps-ploc' -Keys @('title', 'action', 'note')
        $result.status | Should -Be 'FIXABLE'
        @($result.results.resource | Sort-Object) | Should -Be @('action', 'title')

        $repoRoot = Resolve-Path (Join-Path $PSScriptRoot '..\..\..\..')
        $repoSourcePath = Join-Path $repoRoot 'src\cascadia\TerminalSettingsEditor\Resources\en-US\Resources.resw'
        $repoSource = Read-LocalizationFile -Path $repoSourcePath
        $repoEntry = $repoSource.Entries['Profile_PathTranslationStyleWsl.Content']
        $repoPolicy = Resolve-LockPolicy -Comments $repoEntry.Comments -InheritedTokenComments $repoEntry.InheritedTokenComments
        @($repoPolicy.Tokens) | Should -Be @('/mnt/c', 'C:\', 'WSL')

        Write-ReswFixtureFile -Path $literalSourcePath -Resources @(
            @{
                Name = 'path'
                Value = 'WSL uses C:\, \\server\share, %{items}, and an "inner quoted token".'
                Comment = '{Locked="C:\","\\server\share","%{items}","an "inner quoted token""}'
            }
        )
        Write-ReswFixtureFile -Path $literalTargetPath -Resources @(
            @{
                Name = 'path'
                Value = 'WSL utilise \\server\share, %{items} et an "inner quoted token".'
            }
        )

        $literal = Test-LockedContent -SourceFile $literalSourcePath -TargetFile $literalTargetPath
        $literal.status | Should -Be 'FIXABLE'
        @($literal.results.resource) | Should -Be @('path')
        $literal.results[0].expected | Should -Be 'C:\'

        Write-ReswFixtureFile -Path $legacyTokenSourcePath -Resources @(
            @{
                Name = 'answerback'
                Value = 'ENQ (Request Terminal Status) response'
                Comment = '{Locked=ENQ}{Locked="Request Terminal Status"}'
            }
        )
        Write-ReswFixtureFile -Path $legacyTokenTargetPath -Resources @(
            @{
                Name = 'answerback'
                Value = 'REQ (Request Terminal Status) response'
            }
        )

        $legacyToken = Test-LockedContent -SourceFile $legacyTokenSourcePath -TargetFile $legacyTokenTargetPath
        $legacyToken.status | Should -Be 'FIXABLE'
        @($legacyToken.results.resource) | Should -Be @('answerback')
        $legacyToken.results[0].expected | Should -Be 'ENQ'

        $legacyPolicy = Resolve-LockPolicy -Comments @('{Locked=ENQ}') -InheritedTokenComments @() -Locale 'de-DE'
        $legacyPolicy.NeedsLocale | Should -BeFalse
        $legacyPolicy.FullLock | Should -BeFalse
        @($legacyPolicy.Tokens) | Should -Be @('ENQ')
        (Test-IsLocaleScopeName -Value 'fr') | Should -BeTrue
        (Test-IsLocaleScopeName -Value 'ja') | Should -BeTrue
        (Test-IsLocaleScopeName -Value 'fr-FR') | Should -BeTrue
        (Test-IsLocaleScopeName -Value 'qps-ploc') | Should -BeTrue
        (Test-IsLocaleScopeName -Value 'ENQ') | Should -BeFalse

        Write-ReswFixtureFile -Path $localeScopedSourcePath -Resources @(
            @{
                Name = 'product'
                Value = 'Intelligent Terminal'
                Comment = '{Locked=fr}'
            }
        )
        Write-ReswFixtureFile -Path $localeScopedTargetPath -Resources @(
            @{
                Name = 'product'
                Value = 'Terminal intelligent'
            }
        )

        $neutralLocalePolicy = Resolve-LockPolicy -Comments @('{Locked=fr}') -InheritedTokenComments @() -Locale 'fr'
        $neutralLocalePolicy.NeedsLocale | Should -BeTrue
        $neutralLocalePolicy.FullLock | Should -BeTrue
        @($neutralLocalePolicy.Tokens) | Should -Be @()

        $unrelatedNeutralLocalePolicy = Resolve-LockPolicy -Comments @('{Locked=fr}') -InheritedTokenComments @() -Locale 'ja'
        $unrelatedNeutralLocalePolicy.NeedsLocale | Should -BeTrue
        $unrelatedNeutralLocalePolicy.FullLock | Should -BeFalse
        @($unrelatedNeutralLocalePolicy.Tokens) | Should -Be @()

        (Test-LockedContent -SourceFile $localeScopedSourcePath -TargetFile $localeScopedTargetPath -Locale 'ja').status | Should -Be 'PASS'

        Write-ReswFixtureFile -Path $ambiguousSourcePath -Resources @(
            @{
                Name = 'protocol'
                Value = 'ENQ'
                Comment = '{Locked=ENQ,fr-FR}'
            }
        )
        Write-ReswFixtureFile -Path $ambiguousTargetPath -Resources @(
            @{
                Name = 'protocol'
                Value = 'REQ'
            }
        )

        $ambiguous = Test-LockedContent -SourceFile $ambiguousSourcePath -TargetFile $ambiguousTargetPath -Locale 'fr-FR'
        $ambiguous.status | Should -Be 'BLOCKED'
        $ambiguous.results[0].observed | Should -Match 'canonical quoted token form|quote literal tokens'

        Write-Utf8TextFile -Path $fileSourcePath -Content @'
# {Locked="Intelligent Terminal"}

setup.title: "Intelligent Terminal setup"
setup.subtitle: "Welcome"

# ── Auth flow (src/ui/auth.rs) ──────────────────────────────────────────────
auth.brand: "Open Intelligent Terminal Settings"
auth.plain: "Continue"
'@
        Write-Utf8TextFile -Path $fileTargetPath -Content @'
# {Locked="Intelligent Terminal"}

setup.title: "Terminal setup"
setup.subtitle: "Welkom"

# ── Auth flow (src/ui/auth.rs) ──────────────────────────────────────────────
auth.brand: "Open Terminal Settings"
auth.plain: "Gaan voort"
'@

        $fileLevel = Test-LockedContent -SourceFile $fileSourcePath -TargetFile $fileTargetPath
        $fileLevel.status | Should -Be 'FIXABLE'
        @($fileLevel.results.resource | Sort-Object) | Should -Be @('auth.brand', 'setup.title')

        Write-Utf8TextFile -Path $leadingScopedSourcePath -Content @'
# {Locked=qps-ploc}
setup.title: "Welcome"
auth.prompt: "Continue"
'@
        Write-Utf8TextFile -Path $leadingScopedTargetPath -Content @'
setup.title: "Welcome"
auth.prompt: "Continue"
'@

        $leadingScoped = Test-PseudoLocale -SourceFile $leadingScopedSourcePath -TargetFile $leadingScopedTargetPath -Locale 'qps-ploc'
        $leadingScoped.status | Should -Be 'FIXABLE'
        @($leadingScoped.results.resource | Sort-Object) | Should -Be @('auth.prompt')

        $policy = Resolve-LockPolicy -Comments @('{Locked=qps-ploc}') -InheritedTokenComments @() -Locale ''
        $policy.NeedsLocale | Should -BeTrue
        $policy.FullLock | Should -BeFalse
        [string]::IsNullOrWhiteSpace($policy.BlockedReason) | Should -BeTrue
    }

    It 'validates UTF-8, requires BOM for new RESW files, and preserves BOM state when a snapshot is supplied' {
        $originalPath = Join-Path $TestDrive 'encoding\original.resw'
        $currentPath = Join-Path $TestDrive 'encoding\current.resw'
        $legacyOriginalPath = Join-Path $TestDrive 'encoding\legacy-original.resw'
        $legacyCurrentPath = Join-Path $TestDrive 'encoding\legacy-current.resw'
        $newReswPath = Join-Path $TestDrive 'encoding\new.resw'
        $invalidPath = Join-Path $TestDrive 'encoding\invalid.yml'

        Write-ReswFixtureFile -Path $originalPath -Resources @(
            @{ Name = 'key'; Value = 'Value' }
        ) -WithBom $true
        Write-ReswFixtureFile -Path $currentPath -Resources @(
            @{ Name = 'key'; Value = 'Value' }
        ) -WithBom $false
        Write-ReswFixtureFile -Path $legacyOriginalPath -Resources @(
            @{ Name = 'key'; Value = 'Value' }
        ) -WithBom $false
        Write-ReswFixtureFile -Path $legacyCurrentPath -Resources @(
            @{ Name = 'key'; Value = 'Value' }
        ) -WithBom $false
        Write-ReswFixtureFile -Path $newReswPath -Resources @(
            @{ Name = 'key'; Value = 'Value' }
        ) -WithBom $false
        Write-BytesFile -Path $invalidPath -Bytes ([byte[]](0xFF, 0xFE, 0x00))

        (Test-ResourceEncoding -File $newReswPath).status | Should -Be 'FIXABLE'
        (Test-ResourceEncoding -File $currentPath -OriginalFile $originalPath).status | Should -Be 'FIXABLE'
        (Test-ResourceEncoding -File $legacyCurrentPath -OriginalFile $legacyOriginalPath).status | Should -Be 'PASS'
        (Test-ResourceEncoding -File $invalidPath).status | Should -Be 'BLOCKED'
    }

    It 'flags pseudo-locale fallback and wrapper mistakes without forcing WTA wrappers onto RESW' {
        $wtaSource = Join-Path $TestDrive 'pseudo\source.yml'
        $wtaTarget = Join-Path $TestDrive 'pseudo\target.yml'
        $wtaScopedSource = Join-Path $TestDrive 'pseudo\scoped-source.yml'
        $wtaScopedTarget = Join-Path $TestDrive 'pseudo\scoped-target.yml'
        $wtaPassTarget = Join-Path $TestDrive 'pseudo\pass-target.yml'
        $wtaMirrorSource = Join-Path $TestDrive 'pseudo\mirror-source.yml'
        $wtaMirrorTarget = Join-Path $TestDrive 'pseudo\mirror-target.yml'
        $reswSource = Join-Path $TestDrive 'pseudo\source.resw'
        $reswTarget = Join-Path $TestDrive 'pseudo\target.resw'
        $reswWrappedEnglishSource = Join-Path $TestDrive 'pseudo\wrapped-source.resw'
        $reswWrappedEnglishTarget = Join-Path $TestDrive 'pseudo\wrapped-target.resw'

        Write-WtaFixtureFile -Path $wtaSource -Entries @(
            @{ Name = 'welcome'; Value = 'Welcome'; Comments = @() }
            @{ Name = 'prompt'; Value = 'Open %{name}'; Comments = @() }
            @{ Name = 'locked'; Value = '[x]'; Comments = @('{Locked}') }
        )
        Write-WtaFixtureFile -Path $wtaTarget -Entries @(
            @{ Name = 'welcome'; Value = '[Welcome]' }
            @{ Name = 'prompt'; Value = '[Öpen %{name}]' }
            @{ Name = 'locked'; Value = '[x]' }
        )

        $wta = Test-PseudoLocale -SourceFile $wtaSource -TargetFile $wtaTarget -Locale 'qps-ploc'
        $wta.status | Should -Be 'FIXABLE'
        @($wta.results.resource | Sort-Object) | Should -Be @('welcome')

        Write-WtaFixtureFile -Path $wtaPassTarget -Entries @(
            @{ Name = 'welcome'; Value = '[Ŵēļçömē]' }
            @{ Name = 'prompt'; Value = '[Öpen %{name}]' }
            @{ Name = 'locked'; Value = '[x]' }
        )

        (Test-PseudoLocale -SourceFile $wtaSource -TargetFile $wtaPassTarget -Locale 'qps-ploc').status | Should -Be 'PASS'

        Write-Utf8TextFile -Path $wtaScopedSource -Content @'
# Setup screen titles
# {Locked=qps-ploc,qps-ploca,qps-plocm}
setup.title.locked: "Agent not found"

auth.prompt: "Welcome back"
'@
        Write-Utf8TextFile -Path $wtaScopedTarget -Content @'
# Setup screen titles
# {Locked=qps-ploc,qps-ploca,qps-plocm}
setup.title.locked: "Agent not found"

auth.prompt: "Welcome back"
'@

        $scoped = Test-PseudoLocale -SourceFile $wtaScopedSource -TargetFile $wtaScopedTarget -Locale 'qps-ploc'
        $scoped.status | Should -Be 'FIXABLE'
        @($scoped.results.resource | Sort-Object) | Should -Be @('auth.prompt')

        $rtlMark = [string][char]0x200F
        Write-WtaFixtureFile -Path $wtaMirrorSource -Entries @(
            @{ Name = 'welcome'; Value = 'Welcome'; Comments = @() }
        )
        Write-WtaFixtureFile -Path $wtaMirrorTarget -Entries @(
            @{ Name = 'welcome'; Value = ('[!! {0}Welcome{0} !!]' -f $rtlMark) }
        )

        (Test-PseudoLocale -SourceFile $wtaMirrorSource -TargetFile $wtaMirrorTarget -Locale 'qps-plocm').status | Should -Be 'PASS'

        Write-ReswFixtureFile -Path $reswSource -Resources @(
            @{ Name = 'welcome'; Value = 'Welcome'; Comment = '{Locked=qps-ploc}' }
            @{ Name = 'subtitle'; Value = 'Try again'; Comment = '{Locked="again"}' }
        )
        Write-ReswFixtureFile -Path $reswTarget -Resources @(
            @{ Name = 'welcome'; Value = 'Welcome' }
            @{ Name = 'subtitle'; Value = 'Ţřý again' }
        )

        (Test-PseudoLocale -SourceFile $reswSource -TargetFile $reswTarget -Locale 'qps-ploc').status | Should -Be 'PASS'

        Write-ReswFixtureFile -Path $reswWrappedEnglishSource -Resources @(
            @{ Name = 'welcome'; Value = 'Welcome' }
        )
        Write-ReswFixtureFile -Path $reswWrappedEnglishTarget -Resources @(
            @{ Name = 'welcome'; Value = '[Welcome]' }
        )

        (Test-PseudoLocale -SourceFile $reswWrappedEnglishSource -TargetFile $reswWrappedEnglishTarget -Locale 'qps-ploc').status | Should -Be 'FIXABLE'
    }

    It 'ignores distinct equal-length invariant tokens across pseudo wrappers' {
        $sourcePath = Join-Path $TestDrive 'pseudo-equal-length\source.yml'
        $rtlMark = [string][char]0x200F

        Write-WtaFixtureFile -Path $sourcePath -Entries @(
            @{ Name = 'invariant'; Value = '%{name} %{path} B D'; Comments = @('{Locked="B","D"}') }
            @{ Name = 'mixed'; Value = 'Open %{name} %{path} B D'; Comments = @('{Locked="B","D"}') }
            @{ Name = 'fullyLocked'; Value = 'C'; Comments = @('{Locked}') }
        )

        foreach ($case in @(
            @{
                Locale = 'qps-ploc'
                TargetEntries = @(
                    @{ Name = 'invariant'; Value = '[%{name} %{path} B D]' }
                    @{ Name = 'mixed'; Value = '[Öpen %{name} %{path} B D]' }
                    @{ Name = 'fullyLocked'; Value = 'C' }
                )
            },
            @{
                Locale = 'qps-ploca'
                TargetEntries = @(
                    @{ Name = 'invariant'; Value = '[!!_%{name} %{path} B D_!!]' }
                    @{ Name = 'mixed'; Value = '[!!_Öpen %{name} %{path} B D_!!]' }
                    @{ Name = 'fullyLocked'; Value = 'C' }
                )
            },
            @{
                Locale = 'qps-plocm'
                TargetEntries = @(
                    @{ Name = 'invariant'; Value = ('[!! {0}%{{name}} %{{path}} B D{0} !!]' -f $rtlMark) }
                    @{ Name = 'mixed'; Value = ('[!! {0}Öpen %{{name}} %{{path}} B D{0} !!]' -f $rtlMark) }
                    @{ Name = 'fullyLocked'; Value = 'C' }
                )
            }
        )) {
            $targetPath = Join-Path $TestDrive ("pseudo-equal-length\{0}.yml" -f $case.Locale)
            Write-WtaFixtureFile -Path $targetPath -Entries $case.TargetEntries

            $result = Test-PseudoLocale -SourceFile $sourcePath -TargetFile $targetPath -Locale $case.Locale
            $result.status | Should -Be 'PASS'
        }
    }

    It 'finds source-only missing keys across untouched shipped targets and passes once they are translated' {
        $sourcePath = Join-Path $TestDrive 'source-scope\Resources\en-US\Resources.resw'
        $frTargetPath = Join-Path $TestDrive 'source-scope\Resources\fr-FR\Resources.resw'
        $deTargetPath = Join-Path $TestDrive 'source-scope\Resources\de-DE\Resources.resw'

        Write-ReswFixtureFile -Path $sourcePath -Resources @(
            @{ Name = 'existing'; Value = 'Existing' }
            @{ Name = 'newKey'; Value = 'New English text' }
        )
        Write-ReswFixtureFile -Path $frTargetPath -Resources @(
            @{ Name = 'existing'; Value = 'Existant' }
        )
        Write-ReswFixtureFile -Path $deTargetPath -Resources @(
            @{ Name = 'existing'; Value = 'Vorhanden' }
        )

        $frMissing = Test-RequiredKeys -SourceFile $sourcePath -TargetFile $frTargetPath
        $deMissing = Test-RequiredKeys -SourceFile $sourcePath -TargetFile $deTargetPath

        $frMissing.status | Should -Be 'FIXABLE'
        $deMissing.status | Should -Be 'FIXABLE'
        @($frMissing.results.resource) | Should -Contain 'newKey'
        @($deMissing.results.resource) | Should -Contain 'newKey'

        Write-ReswFixtureFile -Path $frTargetPath -Resources @(
            @{ Name = 'existing'; Value = 'Existant' }
            @{ Name = 'newKey'; Value = 'Nouveau texte' }
        )
        Write-ReswFixtureFile -Path $deTargetPath -Resources @(
            @{ Name = 'existing'; Value = 'Vorhanden' }
            @{ Name = 'newKey'; Value = 'Neuer Text' }
        )

        (Test-RequiredKeys -SourceFile $sourcePath -TargetFile $frTargetPath).status | Should -Be 'PASS'
        (Test-RequiredKeys -SourceFile $sourcePath -TargetFile $deTargetPath).status | Should -Be 'PASS'
    }

}
