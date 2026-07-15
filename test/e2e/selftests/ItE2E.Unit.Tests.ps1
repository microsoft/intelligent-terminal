#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# Hermetic unit tests for the ItE2E framework core. No deployed terminal required.
#   Invoke-Pester test/e2e/selftests -Tag Unit

BeforeAll {
    Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
}

Describe 'Invoke-Native' -Tag 'Unit' {
    It 'captures stdout and a zero exit code' {
        $r = Invoke-Native -FilePath 'cmd.exe' -Arguments @('/c', 'echo', 'hello-ite2e')
        $r.ExitCode | Should -Be 0
        $r.StdOut | Should -Match 'hello-ite2e'
    }

    It 'propagates a non-zero exit code' {
        (Invoke-Native -FilePath 'cmd.exe' -Arguments @('/c', 'exit', '7')).ExitCode | Should -Be 7
    }

    It 'does NOT truncate large output (regression for the async-read race)' {
        # Produce ~500 lines; the old Register-ObjectEvent impl truncated to a few bytes.
        $r = Invoke-Native -FilePath 'cmd.exe' -Arguments @('/c', 'for /L %i in (1,1,500) do @echo line%i')
        $r.ExitCode | Should -Be 0
        ([regex]::Matches($r.StdOut, 'line\d+')).Count | Should -Be 500
    }

    It 'times out and flags TimedOut' {
        $r = Invoke-Native -FilePath 'cmd.exe' -Arguments @('/c', 'ping', '-n', '10', '127.0.0.1') -TimeoutSec 1
        $r.TimedOut | Should -BeTrue
        $r.ExitCode | Should -Be -1
    }
}

Describe 'Wait-Until / Test-Until' -Tag 'Unit' {
    It 'returns the truthy value once the condition holds' {
        $script:n = 0
        $v = Wait-Until -TimeoutSec 5 -IntervalSec 0.1 -Condition { $script:n++; if ($script:n -ge 3) { 'ready' } else { $null } }
        $v | Should -Be 'ready'
    }
    It 'throws on timeout' {
        { Wait-Until -TimeoutSec 1 -IntervalSec 0.2 -Condition { $false } -Because 'never' } | Should -Throw
    }
    It 'Test-Until returns a boolean and never throws' {
        Test-Until -TimeoutSec 1 -IntervalSec 0.2 -Condition { $false } | Should -BeFalse
        Test-Until -TimeoutSec 2 -IntervalSec 0.1 -Condition { $true } | Should -BeTrue
    }
}

Describe 'JSON helpers' -Tag 'Unit' {
    It 'ConvertFrom-JsonSafe returns $null on garbage' {
        ConvertFrom-JsonSafe -InputObject 'not json {' | Should -BeNullOrEmpty
        ConvertFrom-JsonSafe -InputObject '' | Should -BeNullOrEmpty
    }
    It 'ConvertFrom-JsonSafe parses valid JSON' {
        (ConvertFrom-JsonSafe -InputObject '{"a":1,"b":[2,3]}').b[1] | Should -Be 3
    }
    It 'ConvertFrom-JsonC strips // comments and trailing commas' {
        $jsonc = @"
{
    // a comment
    "acpAgent": "copilot",
    "autoFixEnabled": true,
}
"@
        $o = ConvertFrom-JsonC -Text $jsonc
        $o.acpAgent | Should -Be 'copilot'
        $o.autoFixEnabled | Should -BeTrue
    }
}

Describe 'Resolve-ItApp' -Tag 'Unit' {
    It 'resolves a descriptor with the expected shape when a package is installed' {
        $installed = Get-AppxPackage | Where-Object { $_.Name -like '*IntelligentTerminal*' }
        if (-not $installed) { Set-ItResult -Skipped -Because 'no IT package installed'; return }
        $app = Resolve-ItApp -Package Auto
        $app.Package | Should -Match 'IntelligentTerminal'
        $app.AppUserModelId | Should -Match '!'
        $app.SettingsPath | Should -Match 'LocalState\\settings\.json$'
        $app.WtcliPath | Should -Not -BeNullOrEmpty
    }
}
