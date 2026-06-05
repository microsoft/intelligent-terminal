<#
.SYNOPSIS
  Try build. Runs the configured build command in a razzle environment
  and captures the result. Default: `cmd /c "tools\razzle.cmd && bz no_clean"`.

.PARAMETER BuildCommand
  Override the default build command. Must be a string the cmd.exe
  shell can execute (razzle is cmd-based; PowerShell-keyword chaining
  like 'if' won't work — keep it cmd-shell-friendly with &&).

.PARAMETER TimeoutMinutes
  Wall-clock cap. Default 45. On timeout, the build is killed and the
  result is classified as 'build-inconclusive'.

.PARAMETER LogDir
  Where to write the full build log. Default:
  `Generated Files/upstream-sync/<YYYY-MM-DD>/build-logs/` (gitignored).

.OUTPUTS
  JSON on stdout:
    {
      "kind":         "build-ok" | "build-failed" | "build-inconclusive",
      "exit_code":    <int>,
      "duration_ms":  <int>,
      "command":      "<the command that ran>",
      "log_path":     "<path to full log>",
      "log_tail":     "<last ~200 lines of output>"
    }

  Exit / error model:
    Stdout JSON on success (orchestrator path).
    Throws on wrapper error (couldn't start the build at all). The
    orchestrator (`04-run-batch.ps1`) catches and routes through its
    own exit-code mapping (0 ok / 10 stuck / 20 error). When the script
    is run standalone for debugging, an uncaught throw exits with
    PowerShell's default code (1) and prints the stack trace.
    `exit 20` is intentionally NOT used here: the script is invoked via
    `& "$PSScriptRoot/10-try-build.ps1"`, and `exit` in that context
    would terminate the orchestrator mid-pipeline.
#>
[CmdletBinding()]
param(
    [string] $BuildCommand = 'tools\razzle.cmd && bz no_clean',
    [int]    $TimeoutMinutes = 45,
    [string] $LogDir
)

. "$PSScriptRoot/Common.ps1"

try {
    if (-not $LogDir) {
        # Default: per-day, per-skill artifact dir under the gitignored
        # `Generated Files/` root. Get-GeneratedDir creates it on demand.
        $LogDir = Get-GeneratedDir -Sub 'build-logs'
    }
    $root = Get-RepoRoot
    if (-not (Test-Path -LiteralPath $LogDir)) { New-Item -ItemType Directory -Path $LogDir -Force | Out-Null }

    $stamp   = (Get-Date).ToString('yyyy-MM-ddTHHmmss')
    $logPath = Join-Path $LogDir "$stamp.log"

    $cmdLine = "/c `"cd /d `"$root`" && $BuildCommand`""
    $started = Get-Date

    # Use Start-Process with redirection so we can both tail and tee.
    $psi = New-Object System.Diagnostics.ProcessStartInfo
    $psi.FileName               = $env:ComSpec
    $psi.Arguments              = $cmdLine
    $psi.WorkingDirectory       = $root
    $psi.RedirectStandardOutput = $true
    $psi.RedirectStandardError  = $true
    $psi.UseShellExecute        = $false
    $psi.CreateNoWindow         = $true

    $proc = [System.Diagnostics.Process]::Start($psi)
    $baseWriter = $null
    $writer     = $null

    try {
        # Tee stdout/stderr into the log file as the build runs. The synchronized
        # wrapper serializes concurrent stdout/stderr DataReceived callbacks.
        $baseWriter = [System.IO.StreamWriter]::new($logPath, $false, [System.Text.UTF8Encoding]::new($false))
        $writer = [System.IO.TextWriter]::Synchronized($baseWriter)
        $proc.add_OutputDataReceived({ param($s,$e) if ($null -ne $e.Data) { $writer.WriteLine($e.Data) } })
        $proc.add_ErrorDataReceived({  param($s,$e) if ($null -ne $e.Data) { $writer.WriteLine($e.Data) } })
        $proc.BeginOutputReadLine()
        $proc.BeginErrorReadLine()

        $timeoutMs = $TimeoutMinutes * 60 * 1000
        $exited    = $proc.WaitForExit($timeoutMs)
        $kind      = $null
        $exitCode  = $null

        if (-not $exited) {
            try { $proc.Kill($true) } catch { }
            $proc.WaitForExit()
            $kind     = 'build-inconclusive'
            $exitCode = -1
        } else {
            # WaitForExit(timeout) can return before async output callbacks drain.
            # The parameterless wait completes only after redirected output events finish.
            $proc.WaitForExit()
            $exitCode = $proc.ExitCode
            $kind     = if ($exitCode -eq 0) { 'build-ok' } else { 'build-failed' }
        }
    }
    finally {
        # Always release the log file handle and process — scheduler runs are
        # unattended and a leaked handle would jam the next run's log write.
        if ($writer)     { try { $writer.Flush() }      catch {}
                           try { $writer.Close() }      catch {} }
        if ($baseWriter) { try { $baseWriter.Dispose() } catch {} }
        if ($proc)       { try { $proc.Dispose() }      catch {} }
    }

    $ended      = Get-Date
    $durationMs = [int]($ended - $started).TotalMilliseconds

    # Capture the last ~200 lines for the report / stuck issue.
    $tailLines = if (Test-Path -LiteralPath $logPath) {
        @(Get-Content -LiteralPath $logPath -Tail 200) -join "`n"
    } else { '' }

    # Emit a repo-relative log_path when the log lives inside the repo
    # (the common case). Absolute paths leak machine-specific details
    # like username + drive letter into GitHub issues/reports. Fall back
    # to the absolute path when the user passed a custom -LogDir that
    # sits outside the repo root.
    $logPathForReport = try { ConvertTo-RepoRelativePath $logPath } catch { $logPath }
    $doc = [ordered] @{
        kind        = $kind
        exit_code   = $exitCode
        duration_ms = $durationMs
        command     = $BuildCommand
        log_path    = $logPathForReport
        log_tail    = $tailLines
    }
    $doc | ConvertTo-Json -Depth 4
}
catch {
    Write-Error $_.Exception.Message
    Write-Error $_.ScriptStackTrace
    throw
}
