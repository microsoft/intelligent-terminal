# Building Installers

There are two installer types for distributing Agentic Terminal.

## 1. MSIX ZIP Installer (Packaged)

A ZIP containing a dev certificate, MSIX package, XAML dependency, and install script. Recipients run `Install.ps1` as admin to sideload the packaged app.

### Output structure

```
agentic-terminal-<version>-<arch>-msix.zip
├── AgenticTerminalDev.cer              # Dev signing certificate
├── CascadiaPackage_<version>_<arch>.msix  # Terminal MSIX package
├── Dependencies/
│   └── Microsoft.UI.Xaml.2.8.appx     # XAML framework dependency
└── Install.ps1                         # Imports cert + installs packages
```

### Prerequisites

- Visual Studio 2022 with C++ desktop & UWP workloads
- Windows 10 SDK (10.0.22621.0+)
- NuGet CLI (`dep\nuget\nuget.exe` in repo)

### Step 1: Build the Terminal MSIX

From a razzle shell or PowerShell with MSBuild on PATH:

```powershell
# Option A: From razzle shell
tools\razzle.cmd
bcz

# Option B: From PowerShell
Import-Module .\tools\OpenConsole.psm1
Set-MsBuildDevEnvironment
msbuild src\cascadia\CascadiaPackage\CascadiaPackage.wapproj `
    /p:Platform=x64 `
    /p:Configuration=Release `
    /p:WindowsTerminalBranding=Dev `
    /p:GenerateAppxPackageOnBuild=true `
    /p:AppxBundle=Never `
    /m

# Option C: Visual Studio F5 (Debug build)
# Set CascadiaPackage as startup project → Build
```

MSBuild outputs to:
```
src\cascadia\CascadiaPackage\AppPackages\CascadiaPackage_<version>_<arch>_Test\
├── CascadiaPackage_<version>_<arch>.msix
├── Dependencies\<arch>\Microsoft.UI.Xaml.2.8.appx
├── Install.ps1            # VS-generated (verbose, 16KB)
└── Add-AppDevPackage.ps1  # VS-generated sideloading script
```

### Step 2: Assemble the MSIX ZIP

Collect the outputs into a distribution ZIP. The existing `artifacts/local-installer/` layout is the template:

```powershell
$version = "0.0.4.5"
$arch = "x64"
$buildOutput = "src\cascadia\CascadiaPackage\AppPackages\CascadiaPackage_${version}_${arch}_Test"
$outDir = "artifacts\local-installer\agentic-terminal-${version}-${arch}-msix"

# Create output directory
New-Item -ItemType Directory -Path $outDir -Force
New-Item -ItemType Directory -Path "$outDir\Dependencies" -Force

# Copy MSIX
Copy-Item "$buildOutput\CascadiaPackage_${version}_${arch}.msix" $outDir

# Copy XAML dependency (match architecture)
Copy-Item "$buildOutput\Dependencies\${arch}\Microsoft.UI.Xaml.2.8.appx" "$outDir\Dependencies\"

# Copy dev certificate (if you have the .cer already)
# The .cer is exported from the PFX used to sign the package.
# If CascadiaPackage_TemporaryKey.pfx exists in CascadiaPackage/, the .cer was
# exported from it. Otherwise use the existing AgenticTerminalDev.cer.
Copy-Item "artifacts\local-installer\agentic-terminal-0.0.4.5-x64-msix\AgenticTerminalDev.cer" $outDir

# Copy Install.ps1 (use the simplified one from existing artifacts)
Copy-Item "artifacts\local-installer\agentic-terminal-0.0.4.5-x64-msix\Install.ps1" $outDir

# Create ZIP
Compress-Archive -Path "$outDir\*" -DestinationPath "artifacts\local-installer\agentic-terminal-${version}-${arch}-msix.zip" -Force
```

### Step 3: Install on target machine

```powershell
# Extract the ZIP, then run as admin:
powershell -ExecutionPolicy Bypass -File .\Install.ps1
```

`Install.ps1` does three things:
1. Removes any old unpackaged install (`%LOCALAPPDATA%\Programs\AgenticTerminal`)
2. Imports `AgenticTerminalDev.cer` into the Trusted People certificate store
3. Installs the XAML dependency and the Terminal MSIX via `Add-AppxPackage`

### Certificate notes

- The dev certificate (`AgenticTerminalDev.cer`) must be trusted on the target machine for sideloading.
- If `CascadiaPackage_TemporaryKey.pfx` exists in `src\cascadia\CascadiaPackage\`, MSBuild signs the MSIX with it. The `.cer` is the public key exported from that PFX.
- Without the PFX, MSBuild sets `AppxPackageSigningEnabled=false` and produces an unsigned MSIX. The VS-generated `Add-AppDevPackage.ps1` handles developer license setup in that case.

---

## 2. Self-Extracting EXE Installer (Unpackaged / Portable)

Built by `build\scripts\New-WtaLocalInstaller.ps1`. Creates a portable distribution with `WindowsTerminal.exe`, `wta.exe`, `wtcli.exe`, and prompt templates — no MSIX, no package identity.

### Prerequisites

- Everything from the MSIX build above
- Rust toolchain (`cargo`, `rustup`) with the target platform installed
- A pre-built Terminal MSIX (from step 1 above, or from a prior VS F5)

### Build command

```powershell
# Full build (builds both Terminal MSIX and WTA from source):
.\build\scripts\New-WtaLocalInstaller.ps1 -Platform x64 -Configuration Release -BuildTerminal

# Using an existing MSIX (skips Terminal rebuild):
.\build\scripts\New-WtaLocalInstaller.ps1 -Platform x64 -Configuration Release

# Skip WTA rebuild too (use a pre-built wta.exe):
.\build\scripts\New-WtaLocalInstaller.ps1 -Platform x64 -Configuration Release `
    -SkipWtaBuild -WtaExePath wta\target\x86_64-pc-windows-msvc\release\wta.exe
```

### Parameters

| Parameter | Default | Description |
|-----------|---------|-------------|
| `-Platform` | `ARM64` | Target arch: `x64`, `ARM64`, `x86` |
| `-Configuration` | `Debug` | `Debug` or `Release` |
| `-Destination` | `artifacts\local-installer` | Output directory |
| `-BuildTerminal` | (off) | Build Terminal MSIX from source before packaging |
| `-SkipWtaBuild` | (off) | Skip Rust build; requires `-WtaExePath` |
| `-WtaExePath` | (auto) | Path to pre-built `wta.exe` |
| `-TerminalMsix` | (auto-detect) | Override path to Terminal MSIX |
| `-XamlAppx` | (auto-detect) | Override path to XAML dependency |

### What it does

1. Locates (or builds) the Terminal MSIX and XAML dependency
2. Runs `New-UnpackagedTerminalDistribution.ps1` to extract the MSIX into a portable layout
3. Builds `wta.exe` (Rust, release, static CRT) for the target platform
4. Injects `wta.exe`, `wtcli.exe`, and prompt templates into the layout
5. Creates `payload.zip` from the layout
6. Builds the Rust bootstrap EXE (`installer\bootstrap\`)
7. Assembles a self-extracting EXE: bootstrap + `install.cmd` + `install-local-terminal.ps1` + `payload.zip`

### Output

```
artifacts\local-installer\agentic-terminal-<version>-<arch>-<config>-setup.exe
```

### Install on target machine

Just run the `.exe`. It self-extracts and launches `install.cmd`, which calls `install-local-terminal.ps1`.

Install location: `%LOCALAPPDATA%\Programs\AgenticTerminal`

Options (pass to `install.cmd`): `/quiet`, `/nopath`, `/noshortcuts`

---

## Quick reference

| Goal | Command |
|------|---------|
| Build MSIX only (for F5 dev) | `tools\razzle.cmd && bcz` |
| Build MSIX ZIP for distribution | Build MSIX → assemble ZIP (see above) |
| Build self-extracting installer | `.\build\scripts\New-WtaLocalInstaller.ps1 -Platform x64` |
| Build everything from scratch | `New-WtaLocalInstaller.ps1 -Platform x64 -BuildTerminal` |

## Key files

| File | Purpose |
|------|---------|
| `build/scripts/New-WtaLocalInstaller.ps1` | Self-extracting EXE builder |
| `build/scripts/New-UnpackagedTerminalDistribution.ps1` | Extracts MSIX into portable layout |
| `installer/bootstrap/` | Rust self-extracting bootstrap |
| `installer/install-local-terminal.ps1` | Unpackaged installer script |
| `installer/install.cmd` | CMD wrapper for the installer |
| `artifacts/local-installer/` | Build output (gitignored) |
