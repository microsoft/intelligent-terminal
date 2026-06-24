# Core.ps1 — process execution, JSON, waiting, logging.
# These are the lowest-level bricks. Everything else builds on them.

$script:ItE2EVerbose = [bool]$env:ITE2E_VERBOSE

function Write-ItLog {
    <#
    .SYNOPSIS
        Structured framework log line (always to the host's verbose/debug stream,
        and to $script:ItE2ELogFile when a harness is active).
    #>
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)][string]$Message,
        [ValidateSet('TRACE', 'DEBUG', 'INFO', 'WARN', 'ERROR')][string]$Level = 'INFO'
    )
    $line = "{0} [{1,-5}] {2}" -f (Get-Date -Format 'HH:mm:ss.fff'), $Level, $Message
    if ($script:ItE2ELogFile) {
        try { Add-Content -LiteralPath $script:ItE2ELogFile -Value $line -Encoding utf8 } catch { }
    }
    switch ($Level) {
        'ERROR' { Write-Verbose $line; if ($script:ItE2EVerbose) { Write-Host $line -ForegroundColor Red } }
        'WARN' { Write-Verbose $line; if ($script:ItE2EVerbose) { Write-Host $line -ForegroundColor Yellow } }
        default { Write-Verbose $line; if ($script:ItE2EVerbose) { Write-Host $line } }
    }
}

function Invoke-Native {
    <#
    .SYNOPSIS
        Run a native executable robustly, capturing stdout, stderr and exit code with
        a hard timeout. Never relies on $LASTEXITCODE leaking across calls and never
        deadlocks on large output (async stream reads).
    .OUTPUTS
        [pscustomobject] @{ ExitCode; StdOut; StdErr; TimedOut; Command }
    #>
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)][string]$FilePath,
        [string[]]$Arguments = @(),
        [int]$TimeoutSec = 30,
        [hashtable]$Environment,
        [string]$WorkingDirectory
    )

    $psi = [System.Diagnostics.ProcessStartInfo]::new()
    $psi.FileName = $FilePath
    foreach ($a in $Arguments) { $psi.ArgumentList.Add([string]$a) }
    $psi.RedirectStandardOutput = $true
    $psi.RedirectStandardError = $true
    $psi.UseShellExecute = $false
    $psi.CreateNoWindow = $true
    if ($WorkingDirectory) { $psi.WorkingDirectory = $WorkingDirectory }
    if ($Environment) {
        foreach ($k in $Environment.Keys) { $psi.Environment[$k] = [string]$Environment[$k] }
    }

    $cmdText = "$FilePath $($Arguments -join ' ')"
    Write-ItLog -Level TRACE -Message "exec: $cmdText"

    $p = [System.Diagnostics.Process]::new()
    $p.StartInfo = $psi
    try {
        [void]$p.Start()
        # Read both streams fully via async tasks to avoid pipe-buffer deadlock and the
        # cross-runspace races of Register-ObjectEvent. ReadToEndAsync completes at EOF
        # (when the child exits and closes its stdio), so GetResult() is safe afterward.
        $outTask = $p.StandardOutput.ReadToEndAsync()
        $errTask = $p.StandardError.ReadToEndAsync()
        $timedOut = $false
        if (-not $p.WaitForExit($TimeoutSec * 1000)) {
            $timedOut = $true
            try { $p.Kill($true) } catch { }
            $p.WaitForExit(2000) | Out-Null
        }
        $out = try { $outTask.GetAwaiter().GetResult() } catch { '' }
        $err = try { $errTask.GetAwaiter().GetResult() } catch { '' }
        $code = if ($timedOut) { -1 } else { $p.ExitCode }
        [pscustomobject]@{
            ExitCode = $code
            StdOut   = $out
            StdErr   = $err
            TimedOut = $timedOut
            Command  = $cmdText
        }
    }
    finally {
        $p.Dispose()
    }
}

function ConvertFrom-JsonSafe {
    <# Tolerant JSON parse: returns $null on empty/invalid instead of throwing. #>
    [CmdletBinding()]
    param([Parameter(ValueFromPipeline)][string]$InputObject)
    process {
        if ([string]::IsNullOrWhiteSpace($InputObject)) { return $null }
        try { return $InputObject | ConvertFrom-Json -Depth 64 } catch { return $null }
    }
}

function Wait-Until {
    <#
    .SYNOPSIS
        Poll a predicate scriptblock until it returns a truthy value or the timeout
        elapses. Returns the last predicate result. Throws on timeout unless -Quiet.
    .NOTES
        This is the ONLY sanctioned wait mechanism — never Start-Sleep a fixed wait.
    #>
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)][scriptblock]$Condition,
        [int]$TimeoutSec = 30,
        [double]$IntervalSec = 0.5,
        [string]$Because = 'condition',
        [switch]$Quiet
    )
    $deadline = (Get-Date).AddSeconds($TimeoutSec)
    $last = $null
    do {
        try { $last = & $Condition } catch { $last = $null; Write-ItLog -Level TRACE -Message "Wait-Until predicate threw: $_" }
        if ($last) { return $last }
        Start-Sleep -Seconds $IntervalSec
    } while ((Get-Date) -lt $deadline)
    if ($Quiet) { return $last }
    throw "Wait-Until timed out after ${TimeoutSec}s waiting for: $Because"
}

function Test-Until {
    <# Boolean-returning Wait-Until (never throws). #>
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)][scriptblock]$Condition,
        [int]$TimeoutSec = 30,
        [double]$IntervalSec = 0.5
    )
    [bool](Wait-Until -Condition $Condition -TimeoutSec $TimeoutSec -IntervalSec $IntervalSec -Quiet)
}
