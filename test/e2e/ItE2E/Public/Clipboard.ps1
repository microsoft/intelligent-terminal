# Clipboard.ps1 — OS clipboard primitives for tests.
#
# The agent pane's Alt+V image paste (issue #211) reads the *live* OS clipboard from the
# wta-helper (a real Win32 process sharing the interactive session's clipboard). To exercise
# it end-to-end a test must place a real image on the clipboard first.
#
# `Clipboard.SetImage` requires an STA thread, so we run it on a dedicated STA runspace
# (pwsh 7 defaults to MTA and has no -STA switch). The bitmap lands as CF_BITMAP/CF_DIB,
# which `read_clipboard_image` picks up via its CF_DIBV5/CF_DIB path (label "screenshot").

function Invoke-ClipboardSta {
    param(
        [Parameter(Mandatory)][scriptblock]$ScriptBlock,
        [object[]]$ArgumentList = @()
    )

    $rs = [runspacefactory]::CreateRunspace()
    $rs.ApartmentState = 'STA'
    $rs.ThreadOptions = 'ReuseThread'
    $rs.Open()
    try {
        $ps = [powershell]::Create()
        try {
            $ps.Runspace = $rs
            $null = $ps.AddScript($ScriptBlock)
            foreach ($argument in $ArgumentList) {
                $null = $ps.AddArgument($argument)
            }
            $result = @($ps.Invoke())
            if ($ps.Streams.Error.Count) { throw "Clipboard operation failed: $($ps.Streams.Error[0])" }
            $result
        }
        finally { $ps.Dispose() }
    }
    finally { $rs.Close(); $rs.Dispose() }
}

function Get-ClipboardSnapshot {
    <# Return a complete in-memory clipboard data object, or $null for an empty clipboard. #>
    [CmdletBinding()] param()

    Invoke-ClipboardSta -ScriptBlock {
        Add-Type -AssemblyName System.Windows.Forms
        $source = [System.Windows.Forms.Clipboard]::GetDataObject()
        if ($null -eq $source) { return }
        $snapshot = [System.Windows.Forms.DataObject]::new()
        foreach ($format in $source.GetFormats($true)) {
            try {
                $data = $source.GetData($format, $true)
                if ($null -ne $data) { $snapshot.SetData($format, $data) }
            }
            catch {
                # Some application-private formats cannot be materialized outside their
                # owner. Preserve every format that the clipboard can actually provide.
            }
        }
        $snapshot
    } | Select-Object -First 1
}

function Restore-ClipboardSnapshot {
    <# Restore a snapshot from Get-ClipboardSnapshot, including the empty state. #>
    [CmdletBinding()] param([AllowNull()]$Snapshot)

    Invoke-ClipboardSta -ScriptBlock {
        param($saved)
        Add-Type -AssemblyName System.Windows.Forms
        if ($null -eq $saved) {
            [System.Windows.Forms.Clipboard]::Clear()
        }
        else {
            [System.Windows.Forms.Clipboard]::SetDataObject($saved, $true)
        }
    } -ArgumentList @($Snapshot) | Out-Null
}

function Set-ClipboardImage {
    <#
    .SYNOPSIS
        Put a synthetic bitmap on the OS clipboard (CF_DIB) so the agent pane's Alt+V has a
        real image to capture. Returns nothing; throws if the clipboard write fails.
    .PARAMETER Width / -Height
        Size of the generated bitmap (default 24x24).
    #>
    [CmdletBinding()]
    param([int]$Width = 24, [int]$Height = 24)

    $worker = {
        param($w, $h)
        Add-Type -AssemblyName System.Windows.Forms, System.Drawing
        $bmp = [System.Drawing.Bitmap]::new($w, $h)
        for ($x = 0; $x -lt $w; $x++) {
            for ($y = 0; $y -lt $h; $y++) {
                $color = [System.Drawing.Color]::FromArgb(255, ($x * 10 % 256), ($y * 10 % 256), 128)
                $bmp.SetPixel($x, $y, $color)
            }
        }
        [System.Windows.Forms.Clipboard]::SetImage($bmp)
        $bmp.Dispose()
    }

    Invoke-ClipboardSta -ScriptBlock $worker -ArgumentList @($Width, $Height) | Out-Null
}
