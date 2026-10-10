#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }

BeforeAll {
    $script:verifier = Join-Path $PSScriptRoot '..\Verify-PackageProvenance.ps1'

    function New-ProvenanceFixture {
        param(
            [switch]$WrongArchive,
            [switch]$WrongInstalled,
            [switch]$DifferentManifest,
            [switch]$StaleManifest,
            [switch]$ExtraArchiveDll,
            [switch]$IncludeEncodedIcon,
            [switch]$WithMetadata,
            [switch]$OmitArchiveDll,
            [switch]$ExternalResource,
            [switch]$ExternalSdkUcrt,
            [switch]$DuplicateMixedSeparator,
            [switch]$TraversalPath,
            [switch]$WrongFamily
        )

        $root = Join-Path $TestDrive ([guid]::NewGuid().ToString('N'))
        $sourceRoot = Join-Path $root 'source'
        $installed = Join-Path $root 'registered\AppX'
        New-Item -ItemType Directory -Path $sourceRoot, $installed,
            (Join-Path $sourceRoot 'ProfileIcons'), (Join-Path $installed 'ProfileIcons') -Force | Out-Null

        $sourceDll = Join-Path $sourceRoot 'TerminalApp.dll'
        $installedDll = Join-Path $installed 'TerminalApp.dll'
        $iconName = if ($IncludeEncodedIcon) { '{fixture}.scale-100.png' } else { 'icon.scale-100.png' }
        $iconPackagePath = "ProfileIcons\$iconName"
        $sourceIcon = Join-Path $sourceRoot $iconPackagePath
        $extraPackagePath = if ($ExternalSdkUcrt) { 'ucrtbased.dll' }
            elseif ($ExternalResource) { 'fixture-data.txt' } else { '' }
        $extraSource = if ($ExternalSdkUcrt) {
            Join-Path ${env:ProgramFiles(x86)} 'Microsoft SDKs\Windows Kits\10\ExtensionSDKs\Microsoft.UniversalCRT.Debug\10.0.26100.0\redist\Debug\x64\ucrtbased.dll'
        } elseif ($ExternalResource) {
            Join-Path $root 'outside\fixture-data.txt'
        } else { '' }
        if ($extraPackagePath) {
            New-Item -ItemType Directory -Path (Split-Path $extraSource -Parent) -Force | Out-Null
            'external-fixture' | Set-Content -LiteralPath $extraSource -NoNewline
            Copy-Item -LiteralPath $extraSource -Destination (Join-Path $installed $extraPackagePath)
        }
        $manifestText = '<Package><Identity Name="IntelligentTerminal" Publisher="CN=Test" Version="0.8.0.2"/></Package>'
        $staleManifestText = if ($StaleManifest) {
            $manifestText.Replace('</Package>', '<Capabilities><Capability Name="privateNetworkClientServer"/></Capabilities></Package>')
        } else { $manifestText }
        'current-binary' | Set-Content -LiteralPath $sourceDll -NoNewline
        Copy-Item -LiteralPath $sourceDll -Destination $installedDll
        if ($WrongInstalled) { 'previous-binary' | Set-Content -LiteralPath $installedDll -NoNewline }
        'scale-icon' | Set-Content -LiteralPath $sourceIcon -NoNewline
        Copy-Item -LiteralPath $sourceIcon -Destination (Join-Path $installed $iconPackagePath)
        $sourceManifest = Join-Path $sourceRoot 'AppxManifest.xml'
        $manifestText | Set-Content -LiteralPath $sourceManifest -NoNewline
        $(if ($DifferentManifest) {
            $manifestText.Replace('0.8.0.2', '0.8.0.3')
        } else {
            $staleManifestText
        }) | Set-Content -LiteralPath (Join-Path $installed 'AppxManifest.xml') -NoNewline

        $recipe = Join-Path $sourceRoot 'CascadiaPackage.build.appxrecipe'
        $dllPackagePath = if ($TraversalPath) { '..\TerminalApp.dll' } else { 'TerminalApp.dll' }
        $extraIcon = if ($DuplicateMixedSeparator) {
            "<AppxPackagedFile Include=`"$sourceIcon`"><PackagePath>ProfileIcons/$iconName</PackagePath></AppxPackagedFile>"
        } else { '' }
        $extraFile = if ($extraPackagePath) {
            "<AppxPackagedFile Include=`"$extraSource`"><PackagePath>$extraPackagePath</PackagePath></AppxPackagedFile>"
        } else { '' }
        @"
<Project>
  <ItemGroup>
    <AppXManifest Include="$sourceManifest"><PackagePath>AppxManifest.xml</PackagePath></AppXManifest>
    <AppxPackagedFile Include="$sourceDll"><PackagePath>$dllPackagePath</PackagePath></AppxPackagedFile>
    <AppxPackagedFile Include="$sourceIcon"><PackagePath>$iconPackagePath</PackagePath></AppxPackagedFile>
    $extraIcon
    $extraFile
  </ItemGroup>
</Project>
"@ | Set-Content -LiteralPath $recipe

        Add-Type -AssemblyName System.IO.Compression
        $msix = Join-Path $sourceRoot 'CascadiaPackage.msix'
        $zip = [IO.Compression.ZipFile]::Open($msix, [IO.Compression.ZipArchiveMode]::Create)
        try {
            $archiveItems = @(
                @{ Path = 'AppxManifest.xml'; Text = $staleManifestText },
                @{ Path = 'TerminalApp.dll'; Text = $(if ($WrongArchive) { 'stale-binary' } else { 'current-binary' }) }
            )
            if ($ExtraArchiveDll) { $archiveItems += @{ Path = 'old.dll'; Text = 'stale-binary' } }
            if ($extraPackagePath) {
                $archiveItems += @{ Path = $extraPackagePath; Text = 'external-fixture' }
            }
            if ($IncludeEncodedIcon) {
                $archiveItems += @{ Path = 'ProfileIcons/%7Bfixture%7D.scale-100.png'; Text = 'scale-icon' }
            }
            if ($WithMetadata) {
                $archiveItems += @(
                    @{ Path = 'AppxBlockMap.xml'; Text = 'block-map' },
                    @{ Path = '[Content_Types].xml'; Text = 'content-types' },
                    @{ Path = 'AppxSignature.p7x'; Text = 'signature' },
                    @{ Path = 'AppxMetadata/CodeIntegrity.cat'; Text = 'catalog' }
                )
            }
            foreach ($item in $archiveItems) {
                if ($item.Path -eq 'TerminalApp.dll' -and $OmitArchiveDll) { continue }
                $stream = $zip.CreateEntry($item.Path).Open()
                try {
                    $bytes = [Text.Encoding]::UTF8.GetBytes($item.Text)
                    $stream.Write($bytes, 0, $bytes.Length)
                }
                finally { $stream.Dispose() }
            }
        }
        finally { $zip.Dispose() }

        & git -C $sourceRoot init --quiet
        & git -C $sourceRoot config user.name 'Offline provenance test'
        & git -C $sourceRoot config user.email 'provenance-test@example.invalid'
        & git -C $sourceRoot add .
        & git -C $sourceRoot commit --quiet -m 'snapshot package sources'
        if ($LASTEXITCODE -ne 0) { throw 'Could not create clean provenance fixture.' }

        [pscustomobject]@{
            SourceRoot = $sourceRoot
            Head = (& git -C $sourceRoot rev-parse HEAD).Trim()
            Recipe = $recipe
            Msix = $msix
            SourceDll = $sourceDll
            InstalledDll = $installedDll
            Package = [pscustomobject]@{
                Name = 'IntelligentTerminal'
                PackageFamilyName = if ($WrongFamily) { 'Microsoft.IntelligentTerminal_8wekyb3d8bbwe' } else { 'IntelligentTerminal_rd9vj3e6a2mbr' }
                PackageFullName = 'IntelligentTerminal_0.8.0.2_x64__rd9vj3e6a2mbr'
                Version = [version]'0.8.0.2'
                InstallLocation = $installed
            }
        }
    }
}

Describe 'Offline package provenance' -Tag 'Unit' {
    It 'accepts a clean source head, complete MSIX and matching registered payloads' {
        $f = New-ProvenanceFixture
        $prior = (Get-FileHash -LiteralPath $f.InstalledDll -Algorithm SHA256).Hash

        $proof = & $script:verifier -SourceRoot $f.SourceRoot -ExpectedHead $f.Head `
            -RecipePath $f.Recipe -MsixPath $f.Msix -InstalledPackage $f.Package

        $proof.SourceHead | Should -Be $f.Head
        $proof.RecipeEntryCount | Should -Be 2
        $proof.MsixEntryCount | Should -Be 1
        $proof.OmittedScaleAssets | Should -Be @('ProfileIcons\icon.scale-100.png')
        $proof.RecipeManifestSha256 | Should -Be $proof.RegisteredManifestSha256
        $proof.ExternalInputs | Should -HaveCount 0
        (Get-FileHash -LiteralPath $f.InstalledDll -Algorithm SHA256).Hash | Should -Be $prior
    }

    It 'rejects an unexpected source revision' {
        $f = New-ProvenanceFixture
        { & $script:verifier -SourceRoot $f.SourceRoot -ExpectedHead ('f' * 40) `
                -RecipePath $f.Recipe -MsixPath $f.Msix -InstalledPackage $f.Package } |
            Should -Throw '*source HEAD*'
    }

    It 'rejects a dirty source worktree after the build' {
        $f = New-ProvenanceFixture
        'changed' | Set-Content -LiteralPath $f.SourceDll -NoNewline
        { & $script:verifier -SourceRoot $f.SourceRoot -ExpectedHead $f.Head `
                -RecipePath $f.Recipe -MsixPath $f.Msix -InstalledPackage $f.Package } |
            Should -Throw '*clean source*'
    }

    It 'rejects a stale binary inside the MSIX' {
        $f = New-ProvenanceFixture -WrongArchive
        { & $script:verifier -SourceRoot $f.SourceRoot -ExpectedHead $f.Head `
                -RecipePath $f.Recipe -MsixPath $f.Msix -InstalledPackage $f.Package } |
            Should -Throw '*MSIX payload*'
    }

    It 'rejects a registered binary from an older build' {
        $f = New-ProvenanceFixture -WrongInstalled
        { & $script:verifier -SourceRoot $f.SourceRoot -ExpectedHead $f.Head `
                -RecipePath $f.Recipe -MsixPath $f.Msix -InstalledPackage $f.Package } |
            Should -Throw '*installed payload*'
    }

    It 'does not accept a different registered manifest version as an exact package match' {
        $f = New-ProvenanceFixture -DifferentManifest
        { & $script:verifier -SourceRoot $f.SourceRoot -ExpectedHead $f.Head `
                -RecipePath $f.Recipe -MsixPath $f.Msix -InstalledPackage $f.Package } |
            Should -Throw '*manifest*'
    }

    It 'rejects stale registered version metadata even when all manifests and files agree' {
        $f = New-ProvenanceFixture
        $f.Package.Version = [version]'0.8.0.3'
        { & $script:verifier -SourceRoot $f.SourceRoot -ExpectedHead $f.Head `
                -RecipePath $f.Recipe -MsixPath $f.Msix -InstalledPackage $f.Package } |
            Should -Throw '*Registered package version*'
    }

    It 'rejects a stale package full name even when its Version property is current' {
        $f = New-ProvenanceFixture
        $f.Package.PackageFullName = 'IntelligentTerminal_0.8.0.3_x64__rd9vj3e6a2mbr'
        { & $script:verifier -SourceRoot $f.SourceRoot -ExpectedHead $f.Head `
                -RecipePath $f.Recipe -MsixPath $f.Msix -InstalledPackage $f.Package } |
            Should -Throw '*Registered package version*'
    }

    It 'rejects an MSIX and installed manifest with stale capabilities but the same identity' {
        $f = New-ProvenanceFixture -StaleManifest
        { & $script:verifier -SourceRoot $f.SourceRoot -ExpectedHead $f.Head `
                -RecipePath $f.Recipe -MsixPath $f.Msix -InstalledPackage $f.Package } |
            Should -Throw '*MSIX manifest differs from the recipe source*'
    }

    It 'rejects an extra stale DLL in the MSIX even when all recipe files match' {
        $f = New-ProvenanceFixture -ExtraArchiveDll
        { & $script:verifier -SourceRoot $f.SourceRoot -ExpectedHead $f.Head `
                -RecipePath $f.Recipe -MsixPath $f.Msix -InstalledPackage $f.Package } |
            Should -Throw '*Unexpected MSIX payload*old.dll*'
    }

    It 'rejects a matching non-binary payload sourced outside the clean source worktree' {
        $f = New-ProvenanceFixture -ExternalResource
        { & $script:verifier -SourceRoot $f.SourceRoot -ExpectedHead $f.Head `
                -RecipePath $f.Recipe -MsixPath $f.Msix -InstalledPackage $f.Package } |
            Should -Throw '*Recipe source is outside the selected source worktree: fixture-data.txt*'
    }

    It 'records the Windows SDK debug UCRT as the sole allowed external recipe input' {
        $previous = [Environment]::GetEnvironmentVariable('ProgramFiles(x86)', 'Process')
        [Environment]::SetEnvironmentVariable('ProgramFiles(x86)',
            (Join-Path $TestDrive 'MockProgramFiles(x86)'), 'Process')
        try {
            $f = New-ProvenanceFixture -ExternalSdkUcrt
            $proof = & $script:verifier -SourceRoot $f.SourceRoot -ExpectedHead $f.Head `
                -RecipePath $f.Recipe -MsixPath $f.Msix -InstalledPackage $f.Package
            $proof.RecipeEntryCount | Should -Be 3
            $proof.ExternalInputs | Should -HaveCount 1
            $proof.ExternalInputs[0].PackagePath | Should -Be 'ucrtbased.dll'
            $proof.ExternalInputs[0].Sha256 | Should -Be (Get-FileHash -LiteralPath $proof.ExternalInputs[0].SourcePath).Hash
        }
        finally {
            [Environment]::SetEnvironmentVariable('ProgramFiles(x86)', $previous, 'Process')
        }
    }

    It 'matches percent-encoded profile icons to their recipe source' {
        $f = New-ProvenanceFixture -IncludeEncodedIcon
        $proof = & $script:verifier -SourceRoot $f.SourceRoot -ExpectedHead $f.Head `
            -RecipePath $f.Recipe -MsixPath $f.Msix -InstalledPackage $f.Package
        $proof.MsixEntryCount | Should -Be 2
        $proof.OmittedScaleAssets | Should -BeNullOrEmpty
        ($proof.Entries | Where-Object PackagePath -eq 'ProfileIcons\{fixture}.scale-100.png').IncludedInMsix |
            Should -BeTrue
    }

    It 'allows only the documented MSIX packaging metadata beyond recipe files' {
        $f = New-ProvenanceFixture -WithMetadata
        $proof = & $script:verifier -SourceRoot $f.SourceRoot -ExpectedHead $f.Head `
            -RecipePath $f.Recipe -MsixPath $f.Msix -InstalledPackage $f.Package
        $proof.RecipeEntryCount | Should -Be 2
        $proof.MsixEntryCount | Should -Be 1
    }

    It 'rejects a non-icon recipe payload omitted from the MSIX' {
        $f = New-ProvenanceFixture -OmitArchiveDll
        { & $script:verifier -SourceRoot $f.SourceRoot -ExpectedHead $f.Head `
                -RecipePath $f.Recipe -MsixPath $f.Msix -InstalledPackage $f.Package } |
            Should -Throw '*Non-icon recipe payload missing*'
    }

    It 'rejects path traversal in a package recipe' {
        $f = New-ProvenanceFixture -TraversalPath
        { & $script:verifier -SourceRoot $f.SourceRoot -ExpectedHead $f.Head `
                -RecipePath $f.Recipe -MsixPath $f.Msix -InstalledPackage $f.Package } |
            Should -Throw '*Invalid or duplicate package recipe path*'
    }

    It 'rejects duplicate recipe paths with mixed separators before matching MSIX entries' {
        $f = New-ProvenanceFixture -DuplicateMixedSeparator
        { & $script:verifier -SourceRoot $f.SourceRoot -ExpectedHead $f.Head `
                -RecipePath $f.Recipe -MsixPath $f.Msix -InstalledPackage $f.Package } |
            Should -Throw '*Invalid or duplicate package recipe path*'
    }

    It 'rejects a package from the wrong family' {
        $f = New-ProvenanceFixture -WrongFamily
        { & $script:verifier -SourceRoot $f.SourceRoot -ExpectedHead $f.Head `
                -RecipePath $f.Recipe -MsixPath $f.Msix -InstalledPackage $f.Package } |
            Should -Throw '*Expected exactly one installed package*'
    }
}
