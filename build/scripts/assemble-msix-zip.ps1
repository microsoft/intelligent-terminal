param(
    [Parameter(Mandatory = $true)][string]$Version,
    [Parameter(Mandatory = $true)][ValidateSet('x64','ARM64')][string]$Arch
)
$ErrorActionPreference = 'Stop'

$buildOut = "src\cascadia\CascadiaPackage\AppPackages\CascadiaPackage_${Version}_${Arch}_Test"
$archLower = $Arch.ToLower()
$outDir = "artifacts\local-installer\intelligent-terminal-${Version}-${archLower}-msix"
$depSrc = "$buildOut\Dependencies\${archLower}\Microsoft.UI.Xaml.2.8.appx"
$cerSrc = "artifacts\local-installer\IntelligentTerminalDev.cer"
$installSrc = "installer\Install-Msix.ps1"
$freResetSrc = "tools\fre-test-reset.ps1"

if (-not (Test-Path $cerSrc)) {
    Write-Error "Dev certificate not found at '$cerSrc'. Run: powershell -File build\scripts\New-DevSigningCert.ps1"
    exit 1
}

New-Item -ItemType Directory -Path $outDir -Force | Out-Null
New-Item -ItemType Directory -Path "$outDir\Dependencies" -Force | Out-Null
Get-ChildItem $outDir -File | Remove-Item -Force
Get-ChildItem "$outDir\Dependencies" -File | Remove-Item -Force

Copy-Item "$buildOut\CascadiaPackage_${Version}_${Arch}.msix" $outDir

if (Test-Path $depSrc) {
    Copy-Item $depSrc "$outDir\Dependencies\"
} else {
    Write-Warning "XAML dependency not found at '$depSrc' - Dependencies\ will be empty."
}

Copy-Item $cerSrc $outDir
Copy-Item $installSrc $outDir

# FRE test reset helper — testers run this to repeat the First Run Experience.
if (Test-Path $freResetSrc) {
    Copy-Item $freResetSrc $outDir
} else {
    Write-Warning "FRE reset script not found at '$freResetSrc' - skipping."
}

$zip = "artifacts\local-installer\intelligent-terminal-${Version}-${archLower}-msix.zip"
if (Test-Path $zip) { Remove-Item $zip -Force }
Compress-Archive -Path "$outDir\*" -DestinationPath $zip -Force

Write-Host "Created: $zip"
Get-Item $zip | Format-List Name, Length, LastWriteTime
Get-ChildItem $outDir -Recurse | Select-Object FullName, Length
