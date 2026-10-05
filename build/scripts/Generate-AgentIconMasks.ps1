# Copyright (c) Microsoft Corporation.
# Licensed under the MIT license.

# Canonical artwork: TerminalApp\AgentIconResources.xaml. Rasterize its geometry,
# fill rules and opacity without duplicating path data. BitmapIcon tint supplies
# the foreground; transparent masks can be consumed repeatedly by WinUI 2.8.
[CmdletBinding()]
param([switch]$Check)

$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName PresentationCore
Add-Type -AssemblyName WindowsBase
$root = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..\..'))
$source = Join-Path $root 'src\cascadia\TerminalApp\AgentIconResources.xaml'
$output = Join-Path $root 'src\cascadia\CascadiaPackage\AgentIcons\Masks'
[xml]$xml = Get-Content -LiteralPath $source -Raw
$ns = [Xml.XmlNamespaceManager]::new($xml.NameTable)
$ns.AddNamespace('p', 'http://schemas.microsoft.com/winfx/2006/xaml/presentation')
$ns.AddNamespace('x', 'http://schemas.microsoft.com/winfx/2006/xaml')
$size = 128
[IO.Directory]::CreateDirectory($output) | Out-Null

foreach ($id in @('copilot', 'claude', 'codex', 'gemini', 'opencode', 'generic')) {
    $template = $xml.SelectSingleNode("//p:DataTemplate[@x:Key='AgentIcon.$id']", $ns)
    if (-not $template) { throw "Missing canonical template: $id" }
    $visual = [Windows.Media.DrawingVisual]::new()
    $drawing = $visual.RenderOpen()
    try {
        if ($id -eq 'generic') {
            if ($template.SelectSingleNode('.//p:SymbolIcon', $ns).Symbol -ne 'Message') {
                throw 'Generic canonical symbol changed; update its glyph mapping.'
            }
            # SymbolIcon.Message is U+E15F in the existing Segoe MDL2 Assets font.
            $font = [Windows.Media.GlyphTypeface]::new(
                [Uri]::new((Join-Path $env:WINDIR 'Fonts\segmdl2.ttf')))
            $glyph = $font.CharacterToGlyphMap[0xE15F]
            $geometry = $font.GetGlyphOutline($glyph, 1, 1)
            $bounds = $geometry.Bounds
            $scale = $size / [Math]::Max($bounds.Width, $bounds.Height)
            $matrix = [Windows.Media.Matrix]::new($scale, 0, 0, $scale,
                ($size - $bounds.Width * $scale) / 2 - $bounds.Left * $scale,
                ($size - $bounds.Height * $scale) / 2 - $bounds.Top * $scale)
            $drawing.PushTransform([Windows.Media.MatrixTransform]::new($matrix))
            $drawing.DrawGeometry([Windows.Media.Brushes]::White, $null, $geometry)
            $drawing.Pop()
        }
        else {
            $element = $template.SelectSingleNode('p:Viewbox/*', $ns)
            $drawing.PushTransform([Windows.Media.ScaleTransform]::new(
                $size / [double]$element.Width, $size / [double]$element.Height))
            foreach ($path in $template.SelectNodes('.//p:Path', $ns)) {
                $geometry = [Windows.Media.Geometry]::Parse([string]$path.Data)
                $opacity = if ($path.HasAttribute('Opacity')) { [double]$path.Opacity } else { 1.0 }
                $drawing.PushOpacity($opacity)
                $drawing.DrawGeometry([Windows.Media.Brushes]::White, $null, $geometry)
                $drawing.Pop()
            }
            $drawing.Pop()
        }
    }
    finally { $drawing.Close() }
    $bitmap = [Windows.Media.Imaging.RenderTargetBitmap]::new(
        $size, $size, 96, 96, [Windows.Media.PixelFormats]::Pbgra32)
    $bitmap.Render($visual)
    $encoder = [Windows.Media.Imaging.PngBitmapEncoder]::new()
    $encoder.Frames.Add([Windows.Media.Imaging.BitmapFrame]::Create($bitmap))
    $stream = [IO.MemoryStream]::new()
    try {
        $encoder.Save($stream)
        $bytes = $stream.ToArray()
        $file = Join-Path $output "$id.png"
        if ($Check) {
            if (-not (Test-Path -LiteralPath $file) -or
                [Convert]::ToBase64String([IO.File]::ReadAllBytes($file)) -ne [Convert]::ToBase64String($bytes)) {
                throw "Generated mask differs: $file"
            }
        }
        else { [IO.File]::WriteAllBytes($file, $bytes) }
        Write-Output "$id mask: $size x $size"
    }
    finally { $stream.Dispose() }
}
