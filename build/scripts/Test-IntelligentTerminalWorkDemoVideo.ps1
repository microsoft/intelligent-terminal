#requires -Version 7.0
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT license.
<#
.SYNOPSIS
Verify the eight-scene Work demo from actual recorded video pixels, offline.
.DESCRIPTION
Run with PowerShell 7. Requires FFmpeg, its sibling ffprobe.exe, and Windows
PowerShell 5.1 with built-in English Windows.Media.Ocr support. No applications
are launched except these local analysis tools. UIA dumps, subtitle text, and recording metadata are never
used as proof of visible content. Run against the final work-demo.mp4.

ChaptersPath is a JSON array of { Scene, Title, Seconds }, one entry per scene
1-8 in chronological order. Callback timestamps may precede display: four
frames at 10%, 35%, 65%, and 90% of each chapter are combined for OCR marker checks.
Static reading pauses are expected; a freeze spanning more than the longest
chapter and identical content across scenes are rejected.

OutputDirectory must be new. verification.json contains success/failure,
per-scene recognized text, missing markers, frame paths, and pixel diagnostics.
ffprobe.json, decode.log, frames, and ocr.json preserve the raw evidence.
The script throws after writing its report on failure. Nothing is uploaded.
.EXAMPLE
.\Test-IntelligentTerminalWorkDemoVideo.ps1 -VideoPath C:\recording\work-demo.mp4 `
    -FfmpegPath C:\ffmpeg\bin\ffmpeg.exe -ChaptersPath C:\recording\chapters.json `
    -OutputDirectory C:\recording\verification
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$VideoPath,
    [Parameter(Mandatory)][string]$FfmpegPath,
    [Parameter(Mandatory)][string]$ChaptersPath,
    [Parameter(Mandatory)][string]$OutputDirectory
)
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $false
$OutputDirectory = [IO.Path]::GetFullPath($OutputDirectory)
if (Test-Path -LiteralPath $OutputDirectory) { throw 'Choose a new verification output directory; existing evidence is never overwritten.' }
New-Item -ItemType Directory -Path $OutputDirectory | Out-Null
$reportPath = Join-Path $OutputDirectory 'verification.json'
$report = [ordered]@{
    schemaVersion = 1; status = 'failed'; offline = $true
    videoPath = $VideoPath; chaptersPath = $ChaptersPath; videoSha256 = $null
    startedUtc = [datetime]::UtcNow.ToString('o'); completedUtc = $null
    evidencePolicy = 'Decoded video pixels only; UIA, subtitles, chapter titles and recording metadata are not OCR evidence.'
    metadata = $null; decode = $null; ocr = $null; scenes = @(); errors = @()
}
$scenes = [Collections.Generic.List[object]]::new()
$invariant = [Globalization.CultureInfo]::InvariantCulture
function ConvertTo-NormalizedStoryText {
    param([string]$Text)
    (($Text.Normalize([Text.NormalizationForm]::FormKC).ToLowerInvariant() -replace '[^a-z0-9]', ' ') -replace '\s+', ' ').Trim()
}
function Get-ContentDifference {
    param($Left, $Right)
    $difference = 0.0
    $changed = 0
    for ($i = 0; $i -lt $Left.Count; $i++) {
        $delta = [Math]::Abs([double]$Left[$i] - [double]$Right[$i])
        $difference += $delta
        if ($delta -gt 12) { $changed++ }
    }
    [ordered]@{
        meanAbsoluteLuminanceDifference = $difference / $Left.Count
        changedPixelFraction = $changed / [double]$Left.Count
        substantiallyDifferent = ($difference / $Left.Count -gt 0.15 -and $changed / [double]$Left.Count -gt 0.003)
    }
}
try {
    $VideoPath = (Resolve-Path -LiteralPath $VideoPath).Path
    $FfmpegPath = (Resolve-Path -LiteralPath $FfmpegPath).Path
    $ChaptersPath = (Resolve-Path -LiteralPath $ChaptersPath).Path
    $ffprobe = Join-Path (Split-Path $FfmpegPath) 'ffprobe.exe'
    if (-not (Test-Path -LiteralPath $ffprobe)) { throw 'A sibling ffprobe.exe is required.' }
    $report.videoPath = $VideoPath
    $report.chaptersPath = $ChaptersPath
    $report.videoSha256 = (Get-FileHash -LiteralPath $VideoPath -Algorithm SHA256).Hash
    $chapterData = Get-Content -LiteralPath $ChaptersPath -Raw | ConvertFrom-Json
    $chapters = @($chapterData)
    if ($chapters.Count -ne 8) { throw 'Expected exactly eight chapter entries.' }
    $previous = -1.0
    for ($i = 0; $i -lt 8; $i++) {
        $chapter = $chapters[$i]
        $seconds = [double]::Parse([string]$chapter.Seconds, $invariant)
        if ([int]$chapter.Scene -ne $i + 1 -or [double]::IsNaN($seconds) -or
            [double]::IsInfinity($seconds) -or $seconds -lt 0 -or $seconds -le $previous) {
            throw 'Chapters must contain scenes 1-8 in order with finite, increasing nonnegative Seconds.'
        }
        $chapter.Seconds = $seconds
        $previous = $seconds
    }
    $probePath = Join-Path $OutputDirectory 'ffprobe.json'
    & $ffprobe -v error -count_frames -select_streams v:0 -show_streams -show_format -of json $VideoPath `
        2> (Join-Path $OutputDirectory 'ffprobe-errors.log') |
        Set-Content -LiteralPath $probePath -Encoding UTF8
    if ($LASTEXITCODE -ne 0) { throw 'ffprobe could not read/count the video. See ffprobe-errors.log.' }
    $probe = Get-Content -LiteralPath $probePath -Raw | ConvertFrom-Json
    $streams = @($probe.streams)
    if ($streams.Count -ne 1) { throw 'No primary video stream was found.' }
    $stream = $streams[0]
    $duration = [double]::Parse([string]$probe.format.duration, $invariant)
    $rate = ([string]$stream.avg_frame_rate).Split('/')
    $fps = if ($rate.Count -eq 2 -and [double]$rate[1] -ne 0) { [double]$rate[0] / [double]$rate[1] } else { 0 }
    $report.metadata = [ordered]@{
        durationSeconds = $duration; width = [int]$stream.width; height = [int]$stream.height
        averageFramesPerSecond = $fps; decodedFrames = [long]$stream.nb_read_frames
        codec = $stream.codec_name; rawProbePath = $probePath
    }
    if ($duration -le 0 -or $duration -gt 3600 -or $fps -lt 10 -or [long]$stream.nb_read_frames -lt 80 -or
        [int]$stream.width -lt 640 -or [int]$stream.height -lt 360) {
        $report.errors += 'Video duration, resolution, frame rate, or decoded frame count is unsuitable for a readable eight-scene recording.'
    }
    if ($duration -le [double]$chapters[-1].Seconds + 0.5) { throw 'The video ends before the final chapter can be sampled.' }
    $maximumChapter = 0.0
    for ($i = 0; $i -lt 8; $i++) {
        $end = if ($i -lt 7) { [double]$chapters[$i + 1].Seconds } else { $duration }
        $length = $end - [double]$chapters[$i].Seconds
        if ($length -lt 1) { $report.errors += "Scene $($i + 1) lasts less than one second." }
        $maximumChapter = [Math]::Max($maximumChapter, $length)
    }
    $decodePath = Join-Path $OutputDirectory 'decode.log'
    $freezeSeconds = ($maximumChapter + 2).ToString('0.###', $invariant)
    & $FfmpegPath -hide_banner -nostdin -loglevel info -xerror -err_detect explode -i $VideoPath `
        -map 0:v:0 -an -sn -vf "blackdetect=d=2:pix_th=0.02:pic_th=0.999,freezedetect=n=-50dB:d=$freezeSeconds" `
        -f null - 2> $decodePath | Out-Null
    $decodeExit = $LASTEXITCODE
    $decodeText = Get-Content -LiteralPath $decodePath -Raw
    $blackEvents = @([regex]::Matches($decodeText, 'black_start:(?<start>[\d.]+)\s+black_end:(?<end>[\d.]+)\s+black_duration:(?<duration>[\d.]+)') | ForEach-Object {
        [ordered]@{ startSeconds = [double]::Parse($_.Groups['start'].Value, $invariant)
            endSeconds = [double]::Parse($_.Groups['end'].Value, $invariant)
            durationSeconds = [double]::Parse($_.Groups['duration'].Value, $invariant) }
    })
    $freezeStarts = @([regex]::Matches($decodeText, 'freeze_start:\s*(?<start>[\d.]+)') | ForEach-Object {
        [double]::Parse($_.Groups['start'].Value, $invariant)
    })
    $report.decode = [ordered]@{
        status = if ($decodeExit -eq 0) { 'passed' } else { 'failed' }
        exitCode = $decodeExit; logPath = $decodePath; blackIntervals = $blackEvents
        excessiveFreezeStartSeconds = $freezeStarts; maximumAllowedFreezeSeconds = $maximumChapter + 2
    }
    if ($decodeExit -ne 0) { $report.errors += 'FFmpeg full-video decode failed; see decode.log.' }
    if ($blackEvents.Count) { $report.errors += 'Decoded video contains a nearly all-black interval of at least two seconds; see decode.log.' }
    if ($freezeStarts.Count) { $report.errors += 'Decoded video freezes for longer than any complete chapter plus two seconds.' }

    # Each entry is an independently required marker; punctuation is deliberately ignored.
    $requirements = @{
        1 = @(@('Fix bug', 'fix\s+bug'), @('Code review', 'code\s+review'), @('Migration', 'migration'), @('Research', 'research'))
        2 = @(@('Fix Issue #4821', 'fix\s+issue\s*4821'), @('Resumed existing Work', '\bresumed\b'), @('Blocked summary', '\bblocked\b'))
        3 = @(@('Completed evidence', '\bcompleted\b'), @('Blocked evidence', '\bblocked\b'), @('Fix Issue #4821', 'fix\s+issue\s*4821'),
            @('Repository microsoft/foo', 'microsoft\s+foo'))
        4 = @(@('Investigate API v2', 'investigate\s+api\s+v\s*2'), @('Related Work', 'related\s+work'))
        5 = @(@('Schema 2.3', 'schema\s*2\s*3'), @('Breaking changes', 'breaking\s+changes'))
        6 = @(@('Proactive attention', '\battention\b|human\s+decision\s+needed'), @('Decision', '\bdecision\b'), @('Ignore option', '\bignore\b'), @('Fix compatibility option', 'fix\s+compatibility'),
            @('Roll back option', 'roll\s*back'), @('Recommended', 'recommended'))
        7 = @(@('Token cap setting', '\btoken\s+cap\b'), @('10K preset', '\b10\s*k\b'), @('30K preset', '\b30\s*k\b'),
            @('20,000 token budget', '\b20\s*000\b'), @('Paused', '\bpaused\b'), @('Findings', '\bfindings\b'))
        8 = @(@('Fix Issue #4821', 'fix\s+issue\s*4821'), @('Investigate API v2', 'investigate\s+api\s+v\s*2'),
            @('Prepare Documentation', 'prepare\s+documentation'))
    }
    $frameDirectory = Join-Path $OutputDirectory 'frames'
    New-Item -ItemType Directory -Path $frameDirectory | Out-Null
    $manifest = [Collections.Generic.List[object]]::new()
    for ($i = 0; $i -lt 8; $i++) {
        $chapter = $chapters[$i]
        $end = if ($i -lt 7) { [double]$chapters[$i + 1].Seconds } else { $duration }
        $scene = [ordered]@{
            scene = $i + 1; title = [string]$chapter.Title
            startSeconds = [double]$chapter.Seconds; endSeconds = $end; status = 'failed'
            frames = @(); combinedOcrText = ''; normalizedOcrText = ''; markers = @(); errors = @()
            differenceFromPreviousScene = $null
        }
        $sample = 0
        foreach ($fraction in @(0.10, 0.35, 0.65, 0.90)) {
            $sample++
            $seconds = [double]$chapter.Seconds + ($end - [double]$chapter.Seconds) * $fraction
            $path = Join-Path $frameDirectory ('scene-{0:D2}-sample-{1:D2}.png' -f ($i + 1), $sample)
            $extractLog = Join-Path $frameDirectory ('scene-{0:D2}-sample-{1:D2}.log' -f ($i + 1), $sample)
            & $FfmpegPath -hide_banner -nostdin -loglevel error -ss $seconds.ToString('0.000', $invariant) `
                -i $VideoPath -map 0:v:0 -an -sn -frames:v 1 -update 1 $path 2> $extractLog | Out-Null
            if ($LASTEXITCODE -ne 0 -or -not (Test-Path -LiteralPath $path)) {
                $scene.errors += "Could not extract sample $sample at $seconds seconds; see $extractLog"
            } else {
                $manifest.Add([pscustomobject]@{ Path = $path; Scene = $i + 1; Seconds = $seconds })
            }
        }
        $scenes.Add([pscustomobject]$scene)
    }
    $manifestPath = Join-Path $OutputDirectory 'frames.json'
    ConvertTo-Json -InputObject @($manifest.ToArray()) -Depth 5 |
        Set-Content -LiteralPath $manifestPath -Encoding UTF8
    $ocrPath = Join-Path $OutputDirectory 'ocr.json'
    $windowsPowerShell = Join-Path $env:SystemRoot 'System32\WindowsPowerShell\v1.0\powershell.exe'
    if (-not (Test-Path -LiteralPath $windowsPowerShell)) { throw 'Windows PowerShell 5.1 is unavailable; offline pixel OCR cannot run.' }
    & $windowsPowerShell -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass `
        -File (Join-Path $PSScriptRoot 'Read-IntelligentTerminalWorkDemoFrames.ps1') `
        -ManifestPath $manifestPath -OutputPath $ocrPath `
        > (Join-Path $OutputDirectory 'ocr-runtime.log') 2>&1
    $ocrExit = $LASTEXITCODE
    if (-not (Test-Path -LiteralPath $ocrPath)) { throw 'Windows OCR produced no report; see ocr-runtime.log. Extracted frames are preserved.' }
    $ocr = Get-Content -LiteralPath $ocrPath -Raw | ConvertFrom-Json
    $report.ocr = [ordered]@{ status = $ocr.status; engine = $ocr.engine; language = $ocr.language; reportPath = $ocrPath; exitCode = $ocrExit; errors = @($ocr.errors) }
    if ($ocrExit -ne 0 -or $ocr.status -ne 'passed') { $report.errors += 'Offline Windows OCR failed; see ocr.json and ocr-runtime.log.' }
    $previousRepresentative = $null
    foreach ($scene in $scenes) {
        $frames = @($ocr.frames | Where-Object scene -EQ $scene.scene)
        $scene.frames = @(foreach ($frame in $frames) {
            [ordered]@{
                path = $frame.path; seconds = $frame.seconds; status = $frame.status
                text = $frame.text; pixels = $frame.pixels; errors = @($frame.errors)
                sha256 = (Get-FileHash -LiteralPath $frame.path -Algorithm SHA256).Hash
                width = $frame.width; height = $frame.height; ocrWidth = $frame.ocrWidth; ocrHeight = $frame.ocrHeight
            }
        })
        foreach ($extracted in @($manifest | Where-Object Scene -EQ $scene.scene)) {
            if ($extracted.Path -notin @($frames.path)) {
                $scene.frames += [ordered]@{
                    path = $extracted.Path; seconds = $extracted.Seconds; status = 'ocr-unavailable'
                    text = ''; pixels = $null; errors = @('Frame extracted, but Windows OCR returned no result.')
                    sha256 = (Get-FileHash -LiteralPath $extracted.Path -Algorithm SHA256).Hash
                }
            }
        }
        if ($frames.Count -ne 4) { $scene.errors += 'Expected four decoded, OCR-processed representative frames.' }
        $usable = @($frames | Where-Object { $_.status -eq 'passed' -and -not $_.pixels.blackOrBlank -and $_.text.Trim().Length -ge 30 })
        if ($usable.Count -lt 2) { $scene.errors += 'Fewer than two samples contain substantial nonblank readable content.' }
        $scene.combinedOcrText = ($usable | ForEach-Object text) -join "`n`n"
        $scene.normalizedOcrText = ConvertTo-NormalizedStoryText $scene.combinedOcrText
        $scene.markers = @(foreach ($requirement in $requirements[[int]$scene.scene]) {
            $matchingFrames = @($usable | Where-Object { (ConvertTo-NormalizedStoryText $_.text) -match $requirement[1] } | ForEach-Object path)
            [ordered]@{ marker = $requirement[0]; pattern = $requirement[1]; passed = $matchingFrames.Count -gt 0; framePaths = $matchingFrames }
        })
        foreach ($marker in $scene.markers) {
            if (-not $marker.passed) { $scene.errors += "Missing visible OCR marker: $($marker.marker)" }
        }
        if ($usable.Count) {
            $representative = $usable[-1]
            if ($previousRepresentative) {
                $scene.differenceFromPreviousScene = Get-ContentDifference $previousRepresentative.contentSignature $representative.contentSignature
                if (-not $scene.differenceFromPreviousScene.substantiallyDifferent) {
                    $scene.errors += 'Story-content pixels are effectively unchanged from the previous chapter (stale or frozen window).'
                }
            }
            $previousRepresentative = $representative
        }
        if ($scene.errors.Count -eq 0) { $scene.status = 'passed' }
        else { $report.errors += "Scene $($scene.scene) failed actual-pixel verification." }
    }
    if ($report.errors.Count -eq 0 -and $scenes.Count -eq 8) { $report.status = 'passed' }
} catch {
    $report.errors += $_.Exception.ToString()
} finally {
    $report.scenes = @($scenes.ToArray())
    $report.completedUtc = [datetime]::UtcNow.ToString('o')
    $report | ConvertTo-Json -Depth 15 | Set-Content -LiteralPath $reportPath -Encoding UTF8
}
if ($report.status -ne 'passed') { throw "Work demo video verification FAILED. Evidence: $reportPath`n$($report.errors -join "`n")" }
Write-Output $reportPath
