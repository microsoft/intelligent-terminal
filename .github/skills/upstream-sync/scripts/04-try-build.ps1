<#
.SYNOPSIS
  Try-build. Runs the configured build command in a razzle environment and
  captures the result. Default: `cmd /c "tools\razzle.cmd && bz no_clean"`.

.DESCRIPTION
  Run AFTER cherry-picking (03) and BEFORE finalizing the PR (SKILL.md
  step 8). If the build fails, the agent commits a fix on the same sync
  branch so it lands in the same PR — that is why try-build is step 04,
  not the last step.

.PARAMETER BuildCommand
  Override the default build command. Must be a string the cmd.exe shell
  can execute (razzle is cmd-based; PowerShell-keyword chaining like 'if'
  won't work — keep it cmd-shell-friendly with &&).

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
      "log_path":     "<repo-relative path to full log>",
      "log_tail":     "<last ~200 lines of output>"
    }
#>
[CmdletBinding()]
param(
    [string] $BuildCommand   = 'tools\razzle.cmd && bz no_clean',
    [int]    $TimeoutMinutes = 45,
    [string] $LogDir
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

# --- Inlined helpers (single-use) ----------

function Get-RepoRoot {
    $r = git rev-parse --show-toplevel 2>$null
    if ($LASTEXITCODE -ne 0) { throw "Not inside a git repo." }
    return $r.Trim()
}

function Get-GeneratedDir {
    # Per-skill, per-day artifact dir under the repo's gitignored
    # `Generated Files/` root (matches the workspace convention; the repo's
    # top-level .gitignore has `**/Generated Files/`).
    param([string] $Sub)
    $root = Get-RepoRoot
    $date = (Get-Date).ToUniversalTime().ToString('yyyy-MM-dd')
    $path = Join-Path $root "Generated Files/upstream-sync/$date"
    if ($Sub) { $path = Join-Path $path $Sub }
    if (-not (Test-Path -LiteralPath $path)) {
        New-Item -ItemType Directory -Path $path -Force | Out-Null
    }
    return $path
}

function ConvertTo-RepoRelativePath {
    # Normalize to forward-slash, repo-relative form so callers can safely
    # embed it in committed text without leaking machine-specific drive
    # letters / user dirs.
    param([Parameter(Mandatory)] [string] $Path)
    $root = ((Get-RepoRoot) -replace '\\','/').TrimEnd('/')
    $abs  = $Path -replace '\\','/'
    if ($abs.Equals($root, [System.StringComparison]::OrdinalIgnoreCase)) {
        throw "ConvertTo-RepoRelativePath: refusing to return empty (path == repo root): $Path"
    }
    $prefix = "$root/"
    if ($abs.StartsWith($prefix, [System.StringComparison]::OrdinalIgnoreCase)) {
        return $abs.Substring($prefix.Length)
    }
    throw "ConvertTo-RepoRelativePath: '$Path' is not under repo root '$root'."
}

# --- Main logic ------------------------------------------------------------

try {
    if (-not $LogDir) { $LogDir = Get-GeneratedDir -Sub 'build-logs' }
    $root = Get-RepoRoot
    if (-not (Test-Path -LiteralPath $LogDir)) { New-Item -ItemType Directory -Path $LogDir -Force | Out-Null }

    $stamp   = (Get-Date).ToUniversalTime().ToString('yyyy-MM-ddTHHmmss.fff')
    $suffix  = [guid]::NewGuid().ToString('N').Substring(0,4)
    $logPath = Join-Path $LogDir "$stamp-$suffix.log"

    $cmdLine = "/c `"cd /d `"$root`" && $BuildCommand`""
    $started = Get-Date

    $psi = New-Object System.Diagnostics.ProcessStartInfo
    $shell = if ($env:ComSpec) { $env:ComSpec } else { 'cmd.exe' }
    $psi.FileName               = $shell
    $psi.Arguments              = $cmdLine
    $psi.WorkingDirectory       = $root
    $psi.RedirectStandardOutput = $true
    $psi.RedirectStandardError  = $true
    $psi.UseShellExecute        = $false
    $psi.CreateNoWindow         = $true

    $proc       = [System.Diagnostics.Process]::Start($psi)
    $baseWriter = $null
    $writer     = $null

    try {
        # Tee stdout/stderr to the log file as the build runs. The synchronized
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
        if ($writer)     { try { $writer.Flush() }      catch {}
                           try { $writer.Close() }      catch {} }
        if ($baseWriter) { try { $baseWriter.Dispose() } catch {} }
        if ($proc)       { try { $proc.Dispose() }      catch {} }
    }

    $ended      = Get-Date
    $durationMs = [int]($ended - $started).TotalMilliseconds

    $tailLines = if (Test-Path -LiteralPath $logPath) {
        @(Get-Content -LiteralPath $logPath -Tail 200) -join "`n"
    } else { '' }

    $logPathForReport = ConvertTo-RepoRelativePath $logPath  # fails fast if outside repo — log_path is a public contract field

    [ordered] @{
        kind        = $kind
        exit_code   = $exitCode
        duration_ms = $durationMs
        command     = $BuildCommand
        log_path    = $logPathForReport
        log_tail    = $tailLines
    } | ConvertTo-Json -Depth 4
}
catch {
    Write-Error $_.Exception.Message
    Write-Error $_.ScriptStackTrace
    throw
}
