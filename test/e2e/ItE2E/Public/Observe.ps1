# Observe.ps1 — read-only capture: logs, the wtcli event stream, and context bundles.

# ── Logs ─────────────────────────────────────────────────────────────────────
function Get-ItLogDir {
    <# Newest versioned log dir (prefer the package version, else most-recent). #>
    [CmdletBinding()] param([Parameter(Mandatory, ValueFromPipeline)]$App)
    process {
        if (-not (Test-Path $App.LogRootDir)) { return $null }
        $byVer = Join-Path $App.LogRootDir $App.Version
        if (Test-Path $byVer) { return $byVer }
        Get-ChildItem $App.LogRootDir -Directory | Sort-Object LastWriteTime -Descending | Select-Object -First 1 -ExpandProperty FullName
    }
}

function Initialize-LogOffsets {
    <# Record current byte length of each log file, so -SinceStart reads only new data. #>
    [CmdletBinding()] param([Parameter(Mandatory, ValueFromPipeline)]$App)
    process {
        $App.LogStartOffset = @{}
        $dir = Get-ItLogDir -App $App
        if (-not $dir) { return $App }
        foreach ($f in Get-ChildItem $dir -Filter *.log -ErrorAction SilentlyContinue) {
            $App.LogStartOffset[$f.Name] = $f.Length
        }
        $App
    }
}

function Get-ItLogText {
    <#
    .SYNOPSIS
        Read log text from the versioned dir. -Name filters file names (wildcard).
        -SinceStart returns only bytes appended after Initialize-LogOffsets (this test's slice).
    #>
    [CmdletBinding()]
    param(
        [Parameter(Mandatory, ValueFromPipeline)]$App,
        [string]$Name = '*',
        [switch]$SinceStart
    )
    process {
        $dir = Get-ItLogDir -App $App
        if (-not $dir) { return '' }
        $sb = [System.Text.StringBuilder]::new()
        foreach ($f in Get-ChildItem $dir -Filter *.log -ErrorAction SilentlyContinue | Where-Object Name -like $Name) {
            $start = if ($SinceStart -and $App.LogStartOffset.ContainsKey($f.Name)) { [int64]$App.LogStartOffset[$f.Name] } else { 0 }
            try {
                $fs = [System.IO.FileStream]::new($f.FullName, 'Open', 'Read', 'ReadWrite')
                try {
                    if ($start -gt 0 -and $start -le $fs.Length) { $fs.Seek($start, 'Begin') | Out-Null }
                    $sr = [System.IO.StreamReader]::new($fs)
                    [void]$sb.AppendLine("# ==== $($f.Name) ====")
                    [void]$sb.Append($sr.ReadToEnd())
                }
                finally { $fs.Dispose() }
            }
            catch { Write-ItLog -Level WARN -Message "read log $($f.Name) failed: $_" }
        }
        $sb.ToString()
    }
}

# ── Event stream (wtcli listen) ──────────────────────────────────────────────
function Start-WtEventListener {
    <#
    .SYNOPSIS
        Start a background `wtcli listen --json` and buffer parsed events into a
        synchronized list. Start this BEFORE the action you want to observe.
    .OUTPUTS
        Listener object: @{ Process; Events; Reg; App }
    #>
    [CmdletBinding()]
    param([Parameter(Mandatory, ValueFromPipeline)]$App, [string]$EventFilter, [string]$SessionId)
    process {
        if (-not $App.ComClsid) { Resolve-WtComClsid -App $App | Out-Null }
        $args = @('--json', 'listen')
        if ($SessionId) { $args += @('-t', $SessionId) }
        if ($EventFilter) { $args += @('--event', $EventFilter) }

        $psi = [System.Diagnostics.ProcessStartInfo]::new()
        $psi.FileName = $App.WtcliPath
        foreach ($a in $args) { $psi.ArgumentList.Add([string]$a) }
        $psi.RedirectStandardOutput = $true
        $psi.RedirectStandardError = $true
        $psi.UseShellExecute = $false
        $psi.CreateNoWindow = $true
        $psi.Environment['WT_COM_CLSID'] = $App.ComClsid

        $events = [System.Collections.ArrayList]::Synchronized([System.Collections.ArrayList]::new())
        $p = [System.Diagnostics.Process]::new(); $p.StartInfo = $psi
        $handler = {
            if ($null -ne $EventArgs.Data) {
                try { $obj = $EventArgs.Data | ConvertFrom-Json -Depth 64; [void]$Event.MessageData.Add($obj) } catch { }
            }
        }
        # Explicit, unique SourceIdentifier so Stop-WtEventListener can unregister
        # deterministically (don't rely on the auto-generated job name).
        $sourceId = "ItE2E.WtEventListener.$([guid]::NewGuid())"
        $reg = Register-ObjectEvent -InputObject $p -EventName OutputDataReceived -Action $handler -MessageData $events -SourceIdentifier $sourceId
        [void]$p.Start(); $p.BeginOutputReadLine(); $p.BeginErrorReadLine()
        Write-ItLog -Level INFO -Message "Event listener started (filter='$EventFilter' session='$SessionId')."
        [pscustomobject]@{ Process = $p; Events = $events; Reg = $reg; SourceId = $sourceId; App = $App }
    }
}

function Get-WtEvents {
    <# Snapshot of buffered events, optionally filtered by predicate. #>
    [CmdletBinding()]
    param([Parameter(Mandatory, ValueFromPipeline)]$Listener, [scriptblock]$Predicate)
    process {
        $snap = @($Listener.Events.ToArray())
        if ($Predicate) { $snap = $snap | Where-Object $Predicate }
        $snap
    }
}

function Wait-WtEvent {
    <#
    .SYNOPSIS
        Wait until a buffered event matches the predicate. Requires a listener started
        before the triggering action. Returns the first matching event.
    #>
    [CmdletBinding()]
    param(
        [Parameter(Mandatory, ValueFromPipeline)]$Listener,
        [Parameter(Mandatory)][scriptblock]$Predicate,
        [int]$TimeoutSec = 30
    )
    process {
        Wait-Until -TimeoutSec $TimeoutSec -IntervalSec 0.4 -Because "a matching WT event" -Condition {
            @(Get-WtEvents -Listener $Listener -Predicate $Predicate) | Select-Object -First 1
        }
    }
}

function Stop-WtEventListener {
    [CmdletBinding()] param([Parameter(Mandatory, ValueFromPipeline)]$Listener)
    process {
        try { if (-not $Listener.Process.HasExited) { $Listener.Process.Kill($true) } } catch { }
        # Prefer the explicit SourceIdentifier we assigned; fall back to the job name.
        $sid = if ($Listener.PSObject.Properties.Name -contains 'SourceId' -and $Listener.SourceId) { $Listener.SourceId } else { $Listener.Reg.Name }
        try { if ($sid) { Unregister-Event -SourceIdentifier $sid -ErrorAction SilentlyContinue } } catch { }
        try { $Listener.Process.Dispose() } catch { }
    }
}

# ── Context bundle (for Verify.AI / diagnostics) ─────────────────────────────
function Get-ContextBundle {
    <#
    .SYNOPSIS
        Aggregate the observable state into one object: active pane buffer, UI tree,
        recent log slice, and a screenshot path. Consumed by Assert-AI and failure dumps.
    #>
    [CmdletBinding()]
    param([Parameter(Mandatory, ValueFromPipeline)]$App, [string]$SessionId, [switch]$Screenshot)
    process {
        $active = try { Get-ActivePane -App $App } catch { $null }
        $sid = if ($SessionId) { $SessionId } elseif ($active) { $active.session_id } else { $null }
        $paneText = if ($sid) { try { Get-WtCapture -App $App -SessionId $sid -MaxLines 100 } catch { '' } } else { '' }
        $tree = try { Get-UiTree -App $App -Depth 4 } catch { '' }
        $logSlice = try { Get-ItLogText -App $App -SinceStart } catch { '' }
        $shot = $null
        if ($Screenshot) { $shot = Save-UiScreenshot -App $App -Path (Join-Path $env:TEMP ("ite2e-shot-{0}.png" -f (Get-Date -Format 'HHmmss'))) }
        [pscustomobject]@{
            Package    = $App.Package
            SessionId  = $sid
            PaneText   = $paneText
            UiTree     = $tree
            LogSlice   = $logSlice
            Screenshot = $shot
            Timestamp  = (Get-Date).ToString('o')
        }
    }
}

function ConvertTo-ContextText {
    <# Flatten a context bundle to a compact text block for an LLM judge. #>
    [CmdletBinding()] param([Parameter(Mandatory, ValueFromPipeline)]$Bundle)
    process {
        @"
== ACTIVE PANE BUFFER ==
$($Bundle.PaneText)

== UI TREE (depth 4) ==
$($Bundle.UiTree)

== RECENT LOG SLICE ==
$($Bundle.LogSlice)
"@
    }
}
