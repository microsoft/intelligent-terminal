#Requires -Version 7.0
#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
param(
    [string]$HeaderPath,
    [string]$ArtifactRoot
)

# Source/process coverage only: no installed package, profile, policy or desktop access.
BeforeDiscovery {
    $cases = foreach ($shell in @('pwsh.exe', 'powershell.exe')) {
        foreach ($scenario in @('HostGate', 'FirstStatus', 'Lifecycle', 'Chaining', 'WorkChangingChain', 'Throwing', 'FailSafe', 'Constrained')) {
            @{ Shell = $shell; Scenario = $scenario }
        }
    }
}

Describe 'Source: PowerShell prompt lifecycle' -Tag 'Source' {
    BeforeAll {
        function Get-PowerShellIntegration([string]$Header) {
            $literal = 'R"\((?:(?!\)").)*\)"'
            $body = [regex]::Match($Header, "(?s)inline\s+std::string\s+ScriptContent\s*\(\s*\)\s*\{\s*return\s+std::string\s*\{(?<list>(?:\s*$literal)+\s*)\}\s*;\s*\}")
            if (-not $body.Success) { throw 'Cannot extract canonical PowerShell ScriptContent raw-literal initializer' }
            ([regex]::Matches($body.Groups['list'].Value, '(?s)R"\((.*?)\)"') |
                ForEach-Object { $_.Groups[1].Value }) -join ''
        }

        . (Join-Path $PSScriptRoot '..\ItE2E\Private\Core.ps1')
        $root = (Resolve-Path (Join-Path $PSScriptRoot '..\..\..')).Path
        if (-not $HeaderPath) { $HeaderPath = Join-Path $root 'src\cascadia\inc\PowerShellShellIntegration.h' }
        if (-not $ArtifactRoot) { $ArtifactRoot = Join-Path $root 'Generated Files\ItE2E\PowerShellPromptLifecycle' }
        $run = Join-Path $ArtifactRoot ([guid]::NewGuid().ToString('N'))
        $null = New-Item -ItemType Directory -Path $run -Force

        # ScriptContent concatenates C++ raw literals; execute that embedded script,
        # not a handwritten copy of the PowerShell or an installed stale script.
        $header = Get-Content -LiteralPath $HeaderPath -Raw
        $integration = Get-PowerShellIntegration $header
        $script:integrationPath = Join-Path $run 'shell-integration.ps1'
        [IO.File]::WriteAllText($script:integrationPath, $integration, [Text.UTF8Encoding]::new($true))
        $script:run = $run
        $script:fixture = Join-Path $PSScriptRoot 'helpers\PowerShellPromptLifecycle.ps1'
        @{
            Header = (Resolve-Path $HeaderPath).Path
            HeaderSha256 = (Get-FileHash -LiteralPath $HeaderPath -Algorithm SHA256).Hash
            ScriptSha256 = (Get-FileHash -LiteralPath $script:integrationPath -Algorithm SHA256).Hash
        } | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $run 'source.json')
    }

    It 'extracts byte-identical payload after unrelated adjacent comment changes' {
        $withComment = $header -replace '(\)"\s*\};\s*\})', ('$1' + "`n    // Fixture-owned adjacent comment")
        $withComment | Should -Not -BeExactly $header
        $changed = $withComment.Replace('    // Fixture-owned adjacent comment', "`t// Renamed unrelated comment")
        $changed | Should -Not -BeExactly $withComment
        [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes((Get-PowerShellIntegration $changed))) |
            Should -BeExactly ([Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($integration)))
    }

    It 'extracts byte-identical payload after structural whitespace changes' {
        $changed = $header -replace 'inline std::string ScriptContent\(\)\s*\{\s*return std::string\{', "inline  std::string`n ScriptContent ( )`n{`n return  std::string { "
        $changed = $changed -replace '\)"\s*R"\(', ")`" `n`t R`"("
        $changed = $changed -replace '\)"\s*\};\s*\}', ")`"`n } `n ; `n }"
        $changed | Should -Not -BeExactly $header
        [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes((Get-PowerShellIntegration $changed))) |
            Should -BeExactly ([Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($integration)))
    }

    It 'rejects malformed extraction: <Damage>' -ForEach @(
        @{ Damage = 'function'; From = 'ScriptContent\(\)'; To = 'OtherContent()' }
        @{ Damage = 'initializer'; From = 'return std::string\{'; To = 'return std::string[' }
        @{ Damage = 'initializer end'; From = '\)"\s*\};'; To = ')" ];' }
        @{ Damage = 'function end'; From = '\)"\s*\};\s*\}'; To = ')" };' }
        @{ Damage = 'literals'; From = '(?s)R"\((.*?)\)"'; To = '' }
    ) {
        $changed = $header -replace $From, $To
        $changed | Should -Not -BeExactly $header
        { Get-PowerShellIntegration $changed } | Should -Throw
    }

    It '<Shell> preserves prompt behavior through <Scenario>' -ForEach $cases {
        $executable = (Get-Command $Shell -CommandType Application -ErrorAction Stop | Select-Object -First 1).Source
        $result = Invoke-Native -FilePath $executable -Arguments @(
            '-NoLogo', '-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass',
            '-File', $script:fixture, '-IntegrationPath', $script:integrationPath,
            '-Scenario', $Scenario
        ) -TimeoutSec 30 -WorkingDirectory $script:run
        $result | ConvertTo-Json -Depth 4 |
            Set-Content -LiteralPath (Join-Path $script:run "$Shell-$Scenario.json")
        $result.TimedOut | Should -BeFalse
        $result.ExitCode | Should -Be 0 -Because "$Shell $Scenario must complete: $($result.StdErr) $($result.StdOut)"
        $result.StdErr | Should -BeNullOrEmpty
        $receipt = $result.StdOut | ConvertFrom-Json
        $receipt.Scenario | Should -Be $Scenario
        $receipt.Assertions | Should -BeGreaterThan 0
        if ($Shell -eq 'powershell.exe') { $receipt.Version | Should -BeLike '5.1.*' }
        else { [version]$receipt.Version | Should -BeGreaterOrEqual ([version]'7.0') }
    }
}
