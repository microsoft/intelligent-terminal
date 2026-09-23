# Copyright (c) Microsoft Corporation.
# Licensed under the MIT license.
<#
.SYNOPSIS
Read PNG frames locally with the Windows built-in English OCR engine.
.DESCRIPTION
Run with Windows PowerShell 5.1, not pwsh. ManifestPath is a JSON array of
{ Path, Scene, Seconds }. OutputPath receives real OCR and pixel diagnostics,
including errors when the Windows OCR runtime or English language is unavailable.
No UI automation, network calls, applications, or language installation is used.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$ManifestPath,
    [Parameter(Mandatory)][string]$OutputPath
)
$ErrorActionPreference = 'Stop'
$results = [Collections.Generic.List[object]]::new()
$report = [ordered]@{
    engine = 'Windows.Media.Ocr'; language = 'en-US'; offline = $true
    status = 'failed'; errors = @(); frames = @()
}
try {
    if ($PSVersionTable.PSEdition -ne 'Desktop') { throw 'Use Windows PowerShell 5.1 for the WinRT OCR bridge.' }
    Add-Type -AssemblyName System.Runtime.WindowsRuntime
    Add-Type -AssemblyName System.Drawing
    $null = [Windows.Storage.StorageFile, Windows.Storage, ContentType = WindowsRuntime]
    $null = [Windows.Storage.Streams.IRandomAccessStream, Windows.Storage.Streams, ContentType = WindowsRuntime]
    $null = [Windows.Graphics.Imaging.BitmapDecoder, Windows.Graphics.Imaging, ContentType = WindowsRuntime]
    $null = [Windows.Graphics.Imaging.SoftwareBitmap, Windows.Graphics.Imaging, ContentType = WindowsRuntime]
    $null = [Windows.Graphics.Imaging.BitmapTransform, Windows.Graphics.Imaging, ContentType = WindowsRuntime]
    $null = [Windows.Media.Ocr.OcrEngine, Windows.Foundation, ContentType = WindowsRuntime]
    $null = [Windows.Media.Ocr.OcrResult, Windows.Foundation, ContentType = WindowsRuntime]
    $null = [Windows.Globalization.Language, Windows.Globalization, ContentType = WindowsRuntime]
    $asTask = [System.WindowsRuntimeSystemExtensions].GetMethods() | Where-Object {
        $_.Name -eq 'AsTask' -and $_.IsGenericMethod -and
        $_.GetGenericArguments().Count -eq 1 -and $_.GetParameters().Count -eq 1 -and
        $_.GetParameters()[0].ParameterType.Name -eq 'IAsyncOperation`1'
    } | Select-Object -First 1
    if (-not $asTask) { throw 'WinRT IAsyncOperation-to-Task bridge is unavailable.' }
    function Wait-WinRt {
        param($Operation, [type]$ResultType)
        $task = $asTask.MakeGenericMethod($ResultType).Invoke($null, @($Operation))
        if (-not $task.Wait(30000)) { throw 'Windows OCR operation timed out after 30 seconds.' }
        $task.GetAwaiter().GetResult()
    }
    $language = [Windows.Globalization.Language]::new('en-US')
    $engine = [Windows.Media.Ocr.OcrEngine]::TryCreateFromLanguage($language)
    if (-not $engine) {
        $available = @([Windows.Media.Ocr.OcrEngine]::AvailableRecognizerLanguages | ForEach-Object LanguageTag)
        throw "Built-in English OCR is unavailable. Installed OCR languages: $($available -join ', '). No language packs were installed."
    }
    $report.language = $engine.RecognizerLanguage.LanguageTag
    Add-Type -ReferencedAssemblies System.Drawing -TypeDefinition @'
using System;
using System.Drawing;
public static class WorkDemoFramePixels
{
    public static double[] Read(string path)
    {
        using (var bitmap = new Bitmap(path))
        {
            // Exclude native title bar and input footer; compare actual story content.
            const int width = 320, height = 180;
            var result = new double[width * height + 3];
            int bright = 0;
            double total = 0, squares = 0;
            for (int y = 0; y < height; y++)
                for (int x = 0; x < width; x++)
                {
                    int px = Math.Min(bitmap.Width - 1, x * bitmap.Width / width);
                    int py = Math.Min(bitmap.Height - 1, (int)((0.06 + 0.86 * y / height) * bitmap.Height));
                    var c = bitmap.GetPixel(px, py);
                    double value = 0.2126 * c.R + 0.7152 * c.G + 0.0722 * c.B;
                    result[3 + y * width + x] = value;
                    total += value;
                    squares += value * value;
                    if (value > 45) bright++;
                }
            int count = width * height;
            result[0] = total / count;
            result[1] = Math.Sqrt(Math.Max(0, squares / count - result[0] * result[0]));
            result[2] = (double)bright / count;
            return result;
        }
    }
}
'@
    $inputFrames = Get-Content -LiteralPath $ManifestPath -Raw | ConvertFrom-Json
    foreach ($frame in $inputFrames) {
        $stream = $null
        $bitmap = $null
        $entry = [ordered]@{
            path = [string]$frame.Path; scene = [int]$frame.Scene; seconds = [double]$frame.Seconds
            status = 'failed'; text = ''; lines = @(); errors = @()
            width = $null; height = $null; ocrWidth = $null; ocrHeight = $null
            pixels = $null; contentSignature = @()
        }
        try {
            $path = (Resolve-Path -LiteralPath $frame.Path).Path
            $pixelData = [WorkDemoFramePixels]::Read($path)
            $entry.pixels = [ordered]@{
                meanLuminance = $pixelData[0]; standardDeviation = $pixelData[1]
                brightFraction = $pixelData[2]
                blackOrBlank = ($pixelData[1] -lt 2 -or $pixelData[2] -lt 0.0005)
                region = 'Full width, vertical 6%-92%; excludes title bar and input footer'
            }
            $entry.contentSignature = $pixelData[3..($pixelData.Length - 1)]
            $file = Wait-WinRt ([Windows.Storage.StorageFile]::GetFileFromPathAsync($path)) ([Windows.Storage.StorageFile])
            $stream = Wait-WinRt ($file.OpenAsync([Windows.Storage.FileAccessMode]::Read)) ([Windows.Storage.Streams.IRandomAccessStream])
            $decoder = Wait-WinRt ([Windows.Graphics.Imaging.BitmapDecoder]::CreateAsync($stream)) ([Windows.Graphics.Imaging.BitmapDecoder])
            $entry.width = $decoder.PixelWidth
            $entry.height = $decoder.PixelHeight
            $scale = [Math]::Min(1.0, [Windows.Media.Ocr.OcrEngine]::MaxImageDimension / [double][Math]::Max($decoder.PixelWidth, $decoder.PixelHeight))
            $transform = [Windows.Graphics.Imaging.BitmapTransform]::new()
            $transform.ScaledWidth = [uint32][Math]::Max(1, [Math]::Floor($decoder.PixelWidth * $scale))
            $transform.ScaledHeight = [uint32][Math]::Max(1, [Math]::Floor($decoder.PixelHeight * $scale))
            $entry.ocrWidth = $transform.ScaledWidth
            $entry.ocrHeight = $transform.ScaledHeight
            $bitmap = Wait-WinRt ($decoder.GetSoftwareBitmapAsync(
                [Windows.Graphics.Imaging.BitmapPixelFormat]::Bgra8,
                [Windows.Graphics.Imaging.BitmapAlphaMode]::Ignore,
                $transform,
                [Windows.Graphics.Imaging.ExifOrientationMode]::IgnoreExifOrientation,
                [Windows.Graphics.Imaging.ColorManagementMode]::DoNotColorManage
            )) ([Windows.Graphics.Imaging.SoftwareBitmap])
            $recognized = Wait-WinRt ($engine.RecognizeAsync($bitmap)) ([Windows.Media.Ocr.OcrResult])
            $entry.text = $recognized.Text
            $entry.lines = @(foreach ($line in $recognized.Lines) {
                [ordered]@{
                    text = $line.Text
                    words = @(foreach ($word in $line.Words) {
                        [ordered]@{
                            text = $word.Text; x = $word.BoundingRect.X; y = $word.BoundingRect.Y
                            width = $word.BoundingRect.Width; height = $word.BoundingRect.Height
                        }
                    })
                }
            })
            $entry.status = 'passed'
        } catch {
            $entry.errors += $_.Exception.ToString()
        } finally {
            if ($bitmap) { $bitmap.Dispose() }
            if ($stream) { $stream.Dispose() }
        }
        $results.Add([pscustomobject]$entry)
    }
    if (@($results | Where-Object status -EQ 'failed').Count -eq 0 -and $results.Count -gt 0) {
        $report.status = 'passed'
    }
} catch {
    $report.errors += $_.Exception.ToString()
} finally {
    $report.frames = @($results.ToArray())
    $report | ConvertTo-Json -Depth 12 -Compress | Set-Content -LiteralPath $OutputPath -Encoding UTF8
}
if ($report.status -ne 'passed') { exit 1 }
