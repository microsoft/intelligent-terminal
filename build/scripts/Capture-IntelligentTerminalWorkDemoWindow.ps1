# Copyright (c) Microsoft Corporation.
# Licensed under the MIT license.

[CmdletBinding()]
param(
    [Parameter(Mandatory)][long]$WindowHandle,
    [Parameter(Mandatory)][int]$OwnerProcessId,
    [Parameter(Mandatory)][string]$FfmpegPath,
    [Parameter(Mandatory)][string]$OutputPath,
    [Parameter(Mandatory)][string]$ProgressPath,
    [Parameter(Mandatory)][string]$LogPath,
    [Parameter(Mandatory)][string]$StopPath
)
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing
Add-Type @'
using System;
using System.Runtime.InteropServices;
public static class WorkWindowCapture {
    [StructLayout(LayoutKind.Sequential)]
    public struct Rect { public int Left, Top, Right, Bottom; }
    [DllImport("user32.dll", SetLastError=true)]
    public static extern bool GetWindowRect(IntPtr hwnd, out Rect rect);
    [DllImport("user32.dll")]
    public static extern uint GetWindowThreadProcessId(IntPtr hwnd, out uint pid);
    [DllImport("user32.dll", SetLastError=true)]
    public static extern bool PrintWindow(IntPtr hwnd, IntPtr dc, uint flags);
    [DllImport("user32.dll", SetLastError=true)]
    public static extern IntPtr SetThreadDpiAwarenessContext(IntPtr context);
}
'@
$hwnd = [IntPtr]$WindowHandle
$previousDpi = [WorkWindowCapture]::SetThreadDpiAwarenessContext([IntPtr](-4))
if ($previousDpi -eq [IntPtr]::Zero) { throw 'Cannot establish pixel-accurate window capture.' }
$bounds = [WorkWindowCapture+Rect]::new()
if (-not [WorkWindowCapture]::GetWindowRect($hwnd, [ref]$bounds)) { throw 'Owned window is unavailable.' }
$width = $bounds.Right - $bounds.Left
$height = $bounds.Bottom - $bounds.Top
if ($width -le 0 -or $height -le 0) { throw 'Owned window has no drawable bounds.' }
$bitmap = [Drawing.Bitmap]::new($width, $height, [Drawing.Imaging.PixelFormat]::Format24bppRgb)
$graphics = [Drawing.Graphics]::FromImage($bitmap)
$start = [Diagnostics.ProcessStartInfo]::new($FfmpegPath)
$start.UseShellExecute = $false
$start.RedirectStandardInput = $true
$start.RedirectStandardError = $true
foreach ($arg in @(
    '-hide_banner', '-loglevel', 'info', '-f', 'image2pipe', '-vcodec', 'png',
    '-framerate', '8', '-use_wallclock_as_timestamps', '1', '-i', 'pipe:0',
    '-vf', 'pad=ceil(iw/2)*2:ceil(ih/2)*2,format=yuv420p',
    '-c:v', 'libx264', '-preset', 'veryfast', '-tune', 'zerolatency',
    '-crf', '18', '-r', '24', '-an', '-movflags', '+faststart',
    '-progress', $ProgressPath, '-stats_period', '0.5', $OutputPath
)) { $start.ArgumentList.Add($arg) }
$encoder = $null
try {
    $encoder = [Diagnostics.Process]::Start($start)
    $errors = $encoder.StandardError.ReadToEndAsync()
    $clock = [Diagnostics.Stopwatch]::StartNew()
    while (-not (Test-Path -LiteralPath $StopPath)) {
        if ($clock.Elapsed.TotalSeconds -gt 600) { throw 'Window capture exceeded its ten-minute safety limit.' }
        if ($encoder.HasExited) { throw 'Window video encoder stopped unexpectedly.' }
        [uint32]$currentOwner = 0
        [void][WorkWindowCapture]::GetWindowThreadProcessId($hwnd, [ref]$currentOwner)
        if ($currentOwner -ne $OwnerProcessId) { throw 'Window ownership changed; capture stopped.' }
        $frameStart = $clock.Elapsed.TotalMilliseconds
        $dc = $graphics.GetHdc()
        try {
            if (-not [WorkWindowCapture]::PrintWindow($hwnd, $dc, 2)) {
                throw 'PrintWindow failed; no desktop or synthetic fallback is allowed.'
            }
        } finally { $graphics.ReleaseHdc($dc) }
        $frame = [IO.MemoryStream]::new()
        try {
            $bitmap.Save($frame, [Drawing.Imaging.ImageFormat]::Png)
            $frame.Position = 0
            $frame.CopyTo($encoder.StandardInput.BaseStream)
            $encoder.StandardInput.BaseStream.Flush()
        } finally { $frame.Dispose() }
        $remaining = 125 - ($clock.Elapsed.TotalMilliseconds - $frameStart)
        if ($remaining -gt 0) { Start-Sleep -Milliseconds ([int]$remaining) }
    }
    $encoder.StandardInput.Close()
    if (-not $encoder.WaitForExit(15000)) { throw 'Window encoder did not finish.' }
    if ($encoder.ExitCode -ne 0) { throw "Window encoder failed: $($encoder.ExitCode)." }
} finally {
    $graphics.Dispose()
    $bitmap.Dispose()
    [void][WorkWindowCapture]::SetThreadDpiAwarenessContext($previousDpi)
    if ($encoder) {
        if (-not $encoder.HasExited) {
            $encoder.StandardInput.Close()
            if (-not $encoder.WaitForExit(5000)) { $encoder.Kill(); $encoder.WaitForExit() }
        }
        $errors.GetAwaiter().GetResult() | Set-Content -LiteralPath $LogPath -Encoding utf8NoBOM
        $encoder.Dispose()
    }
}
