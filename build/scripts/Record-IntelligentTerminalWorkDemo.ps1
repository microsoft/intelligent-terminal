# Copyright (c) Microsoft Corporation.
# Licensed under the MIT license.

<#
.SYNOPSIS
Record the natural Work demo from its owned native window, never the desktop.
.DESCRIPTION
Requires FFmpeg and its sibling ffprobe. PrintWindow captures continuous real
window frames; GraphicsCapture optionally uses the Windows gfxcapture filter.
Requires an interactive desktop and brings only the owned demo window forward.
Scenario data is simulated; no real agents are launched.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$FfmpegPath,
    [Parameter(Mandatory)][string]$OutputDirectory,
    [Parameter(Mandatory)][string]$ExpectedWtaSha256,
    [ValidateSet('GraphicsCapture', 'GdiWindow', 'PrintWindow')][string]$CaptureBackend = 'PrintWindow',
    [ValidateRange(2, 15)][int]$PauseSeconds = 5
)
$ErrorActionPreference = 'Stop'
$repositoryRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..\..'))
$FfmpegPath = (Resolve-Path -LiteralPath $FfmpegPath).Path
$ffprobe = Join-Path (Split-Path $FfmpegPath) 'ffprobe.exe'
if (-not (Test-Path -LiteralPath $ffprobe)) { throw 'FFmpeg requires a sibling ffprobe.exe for verification.' }
$OutputDirectory = [IO.Path]::GetFullPath($OutputDirectory)
if (Test-Path -LiteralPath $OutputDirectory) { throw 'Choose a fresh output directory; recordings are never overwritten.' }
New-Item -ItemType Directory -Path $OutputDirectory | Out-Null
Import-Module (Join-Path $repositoryRoot 'test\e2e\ItE2E\ItE2E.psd1') -Force
. (Join-Path $repositoryRoot 'test\e2e\tests\helpers\NativeWorkStoryDemo.ps1')
$context = @{}
$recorder = $null
$clock = [Diagnostics.Stopwatch]::new()
$chapters = [Collections.Generic.List[object]]::new()
$raw = Join-Path $OutputDirectory 'work-demo-capture.mp4'
$video = Join-Path $OutputDirectory 'work-demo.mp4'
$log = Join-Path $OutputDirectory 'capture.log'
$progress = Join-Path $OutputDirectory 'capture-progress.txt'
$stopPath = Join-Path $OutputDirectory 'stop-capture'
try {
    Start-NativeWorkStoryDemo -Context $context -Package Dev `
        -StateDirectory (Join-Path $OutputDirectory 'state') `
        -ArtifactDirectory (Join-Path $OutputDirectory 'native') `
        -ExpectedWtaSha256 $ExpectedWtaSha256 | Out-Null
    $context.InputRoute = 'ConsoleInput'
    if (-not (Set-WtWindowForeground -App $context.App)) {
        throw 'The owned demo could not become foreground. Use an unlocked interactive desktop; cached background frames are not a recording.'
    }
    $start = [Diagnostics.ProcessStartInfo]::new($FfmpegPath)
    $start.UseShellExecute = $false
    $start.RedirectStandardError = $true
    $start.RedirectStandardInput = $true
    $captureArguments = if ($CaptureBackend -eq 'GraphicsCapture') {
        @('-f', 'lavfi', '-i',
          "gfxcapture=hwnd=$($context.App.Hwnd):max_framerate=24:capture_cursor=0:width=-2:height=-2",
          '-vf', 'hwdownload,format=bgra,format=yuv420p')
    } else {
        @('-f', 'gdigrab', '-framerate', '24', '-draw_mouse', '0',
          '-i', "hwnd=$($context.App.Hwnd)",
          '-vf', 'pad=ceil(iw/2)*2:ceil(ih/2)*2,format=yuv420p')
    }
    foreach ($argument in (@('-hide_banner', '-loglevel', 'info') + $captureArguments + @(
        '-c:v', 'libx264', '-preset', 'veryfast', '-tune', 'zerolatency',
        '-crf', '18', '-r', '24',
        '-an', '-t', '600', '-progress', $progress, '-stats_period', '0.5',
        '-movflags', '+faststart', $raw
    ))) { $start.ArgumentList.Add($argument) }
    if ($CaptureBackend -eq 'PrintWindow') {
        $start.FileName = (Get-Command pwsh -ErrorAction Stop).Source
        $start.ArgumentList.Clear()
        foreach ($argument in @(
            '-NoProfile', '-File', (Join-Path $PSScriptRoot 'Capture-IntelligentTerminalWorkDemoWindow.ps1'),
            '-WindowHandle', [string]$context.App.Hwnd, '-OwnerProcessId', [string]$context.App.Pid,
            '-FfmpegPath', $FfmpegPath, '-OutputPath', $raw,
            '-ProgressPath', $progress, '-LogPath', (Join-Path $OutputDirectory 'encoder.log'),
            '-StopPath', $stopPath
        )) { $start.ArgumentList.Add($argument) }
    }
    $clock.Start()
    $recorder = [Diagnostics.Process]::Start($start)
    $stderr = $recorder.StandardError.ReadToEndAsync()
    $deadline = [DateTime]::UtcNow.AddSeconds(20)
    do {
        if ($recorder.HasExited) {
            $stderr.GetAwaiter().GetResult() | Set-Content -LiteralPath $log -Encoding utf8NoBOM
            throw "Window recorder exited before the journey; see $log"
        }
        $capturing = (Test-Path -LiteralPath $progress) -and
            ((Get-Content -LiteralPath $progress -Raw) -match '(?m)^frame=[1-9][0-9]*\r?$')
        if ($capturing) { break }
        Start-Sleep -Milliseconds 250
    } while ([DateTime]::UtcNow -lt $deadline)
    if (-not $capturing) {
        throw 'No actual window frames arrived. Unlock an interactive desktop before recording; no desktop or synthetic fallback is used.'
    }
    $step = {
        param($event)
        if ($recorder.HasExited) { throw 'Window recorder stopped during the journey.' }
        $chapter = [pscustomobject]@{
            Scene = [int]$event.Scene
            Title = [string]$event.Title
            Seconds = [Math]::Round($clock.Elapsed.TotalSeconds, 3)
        }
        $chapters.Add($chapter)
        $chapters | ConvertTo-Json -Depth 6 |
            Set-Content -LiteralPath (Join-Path $OutputDirectory 'chapters.json') -Encoding utf8NoBOM
        Get-NativeWorkStoryText -Context $context |
            Set-Content -LiteralPath (Join-Path $OutputDirectory ("scene-{0:D2}.txt" -f $chapter.Scene)) -Encoding utf8NoBOM
    }
    Invoke-NativeWorkStoryWalkthrough -Context $context -PauseSeconds $PauseSeconds -OnStep $step | Out-Null
    Start-Sleep -Seconds $PauseSeconds
    if ($CaptureBackend -eq 'PrintWindow') { [IO.File]::WriteAllText($stopPath, 'stop') }
    else { $recorder.StandardInput.WriteLine('q') }
    if (-not $recorder.WaitForExit(15000)) { throw 'Window recorder did not finalize its MP4.' }
    $stderr.GetAwaiter().GetResult() | Set-Content -LiteralPath $log -Encoding utf8NoBOM
    if ($recorder.ExitCode -ne 0) { throw "Window recorder failed with exit code $($recorder.ExitCode)." }
    if (@($chapters.Scene | Sort-Object -Unique).Count -ne 8) { throw 'The recording did not cover all eight scenes.' }
    $metadata = & $ffprobe -v error -count_frames -show_streams -show_format -of json $raw | ConvertFrom-Json
    if ($LASTEXITCODE -ne 0) { throw 'Recorded video failed ffprobe verification.' }
    $stream = @($metadata.streams | Where-Object codec_type -EQ video)
    if ($stream.Count -ne 1 -or [double]$metadata.format.duration -lt 45 -or
        [int]$stream[0].nb_read_frames -lt 500) { throw 'Video is missing a complete sequence of captured frames.' }
    $metadata | ConvertTo-Json -Depth 12 |
        Set-Content -LiteralPath (Join-Path $OutputDirectory 'video-metadata.json') -Encoding utf8NoBOM
    $subtitles = [Collections.Generic.List[string]]::new()
    for ($i = 0; $i -lt $chapters.Count; $i++) {
        $end = if ($i + 1 -lt $chapters.Count) { $chapters[$i + 1].Seconds } else { [double]$metadata.format.duration }
        $begin = [Math]::Min($chapters[$i].Seconds, $end)
        $from = [TimeSpan]::FromSeconds($begin).ToString('hh\:mm\:ss\,fff')
        $to = [TimeSpan]::FromSeconds($end).ToString('hh\:mm\:ss\,fff')
        $subtitles.Add("$($i + 1)`n$from --> $to`n$($chapters[$i].Scene). $($chapters[$i].Title)`n")
    }
    $srt = Join-Path $OutputDirectory 'work-demo.en.srt'
    $subtitles | Set-Content -LiteralPath $srt -Encoding utf8NoBOM
    & $FfmpegPath -hide_banner -loglevel error -i $raw -i $srt -map 0:v -map 1:0 `
        -c:v copy -c:s mov_text -metadata:s:s:0 language=eng -disposition:s:0 default `
        -metadata comment='Actual native Intelligent Terminal window capture; isolated simulated Work data; automated demonstration input; no real agent execution or token billing.' `
        -movflags +faststart $video
    if ($LASTEXITCODE -ne 0) { throw 'Failed to mux the recorded window video and scene captions.' }
    [ordered]@{
        video = $video
        capture = "$CaptureBackend; exact owned HWND only; never desktop"
        hwnd = $context.App.Hwnd
        wtaSha256 = $context.Sha256
        simulatedData = $true
        input = 'Guarded input records to the uniquely owned native demo Console; no global keyboard injection'
        durationSeconds = [double]$metadata.format.duration
        frames = [int]$stream[0].nb_read_frames
    } | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $OutputDirectory 'recording.json') -Encoding utf8NoBOM
    $video
} finally {
    if ($recorder -and -not $recorder.HasExited) {
        if ($CaptureBackend -eq 'PrintWindow') { [IO.File]::WriteAllText($stopPath, 'stop') }
        else { $recorder.StandardInput.WriteLine('q') }
        if (-not $recorder.WaitForExit(5000)) {
            $recorder.Kill($true)
            $recorder.WaitForExit()
        }
    }
    if ($recorder) { $recorder.Dispose() }
    if ($context.OwnedWindowId) { Stop-NativeWorkStoryDemo -Context $context }
}
