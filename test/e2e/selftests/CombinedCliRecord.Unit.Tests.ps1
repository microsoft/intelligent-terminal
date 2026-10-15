#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }

BeforeAll {
    $script:suiteAst = [Management.Automation.Language.Parser]::ParseFile(
        (Join-Path $PSScriptRoot '..\tests\Feature.CombinedAgentsSidebar.Tests.ps1'), [ref]$null, [ref]$null)
    $coreAst = [Management.Automation.Language.Parser]::ParseFile(
        (Join-Path $PSScriptRoot '..\ItE2E\Private\Core.ps1'), [ref]$null, [ref]$null)
    foreach ($entry in @(
        @{ Ast = $script:suiteAst; Name = 'Wait-CombinedCliLaunchRecord' }
        @{ Ast = $coreAst; Name = 'Wait-Until' }
    )) {
        $definition = $entry.Ast.Find({
            param($node)
            $node -is [Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -eq $entry.Name
        }, $true)
        . ([scriptblock]::Create($definition.Extent.Text))
    }
    function Write-ItLog { param($Level, $Message) }
    $script:record = '  {"session_id":"original","pid":42}  '
}

Describe 'Combined native fixture first-record readiness' -Tag 'Unit' {
    BeforeEach {
        # Keep all file operations in the repository, not Pester's temporary TestDrive.
        $script:folder = Join-Path $PSScriptRoot "..\artifacts\cli-record-$([guid]::NewGuid().ToString('N'))"
        $null = New-Item -ItemType Directory -Path $script:folder
        $script:path = Join-Path $script:folder 'launch[1].jsonl'
        Mock Start-Sleep {}
    }
    AfterEach {
        Remove-Item -LiteralPath $script:folder -Recurse -Force
    }

    It 'keeps <State> pending without a terminating LF' -ForEach @(
        @{ State = 'missing'; Content = $null }
        @{ State = 'empty'; Content = '' }
        @{ State = 'partial'; Content = '{"session_id":' }
        @{ State = 'full JSON without LF'; Content = '{"session_id":"original","pid":42}' }
    ) {
        if ($null -ne $Content) { [IO.File]::WriteAllText($script:path, $Content) }
        { Wait-CombinedCliLaunchRecord -Path $script:path -TimeoutSec 0 } |
            Should -Throw '*Wait-Until timed out*'
    }

    It 'captures the exact first <Ending> record without trimming payload or reading again' -ForEach @(
        @{ Ending = 'LF'; Newline = "`n" }
        @{ Ending = 'CRLF'; Newline = "`r`n" }
    ) {
        [IO.File]::WriteAllText($script:path, $script:record + $Newline + '{"pid":99}' + "`n")
        Mock Get-Content {
            $snapshot = [IO.File]::ReadAllText($script:path)
            [IO.File]::WriteAllText($script:path, '{"pid":100}' + "`n")
            $snapshot
        }
        $line = Wait-CombinedCliLaunchRecord -Path $script:path -TimeoutSec 1
        $line | Should -BeExactly $script:record
        ($line | ConvertFrom-Json -ErrorAction Stop).pid | Should -Be 42
        Should -Invoke Get-Content -Times 1 -Exactly
    }

    It 'waits through deterministic writer pauses at creation, partial JSON and complete JSON without LF' {
        [IO.File]::WriteAllText($script:path, '')
        $script:phase = 0
        Mock Start-Sleep {
            $script:phase++
            $next = switch ($script:phase) {
                1 { '{"session_id":' }
                2 { $script:record }
                3 { $script:record + "`n" }
                default { throw 'Reader failed to accept the completed record.' }
            }
            [IO.File]::WriteAllText($script:path, $next)
        }
        $line = Wait-CombinedCliLaunchRecord -Path $script:path -TimeoutSec 20
        $script:phase | Should -Be 3
        $line | Should -BeExactly $script:record
        ($line | ConvertFrom-Json -ErrorAction Stop).session_id | Should -Be 'original'
    }

    It 'fails strict parsing of a completed malformed record rather than retrying' {
        [IO.File]::WriteAllText($script:path, "{malformed`n")
        $line = Wait-CombinedCliLaunchRecord -Path $script:path -TimeoutSec 20
        $line | Should -BeExactly '{malformed'
        { $line | ConvertFrom-Json -ErrorAction Stop } | Should -Throw
        Should -Invoke Start-Sleep -Times 0 -Exactly
    }

    It 'rethrows a <Command> I/O error despite the real Wait-Until predicate catch' -ForEach @(
        @{ Command = 'Get-Content' }
        @{ Command = 'Test-Path' }
    ) {
        [IO.File]::WriteAllText($script:path, '')
        Mock $Command { throw [IO.IOException]::new('fixture read denied') }
        { Wait-CombinedCliLaunchRecord -Path $script:path -TimeoutSec 20 } |
            Should -Throw '*fixture read denied*'
        Should -Invoke $Command -Times 1 -Exactly
        Should -Invoke Start-Sleep -Times 0 -Exactly
    }

    It 'uses the gate at exactly the three coupled consumers with strict parsing outside it' {
        $calls = @($script:suiteAst.FindAll({
            param($node)
            $node -is [Management.Automation.Language.CommandAst] -and
                $node.GetCommandName() -eq 'Wait-CombinedCliLaunchRecord'
        }, $true))
        $calls | Should -HaveCount 3
        @($calls | Where-Object { $_.Extent.Text -match '-TimeoutSec 20' }) | Should -HaveCount 2
        @($calls | Where-Object { $_.Extent.Text -match '-TimeoutSec 15' }) | Should -HaveCount 1
        foreach ($call in $calls) {
            $call.Parent.Parent.Extent.Text | Should -Match 'ConvertFrom-Json -ErrorAction Stop'
        }
    }
}
