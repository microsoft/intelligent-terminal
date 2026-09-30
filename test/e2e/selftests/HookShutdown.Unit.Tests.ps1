#Requires -Version 7.0
#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }

Describe 'Hook shutdown negative oracle' -Tag Unit {
    BeforeAll {
        . (Join-Path $PSScriptRoot '..\ItE2E\Private\Core.ps1')
        $tokens = $null
        $errors = $null
        $path = Join-Path $PSScriptRoot '..\tests\Feature.HookShutdown.Tests.ps1'
        $ast = [System.Management.Automation.Language.Parser]::ParseFile($path, [ref]$tokens, [ref]$errors)
        if ($errors) { throw 'Hook shutdown source could not be parsed.' }
        # Load only the oracle, never the live suite's discovery or lifecycle.
        $oracle = $ast.Find({
            param($node)
            $node -is [System.Management.Automation.Language.FunctionDefinitionAst] -and
            $node.Name -eq 'Assert-NoTestServer'
        }, $true)
        . ([scriptblock]::Create($oracle.Extent.Text))
        function Get-TestServers {}
        function Register-TestServers {}
    }

    BeforeEach {
        $script:clock = [datetime]'2026-01-01T00:00:00Z'
        $script:started = $script:clock
        $script:replacement = [pscustomobject]@{ Id = 123 }
        Mock Get-Date { $script:clock }
        Mock Start-Sleep { $script:clock = $script:clock.AddSeconds($Seconds) }
        Mock Get-TestServers {}
        Mock Register-TestServers {}
    }

    It 'observes all five seconds quietly when no replacement appears' {
        { Assert-NoTestServer } | Should -Not -Throw
        ($script:clock - $script:started).TotalSeconds | Should -Be 5
        Should -Invoke Start-Sleep -Times 50 -Exactly -ParameterFilter { $Seconds -eq 0.1 }
        Should -Invoke Register-TestServers -Times 1 -Exactly
    }

    It 'rejects an asynchronous replacement appearing after two seconds' {
        Mock Get-TestServers {
            if (($script:clock - $script:started).TotalSeconds -ge 3) { $script:replacement }
        }
        { Assert-NoTestServer } | Should -Throw '*notifications must never activate*'
        ($script:clock - $script:started).TotalSeconds | Should -Be 3
        Should -Invoke Register-TestServers -Times 1 -Exactly
    }

    It 'refreshes the snapshot after final process registration' {
        $script:registered = $false
        Mock Register-TestServers { $script:registered = $true }
        Mock Get-TestServers { if ($script:registered) { $script:replacement } }
        { Assert-NoTestServer } | Should -Throw '*notifications must never activate*'
        ($script:clock - $script:started).TotalSeconds | Should -Be 5
        Should -Invoke Register-TestServers -Times 1 -Exactly
    }

    It 'retains an observed replacement even if it exits during registration' {
        $script:registered = $false
        Mock Register-TestServers { $script:registered = $true }
        Mock Get-TestServers { if (-not $script:registered) { $script:replacement } }
        { Assert-NoTestServer } | Should -Throw '*notifications must never activate*'
        Should -Invoke Register-TestServers -Times 1 -Exactly
        Should -Invoke Start-Sleep -Times 0 -Exactly
    }
}
