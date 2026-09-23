#requires -Version 7.0
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT license.
<#
.SYNOPSIS
Burn concise English captions and region focus into a real Work demo recording.
.DESCRIPTION
Offline, Windows-only compositor. Requires FFmpeg and sibling ffprobe.exe.
The source must be an upright 1360x900 native-window recording. It is placed
unscaled at (520,120) in a 1920x1080 canvas; editorial text never covers it.
Focus rectangles are source-relative pixels. Only the area outside a focus is
dimmed; a null focus leaves the entire native window unobscured.

CuesPath is UTF-8 JSON:
{ "schemaVersion":1, "videoWidth":1360, "videoHeight":900, "cues":[
  { "scene":1, "title":"The Problem", "phase":"goal", "text":"One issue. Four sessions.",
    "key":"", "startSeconds":0, "focus":null }
] }
Provide all eight consecutive scenes. Each begins with goal, includes action
or automatic, and ends with result. Titles stay constant within each scene.
Cues start at zero, increase strictly, and last at least 0.7 seconds.
End times come from the next cue or video duration. Boundaries are rounded to
24fps frames (reported in cues-used.json). Titles may occupy two lines and
captions three; overlong strings fail rather than silently clipping.

Each cue may optionally provide heading, disclosure, and sourceKind strings.
heading replaces the upper-right BEFORE/AFTER/HOW line; disclosure replaces
the persistent footer. Both must be nonempty single-line English text fitting
their existing regions. Omitted fields preserve the original scene-based
heading and simulated-demo disclosure, so existing callers are unchanged.
sourceKind is a caller-supplied provenance label retained in cues-used.json
and edit-metadata.json; it is not verification of the underlying recording.
For example:
  "heading":"BEFORE | Independent sessions",
  "disclosure":"Real Copilot agents | Example project | Independent sessions",
  "sourceKind":"before-real-agents"
For prototype footage, explicitly disclose simulated execution/token usage.
Do not label prototype footage as real agent execution or equate agent credit
usage with simulated Work tokens. Mixed-source assembly and frame provenance
remain the caller's responsibility; this script never generates product UI.

OutputDirectory must not exist. Outputs: focused-work-demo.mp4 (H.264, 24fps,
yuv420p, no audio), chapters.json, cues-used.json, ffmpeg.log and
edit-metadata.json. chapters.json uses Scene/Title/Seconds/EndSeconds.
PNG overlays and the concat manifest are deleted only after successful encode
and output probing; failures retain them for diagnostics. No app is launched,
no product UI is synthesized, and no claim of semantic case verification is made.
.EXAMPLE
.\build\scripts\Edit-IntelligentTerminalWorkDemoVideo.ps1 `
    -InputVideo C:\recordings\work-demo.mp4 -CuesPath C:\recordings\cues.json `
    -OutputDirectory C:\recordings\edited -FfmpegPath C:\ffmpeg\bin\ffmpeg.exe
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$InputVideo,
    [Parameter(Mandatory)][string]$CuesPath,
    [Parameter(Mandatory)][string]$OutputDirectory,
    [Parameter(Mandatory)][string]$FfmpegPath
)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $false
$invariant = [Globalization.CultureInfo]::InvariantCulture
$fps = 24
$app = @{ x = 520; y = 120; width = 1360; height = 900; scale = 1 }

function Assert-Number {
    param($Value, [string]$Name)
    if ($null -eq $Value -or $Value -is [string] -or $Value -is [bool] -or
        $Value -isnot [ValueType] -or
        -not [double]::IsFinite([double]$Value)) {
        throw "$Name must be a finite JSON number."
    }
}
function Assert-Text {
    param($Value, [string]$Name)
    if ($Value -isnot [string] -or [string]::IsNullOrWhiteSpace($Value) -or
        $Value -match '[\x00-\x1f\x7f]') {
        throw "$Name must be a nonempty, single-line string without control characters."
    }
}
function Get-Probe {
    param([string]$Path)
    $raw = & $script:ffprobe -v error -select_streams v:0 -show_streams -show_format -of json $Path
    if ($LASTEXITCODE -ne 0) { throw "ffprobe failed for $Path" }
    $raw | ConvertFrom-Json -AsHashtable
}
function Write-Json {
    param($Data, [string]$Path)
    ConvertTo-Json -InputObject $Data -Depth 15 | Set-Content -LiteralPath $Path -Encoding utf8
}

$InputVideo = (Resolve-Path -LiteralPath $InputVideo).Path
$CuesPath = (Resolve-Path -LiteralPath $CuesPath).Path
$FfmpegPath = (Resolve-Path -LiteralPath $FfmpegPath).Path
$OutputDirectory = [IO.Path]::GetFullPath($OutputDirectory)
if (Test-Path -LiteralPath $OutputDirectory) { throw 'OutputDirectory must be fresh; no existing directory or source is overwritten.' }
$ffprobe = Join-Path (Split-Path $FfmpegPath) 'ffprobe.exe'
if (-not (Test-Path -LiteralPath $ffprobe -PathType Leaf)) { throw 'A sibling ffprobe.exe is required.' }
$probe = Get-Probe $InputVideo
if (@($probe.streams).Count -ne 1) { throw 'The source must contain a video stream.' }
$stream = $probe.streams[0]
if ($stream.width -ne 1360 -or $stream.height -ne 900) { throw 'Source video must be exactly 1360x900; automatic scaling is deliberately disabled.' }
if ($stream.ContainsKey('side_data_list')) {
    foreach ($side in $stream.side_data_list) {
        if ($side.ContainsKey('rotation') -and $side.rotation -ne 0) { throw 'Rotated source video is not supported.' }
    }
}
if (-not $stream.ContainsKey('duration')) { throw 'Source video stream must expose a finite duration.' }
$duration = [double]::Parse([string]$stream.duration, $invariant)
if (-not [double]::IsFinite($duration) -or $duration -le 0) { throw 'Source duration must be finite and positive.' }
$frameCount = [long][Math]::Round($duration * $fps, [MidpointRounding]::AwayFromZero)
$outputDuration = $frameCount / [double]$fps
$data = Get-Content -LiteralPath $CuesPath -Raw -Encoding utf8 | ConvertFrom-Json -AsHashtable
if ($data -isnot [Collections.IDictionary] -or $data.schemaVersion -cne 1 -or
    $data.videoWidth -cne 1360 -or $data.videoHeight -cne 900 -or
    $data.cues -isnot [array] -or $data.cues.Count -lt 24) {
    throw 'Expected schemaVersion 1, videoWidth 1360, videoHeight 900, and at least 24 cues covering eight goal/action-or-automatic/result scenes.'
}
$normalized = [Collections.Generic.List[object]]::new()
$previousTime = -1.0
$previousScene = 1
for ($i = 0; $i -lt $data.cues.Count; $i++) {
    $cue = $data.cues[$i]
    if ($cue -isnot [Collections.IDictionary]) { throw "Cue $i must be an object." }
    Assert-Number $cue.scene "Cue $i scene"
    if ($cue.scene -ne [Math]::Truncate($cue.scene) -or $cue.scene -lt 1 -or $cue.scene -gt 8 -or
        ($i -eq 0 -and $cue.scene -ne 1) -or
        $cue.scene -lt $previousScene -or $cue.scene -gt $previousScene + 1) {
        throw "Cue $i has invalid scene ordering; scenes must run consecutively from 1 through 8."
    }
    Assert-Text $cue.title "Cue $i title"
    Assert-Text $cue.text "Cue $i text"
    $heading = if ($cue.scene -eq 1) { 'BEFORE  |  Manage agents and sessions' }
        elseif ($cue.scene -in @(2, 8)) { 'AFTER  |  Manage work' }
        else { 'HOW  |  Evidence, decisions and budgets' }
    $disclosure = 'Demo | Simulated execution and token usage | Recorded product UI'
    $sourceKind = $null
    foreach ($field in @('heading', 'disclosure', 'sourceKind')) {
        if ($cue.ContainsKey($field)) {
            Assert-Text $cue[$field] "Cue $i $field"
            if ($cue[$field] -match '[^\x20-\x7e]') { throw "Cue $i $field must use English ASCII text." }
            switch ($field) {
                heading { $heading = $cue[$field] }
                disclosure { $disclosure = $cue[$field] }
                sourceKind { $sourceKind = $cue[$field] }
            }
        }
    }
    if ($cue.phase -cnotin @('goal', 'action', 'automatic', 'result')) { throw "Cue $i has an invalid phase." }
    $key = ''
    if ($cue.ContainsKey('key')) {
        if ($cue.key -isnot [string] -or $cue.key -match '[\x00-\x1f\x7f]') { throw "Cue $i key must be a string without control characters." }
        $key = $cue.key.Trim()
    }
    if ($key -and $cue.phase -ne 'action') { throw "Cue $i key is allowed only during a human action." }
    Assert-Number $cue.startSeconds "Cue $i startSeconds"
    $start = [double]$cue.startSeconds
    if (($i -eq 0 -and $start -ne 0) -or $start -le $previousTime -or $start -ge $duration) {
        throw "Cue $i times must start at zero and strictly increase within the video."
    }
    if (-not $cue.ContainsKey('focus')) { throw "Cue $i must specify focus (rectangle or null)." }
    $focus = $null
    if ($null -ne $cue.focus) {
        if ($cue.focus -isnot [Collections.IDictionary]) { throw "Cue $i focus must be a rectangle or null." }
        foreach ($name in @('x', 'y', 'width', 'height')) {
            Assert-Number $cue.focus[$name] "Cue $i focus.$name"
        }
        $f = $cue.focus
        if ($f.x -lt 0 -or $f.y -lt 0 -or $f.width -le 0 -or $f.height -le 0 -or
            $f.x + $f.width -gt 1360 -or $f.y + $f.height -gt 900) { throw "Cue $i focus is outside the native window." }
        # Expand fractional UIA bounds, never shrink the clear area into a glyph.
        $focus = @{
            x = [int][Math]::Floor($f.x); y = [int][Math]::Floor($f.y)
            width = [int]([Math]::Ceiling($f.x + $f.width) - [Math]::Floor($f.x))
            height = [int]([Math]::Ceiling($f.y + $f.height) - [Math]::Floor($f.y))
        }
    }
    $normalized.Add([ordered]@{
        scene = [int]$cue.scene; title = $cue.title; phase = $cue.phase; text = $cue.text; key = $key
        heading = $heading; disclosure = $disclosure; sourceKind = $sourceKind
        requestedStartSeconds = $start
        startFrame = [long][Math]::Round($start * $fps, [MidpointRounding]::AwayFromZero)
        startSeconds = 0.0; endSeconds = 0.0; focus = $focus; outputFocus = $null
    })
    $previousTime = $start
    $previousScene = $cue.scene
}
for ($i = 0; $i -lt $normalized.Count; $i++) {
    $cue = $normalized[$i]
    $end = if ($i + 1 -lt $normalized.Count) { $normalized[$i + 1].requestedStartSeconds } else { $duration }
    $endFrame = if ($i + 1 -lt $normalized.Count) { $normalized[$i + 1].startFrame } else { $frameCount }
    if ($end - $cue.requestedStartSeconds -lt 0.7 -or $endFrame - $cue.startFrame -lt 17) {
        throw "Cue $i is too short: each cue must last at least 0.7 seconds (17 output frames)."
    }
    $cue.startSeconds = $cue.startFrame / [double]$fps
    $cue.endSeconds = $endFrame / [double]$fps
    if ($null -ne $cue.focus) {
        $cue.outputFocus = @{ x = $app.x + $cue.focus.x; y = $app.y + $cue.focus.y
            width = $cue.focus.width; height = $cue.focus.height }
    }
}
$chapters = @(
    foreach ($scene in 1..8) {
        $group = @($normalized | Where-Object { $_.scene -eq $scene })
        if ($group.Count -lt 3 -or $group[0].phase -ne 'goal' -or $group[-1].phase -ne 'result' -or
            @($group | Where-Object { $_.phase -in @('action', 'automatic') }).Count -eq 0) {
            throw "Scene $scene must begin with goal, contain action or automatic, and end with result."
        }
        if (@($group | Where-Object { $_.title -cne $group[0].title }).Count) { throw "Scene $scene must use a consistent title." }
        [ordered]@{ Scene = $scene; Title = $group[0].title; Seconds = $group[0].startSeconds; EndSeconds = $group[-1].endSeconds }
    }
)

Add-Type -AssemblyName System.Drawing
$installed = [Drawing.Text.InstalledFontCollection]::new()
try {
    $fontFamily = @('Segoe UI', 'Arial') | Where-Object { $_ -in $installed.Families.Name } | Select-Object -First 1
} finally { $installed.Dispose() }
if (-not $fontFamily) { throw 'Segoe UI or Arial must be installed for English captions.' }
$fonts = @{}
$brushes = @{}
function Get-Brush {
    param([string]$Color)
    if (-not $brushes.ContainsKey($Color)) {
        $brushes[$Color] = [Drawing.SolidBrush]::new([Drawing.ColorTranslator]::FromHtml($Color))
    }
    $brushes[$Color]
}
function Draw-Text {
    param($Graphics, [string]$Text, [string]$Font, [string]$Color, [float]$X, [float]$Y)
    $Graphics.DrawString($Text, $fonts[$Font], (Get-Brush $Color), $X, $Y, $script:textFormat)
}
function Draw-Wrapped {
    param($Graphics, [string]$Text, [string]$Font, [string]$Color, [float]$X, [float]$Y,
        [float]$Width, [int]$MaxLines, [float]$LineHeight)
    $lines = [Collections.Generic.List[string]]::new()
    $line = ''
    $elements = [Globalization.StringInfo]::GetTextElementEnumerator($Text)
    while ($elements.MoveNext()) {
        $next = $elements.GetTextElement()
        if ($Graphics.MeasureString($line + $next, $fonts[$Font], [int]10000, $script:textFormat).Width -gt $Width) {
            if (-not $line) { throw "Text does not fit the sidebar: $Text" }
            $break = $line.LastIndexOf(' ')
            if ($next -eq ' ') {
                $lines.Add($line.TrimEnd())
                $line = ''
            } elseif ($break -gt 0) {
                $lines.Add($line.Substring(0, $break).TrimEnd())
                $line = $line.Substring($break + 1) + $next
            } else {
                $lines.Add($line)
                $line = $next
            }
        } else { $line += $next }
    }
    if ($line) { $lines.Add($line) }
    # Prefer a sentence boundary to a trailing one- or two-character orphan.
    if ($lines.Count -eq 2) {
        $bestSplit = -1
        $bestBalance = [double]::MaxValue
        for ($split = 1; $split -lt $Text.Length; $split++) {
            if ($Text[$split - 1] -notin @('，', '、', '：', '；', '。')) { continue }
            $left = $Text.Substring(0, $split).TrimEnd()
            $right = $Text.Substring($split).TrimStart()
            if (-not $right) { continue }
            $leftWidth = $Graphics.MeasureString($left, $fonts[$Font], [int]10000, $script:textFormat).Width
            $rightWidth = $Graphics.MeasureString($right, $fonts[$Font], [int]10000, $script:textFormat).Width
            $balance = [Math]::Abs($leftWidth - $rightWidth)
            if ($leftWidth -le $Width -and $rightWidth -le $Width -and $balance -lt $bestBalance) {
                $bestBalance = $balance
                $bestSplit = $split
            }
        }
        if ($bestSplit -gt 0) {
            $lines[0] = $Text.Substring(0, $bestSplit).TrimEnd()
            $lines[1] = $Text.Substring($bestSplit).TrimStart()
        }
    }
    if ($lines.Count -gt $MaxLines) { throw "Shorten '$Text': it exceeds $MaxLines sidebar lines." }
    for ($j = 0; $j -lt $lines.Count; $j++) {
        Draw-Text $Graphics $lines[$j] $Font $Color $X ($Y + $j * $LineHeight)
    }
}

New-Item -ItemType Directory -Path $OutputDirectory | Out-Null
$intermediate = Join-Path $OutputDirectory '.editor-intermediate'
New-Item -ItemType Directory -Path $intermediate | Out-Null
$textFormat = [Drawing.StringFormat]::GenericTypographic.Clone()
$textFormat.FormatFlags = [Drawing.StringFormatFlags]::MeasureTrailingSpaces
$dimBrush = [Drawing.SolidBrush]::new([Drawing.Color]::FromArgb(102, 0, 0, 0))
$clearBrush = [Drawing.SolidBrush]::new([Drawing.Color]::Transparent)
$focusPen = [Drawing.Pen]::new([Drawing.ColorTranslator]::FromHtml('#67E8D1'), 3)
$borderPen = [Drawing.Pen]::new([Drawing.ColorTranslator]::FromHtml('#27364D'), 1)
$manifest = [Collections.Generic.List[string]]::new()
$manifest.Add('ffconcat version 1.0')
$phaseStyle = @{
    goal = @('Goal', '#A8B6CC', '#202D42')
    action = @('Action', '#7EBBFF', '#153657')
    automatic = @('Automatic', '#FFD083', '#45341E')
    result = @('Result', '#79E8BD', '#163D35')
}
try {
    foreach ($entry in @{ title = 44; caption = 34; phase = 26; key = 30; small = 22; index = 24 }.GetEnumerator()) {
        $style = if ($entry.Key -in @('title', 'key')) { [Drawing.FontStyle]::Bold } else { [Drawing.FontStyle]::Regular }
        $fonts[$entry.Key] = [Drawing.Font]::new($fontFamily, [float]$entry.Value, $style, [Drawing.GraphicsUnit]::Pixel)
    }
    for ($i = 0; $i -lt $normalized.Count; $i++) {
        $cue = $normalized[$i]
        $png = Join-Path $intermediate ('cue-{0:D4}.png' -f $i)
        $bitmap = [Drawing.Bitmap]::new(1920, 1080, [Drawing.Imaging.PixelFormat]::Format32bppArgb)
        $g = [Drawing.Graphics]::FromImage($bitmap)
        try {
            $g.Clear([Drawing.ColorTranslator]::FromHtml('#0B1220'))
            $g.CompositingMode = [Drawing.Drawing2D.CompositingMode]::SourceCopy
            $g.FillRectangle($clearBrush, $app.x, $app.y, $app.width, $app.height)
            $g.CompositingMode = [Drawing.Drawing2D.CompositingMode]::SourceOver
            $g.TextRenderingHint = [Drawing.Text.TextRenderingHint]::AntiAliasGridFit
            $g.FillRectangle((Get-Brush '#111D30'), 40, 120, 432, 900)
            Draw-Text $g 'Intelligent Terminal  |  Work demo' small '#C4D2E5' 520 56
            if ($g.MeasureString($cue.heading, $fonts.index, 10000, $textFormat).Width -gt 760) {
                throw "Cue $i heading is too long for the upper-right region."
            }
            if ($g.MeasureString($cue.disclosure, $fonts.small, 10000, $textFormat).Width -gt 1360) {
                throw "Cue $i disclosure is too long for the footer."
            }
            Draw-Text $g $cue.heading index '#79E8BD' 1120 56
            Draw-Text $g ('{0:D2} / 08' -f $cue.scene) index '#95A6BF' 64 156
            $phase = $phaseStyle[$cue.phase]
            $g.FillRectangle((Get-Brush $phase[2]), 64, 214, 172, 48)
            Draw-Text $g $phase[0] phase $phase[1] 82 221
            Draw-Wrapped $g $cue.title title '#F3F7FF' 64 300 380 2 62
            $g.FillRectangle((Get-Brush $phase[1]), 64, 453, 44, 3)
            Draw-Wrapped $g $cue.text caption '#D6E1F0' 64 488 380 3 52
            if ($cue.key) {
                $keyWidth = [Math]::Ceiling($g.MeasureString($cue.key, $fonts.key, 10000, $textFormat).Width) + 36
                if ($keyWidth -gt 380) { throw "Cue $i key is too long for a keycap." }
                $g.FillRectangle((Get-Brush '#213D5E'), 64, 690, [int]$keyWidth, 58)
                Draw-Text $g $cue.key key '#D3E9FF' 82 702
            }
            Draw-Text $g '8 scenes. One work story.' small '#95A6BF' 64 844
            for ($step = 1; $step -le 8; $step++) {
                $color = if ($step -eq $cue.scene) { $phase[1] } elseif ($step -lt $cue.scene) { '#62758E' } else { '#29384E' }
                $g.FillRectangle((Get-Brush $color), (64 + ($step - 1) * 48), 899, 36, 5)
                Draw-Text $g ([string]$step) small $color (73 + ($step - 1) * 48) 921
            }
            Draw-Text $g $cue.disclosure small '#95A6BF' 520 1040
            $g.DrawRectangle($borderPen, 518, 118, 1364, 904)
            if ($null -ne $cue.outputFocus) {
                $f = $cue.outputFocus
                $right = $app.x + $app.width
                $bottom = $app.y + $app.height
                $g.FillRectangle($dimBrush, $app.x, $app.y, $app.width, ($f.y - $app.y))
                $g.FillRectangle($dimBrush, $app.x, ($f.y + $f.height), $app.width, ($bottom - $f.y - $f.height))
                $g.FillRectangle($dimBrush, $app.x, $f.y, ($f.x - $app.x), $f.height)
                $g.FillRectangle($dimBrush, ($f.x + $f.width), $f.y, ($right - $f.x - $f.width), $f.height)
                $g.DrawRectangle($focusPen, ($f.x - 2), ($f.y - 2), ($f.width + 4), ($f.height + 4))
            }
            $bitmap.Save($png, [Drawing.Imaging.ImageFormat]::Png)
        } finally { $g.Dispose(); $bitmap.Dispose() }
        # Relative fixed ASCII filenames avoid concat quoting/Unicode path pitfalls.
        $manifest.Add("file '$(Split-Path -Leaf $png)'")
        $manifest.Add('option framerate 24')
        $manifest.Add('duration ' + ($cue.endSeconds - $cue.startSeconds).ToString('0.000000000', $invariant))
    }
    $manifest.Add("file '$(Split-Path -Leaf $png)'")
    $manifest.Add('option framerate 24')
} finally {
    foreach ($font in $fonts.Values) { $font.Dispose() }
    foreach ($brush in $brushes.Values) { $brush.Dispose() }
    $textFormat.Dispose(); $dimBrush.Dispose(); $clearBrush.Dispose(); $focusPen.Dispose(); $borderPen.Dispose()
}
$concatPath = Join-Path $intermediate 'overlays.ffconcat'
$manifest | Set-Content -LiteralPath $concatPath -Encoding utf8NoBOM
Write-Json $chapters (Join-Path $OutputDirectory 'chapters.json')
Write-Json ([ordered]@{ schemaVersion = 1; videoWidth = 1360; videoHeight = 900; cues = @($normalized.ToArray()) }) (Join-Path $OutputDirectory 'cues-used.json')
$output = Join-Path $OutputDirectory 'focused-work-demo.mp4'
$filter = '[0:v]setpts=PTS-STARTPTS,fps=24,setsar=1,pad=1920:1080:520:120:color=0x0B1220[base];[1:v]fps=24[art];[base][art]overlay=eof_action=repeat:format=auto,format=yuv420p[out]'
& $FfmpegPath -hide_banner -nostdin -n -noautorotate -i $InputVideo -f concat -safe 0 -i $concatPath `
    -filter_complex $filter -map '[out]' -an -sn -dn -map_metadata -1 -map_chapters -1 `
    -c:v libx264 -preset medium -crf 17 -pix_fmt yuv420p -r 24 -frames:v $frameCount `
    -movflags +faststart $output 2> (Join-Path $OutputDirectory 'ffmpeg.log') | Out-Null
if ($LASTEXITCODE -ne 0) { throw 'FFmpeg encode failed; see ffmpeg.log. Intermediate overlays are retained.' }
$result = Get-Probe $output
$outStream = $result.streams[0]
$encodedDuration = [double]::Parse([string]$outStream.duration, $invariant)
if ($outStream.width -ne 1920 -or $outStream.height -ne 1080 -or $outStream.codec_name -ne 'h264' -or
    $outStream.pix_fmt -ne 'yuv420p' -or $outStream.avg_frame_rate -ne '24/1' -or
    [Math]::Abs($encodedDuration - $outputDuration) -gt (1.0 / $fps)) {
    throw 'Encoded video failed geometry/codec/frame-rate/duration checks; intermediate overlays are retained.'
}
Write-Json ([ordered]@{
    schemaVersion = 1; status = 'encoded-and-probed'; offline = $true
    source = $InputVideo; sourceSha256 = (Get-FileHash -LiteralPath $InputVideo -Algorithm SHA256).Hash
    cuesSource = $CuesPath; output = $output
    sourceDurationSeconds = $duration; outputDurationSeconds = $encodedDuration
    width = 1920; height = 1080; fps = $fps; frameCount = $frameCount
    appRect = $app; nativeContentOcrCrop = '1360:900:520:120'
    focusDimOpacity = 0.4; focusOutline = @{ color = '#67E8D1'; width = 3; outsideInset = 2 }
    font = @{ family = $fontFamily; units = 'pixels'; title = 44; caption = 34; phase = 26; key = 30; small = 22 }
    captionMethod = 'System.Drawing RGBA PNG overlays, burned into H.264 frames'
    cuePresentation = @($normalized | ForEach-Object {
        @{ scene = $_.scene; startSeconds = $_.startSeconds; endSeconds = $_.endSeconds
            heading = $_.heading; disclosure = $_.disclosure; sourceKind = $_.sourceKind }
    })
    focusMapping = @($normalized | ForEach-Object {
        @{ scene = $_.scene; phase = $_.phase; startSeconds = $_.startSeconds; endSeconds = $_.endSeconds
            sourceRect = $_.focus; outputRect = $_.outputFocus }
    })
    verification = 'Output stream probed only; semantic eight-scene validation remains the caller responsibility. Native pixels are unscaled and not replaced; H.264 encoding is lossy.'
    completedUtc = [datetime]::UtcNow.ToString('o')
}) (Join-Path $OutputDirectory 'edit-metadata.json')
Remove-Item -LiteralPath $intermediate -Recurse -Force
Write-Output $output
