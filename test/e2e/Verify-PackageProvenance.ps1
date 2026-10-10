<#
.SYNOPSIS
    Compare a clean source revision, its package recipe and MSIX, and the installed package.
.DESCRIPTION
    Read-only. A passing comparison does not replace a build-time source receipt: callers
    must establish that the compiler ran against the recorded source before invoking this.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$SourceRoot,
    [Parameter(Mandatory)][ValidatePattern('^[a-fA-F0-9]{40}$')][string]$ExpectedHead,
    [Parameter(Mandatory)][string]$RecipePath,
    [Parameter(Mandatory)][string]$MsixPath,
    [string]$PackageFamilyName = 'IntelligentTerminal_rd9vj3e6a2mbr',
    [Parameter(DontShow)]$InstalledPackage
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$root = [IO.Path]::GetFullPath($SourceRoot).TrimEnd('\')
$sourcePrefix = $root + '\'
if (-not (Test-Path -LiteralPath $root -PathType Container)) {
    throw "Source root does not exist: $root"
}
$head = (& git -C $root rev-parse HEAD).Trim()
if ($LASTEXITCODE -ne 0 -or $head -ne $ExpectedHead) {
    throw "The source HEAD is not the expected revision: expected=$ExpectedHead actual=$head"
}
$dirty = @(& git -C $root status --porcelain=v1 -uall)
if ($LASTEXITCODE -ne 0 -or $dirty.Count) {
    throw "Package validation requires a clean source worktree at $head."
}

$recipe = [IO.Path]::GetFullPath($RecipePath)
$msix = [IO.Path]::GetFullPath($MsixPath)
foreach ($path in @($recipe, $msix)) {
    if (-not $path.StartsWith($sourcePrefix, [StringComparison]::OrdinalIgnoreCase) -or
        -not (Test-Path -LiteralPath $path -PathType Leaf)) {
        throw "Package input is missing or outside the source worktree: $path"
    }
}

$packages = @(
    if ($PSBoundParameters.ContainsKey('InstalledPackage')) {
        $InstalledPackage
    } else {
        Get-AppxPackage | Where-Object PackageFamilyName -eq $PackageFamilyName
    }
)
if ($packages.Count -ne 1 -or -not $packages[0] -or
    $packages[0].PackageFamilyName -ne $PackageFamilyName -or
    -not $packages[0].PackageFullName -or -not $packages[0].InstallLocation) {
    throw "Expected exactly one installed package in family '$PackageFamilyName'."
}
$package = $packages[0]
$layout = [IO.Path]::GetFullPath([string]$package.InstallLocation).TrimEnd('\')
$layoutPrefix = $layout + '\'
$registeredManifest = Join-Path $layout 'AppxManifest.xml'
if (-not (Test-Path -LiteralPath $registeredManifest -PathType Leaf)) {
    throw "Installed package has no manifest: $registeredManifest"
}

function Get-ManifestIdentity([xml]$Document) {
    $identity = $Document.SelectSingleNode("//*[local-name()='Identity']")
    if (-not $identity -or -not $identity.GetAttribute('Name') -or
        -not $identity.GetAttribute('Publisher') -or -not $identity.GetAttribute('Version')) {
        throw 'Package manifest has no complete Identity.'
    }
    [pscustomobject]@{
        Name = $identity.GetAttribute('Name')
        Publisher = $identity.GetAttribute('Publisher')
        Version = $identity.GetAttribute('Version')
    }
}
function Get-ArchiveHash([IO.Compression.ZipArchiveEntry]$Entry) {
    $stream = $Entry.Open()
    try { [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData($stream)) }
    finally { $stream.Dispose() }
}

[xml]$recipeXml = Get-Content -LiteralPath $recipe -Raw
$manifestNode = $recipeXml.SelectSingleNode("//*[local-name()='AppXManifest']")
if (-not $manifestNode -or -not $manifestNode.Include) {
    throw "Package recipe has no manifest source: $recipe"
}
$sourceManifest = [IO.Path]::GetFullPath([Uri]::UnescapeDataString([string]$manifestNode.Include))
if (-not $sourceManifest.StartsWith($sourcePrefix, [StringComparison]::OrdinalIgnoreCase) -or
    -not (Test-Path -LiteralPath $sourceManifest -PathType Leaf)) {
    throw "Recipe manifest source is missing or outside the source worktree: $sourceManifest"
}
$sourceIdentity = Get-ManifestIdentity ([xml](Get-Content -LiteralPath $sourceManifest -Raw))
$sourceManifestHash = (Get-FileHash -LiteralPath $sourceManifest -Algorithm SHA256).Hash
$installedIdentity = Get-ManifestIdentity ([xml](Get-Content -LiteralPath $registeredManifest -Raw))

Add-Type -AssemblyName System.IO.Compression
$archive = [IO.Compression.ZipFile]::OpenRead($msix)
try {
    $archiveEntries = [Collections.Generic.Dictionary[string, IO.Compression.ZipArchiveEntry]]::new(
        [StringComparer]::OrdinalIgnoreCase)
    foreach ($entry in $archive.Entries) {
        if (-not $entry.Name) { continue }
        $name = [Uri]::UnescapeDataString($entry.FullName).Replace('/', '\')
        if ([IO.Path]::IsPathRooted($name) -or $name.Contains(':') -or
            $name -match '(^|[\\/])\.\.([\\/]|$)') {
            throw "MSIX contains invalid package path: $($entry.FullName)"
        }
        if (-not $archiveEntries.TryAdd($name, $entry)) {
            throw "MSIX contains duplicate package path: $name"
        }
    }
    if (-not $archiveEntries.ContainsKey('AppxManifest.xml')) {
        throw "MSIX is missing AppxManifest.xml: $msix"
    }
    $manifestEntry = $archiveEntries['AppxManifest.xml']
    $stream = $manifestEntry.Open()
    try {
        $msixManifest = [xml]::new()
        $msixManifest.Load($stream)
    }
    finally { $stream.Dispose() }
    $msixIdentity = Get-ManifestIdentity $msixManifest
    if ($sourceIdentity.Name -ne $msixIdentity.Name -or
        $sourceIdentity.Publisher -ne $msixIdentity.Publisher -or
        $sourceIdentity.Version -ne $msixIdentity.Version -or
        $installedIdentity.Name -ne $msixIdentity.Name -or
        $installedIdentity.Publisher -ne $msixIdentity.Publisher -or
        $installedIdentity.Version -ne $msixIdentity.Version -or
        $package.Name -ne $installedIdentity.Name) {
        throw 'Recipe, MSIX and installed package manifest identities disagree.'
    }
    $publisherId = $PackageFamilyName.Substring($PackageFamilyName.LastIndexOf('_') + 1)
    $fullName = [string]$package.PackageFullName
    if (-not $package.Version -or [string]$package.Version -cne $installedIdentity.Version -or
        -not $fullName.StartsWith("$($installedIdentity.Name)_$($installedIdentity.Version)_",
            [StringComparison]::OrdinalIgnoreCase) -or
        -not $fullName.EndsWith("_$publisherId", [StringComparison]::OrdinalIgnoreCase)) {
        throw 'Registered package version or full name disagrees with its manifest.'
    }
    $msixManifestHash = Get-ArchiveHash $manifestEntry
    if ($sourceManifestHash -ne $msixManifestHash) {
        throw "The MSIX manifest differs from the recipe source: recipe=$sourceManifestHash msix=$msixManifestHash"
    }
    $installedManifestHash = (Get-FileHash -LiteralPath $registeredManifest -Algorithm SHA256).Hash
    if ($msixManifestHash -ne $installedManifestHash) {
        throw "The registered manifest differs from the MSIX: installed=$($installedIdentity.Version) msix=$($msixIdentity.Version)"
    }

    $seen = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
    $externalInputs = [Collections.Generic.List[object]]::new()
    $sdkUcrtRoot = [IO.Path]::GetFullPath((Join-Path ${env:ProgramFiles(x86)} 'Microsoft SDKs\Windows Kits\10\ExtensionSDKs\Microsoft.UniversalCRT.Debug')).TrimEnd('\') + '\'
    $items = @(
        foreach ($item in $recipeXml.SelectNodes("//*[local-name()='AppxPackagedFile']")) {
            $pathNode = $item.SelectSingleNode("*[local-name()='PackagePath']")
            $relative = if ($pathNode) { ([string]$pathNode.InnerText).Replace('/', '\') } else { '' }
            if (-not $relative -or -not $item.Include -or
                [IO.Path]::IsPathRooted($relative) -or $relative.Contains(':') -or
                $relative -match '(^|\\)\.{1,2}(\\|$)' -or
                $relative -ieq 'AppxManifest.xml' -or -not $seen.Add($relative)) {
                throw "Invalid or duplicate package recipe path: $relative"
            }
            $source = [IO.Path]::GetFullPath([Uri]::UnescapeDataString([string]$item.Include))
            $installed = [IO.Path]::GetFullPath((Join-Path $layout $relative))
            if (-not $installed.StartsWith($layoutPrefix, [StringComparison]::OrdinalIgnoreCase) -or
                -not (Test-Path -LiteralPath $source -PathType Leaf) -or
                -not (Test-Path -LiteralPath $installed -PathType Leaf)) {
                throw "Recipe source or installed payload missing: $relative"
            }
            $inSource = $source.StartsWith($sourcePrefix, [StringComparison]::OrdinalIgnoreCase)
            $sdkUcrt = $relative -ieq 'ucrtbased.dll' -and
                $source.StartsWith($sdkUcrtRoot, [StringComparison]::OrdinalIgnoreCase) -and
                $source.Substring($sdkUcrtRoot.Length) -match '^\d+\.\d+\.\d+\.\d+\\redist\\Debug\\x64\\ucrtbased\.dll$'
            if (-not $inSource -and -not $sdkUcrt) {
                throw "Recipe source is outside the selected source worktree: $relative"
            }

            $sourceHash = (Get-FileHash -LiteralPath $source -Algorithm SHA256).Hash
            if ($sdkUcrt) {
                $externalInputs.Add([pscustomobject]@{
                    PackagePath = $relative
                    SourcePath = $source
                    Sha256 = $sourceHash
                })
            }
            $installedHash = (Get-FileHash -LiteralPath $installed -Algorithm SHA256).Hash
            if ($sourceHash -ne $installedHash) {
                throw "The installed payload differs from the recipe source: $relative"
            }
            $archivePath = $relative
            $included = $archiveEntries.ContainsKey($archivePath)
            if ($included) {
                if ((Get-ArchiveHash $archiveEntries[$archivePath]) -ne $sourceHash) {
                    throw "MSIX payload differs from the recipe source: $relative"
                }
            }
            elseif ($relative -notmatch '^ProfileIcons[\\/][^\\/]+\.scale-\d+\.png$') {
                throw "Non-icon recipe payload missing from the MSIX: $relative"
            }
            [pscustomobject]@{ PackagePath = $relative; IncludedInMsix = $included; Sha256 = $sourceHash }
        }
    )
    $metadata = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
    foreach ($path in @('[Content_Types].xml', 'AppxBlockMap.xml', 'AppxSignature.p7x',
        'AppxMetadata\CodeIntegrity.cat')) {
        [void]$metadata.Add($path)
    }
    foreach ($path in $archiveEntries.Keys) {
        if ($path -ieq 'AppxManifest.xml' -or $metadata.Contains($path) -or $seen.Contains($path)) { continue }
        throw "Unexpected MSIX payload not in the recipe: $path"
    }
}
finally { $archive.Dispose() }

if (-not $items.Count) { throw "Package recipe has no payloads: $recipe" }
[pscustomobject]@{
    SourceHead = $head
    PackageFullName = [string]$package.PackageFullName
    InstalledLayout = $layout
    RecipeSha256 = (Get-FileHash -LiteralPath $recipe -Algorithm SHA256).Hash
    MsixSha256 = (Get-FileHash -LiteralPath $msix -Algorithm SHA256).Hash
    RecipeManifestSha256 = $sourceManifestHash
    RegisteredManifestSha256 = $installedManifestHash
    RecipeEntryCount = $items.Count
    MsixEntryCount = @($items | Where-Object IncludedInMsix).Count
    OmittedScaleAssets = @($items | Where-Object { -not $_.IncludedInMsix } | Select-Object -ExpandProperty PackagePath)
    ExternalInputs = @($externalInputs.ToArray())
    Entries = $items
}
