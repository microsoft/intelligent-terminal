#Requires -Modules @{ ModuleName = 'Pester'; ModuleVersion = '5.0.0' }

BeforeAll {
    . "$PSScriptRoot\..\Wait-WindowsDocker.ps1"
    $script:evidenceDirectory = Join-Path $PSScriptRoot ".docker-readiness-test-$PID"
    New-Item -ItemType Directory -Path $script:evidenceDirectory | Out-Null
    if (-not (Get-Command Get-WindowsOptionalFeature -ErrorAction SilentlyContinue)) {
        function Get-WindowsOptionalFeature { param($Online, $FeatureName) }
    }
}

AfterAll {
    Remove-Item -LiteralPath $script:evidenceDirectory -Recurse -Force
}

Describe 'Windows Docker readiness (no real Docker or service mutation)' {
    BeforeEach {
        Mock Get-WindowsOptionalFeature { [pscustomobject]@{ State = 'Enabled' } }
        Mock Get-Command { [pscustomobject]@{ Source = 'C:\docker\docker.exe' } } -ParameterFilter { $Name -eq 'docker.exe' }
        Mock Get-Service { [pscustomobject]@{ Name = 'docker'; Status = 'Running' } }
        Mock Get-CimInstance { [pscustomobject]@{ PathName = '"C:\docker\dockerd.exe" --run-service'; StartMode = 'Auto' } }
        Mock Test-Path { $true }
        Mock Start-Sleep {}
        Mock Invoke-WindowsDockerPreflightProcess {
            [pscustomobject]@{ ExitCode = 0; Output = '{"OSType":"windows","DockerRootDir":"C:\\ProgramData\\docker"}' }
        }
    }

    It 'returns only a ready Windows engine at the exact endpoint' {
        $result = Wait-WindowsDocker -EvidenceDirectory $script:evidenceDirectory
        $result.OSType | Should -BeExactly 'windows'
        Should -Invoke Invoke-WindowsDockerPreflightProcess -Times 1 -Exactly -ParameterFilter {
            $Arguments[0] -eq '--host' -and $Arguments[1] -eq 'npipe:////./pipe/docker_engine' -and $TimeoutSeconds -le 30
        }
    }

    It 'rejects a non-Windows engine without starting a service' {
        Mock Invoke-WindowsDockerPreflightProcess { [pscustomobject]@{ ExitCode = 0; Output = '{"OSType":"linux","DockerRootDir":"/var/lib/docker"}' } }
        { Wait-WindowsDocker -EvidenceDirectory $script:evidenceDirectory } | Should -Throw '*not a Windows engine*'
        Should -Invoke Invoke-WindowsDockerPreflightProcess -Times 1 -Exactly
    }

    It 'starts only the known stopped Docker service and then waits for the API' {
        $script:serviceStarted = $false
        Mock Get-Service { [pscustomobject]@{ Name = 'docker'; Status = $(if ($script:serviceStarted) { 'Running' } else { 'Stopped' }) } }
        $script:probeCount = 0
        Mock Invoke-WindowsDockerPreflightProcess {
            if ($Arguments[0] -eq '--host') {
                $script:probeCount++
                if ($script:probeCount -eq 1) { return [pscustomobject]@{ ExitCode = 1; Output = '' } }
                return [pscustomobject]@{ ExitCode = 0; Output = '{"OSType":"windows","DockerRootDir":"C:\\ProgramData\\docker"}' }
            }
            [Text.Encoding]::Unicode.GetString([Convert]::FromBase64String($Arguments[-1])) |
                Should -BeExactly 'Start-Service -Name docker -ErrorAction Stop'
            $script:serviceStarted = $true
            return [pscustomobject]@{ ExitCode = 0; Output = '' }
        }
        (Wait-WindowsDocker -EvidenceDirectory $script:evidenceDirectory).OSType | Should -Be 'windows'
        Should -Invoke Invoke-WindowsDockerPreflightProcess -Times 3 -Exactly
    }

    It 'does not accept a successful pipe response while the installed service is stopped' {
        $script:serviceStarted = $false
        Mock Get-Service { [pscustomobject]@{ Name = 'docker'; Status = $(if ($script:serviceStarted) { 'Running' } else { 'Stopped' }) } }
        Mock Invoke-WindowsDockerPreflightProcess {
            if ($Arguments[0] -eq '--host') {
                return [pscustomobject]@{ ExitCode = 0; Output = '{"OSType":"windows","DockerRootDir":"C:\\ProgramData\\docker"}' }
            }
            $script:serviceStarted = $true
            return [pscustomobject]@{ ExitCode = 0; Output = '' }
        }
        (Wait-WindowsDocker -EvidenceDirectory $script:evidenceDirectory).OSType | Should -BeExactly 'windows'
        Should -Invoke Invoke-WindowsDockerPreflightProcess -Times 3 -Exactly
        $script:serviceStarted | Should -BeTrue
    }

    It 'waits for a StartPending service even when its pipe already responds successfully' {
        $script:serviceReads = 0
        Mock Get-Service {
            $script:serviceReads++
            [pscustomobject]@{ Name = 'docker'; Status = $(if ($script:serviceReads -gt 2) { 'Running' } else { 'StartPending' }) }
        }
        (Wait-WindowsDocker -EvidenceDirectory $script:evidenceDirectory).OSType | Should -BeExactly 'windows'
        Should -Invoke Invoke-WindowsDockerPreflightProcess -Times 2 -Exactly
    }

    It 'does not accept a ready response when the service changes to an unsupported state' {
        $script:serviceReads = 0
        Mock Get-Service {
            $script:serviceReads++
            [pscustomobject]@{ Name = 'docker'; Status = $(if ($script:serviceReads -eq 1) { 'Running' } else { 'Paused' }) }
        }
        { Wait-WindowsDocker -EvidenceDirectory $script:evidenceDirectory } | Should -Throw '*unsupported status Paused*'
    }

    It 'handles Running while the API is transiently unavailable without restarting it' {
        $script:probeCount = 0
        Mock Invoke-WindowsDockerPreflightProcess {
            $script:probeCount++
            if ($script:probeCount -eq 1) { return [pscustomobject]@{ ExitCode = 1; Output = '' } }
            [pscustomobject]@{ ExitCode = 0; Output = '{"OSType":"windows","DockerRootDir":"C:\\ProgramData\\docker"}' }
        }
        (Wait-WindowsDocker -EvidenceDirectory $script:evidenceDirectory).OSType | Should -Be 'windows'
        Should -Invoke Invoke-WindowsDockerPreflightProcess -Times 0 -Exactly -ParameterFilter { $Arguments[0] -eq '-NoProfile' }
    }

    It 'fails within the readiness budget when the API remains unavailable' {
        Mock Invoke-WindowsDockerPreflightProcess {
            [Threading.Thread]::Sleep(100)
            [pscustomobject]@{ ExitCode = 1; Output = '' }
        }
        { Wait-WindowsDocker -EvidenceDirectory $script:evidenceDirectory -TimeoutSeconds 1 } | Should -Throw '*DaemonUnavailable*within 1s*'
    }

    It 'rejects unsupported service statuses rather than mapping unknown Docker statuses' {
        Mock Get-Service { [pscustomobject]@{ Name = 'docker'; Status = 'Paused' } }
        { Wait-WindowsDocker -EvidenceDirectory $script:evidenceDirectory } | Should -Throw '*unsupported transition*'
        Should -Invoke Invoke-WindowsDockerPreflightProcess -Times 0 -Exactly
    }

    It 'fails before Docker when Containers is not enabled' {
        Mock Get-WindowsOptionalFeature { [pscustomobject]@{ State = 'Disabled' } }
        { Wait-WindowsDocker -EvidenceDirectory $script:evidenceDirectory } | Should -Throw '*Containers feature is Disabled*'
        Should -Invoke Invoke-WindowsDockerPreflightProcess -Times 0 -Exactly
    }

    It 'does not start an unrecognized service executable' {
        Mock Get-CimInstance { [pscustomobject]@{ PathName = '"C:\other\unknown.exe"'; StartMode = 'Auto' } }
        { Wait-WindowsDocker -EvidenceDirectory $script:evidenceDirectory } | Should -Throw '*installed absolute dockerd.exe*'
        Should -Invoke Invoke-WindowsDockerPreflightProcess -Times 0 -Exactly
    }

    It 'fails closed when the supported service start fails' {
        Mock Get-Service { [pscustomobject]@{ Name = 'docker'; Status = 'Stopped' } }
        Mock Invoke-WindowsDockerPreflightProcess { [pscustomobject]@{ ExitCode = 1; Output = '' } }
        { Wait-WindowsDocker -EvidenceDirectory $script:evidenceDirectory } | Should -Throw '*Start-Service docker failed*'
        Should -Invoke Invoke-WindowsDockerPreflightProcess -Times 2 -Exactly
    }

    It 'rejects a disabled Docker service without enabling it' {
        Mock Get-CimInstance { [pscustomobject]@{ PathName = '"C:\docker\dockerd.exe"'; StartMode = 'Disabled' } }
        { Wait-WindowsDocker -EvidenceDirectory $script:evidenceDirectory } | Should -Throw '*startMode=Disabled*'
        Should -Invoke Invoke-WindowsDockerPreflightProcess -Times 0 -Exactly
    }

    It 'rejects a missing daemon binary without launching anything' {
        Mock Test-Path { $false }
        { Wait-WindowsDocker -EvidenceDirectory $script:evidenceDirectory } | Should -Throw '*installed absolute dockerd.exe*'
        Should -Invoke Invoke-WindowsDockerPreflightProcess -Times 0 -Exactly
    }

    It 'does not invent an unmanaged daemon fallback when the known service is absent' {
        Mock Get-Service { throw 'Docker service not installed.' }
        { Wait-WindowsDocker -EvidenceDirectory $script:evidenceDirectory } | Should -Throw '*Docker service not installed*'
        Should -Invoke Invoke-WindowsDockerPreflightProcess -Times 0 -Exactly
    }

    It 'retries a terminated info probe timeout and accepts the next ready response' {
        $script:probeCount = 0
        Mock Invoke-WindowsDockerPreflightProcess {
            $script:probeCount++
            if ($script:probeCount -eq 1) { throw [TimeoutException]::new('Owned info child terminated after timeout.') }
            [pscustomobject]@{ ExitCode = 0; Output = '{"OSType":"windows","DockerRootDir":"C:\\ProgramData\\docker"}' }
        }
        $result = Wait-WindowsDocker -EvidenceDirectory $script:evidenceDirectory -WarningVariable warnings
        $result.OSType | Should -BeExactly 'windows'
        @($warnings).Count | Should -Be 1
        $warnings[0].ToString() | Should -BeLike '*Owned info child terminated*'
        Should -Invoke Invoke-WindowsDockerPreflightProcess -Times 2 -Exactly
    }

    It 'starts the known stopped service even when the first info probe times out' {
        $script:serviceStarted = $false
        $script:probeCount = 0
        Mock Get-Service { [pscustomobject]@{ Name = 'docker'; Status = $(if ($script:serviceStarted) { 'Running' } else { 'Stopped' }) } }
        Mock Invoke-WindowsDockerPreflightProcess {
            if ($Arguments[0] -eq '--host') {
                $script:probeCount++
                if ($script:probeCount -eq 1) { throw [TimeoutException]::new('Owned info child terminated after timeout.') }
                return [pscustomobject]@{ ExitCode = 0; Output = '{"OSType":"windows","DockerRootDir":"C:\\ProgramData\\docker"}' }
            }
            $script:serviceStarted = $true
            return [pscustomobject]@{ ExitCode = 0; Output = '' }
        }
        (Wait-WindowsDocker -EvidenceDirectory $script:evidenceDirectory -WarningAction SilentlyContinue).OSType | Should -BeExactly 'windows'
        $script:serviceStarted | Should -BeTrue
        Should -Invoke Invoke-WindowsDockerPreflightProcess -Times 3 -Exactly
    }

    It 'fails at the original global deadline when every info probe times out' {
        $script:deadlines = [Collections.Generic.List[DateTime]]::new()
        Mock Invoke-WindowsDockerPreflightProcess {
            $script:deadlines.Add($(if ($null -eq $DeadlineUtc) { [DateTime]::MinValue } else { $DeadlineUtc }))
            [Threading.Thread]::Sleep(250)
            throw [TimeoutException]::new('Owned info child terminated after timeout.')
        }
        $timer = [Diagnostics.Stopwatch]::StartNew()
        { Wait-WindowsDocker -EvidenceDirectory $script:evidenceDirectory -TimeoutSeconds 1 -WarningAction SilentlyContinue } | Should -Throw '*DaemonUnavailable*within 1s*'
        $timer.Stop()
        $timer.Elapsed.TotalSeconds | Should -BeLessThan 3
        $script:deadlines.Count | Should -BeGreaterThan 1
        @($script:deadlines | Select-Object -Unique).Count | Should -Be 1
        $script:deadlines[0] | Should -BeGreaterThan ([DateTime]::MinValue)
    }

    It 'does not retry a failure to terminate an owned info child' {
        Mock Invoke-WindowsDockerPreflightProcess { throw [InvalidOperationException]::new('Cannot terminate preflight child PID=123.') }
        { Wait-WindowsDocker -EvidenceDirectory $script:evidenceDirectory } | Should -Throw '*Cannot terminate preflight child*'
        Should -Invoke Invoke-WindowsDockerPreflightProcess -Times 1 -Exactly
    }

    It 'does not retry a service-start timeout' {
        Mock Get-Service { [pscustomobject]@{ Name = 'docker'; Status = 'Stopped' } }
        Mock Invoke-WindowsDockerPreflightProcess {
            if ($Arguments[0] -eq '--host') { return [pscustomobject]@{ ExitCode = 1; Output = '' } }
            throw [TimeoutException]::new('Service-start child terminated after timeout.')
        }
        { Wait-WindowsDocker -EvidenceDirectory $script:evidenceDirectory } | Should -Throw '*Service-start child terminated*'
        Should -Invoke Invoke-WindowsDockerPreflightProcess -Times 2 -Exactly
    }
}

Describe 'Owned preflight child process bounds' {
    BeforeAll {
        if (-not ('WindowsDockerTimeoutRaceTestProcess' -as [type])) {
            Add-Type @'
public sealed class WindowsDockerTimeoutRaceTestProcess
{
    public bool HasExited { get; set; } = true;
    public int Id => 123;
    public int WaitCount { get; private set; }
    public bool WaitForExit(int milliseconds) => ++WaitCount > 1;
    public void Kill() => throw new System.InvalidOperationException("Termination failed or process already exited.");
}
'@
        }
        $tokens = $null
        $parseErrors = $null
        $ast = [Management.Automation.Language.Parser]::ParseFile("$PSScriptRoot\..\Wait-WindowsDocker.ps1", [ref]$tokens, [ref]$parseErrors)
        if ($parseErrors.Count) { throw $parseErrors[0] }
        $timeoutBranch = $ast.Find({
            param($node)
            $node -is [Management.Automation.Language.IfStatementAst] -and
                $node.Extent.Text.StartsWith('if (-not $process.WaitForExit($waitMilliseconds))')
        }, $true)
        if (-not $timeoutBranch) { throw 'Production timeout branch not found.' }
        $script:timeoutBranch = [scriptblock]::Create($timeoutBranch.Extent.Text)
    }

    It 'keeps an already-exited kill race retryable and retains timeout diagnostics' {
        $process = [WindowsDockerTimeoutRaceTestProcess]::new()
        $waitMilliseconds = 1
        $DeadlineUtc = [DateTime]::UtcNow.AddSeconds(5)
        $TimeoutSeconds = 1
        $LogPrefix = Join-Path $script:evidenceDirectory 'exit-race'
        $stdout = [IO.StringReader]::new('retained stdout').ReadToEndAsync()
        $stderr = [IO.StringReader]::new('retained stderr').ReadToEndAsync()
        { . $script:timeoutBranch } | Should -Throw '*preflight command timed out*' -ExceptionType ([TimeoutException])
        Get-Content "$LogPrefix.stdout.log" | Should -BeExactly 'retained stdout'
        Get-Content "$LogPrefix.stderr.log" | Should -BeExactly 'retained stderr'
        $process.WaitCount | Should -Be 2
    }

    It 'does not suppress a kill failure while the exact child is still running' {
        $process = [WindowsDockerTimeoutRaceTestProcess]::new()
        $process.HasExited = $false
        $waitMilliseconds = 1
        { . $script:timeoutBranch } | Should -Throw '*Termination failed or process already exited*'
    }

    It 'captures real stdout, stderr and exit code from a harmless child' {
        $result = Invoke-WindowsDockerPreflightProcess -FilePath (Join-Path $PSHOME 'pwsh.exe') -Arguments @('-NoProfile', '-NonInteractive', '-Command', '[Console]::Out.WriteLine("ready"); [Console]::Error.WriteLine("diagnostic"); exit 7') -TimeoutSeconds 30 -LogPrefix (Join-Path $script:evidenceDirectory 'dummy')
        $result.ExitCode | Should -Be 7
        $result.Output.Trim() | Should -BeExactly 'ready'
        Get-Content (Join-Path $script:evidenceDirectory 'dummy.stderr.log') | Should -BeExactly 'diagnostic'
    }

    Describe 'Native workflow cleanup after failed preflight' {
        BeforeAll {
            $workflow = Get-Content "$PSScriptRoot\..\..\..\.github\workflows\ghaw-pr-security-validate-windows.yml" -Raw
            $cleanup = [regex]::Match($workflow, '(?s)- name: Independently clean only owned validation container.*?run: \|\r?\n(.*?)(?=\r?\n      - name:)').Groups[1].Value
            $script:cleanupScript = [scriptblock]::Create([regex]::Replace($cleanup, '(?m)^          ', ''))
            function docker { throw 'Real Docker must never be invoked in these tests.' }
        }

        BeforeEach {
            $script:originalRunnerTemp = $env:RUNNER_TEMP
            $script:originalPrNumber = $env:PR_NUMBER
            $script:originalExitCode = $global:LASTEXITCODE
            $env:RUNNER_TEMP = $script:evidenceDirectory
            $env:PR_NUMBER = '5'
            New-Item -ItemType Directory -Path (Join-Path $script:evidenceDirectory 'ghaw-windows-proof') -Force | Out-Null
            Mock docker { $global:LASTEXITCODE = 1 }
        }

        AfterEach {
            $env:RUNNER_TEMP = $script:originalRunnerTemp
            $env:PR_NUMBER = $script:originalPrNumber
            $global:LASTEXITCODE = $script:originalExitCode
            Remove-Item -LiteralPath (Join-Path $script:evidenceDirectory 'ghaw-windows-proof') -Recurse -Force
        }

        It 'warns and never queries a broken daemon when container creation was never attempted' {
            $warnings = @(& $script:cleanupScript 3>&1)
            $warnings.Count | Should -Be 1
            $warnings[0].ToString() | Should -BeLike '*Native validation remains unproven*'
            Should -Invoke docker -Times 0 -Exactly
        }

        It 'still fails on an unavailable daemon when a creation attempt requires ownership verification' {
            'attempted' | Set-Content (Join-Path $script:evidenceDirectory 'ghaw-windows-proof\container-create-attempted.txt')
            { & $script:cleanupScript } | Should -Throw '*Cannot query cleanup ownership*'
            Should -Invoke docker -Times 1 -Exactly
        }
    }

    It 'terminates only its own timed-out harmless child and retains logs' {
        { Invoke-WindowsDockerPreflightProcess -FilePath (Join-Path $PSHOME 'pwsh.exe') -Arguments @('-NoProfile', '-NonInteractive', '-Command', '[Console]::Out.WriteLine("waiting"); [Threading.Thread]::Sleep(10000)') -TimeoutSeconds 1 -LogPrefix (Join-Path $script:evidenceDirectory 'timeout') } | Should -Throw '*preflight command timed out*' -ExceptionType ([TimeoutException])
        Test-Path (Join-Path $script:evidenceDirectory 'timeout.stdout.log') | Should -BeTrue
        Test-Path (Join-Path $script:evidenceDirectory 'timeout.stderr.log') | Should -BeTrue
    }

    It 'includes owned child termination in the supplied global deadline' {
        $timer = [Diagnostics.Stopwatch]::StartNew()
        { Invoke-WindowsDockerPreflightProcess -FilePath (Join-Path $PSHOME 'pwsh.exe') -Arguments @('-NoProfile', '-NonInteractive', '-Command', '[Threading.Thread]::Sleep(10000)') -TimeoutSeconds 30 -DeadlineUtc ([DateTime]::UtcNow.AddSeconds(1)) -LogPrefix (Join-Path $script:evidenceDirectory 'global-timeout') } | Should -Throw '*preflight command timed out*' -ExceptionType ([TimeoutException])
        $timer.Stop()
        $timer.Elapsed.TotalSeconds | Should -BeLessThan 2
        Test-Path (Join-Path $script:evidenceDirectory 'global-timeout.stdout.log') | Should -BeTrue
        Test-Path (Join-Path $script:evidenceDirectory 'global-timeout.stderr.log') | Should -BeTrue
    }
}
