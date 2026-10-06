#Requires -Version 7.0
#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }

Describe 'Axe test host owned-process cleanup' -Tag Unit {
    BeforeAll {
        $tokens = $null
        $errors = $null
        $path = Join-Path $PSScriptRoot 'Invoke-AxeWindowsTestHost.ps1'
        $ast = [System.Management.Automation.Language.Parser]::ParseFile($path, [ref]$tokens, [ref]$errors)
        if ($errors) { throw 'Test host harness could not be parsed.' }
        foreach ($name in @('Get-OwnedProcess', 'Stop-OwnedProcess')) {
            $definition = $ast.Find({
                param($node)
                $node -is [System.Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -eq $name
            }, $true)
            . ([scriptblock]::Create($definition.Extent.Text))
        }
        $outerTry = $ast.EndBlock.Statements |
            Where-Object { $_ -is [System.Management.Automation.Language.TryStatementAst] } |
            Select-Object -Last 1
        $text = $outerTry.Finally.Extent.Text
        $script:cleanupBlock = [scriptblock]::Create($text.Substring(1, $text.Length - 2))
        function Remove-AppxPackage {}
        function Add-AppxPackage {}
    }

    BeforeEach {
        $script:started = [datetime]::new(2026, 10, 6, 1, 0, 0, [DateTimeKind]::Utc)
        $script:fake = [pscustomobject]@{
            Id = 123
            Handle = [intptr]123
            StartTime = $script:started
            MainModule = [pscustomobject]@{ FileName = 'C:\test\TestHostApp.exe' }
            HasExited = $false
            CloseCount = 0
            KillCount = 0
            DisposeCount = 0
            Waits = [System.Collections.Generic.List[int]]::new()
            ExitOnClose = $false
            ThrowOnClose = $false
            ThrowOnKill = $false
            ThrowOnDispose = $false
        }
        $script:fake | Add-Member ScriptMethod CloseMainWindow {
            $this.CloseCount++
            if ($this.ExitOnClose) { $this.HasExited = $true }
            if ($this.ThrowOnClose) { throw 'Close raced with process exit.' }
            return $true
        }
        $script:fake | Add-Member ScriptMethod WaitForExit {
            param($milliseconds)
            $this.Waits.Add($milliseconds)
            return $this.HasExited
        }
        $script:fake | Add-Member ScriptMethod Kill {
            $this.KillCount++
            if ($this.ThrowOnKill) { throw 'Forced termination denied.' }
            $this.HasExited = $true
        }
        $script:fake | Add-Member ScriptMethod Dispose {
            $this.DisposeCount++
            if ($this.ThrowOnDispose) { throw 'Handle release failed.' }
        }
        $script:owned = [pscustomobject]@{
            Process = $script:fake
            ProcessId = 123
            StartTime = $script:started
            Executable = 'C:\test\TestHostApp.exe'
            ExpectedExecutable = 'C:\test\TestHostApp.exe'
        }
        Mock Get-Process { throw 'PID relookup must never occur during cleanup.' }
        Mock Stop-Process { throw 'PID-based termination must never occur.' }
        Mock Write-Warning {}
    }

    It 'captures the activated process and its executable and start time' {
        Mock Get-Process { $script:fake }
        $captured = Get-OwnedProcess -ProcessId 123 -ExpectedExecutable $script:owned.Executable -ActivationStarted $script:started
        [object]::ReferenceEquals($captured.Process, $script:fake) | Should -BeTrue
        $captured.StartTime | Should -Be $script:started
        Should -Invoke Get-Process -Times 1 -Exactly -ParameterFilter { $Id -eq 123 }
    }

    It 'rejects a reused PID pointing at a different executable' {
        Mock Get-Process { $script:fake }
        $script:fake.MainModule.FileName = 'C:\unrelated.exe'
        { Get-OwnedProcess -ProcessId 123 -ExpectedExecutable $script:owned.Executable -ActivationStarted $script:started } |
            Should -Throw '*identity does not match*'
        $script:fake.KillCount | Should -Be 0
        $script:fake.DisposeCount | Should -Be 1
    }

    It 'rejects a preexisting same-binary process at activation' {
        Mock Get-Process { $script:fake }
        { Get-OwnedProcess -ProcessId 123 -ExpectedExecutable $script:owned.Executable -ActivationStarted $script:started.AddSeconds(1) } |
            Should -Throw '*identity does not match*'
        $script:fake.CloseCount | Should -Be 0
    }

    It 'refuses termination when the captured identity no longer matches' {
        $script:fake.StartTime = $script:started.AddSeconds(1)
        $result = Stop-OwnedProcess $script:owned
        $result.status | Should -Be 'BLOCKED'
        $result.reason | Should -BeLike '*identity changed*'
        $script:fake.CloseCount | Should -Be 0
        $script:fake.KillCount | Should -Be 0
        Should -Invoke Write-Warning -Times 1 -Exactly
    }

    It 'does not touch a replacement PID after the owned process exits' {
        $script:fake.HasExited = $true
        $result = Stop-OwnedProcess $script:owned
        $result.status | Should -Be 'PASS'
        $script:fake.CloseCount | Should -Be 0
        $script:fake.KillCount | Should -Be 0
        Should -Invoke Get-Process -Times 0 -Exactly
        Should -Invoke Stop-Process -Times 0 -Exactly
    }

    It 'treats exit during graceful close as successful cleanup' {
        $script:fake.ExitOnClose = $true
        $script:fake.ThrowOnClose = $true
        $result = Stop-OwnedProcess $script:owned
        $result.status | Should -Be 'PASS'
        $result.action | Should -Be 'exited-during-cleanup'
        $script:fake.KillCount | Should -Be 0
        $script:fake.DisposeCount | Should -Be 1
    }

    It 'uses the original object for bounded forced termination' {
        $result = Stop-OwnedProcess $script:owned
        $result.status | Should -Be 'PASS'
        $script:fake.KillCount | Should -Be 1
        @($script:fake.Waits) | Should -Be @(5000, 5000)
        Should -Invoke Get-Process -Times 0 -Exactly
        Should -Invoke Stop-Process -Times 0 -Exactly
    }

    It 'records forced termination failure without throwing' {
        $script:fake.ThrowOnKill = $true
        $result = Stop-OwnedProcess $script:owned
        $result.status | Should -Be 'BLOCKED'
        $result.reason | Should -BeLike '*Forced termination denied*'
        $script:fake.DisposeCount | Should -Be 1
        Should -Invoke Write-Warning -Times 1 -Exactly
    }

    It 'records handle release failure without throwing' {
        $script:fake.HasExited = $true
        $script:fake.ThrowOnDispose = $true
        (Stop-OwnedProcess $script:owned).status | Should -Be 'BLOCKED'
        Should -Invoke Write-Warning -Times 1 -Exactly
    }

    It 'still writes evidence when package cleanup and registration restoration fail' {
        $status = 'PASS'
        $reason = 'Scan completed'
        $ownedProcess = $null
        $axe = $null
        $package = [pscustomobject]@{ PackageFullName = 'WindowsTerminal.TestHost_test' }
        $previousPackage = [pscustomobject]@{ InstallLocation = 'C:\previous-test-host' }
        $KeepRegistered = $false
        $cleanup = [System.Collections.Generic.List[object]]::new()
        $SourceSha = 'a' * 40
        $Surface = 'fre'
        $axeExitCode = 0
        $processId = 123
        $axeResultPath = 'not-used'
        $resultPath = 'not-written'
        Mock Test-Path { $true }
        Mock Remove-AppxPackage { throw 'Package removal failed.' }
        Mock Add-AppxPackage { throw 'Registration restoration failed.' }
        Mock Set-Content { $script:written = $Value }
        . $script:cleanupBlock
        $result = $script:written | ConvertFrom-Json
        $result.status | Should -Be 'BLOCKED'
        $result.primary_status | Should -Be 'PASS'
        $result.cleanup.Count | Should -Be 2
        Should -Invoke Set-Content -Times 1 -Exactly
        Should -Invoke Write-Warning -Times 1 -Exactly -ParameterFilter {
            $Message -like 'Failed to restore the previous WindowsTerminal.TestHost registration:*'
        }
    }

    It 'still writes BLOCKED result evidence preserving the primary <Primary> status' -ForEach @(
        @{ Primary = 'PASS' }, @{ Primary = 'FAIL' }, @{ Primary = 'BLOCKED' }
    ) {
        $status = $Primary
        $reason = 'Primary scan reason'
        $ownedProcess = $script:owned
        $script:fake.ThrowOnKill = $true
        $axe = $null
        $package = $null
        $KeepRegistered = $false
        $cleanup = [System.Collections.Generic.List[object]]::new()
        $SourceSha = 'a' * 40
        $Surface = 'fre'
        $axeExitCode = if ($Primary -eq 'PASS') { 0 } else { 1 }
        $processId = 123
        $axeResultPath = 'not-used'
        $resultPath = 'not-written'
        Mock Test-Path { $false }
        Mock Set-Content { $script:written = $Value }
        . $script:cleanupBlock
        $result = $script:written | ConvertFrom-Json
        $result.status | Should -Be 'BLOCKED'
        $result.primary_status | Should -Be $Primary
        $result.primary_reason | Should -Be 'Primary scan reason'
        $result.cleanup[0].status | Should -Be 'BLOCKED'
        Should -Invoke Set-Content -Times 1 -Exactly
    }
}
