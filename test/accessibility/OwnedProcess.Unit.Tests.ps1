#Requires -Version 7.0
#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }

Describe 'Axe test host owned-process cleanup' -Tag Unit {
    BeforeAll {
        $tokens = $null
        $errors = $null
        $path = Join-Path $PSScriptRoot 'Invoke-AxeWindowsTestHost.ps1'
        $ast = [System.Management.Automation.Language.Parser]::ParseFile($path, [ref]$tokens, [ref]$errors)
        if ($errors) { throw 'Test host harness could not be parsed.' }
        foreach ($name in @('Get-OwnedProcess', 'Stop-OwnedProcess', 'Get-TestHostPackage')) {
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
        function Remove-AppxPackage { [CmdletBinding()]param([string]$Package) }
        function Add-AppxPackage { [CmdletBinding()]param([string]$Register, [switch]$DisableDevelopmentMode) }
        function Get-AppxPackage {}
    }

    BeforeEach {
        $script:registrationAttempted = $false
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
        Mock Get-Process { throw 'A fresh PID lookup must never occur during cleanup.' }
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
        $registrationAttempted = $true
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

    It 'restores previous registration after lookup failure with recovery <Recovered>' -ForEach @(
        @{ Recovered = $true }, @{ Recovered = $false }
    ) {
        $status = 'BLOCKED'
        $reason = 'Registration lookup failed'
        $ownedProcess = $null
        $axe = $null
        $package = $null
        $registrationAttempted = $true
        $previousPackage = [pscustomobject]@{ InstallLocation = 'C:\previous-TestHostApp' }
        $manifest = [xml]'<Package><Identity /></Package>'
        $resolvedManifest = 'C:\TestHostApp\AppxManifest.xml'
        $KeepRegistered = $false
        $cleanup = [System.Collections.Generic.List[object]]::new()
        $SourceSha = 'a' * 40
        $Surface = 'fre'
        $axeExitCode = $null
        $processId = 0
        $axeResultPath = 'not-used'
        $resultPath = 'not-written'
        $script:recoverPackage = $Recovered
        Mock Get-TestHostPackage {
            if ($script:recoverPackage) { [pscustomobject]@{ PackageFullName = 'verified-TestHostApp' } }
        }
        Mock Test-Path { $true }
        Mock Remove-AppxPackage {}
        Mock Add-AppxPackage {}
        Mock Set-Content { $script:written = $Value }
        . $script:cleanupBlock
        $result = $script:written | ConvertFrom-Json
        $result.status | Should -Be 'BLOCKED'
        $result.primary_reason | Should -Be 'Registration lookup failed'
        Should -Invoke Get-TestHostPackage -Times 1 -Exactly
        Should -Invoke Add-AppxPackage -Times 1 -Exactly -ParameterFilter {
            $Register -eq 'C:\previous-TestHostApp\AppxManifest.xml'
        }
        if ($Recovered) {
            Should -Invoke Remove-AppxPackage -Times 1 -Exactly -ParameterFilter { $Package -eq 'verified-TestHostApp' }
            $result.cleanup.Count | Should -Be 0
        } else {
            Should -Invoke Remove-AppxPackage -Times 0 -Exactly
            $result.cleanup[0].reason | Should -BeLike '*refusing to remove an unverified package*'
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

Describe 'Axe test host package identity' -Tag Unit {
    BeforeAll {
        $path = Join-Path $PSScriptRoot 'Invoke-AxeWindowsTestHost.ps1'
        $errors = $null
        $ast = [System.Management.Automation.Language.Parser]::ParseFile($path, [ref]$null, [ref]$errors)
        if ($errors) { throw 'Test host harness could not be parsed.' }
        $definition = $ast.Find({
            param($node)
            $node -is [System.Management.Automation.Language.FunctionDefinitionAst] -and
                $node.Name -eq 'Get-TestHostPackage'
        }, $true)
        . ([scriptblock]::Create($definition.Extent.Text))
        function Get-AppxPackage {}
    }

    BeforeEach {
        $script:manifest = [xml]'<Package><Identity Name="WindowsTerminal.TestHost" Publisher="CN=Windows Terminal Team" ProcessorArchitecture="x64" Version="1.0.0.0" /></Package>'
        $script:matching = [pscustomobject]@{
            Name = 'WindowsTerminal.TestHost'
            Publisher = 'CN=Windows Terminal Team'
            Architecture = 'X64'
            Version = '1.0.0.0'
            InstallLocation = 'C:\TestHostApp'
        }
        $script:packages = @($script:matching)
        Mock Get-AppxPackage { $script:packages }
    }

    It 'does not select a same-name package from another publisher' {
        $script:matching.Publisher = 'CN=Other publisher'
        Get-TestHostPackage -Manifest $script:manifest | Should -BeNullOrEmpty
    }

    It 'ignores wrong publisher even when its version is higher' {
        $wrong = $script:matching.PSObject.Copy()
        $wrong.Publisher = 'CN=Other publisher'
        $wrong.Version = '99.0.0.0'
        $script:packages = @($wrong, $script:matching)
        (Get-TestHostPackage -Manifest $script:manifest).Publisher | Should -Be 'CN=Windows Terminal Team'
    }

    It 'does not select another name or architecture' -ForEach @(
        @{ Property='Name'; Value='Other.App' }, @{ Property='Architecture'; Value='X86' }
    ) {
        $script:matching.$Property = $Value
        Get-TestHostPackage -Manifest $script:manifest | Should -BeNullOrEmpty
    }

    It 'requires the registered manifest version and install location' {
        Get-TestHostPackage -Manifest $script:manifest -InstallLocation 'C:\TestHostApp' | Should -Not -BeNullOrEmpty
        Get-TestHostPackage -Manifest $script:manifest -InstallLocation 'C:\other' | Should -BeNullOrEmpty
        $script:matching.Version = '2.0.0.0'
        Get-TestHostPackage -Manifest $script:manifest -InstallLocation 'C:\TestHostApp' | Should -BeNullOrEmpty
    }

    It 'sorts previous owned registrations by numeric version' {
        $newer = $script:matching.PSObject.Copy()
        $newer.Version = '10.0.0.0'
        $script:matching.Version = '2.0.0.0'
        $script:packages = @($script:matching, $newer)
        (Get-TestHostPackage -Manifest $script:manifest).Version | Should -Be '10.0.0.0'
    }

    It 'rejects a manifest for a different package before querying registrations' {
        $script:manifest.Package.Identity.Name = 'Other.App'
        { Get-TestHostPackage -Manifest $script:manifest } | Should -Throw '*expected WindowsTerminal.TestHost*'
        Should -Invoke Get-AppxPackage -Times 0 -Exactly
    }

    It 'rejects ambiguous current registrations instead of selecting one arbitrarily' {
        $script:packages = @($script:matching, $script:matching.PSObject.Copy())
        { Get-TestHostPackage -Manifest $script:manifest -InstallLocation 'C:\TestHostApp' } | Should -Throw '*ambiguous*'
    }
}
