# Sessions.ps1 — agent session registry via `wta sessions list --json`.

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
        $j = Invoke-Wta -App $App -Arguments @('--json', 'sessions', 'list', '--origin', $Origin)
        if ($null -eq $j) { return @() }
        if ($j.sessions) { return $j.sessions }
        if ($j -is [System.Array]) { return $j }
        @($j)
    }
}
