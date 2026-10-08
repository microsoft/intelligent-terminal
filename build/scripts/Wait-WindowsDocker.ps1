function Invoke-WindowsDockerPreflightProcess {
    param(
        [string]$FilePath,
        [string[]]$Arguments,
        [int]$TimeoutSeconds,
        [string]$LogPrefix,
        [DateTime]$DeadlineUtc = [DateTime]::MaxValue
    )
    $start = [Diagnostics.ProcessStartInfo]::new()
    $start.FileName = $FilePath
    $start.UseShellExecute = $false
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    foreach ($argument in $Arguments) { $start.ArgumentList.Add($argument) }
    $process = [Diagnostics.Process]::new()
    $process.StartInfo = $start
    try {
        # Reserve up to five seconds for verified termination within the caller's deadline.
        $waitMilliseconds = [int][Math]::Max(0.0, [Math]::Min([double]($TimeoutSeconds * 1000), ($DeadlineUtc - [DateTime]::UtcNow).TotalMilliseconds - 5000))
        if (-not $process.Start()) { throw "Cannot start preflight command: $FilePath." }
        $stdout = $process.StandardOutput.ReadToEndAsync()
        $stderr = $process.StandardError.ReadToEndAsync()
        if (-not $process.WaitForExit($waitMilliseconds)) {
            # Only this invocation's exact child PID is terminated, never the daemon.
            try {
                $process.Kill()
            } catch [InvalidOperationException] {
                if (-not $process.HasExited) { throw }
            }
            $terminationMilliseconds = [int][Math]::Max(0.0, [Math]::Min(5000.0, ($DeadlineUtc - [DateTime]::UtcNow).TotalMilliseconds))
            if (-not $process.WaitForExit($terminationMilliseconds)) { throw "Cannot terminate preflight child PID=$($process.Id)." }
            [IO.File]::WriteAllText("$LogPrefix.stdout.log", $stdout.GetAwaiter().GetResult())
            [IO.File]::WriteAllText("$LogPrefix.stderr.log", $stderr.GetAwaiter().GetResult())
            throw [System.TimeoutException]::new("DaemonUnavailable: preflight command timed out within its ${TimeoutSeconds}s/deadline budget; owned PID=$($process.Id) terminated.")
        }
        $output = $stdout.GetAwaiter().GetResult()
        $errorOutput = $stderr.GetAwaiter().GetResult()
        [IO.File]::WriteAllText("$LogPrefix.stdout.log", $output)
        [IO.File]::WriteAllText("$LogPrefix.stderr.log", $errorOutput)
        if ($errorOutput) { Write-Host $errorOutput }
        return [pscustomobject]@{ ExitCode = $process.ExitCode; Output = $output }
    } finally {
        $process.Dispose()
    }
}

function Wait-WindowsDocker {
    param(
        [Parameter(Mandatory)][string]$EvidenceDirectory,
        [ValidatePattern('^[a-z][a-z0-9-]{0,40}$')][string]$DiagnosticPrefix = 'docker',
        [ValidateRange(1, 120)][int]$TimeoutSeconds = 120
    )
    if (-not $IsWindows) { throw 'Windows Docker preflight requires Windows.' }
    $feature = Get-WindowsOptionalFeature -Online -FeatureName Containers -ErrorAction Stop
    if ($feature.State -ne 'Enabled') { throw "DaemonUnavailable: Containers feature is $($feature.State); no feature installation or reboot permitted." }
    $dockerPath = (Get-Command docker.exe -CommandType Application -ErrorAction Stop).Source
    $service = Get-Service -Name docker -ErrorAction Stop
    $configuration = Get-CimInstance -ClassName Win32_Service -Filter "Name='docker'" -OperationTimeoutSec 30 -ErrorAction Stop
    if ($configuration.PathName -notmatch '^\s*(?:"([^"]+)"|(\S+))(?=\s|$)') { throw 'DaemonUnavailable: Docker service binary is not identifiable.' }
    $daemonPath = if ($Matches[1]) { $Matches[1] } else { $Matches[2] }
    if (-not [IO.Path]::IsPathFullyQualified($daemonPath) -or
        [IO.Path]::GetFileName($daemonPath) -ine 'dockerd.exe' -or
        -not (Test-Path -LiteralPath $daemonPath -PathType Leaf)) {
        throw 'DaemonUnavailable: Docker service must reference an installed absolute dockerd.exe path.'
    }
    [ordered]@{
        endpoint = 'npipe:////./pipe/docker_engine'
        containersFeature = [string]$feature.State
        dockerPath = $dockerPath
        serviceName = $service.Name
        serviceStatus = [string]$service.Status
        serviceStartMode = $configuration.StartMode
        serviceBinary = $configuration.PathName
    } | ConvertTo-Json | Set-Content (Join-Path $EvidenceDirectory "$DiagnosticPrefix-readiness.json")
    if ($configuration.StartMode -eq 'Disabled' -or $service.Status -notin @('Stopped', 'StartPending', 'Running')) {
        throw "DaemonUnavailable: Docker service status=$($service.Status), startMode=$($configuration.StartMode); refusing unsupported transition."
    }
    $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
    $attempt = 0
    $started = $false
    while ([DateTime]::UtcNow -lt $deadline) {
        $attempt++
        $remaining = [Math]::Max(1, [int][Math]::Ceiling(($deadline - [DateTime]::UtcNow).TotalSeconds))
        try {
            $result = Invoke-WindowsDockerPreflightProcess -FilePath $dockerPath -Arguments @('--host', 'npipe:////./pipe/docker_engine', 'info', '--format', '{{json .}}') -TimeoutSeconds ([Math]::Min(30, $remaining)) -LogPrefix (Join-Path $EvidenceDirectory "$DiagnosticPrefix-info-$attempt") -DeadlineUtc $deadline
        } catch [System.TimeoutException] {
            Write-Warning "Docker info probe timed out: attempt=$attempt; $($_.Exception.Message) Raw diagnostics: $DiagnosticPrefix-info-$attempt.stdout.log / .stderr.log."
            $result = [pscustomobject]@{ ExitCode = -1; Output = '' }
        }
        if ([DateTime]::UtcNow -ge $deadline) { break }
        $service = Get-Service -Name docker -ErrorAction Stop
        if ($result.ExitCode -eq 0 -and $service.Status -eq 'Running') {
            $info = $result.Output | ConvertFrom-Json -ErrorAction Stop
            if ($info.OSType -cne 'windows') { throw "Docker endpoint is not a Windows engine: OSType=$($info.OSType)." }
            if (-not [IO.Path]::IsPathFullyQualified($info.DockerRootDir)) { throw 'Windows Docker data root is unavailable.' }
            return $info
        }
        Write-Host "Docker API not ready: attempt=$attempt, exit=$($result.ExitCode), service=$($service.Status)."
        if (-not $started -and $service.Status -eq 'Stopped') {
            $started = $true
            $remaining = [Math]::Max(1, [int][Math]::Ceiling(($deadline - [DateTime]::UtcNow).TotalSeconds))
            if ([DateTime]::UtcNow -ge $deadline) { break }
            $command = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes("Start-Service -Name docker -ErrorAction Stop"))
            $startResult = Invoke-WindowsDockerPreflightProcess -FilePath (Join-Path $PSHOME 'pwsh.exe') -Arguments @('-NoProfile', '-NonInteractive', '-EncodedCommand', $command) -TimeoutSeconds ([Math]::Min(30, $remaining)) -LogPrefix (Join-Path $EvidenceDirectory "$DiagnosticPrefix-service-start") -DeadlineUtc $deadline
            if ($startResult.ExitCode -ne 0) { throw "DaemonUnavailable: Start-Service docker failed, exit=$($startResult.ExitCode); see raw service logs." }
        } elseif ($service.Status -notin @('Running', 'StartPending')) {
            throw "DaemonUnavailable: Docker service entered unsupported status $($service.Status)."
        }
        if ([DateTime]::UtcNow -lt $deadline) {
            Start-Sleep -Milliseconds ([Math]::Min(2000, [Math]::Max(1, [int](($deadline - [DateTime]::UtcNow).TotalMilliseconds))))
        }
    }
    throw "DaemonUnavailable: exact Docker endpoint did not become ready within ${TimeoutSeconds}s; see $DiagnosticPrefix-info-*.stderr.log."
}
