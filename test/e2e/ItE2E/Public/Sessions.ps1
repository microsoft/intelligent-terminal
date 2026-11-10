# Sessions.ps1 — agent session registry via `wta sessions list --json`.

function Get-WtaMasterPipe {
    <# Read the exact test app's master endpoint from its child process, not package-wide state. #>
    [CmdletBinding()]
    param([Parameter(Mandatory)]$App, [int]$TimeoutSec = 30)
    if (-not $App.Pid) { throw 'A Terminal process ID is required to select its WTA master.' }
    $root = Get-CimInstance Win32_Process -Filter "ProcessId=$($App.Pid)" -ErrorAction Stop
    if (-not $root -or $root.ExecutablePath -ine $App.WindowsTerminal -or -not $root.CreationDate) {
        throw "Cannot verify the selected Terminal process pid=$($App.Pid)."
    }
    $masterCommand = '^\s*(?:"[^"]+"|\S+)\s+--master\s+(?:"([^"]+)"|(\S+))'
    $pipe = Wait-Until -TimeoutSec $TimeoutSec -Because "the WTA master owned by Terminal pid=$($App.Pid)" -Condition {
        $masters = @(Get-CimInstance Win32_Process -Filter "ParentProcessId=$($App.Pid) AND Name='wta.exe'" -ErrorAction Stop |
            Where-Object {
                $_.ExecutablePath -ieq $App.WtaPath -and $_.CreationDate -ge $root.CreationDate -and
                $_.CommandLine -match $masterCommand
            })
        if ($masters.Count -eq 0) { return $null }
        if ($masters.Count -ne 1) { throw 'The selected Terminal process has multiple WTA masters.' }
        $match = [regex]::Match($masters[0].CommandLine, $masterCommand)
        $endpoint = if ($match.Groups[1].Success) { $match.Groups[1].Value } else { $match.Groups[2].Value }
        if (-not $endpoint.StartsWith('\\.\pipe\wta-master-', [StringComparison]::OrdinalIgnoreCase)) {
            throw 'The owned WTA master has an unexpected endpoint.'
        }
        $endpoint
    }
    $current = Get-CimInstance Win32_Process -Filter "ProcessId=$($App.Pid)" -ErrorAction Stop
    if (-not $current -or $current.CreationDate -ne $root.CreationDate) {
        throw 'The selected Terminal process changed while resolving its master.'
    }
    $pipe
}

function Invoke-Wta {
    <# Run a wta subcommand via a runnable (copied) binary, return parsed JSON (or raw). #>
    [CmdletBinding()]
    param([Parameter(Mandatory)]$App, [Parameter(Mandatory)][string[]]$Arguments, [int]$TimeoutSec = 30, [switch]$Raw)
    $exe = Get-RunnableWtaPath -App $App
    if (-not $App.ComClsid) { try { Resolve-WtComClsid -App $App | Out-Null } catch { } }
    $r = Invoke-Native -FilePath $exe -Arguments $Arguments -TimeoutSec $TimeoutSec -Environment @{ WT_COM_CLSID = $App.ComClsid }
    if ($Raw) { return $r }
    if ($r.ExitCode -ne 0) { throw "wta $($Arguments -join ' ') failed (exit $($r.ExitCode)): $($r.StdErr.Trim())" }
    $r.StdOut | ConvertFrom-JsonSafe
}

function Get-WtSessions {
    <# List agent sessions. -Origin shell|agent-pane|all (default all). #>
    [CmdletBinding()]
    param(
        [Parameter(Mandatory, ValueFromPipeline)]$App,
        [ValidateSet('shell', 'agent-pane', 'all')][string]$Origin = 'all'
    )
    process {
        $master = Get-WtaMasterPipe -App $App
        $j = Invoke-Wta -App $App -Arguments @('--json', 'sessions', 'list', '--master', $master, '--origin', $Origin)
        if ($null -eq $j) { return @() }
        if ($j.sessions) { return $j.sessions }
        if ($j -is [System.Array]) { return $j }
        @($j)
    }
}
