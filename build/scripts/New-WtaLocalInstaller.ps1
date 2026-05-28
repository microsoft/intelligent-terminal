[CmdletBinding()]
param(
    [ValidateSet('ARM64', 'x64', 'x86')]
    [string]$Platform = 'ARM64',

    [ValidateSet('Debug', 'Release')]
    [string]$Configuration = 'Debug',

    [string]$Destination = (Join-Path $PSScriptRoot '..\..\artifacts\local-installer'),

    [string]$TerminalMsix,

    [string]$XamlAppx,

    [switch]$BuildTerminal,

    [switch]$SkipWtaBuild,

    [string]$WtaExePath
)

$ErrorActionPreference = 'Stop'

function Write-Status {
    param([string]$Message)

    Write-Host "[local-installer] $Message"
}

function Ensure-Directory {
    param([string]$Path)

    if (-not (Test-Path $Path -PathType Container)) {
        New-Item -ItemType Directory -Path $Path | Out-Null
    }
}

function Resolve-AbsolutePath {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Path,

        [string]$BasePath = (Get-Location).Path
    )

    if ([System.IO.Path]::IsPathRooted($Path)) {
        return [System.IO.Path]::GetFullPath($Path)
    }

    return [System.IO.Path]::GetFullPath((Join-Path $BasePath $Path))
}

function Get-RustTarget {
    param([string]$PlatformName)

    switch ($PlatformName) {
        'ARM64' { return 'aarch64-pc-windows-msvc' }
        'x64' { return 'x86_64-pc-windows-msvc' }
        'x86' { return 'i686-pc-windows-msvc' }
        default { throw "Unsupported platform: $PlatformName" }
    }
}

function Get-XamlDependencyArch {
    param([string]$PlatformName)

    switch ($PlatformName) {
        'ARM64' { return 'arm64' }
        'x64' { return 'x64' }
        'x86' { return 'x86' }
        default { throw "Unsupported platform: $PlatformName" }
    }
}

function Find-CargoPath {
    $command = Get-Command cargo.exe -ErrorAction SilentlyContinue
    if ($command) {
        return $command.Source
    }

    $fallback = Join-Path $env:USERPROFILE '.cargo\bin\cargo.exe'
    if (Test-Path $fallback -PathType Leaf) {
        return $fallback
    }

    throw 'Could not find cargo.exe. Install Rust or add cargo.exe to PATH.'
}

function Get-InstalledRustTargets {
    $rustupPath = Join-Path $env:USERPROFILE '.cargo\bin\rustup.exe'
    if (-not (Test-Path $rustupPath -PathType Leaf)) {
        return @()
    }

    $targets = & $rustupPath target list --installed
    if ($LASTEXITCODE -ne 0) {
        throw 'rustup target list --installed failed.'
    }

    return @($targets | Where-Object { -not [string]::IsNullOrWhiteSpace($_) })
}

function Invoke-RustBuild {
    param(
        [Parameter(Mandatory = $true)]
        [string]$CargoPath,

        [Parameter(Mandatory = $true)]
        [string]$ManifestPath,

        [Parameter(Mandatory = $true)]
        [string]$RustTarget,

        [Parameter(Mandatory = $true)]
        [string]$RepoRoot
    )

    # Cargo's config discovery walks up from the current working directory,
    # not from the manifest path. Pin CWD to the repo root so Cargo finds the
    # repo-root .cargo/config.toml that supplies `+crt-static` — even when
    # this script is launched from outside the repo.
    #
    # Important: do NOT push into the manifest's directory. tools/wta/ has its
    # own rust-toolchain.toml, so letting rustup discover that file from CWD
    # can change toolchain resolution compared to the repo-root configuration
    # this script relies on for local builds.
    Push-Location $RepoRoot
    try {
        & $CargoPath build --manifest-path $ManifestPath --release --target $RustTarget
        if ($LASTEXITCODE -ne 0) {
            throw "cargo build failed for $ManifestPath."
        }
    }
    finally {
        Pop-Location
    }
}

function New-SelfExtractingInstaller {
    param(
        [Parameter(Mandatory = $true)]
        [string]$BootstrapExe,

        [Parameter(Mandatory = $true)]
        [string]$PayloadRoot,

        [Parameter(Mandatory = $true)]
        [string]$OutputPath
    )

    $bundleFiles = @(
        'install.cmd',
        'install-local-terminal.ps1',
        'payload.zip'
    )
    $footerMagic = [System.Text.Encoding]::ASCII.GetBytes('WTA-INSTALLER-V1')

    Copy-Item -Path $BootstrapExe -Destination $OutputPath -Force

    $outputStream = [System.IO.File]::Open($OutputPath, [System.IO.FileMode]::Open, [System.IO.FileAccess]::ReadWrite, [System.IO.FileShare]::Read)
    try {
        $outputStream.Seek(0, [System.IO.SeekOrigin]::End) | Out-Null
        $manifestEntries = New-Object System.Collections.Generic.List[string]

        foreach ($fileName in $bundleFiles) {
            $sourcePath = Join-Path $PayloadRoot $fileName
            if (-not (Test-Path $sourcePath -PathType Leaf)) {
                throw "Installer bundle input not found: $sourcePath"
            }

            $offset = [UInt64]$outputStream.Position
            $inputStream = [System.IO.File]::OpenRead($sourcePath)
            try {
                $inputStream.CopyTo($outputStream)
            }
            finally {
                $inputStream.Dispose()
            }

            $length = [UInt64]($outputStream.Position - [Int64]$offset)
            $manifestEntries.Add(("file|{0}|{1}|{2}" -f $fileName, $offset, $length))
        }

        $manifestText = ($manifestEntries -join "`n") + "`n"
        $manifestBytes = [System.Text.Encoding]::UTF8.GetBytes($manifestText)
        $outputStream.Write($manifestBytes, 0, $manifestBytes.Length)

        $manifestLengthBytes = [BitConverter]::GetBytes([UInt64]$manifestBytes.Length)
        $outputStream.Write($footerMagic, 0, $footerMagic.Length)
        $outputStream.Write($manifestLengthBytes, 0, $manifestLengthBytes.Length)
        $outputStream.Flush()
    }
    finally {
        $outputStream.Dispose()
    }
}

function Find-TerminalMsix {
    param(
        [Parameter(Mandatory = $true)]
        [string]$AppPackagesRoot,

        [Parameter(Mandatory = $true)]
        [string]$PlatformName,

        [Parameter(Mandatory = $true)]
        [string]$ConfigurationName
    )

    $patterns = @()
    if ($ConfigurationName -eq 'Release') {
        $patterns += "CascadiaPackage_.*_{0}\.(msix|appx)$" -f $PlatformName
    }
    $patterns += "CascadiaPackage_.*_{0}_{1}\.(msix|appx)$" -f $PlatformName, $ConfigurationName

    $candidate = Get-ChildItem -Path $AppPackagesRoot -Recurse -File |
        Where-Object {
            if ($_.FullName -match '\\Dependencies\\') {
                return $false
            }

            foreach ($pattern in $patterns) {
                if ($_.Name -match $pattern) {
                    return $true
                }
            }

            return $false
        } |
        Sort-Object LastWriteTime -Descending |
        Select-Object -First 1

    if (-not $candidate) {
        throw "Could not find a Cascadia package for $PlatformName/$ConfigurationName under $AppPackagesRoot."
    }

    return $candidate.FullName
}

function Find-XamlAppx {
    param(
        [Parameter(Mandatory = $true)]
        [string]$TerminalPackagePath,

        [Parameter(Mandatory = $true)]
        [string]$PlatformName
    )

    $dependencyArch = Get-XamlDependencyArch -PlatformName $PlatformName
    $dependencyRoot = Join-Path (Split-Path $TerminalPackagePath -Parent) ("Dependencies\{0}" -f $dependencyArch)

    if (-not (Test-Path $dependencyRoot -PathType Container)) {
        throw "Could not find the dependency folder $dependencyRoot."
    }

    $candidate = Get-ChildItem -Path $dependencyRoot -File -Filter 'Microsoft.UI.Xaml*.appx' |
        Sort-Object LastWriteTime -Descending |
        Select-Object -First 1

    if (-not $candidate) {
        throw "Could not find a Microsoft.UI.Xaml dependency package under $dependencyRoot."
    }

    return $candidate.FullName
}

function Get-AppxPackageIdentity {
    param(
        [Parameter(Mandatory = $true)]
        [string]$PackagePath
    )

    Add-Type -AssemblyName System.IO.Compression.FileSystem

    $archive = [System.IO.Compression.ZipFile]::OpenRead($PackagePath)
    try {
        $manifestEntry = $archive.GetEntry('AppxManifest.xml')
        if (-not $manifestEntry) {
            throw "Could not find AppxManifest.xml inside $PackagePath."
        }

        $reader = New-Object System.IO.StreamReader($manifestEntry.Open())
        try {
            $manifestContent = $reader.ReadToEnd()
        }
        finally {
            $reader.Dispose()
        }
    }
    finally {
        $archive.Dispose()
    }

    [xml]$manifestXml = $manifestContent
    $namespaceManager = New-Object System.Xml.XmlNamespaceManager($manifestXml.NameTable)
    $namespaceManager.AddNamespace('appx', 'http://schemas.microsoft.com/appx/manifest/foundation/windows10')
    $identityNode = $manifestXml.SelectSingleNode('/appx:Package/appx:Identity', $namespaceManager)
    if (-not $identityNode) {
        throw "Could not find the Identity node in AppxManifest.xml from $PackagePath."
    }

    return [pscustomobject]@{
        Name = $identityNode.Name
        Version = $identityNode.Version
        Publisher = $identityNode.Publisher
        ProcessorArchitecture = $identityNode.ProcessorArchitecture
    }
}

function Get-AppxManifestIdentity {
    param(
        [Parameter(Mandatory = $true)]
        [string]$ManifestPath
    )

    [xml]$manifestXml = Get-Content -Path $ManifestPath -Raw
    $namespaceManager = New-Object System.Xml.XmlNamespaceManager($manifestXml.NameTable)
    $namespaceManager.AddNamespace('appx', 'http://schemas.microsoft.com/appx/manifest/foundation/windows10')
    $identityNode = $manifestXml.SelectSingleNode('/appx:Package/appx:Identity', $namespaceManager)
    if (-not $identityNode) {
        throw "Could not find the Identity node in $ManifestPath."
    }

    return [pscustomobject]@{
        Name = $identityNode.Name
        Version = $identityNode.Version
        Publisher = $identityNode.Publisher
        ProcessorArchitecture = $identityNode.ProcessorArchitecture
    }
}

function Get-SingleChildDirectoryOrSelf {
    param([string]$RootPath)

    $children = @(Get-ChildItem -Path $RootPath -Force)
    if ($children.Count -eq 1 -and $children[0].PSIsContainer) {
        return $children[0].FullName
    }

    return $RootPath
}

function Build-TerminalPackage {
    param(
        [Parameter(Mandatory = $true)]
        [string]$RepoRoot,

        [Parameter(Mandatory = $true)]
        [string]$PlatformName,

        [Parameter(Mandatory = $true)]
        [string]$ConfigurationName
    )

    $openConsoleModule = Join-Path $RepoRoot 'tools\OpenConsole.psm1'
    $solutionPath = Join-Path $RepoRoot 'OpenConsole.slnx'
    $packagesConfig = Join-Path $RepoRoot 'dep\nuget\packages.config'
    $packageProject = Join-Path $RepoRoot 'src\cascadia\CascadiaPackage\CascadiaPackage.wapproj'

    Write-Status "Building CascadiaPackage for $PlatformName/$ConfigurationName ..."

    Import-Module $openConsoleModule -Force
    Set-MsbuildDevEnvironment

    & "$RepoRoot\dep\nuget\nuget.exe" restore $solutionPath
    if ($LASTEXITCODE -ne 0) {
        throw "NuGet restore failed for $solutionPath."
    }

    & "$RepoRoot\dep\nuget\nuget.exe" restore $packagesConfig
    if ($LASTEXITCODE -ne 0) {
        throw "NuGet restore failed for $packagesConfig."
    }

    & msbuild.exe $packageProject `
        "/p:Platform=$PlatformName" `
        "/p:Configuration=$ConfigurationName" `
        "/p:SolutionDir=$RepoRoot\" `
        "/p:OpenConsoleDir=$RepoRoot\" `
        '/p:WindowsTerminalBranding=Dev' `
        '/p:GenerateAppxPackageOnBuild=true' `
        '/p:AppxBundle=Never' `
        /m `
        /nologo
    if ($LASTEXITCODE -ne 0) {
        throw "msbuild failed for $packageProject."
    }
}

$repoRoot = Resolve-AbsolutePath -Path (Join-Path $PSScriptRoot '..\..')
$destinationRoot = Resolve-AbsolutePath -Path $Destination
$appPackagesRoot = Join-Path $repoRoot 'src\cascadia\CascadiaPackage\AppPackages'
$unpackagedScript = Join-Path $repoRoot 'build\scripts\New-UnpackagedTerminalDistribution.ps1'
$installerScript = Join-Path $repoRoot 'installer\install-local-terminal.ps1'
$installerCmd = Join-Path $repoRoot 'installer\install.cmd'
$installerBootstrapManifest = Join-Path $repoRoot 'installer\bootstrap\Cargo.toml'
$plannerPromptTemplate = Join-Path $repoRoot 'tools\wta\prompts\terminal-agent.md'
$devManifestPath = Join-Path $repoRoot 'src\cascadia\CascadiaPackage\Package-Dev.appxmanifest'

if (-not (Test-Path $unpackagedScript -PathType Leaf)) {
    throw "Could not find $unpackagedScript."
}
if (-not (Test-Path $installerScript -PathType Leaf)) {
    throw "Could not find $installerScript."
}
if (-not (Test-Path $installerCmd -PathType Leaf)) {
    throw "Could not find $installerCmd."
}
if (-not (Test-Path $installerBootstrapManifest -PathType Leaf)) {
    throw "Could not find $installerBootstrapManifest."
}
if (-not (Test-Path $plannerPromptTemplate -PathType Leaf)) {
    throw "Could not find $plannerPromptTemplate."
}
if (-not (Test-Path $devManifestPath -PathType Leaf)) {
    throw "Could not find $devManifestPath."
}

$expectedManifestIdentity = Get-AppxManifestIdentity -ManifestPath $devManifestPath

Ensure-Directory -Path $destinationRoot

if ($BuildTerminal) {
    Build-TerminalPackage -RepoRoot $repoRoot -PlatformName $Platform -ConfigurationName $Configuration
}

if ($TerminalMsix) {
    $TerminalMsix = Resolve-AbsolutePath -Path $TerminalMsix
} else {
    $TerminalMsix = Find-TerminalMsix -AppPackagesRoot $appPackagesRoot -PlatformName $Platform -ConfigurationName $Configuration
}

if ($XamlAppx) {
    $XamlAppx = Resolve-AbsolutePath -Path $XamlAppx
} else {
    $XamlAppx = Find-XamlAppx -TerminalPackagePath $TerminalMsix -PlatformName $Platform
}

if (-not (Test-Path $TerminalMsix -PathType Leaf)) {
    throw "Terminal package not found: $TerminalMsix"
}
if (-not (Test-Path $XamlAppx -PathType Leaf)) {
    throw "XAML package not found: $XamlAppx"
}

$packageIdentity = Get-AppxPackageIdentity -PackagePath $TerminalMsix
$installerVersion = $packageIdentity.Version

if ($BuildTerminal -and $installerVersion -ne $expectedManifestIdentity.Version) {
    throw "Built package version $installerVersion does not match source manifest version $($expectedManifestIdentity.Version). Refusing to package a stale MSIX."
}

$cargoPath = Find-CargoPath
$rustTarget = Get-RustTarget -PlatformName $Platform
$installedTargets = Get-InstalledRustTargets

if ($installedTargets.Count -gt 0 -and $installedTargets -notcontains $rustTarget) {
    throw "Rust target $rustTarget is not installed. Install it with rustup target add $rustTarget."
}

$timestamp = Get-Date -Format 'yyyyMMdd-HHmmss'
$stageRoot = Join-Path $destinationRoot ("stage-{0}-{1}-{2}" -f $Platform.ToLowerInvariant(), $Configuration.ToLowerInvariant(), $timestamp)
$terminalZipRoot = Join-Path $stageRoot 'terminal-zip'
$payloadExtractRoot = Join-Path $stageRoot 'payload-extracted'
$installerSourceRoot = Join-Path $stageRoot 'installer-source'
$payloadZip = Join-Path $stageRoot 'payload.zip'
$setupExeName = "intelligent-terminal-{0}-{1}-{2}-setup.exe" -f $installerVersion, $Platform.ToLowerInvariant(), $Configuration.ToLowerInvariant()
$setupExePath = Join-Path $destinationRoot $setupExeName

Ensure-Directory -Path $stageRoot
Ensure-Directory -Path $terminalZipRoot
Ensure-Directory -Path $payloadExtractRoot
Ensure-Directory -Path $installerSourceRoot

Write-Status "Creating unpackaged Terminal distribution from:"
Write-Status "  Terminal package: $TerminalMsix"
Write-Status "  XAML dependency:  $XamlAppx"
Write-Status "  Version:          $installerVersion"
$unpackagedZip = & $unpackagedScript -TerminalAppX $TerminalMsix -XamlAppX $XamlAppx -Destination $terminalZipRoot -PortableMode

if (-not $unpackagedZip) {
    throw 'New-UnpackagedTerminalDistribution.ps1 did not return an output ZIP.'
}

$unpackagedZipPath = $unpackagedZip.FullName
if (-not (Test-Path $unpackagedZipPath -PathType Leaf)) {
    throw "Unpackaged Terminal ZIP not found: $unpackagedZipPath"
}

Write-Status "Expanding unpackaged Terminal layout ..."
Expand-Archive -Path $unpackagedZipPath -DestinationPath $payloadExtractRoot -Force
$payloadRoot = Get-SingleChildDirectoryOrSelf -RootPath $payloadExtractRoot

if ($SkipWtaBuild) {
    if (-not $WtaExePath) {
        throw 'Use -WtaExePath when -SkipWtaBuild is set.'
    }
    $resolvedWtaExePath = Resolve-AbsolutePath -Path $WtaExePath
} else {
    Write-Status "Building wta.exe for $rustTarget with a static CRT ..."
    $manifestPath = Join-Path $repoRoot 'tools\wta\Cargo.toml'
    Invoke-RustBuild -CargoPath $cargoPath -ManifestPath $manifestPath -RustTarget $rustTarget -RepoRoot $repoRoot
    $resolvedWtaExePath = Join-Path $repoRoot ("tools\wta\target\{0}\release\wta.exe" -f $rustTarget)
}

if (-not (Test-Path $resolvedWtaExePath -PathType Leaf)) {
    throw "wta.exe not found: $resolvedWtaExePath"
}

Write-Status "Injecting wta.exe into the unpackaged payload ..."
Copy-Item -Path $resolvedWtaExePath -Destination (Join-Path $payloadRoot 'wta.exe') -Force

# wtcli.exe is built by MSBuild (C++) alongside the Terminal.  wta.exe discovers
# it as a sibling, so it must be co-located in the installed layout.
$wtcliSource = Join-Path $repoRoot ("bin\{0}\{1}\wtcli\wtcli.exe" -f $Platform, $Configuration)
if (Test-Path $wtcliSource -PathType Leaf) {
    Write-Status "Injecting wtcli.exe into the unpackaged payload ..."
    Copy-Item -Path $wtcliSource -Destination (Join-Path $payloadRoot 'wtcli.exe') -Force
} else {
    Write-Warning "wtcli.exe not found at $wtcliSource — autofix and protocol features will not work."
}

$payloadPromptDir = Join-Path $payloadRoot 'prompts'
Ensure-Directory -Path $payloadPromptDir
Write-Status "Injecting planner prompt template into the payload ..."
Copy-Item -Path $plannerPromptTemplate -Destination (Join-Path $payloadPromptDir 'terminal-agent.default.md') -Force

$payloadMetadata = [ordered]@{
    productName = 'Intelligent Terminal'
    version = $installerVersion
    packageName = $packageIdentity.Name
    publisher = $packageIdentity.Publisher
    processorArchitecture = $packageIdentity.ProcessorArchitecture
    platform = $Platform
    configuration = $Configuration
    createdAtUtc = (Get-Date).ToUniversalTime().ToString('o')
}
$payloadMetadataPath = Join-Path $payloadRoot 'intelligent-terminal-install-metadata.json'
Set-Content -Path $payloadMetadataPath -Value ($payloadMetadata | ConvertTo-Json -Depth 4) -Encoding utf8

if (Test-Path $payloadZip -PathType Leaf) {
    Remove-Item $payloadZip -Force
}

Write-Status "Packing installer payload ..."
& "$env:SystemRoot\System32\tar.exe" -c --format=zip -f $payloadZip -C (Split-Path $payloadRoot -Parent) (Split-Path $payloadRoot -Leaf)
if ($LASTEXITCODE -ne 0) {
    throw 'Creating payload.zip failed.'
}

Copy-Item -Path $installerScript -Destination (Join-Path $installerSourceRoot 'install-local-terminal.ps1') -Force
Copy-Item -Path $installerCmd -Destination (Join-Path $installerSourceRoot 'install.cmd') -Force
Copy-Item -Path $payloadZip -Destination (Join-Path $installerSourceRoot 'payload.zip') -Force

Write-Status "Building installer bootstrap for $rustTarget ..."
Invoke-RustBuild -CargoPath $cargoPath -ManifestPath $installerBootstrapManifest -RustTarget $rustTarget -RepoRoot $repoRoot
$bootstrapExePath = Join-Path $repoRoot ("installer\bootstrap\target\{0}\release\intelligent-terminal-installer-bootstrap.exe" -f $rustTarget)
if (-not (Test-Path $bootstrapExePath -PathType Leaf)) {
    throw "Installer bootstrap not found: $bootstrapExePath"
}

if (Test-Path $setupExePath -PathType Leaf) {
    Remove-Item $setupExePath -Force
}

Write-Status "Creating target-architecture setup executable ..."
New-SelfExtractingInstaller -BootstrapExe $bootstrapExePath -PayloadRoot $installerSourceRoot -OutputPath $setupExePath

Write-Status "Installer created: $setupExePath"
Get-Item $setupExePath
