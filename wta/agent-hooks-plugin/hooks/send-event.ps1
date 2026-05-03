# send-event.ps1 — Forward Copilot CLI hook events to WTA via wtcli
param([string]$EventType = "agent.hook")

# Skip if not running inside Windows Terminal
if (-not $env:WT_COM_CLSID) { exit 0 }

# Skip if wtcli not available
$wtcliPath = (Get-Command wtcli -ErrorAction SilentlyContinue).Source
if (-not $wtcliPath) { exit 0 }

# Read hook JSON from stdin
$hookData = [Console]::In.ReadToEnd()
if (-not $hookData -or -not $hookData.Trim()) { exit 0 }

# Wrap payload and send via ProcessStartInfo to avoid PowerShell argument mangling
try {
    $parsed = $hookData | ConvertFrom-Json
    $payload = @{ cli_source = "copilot"; payload = $parsed } | ConvertTo-Json -Compress -Depth 5

    # Escape quotes for raw command line: each " becomes \"
    $escaped = $payload.Replace('"', '\"')
    $psi = New-Object System.Diagnostics.ProcessStartInfo
    $psi.FileName = $wtcliPath
    $psi.Arguments = "send-event -e $EventType `"$escaped`""
    $psi.UseShellExecute = $false
    $psi.CreateNoWindow = $true
    $psi.RedirectStandardError = $true
    $proc = [System.Diagnostics.Process]::Start($psi)
    $proc.WaitForExit(5000)
} catch {
    # Silently ignore errors
}
