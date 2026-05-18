param(
    [string]$Source = 'C:\Users\kaitao\Downloads\intelligent terminal.png',
    [string]$Dir    = 'res\terminal\images-Dev',
    [int]$MaxBytes  = 204800
)
$ErrorActionPreference = 'Stop'

Add-Type -AssemblyName System.Drawing
Add-Type -AssemblyName PresentationCore

$src = [System.Drawing.Image]::FromFile($Source)
Write-Host "Source: $($src.Width)x$($src.Height) $Source"

$files = Get-ChildItem -Path $Dir -Filter *.png -File
Write-Host "Regenerating $($files.Count) files into $Dir (max bytes: $MaxBytes)"

function Save-Indexed-Via-WPF {
    param([System.Drawing.Bitmap]$Bitmap, [string]$Path, [int]$Colors)
    # Convert System.Drawing.Bitmap to BitmapSource
    $ms = New-Object System.IO.MemoryStream
    $Bitmap.Save($ms, [System.Drawing.Imaging.ImageFormat]::Png)
    $ms.Position = 0
    $bs = [System.Windows.Media.Imaging.BitmapFrame]::Create($ms,
        [System.Windows.Media.Imaging.BitmapCreateOptions]::PreservePixelFormat,
        [System.Windows.Media.Imaging.BitmapCacheOption]::OnLoad)

    # Quantize to indexed by converting to Indexed8 format
    $palette = [System.Windows.Media.Imaging.BitmapPalettes]::Halftone256Transparent
    $converted = New-Object System.Windows.Media.Imaging.FormatConvertedBitmap
    $converted.BeginInit()
    $converted.Source = $bs
    $converted.DestinationFormat = [System.Windows.Media.PixelFormats]::Indexed8
    $converted.DestinationPalette = $palette
    $converted.EndInit()

    $encoder = New-Object System.Windows.Media.Imaging.PngBitmapEncoder
    $encoder.Frames.Add([System.Windows.Media.Imaging.BitmapFrame]::Create($converted))
    $fs = [System.IO.File]::Open($Path, [System.IO.FileMode]::Create)
    $encoder.Save($fs)
    $fs.Dispose()
    $ms.Dispose()
}

foreach ($f in $files) {
    $existing = [System.Drawing.Image]::FromFile($f.FullName)
    $w = $existing.Width
    $h = $existing.Height
    $existing.Dispose()

    $srcAR = $src.Width / $src.Height
    $dstAR = $w / $h
    if ($srcAR -gt $dstAR) {
        $dw = $w
        $dh = [int]($w / $srcAR)
    } else {
        $dh = $h
        $dw = [int]($h * $srcAR)
    }
    $dx = [int](($w - $dw) / 2)
    $dy = [int](($h - $dh) / 2)

    $bmp = New-Object System.Drawing.Bitmap $w, $h, ([System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.Clear([System.Drawing.Color]::Transparent)
    $g.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
    $g.SmoothingMode     = [System.Drawing.Drawing2D.SmoothingMode]::HighQuality
    $g.PixelOffsetMode   = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
    $rect = New-Object System.Drawing.Rectangle $dx, $dy, $dw, $dh
    $g.DrawImage($src, $rect)
    $g.Dispose()

    $bmp.Save($f.FullName, [System.Drawing.Imaging.ImageFormat]::Png)

    # If too big, re-save as indexed with progressively smaller palettes
    $size = (Get-Item $f.FullName).Length
    $palettes = @(
        [System.Windows.Media.Imaging.BitmapPalettes]::Halftone256Transparent,
        [System.Windows.Media.Imaging.BitmapPalettes]::Halftone125Transparent,
        [System.Windows.Media.Imaging.BitmapPalettes]::Halftone64Transparent,
        [System.Windows.Media.Imaging.BitmapPalettes]::Halftone27Transparent,
        [System.Windows.Media.Imaging.BitmapPalettes]::Halftone8Transparent
    )
    if ($size -gt $MaxBytes) {
        foreach ($pal in $palettes) {
            $ms2 = New-Object System.IO.MemoryStream
            $bmp.Save($ms2, [System.Drawing.Imaging.ImageFormat]::Png)
            $ms2.Position = 0
            $bs2 = [System.Windows.Media.Imaging.BitmapFrame]::Create($ms2,
                [System.Windows.Media.Imaging.BitmapCreateOptions]::PreservePixelFormat,
                [System.Windows.Media.Imaging.BitmapCacheOption]::OnLoad)
            $conv = New-Object System.Windows.Media.Imaging.FormatConvertedBitmap
            $conv.BeginInit()
            $conv.Source = $bs2
            $conv.DestinationFormat = [System.Windows.Media.PixelFormats]::Indexed8
            $conv.DestinationPalette = $pal
            $conv.EndInit()
            $enc = New-Object System.Windows.Media.Imaging.PngBitmapEncoder
            $enc.Frames.Add([System.Windows.Media.Imaging.BitmapFrame]::Create($conv))
            $fs = [System.IO.File]::Open($f.FullName, [System.IO.FileMode]::Create)
            $enc.Save($fs)
            $fs.Dispose()
            $ms2.Dispose()
            $size = (Get-Item $f.FullName).Length
            if ($size -le $MaxBytes) { break }
        }
        if ($size -gt $MaxBytes) {
            Write-Warning "$($f.Name) still $size bytes after all palette reductions"
        }
    }
    $bmp.Dispose()
}

$src.Dispose()
Write-Host "Done."
