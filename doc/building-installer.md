# Building Installers

There are two installer types for distributing Intelligent Terminal.

## 1. MSIX ZIP Installer (Packaged)

A ZIP containing a dev certificate, signed MSIX package, XAML dependency, install script, and FRE reset helper. Recipients run `Install-Msix.ps1` to sideload the packaged app.

### Output structure

```
intelligent-terminal-<version>-<arch>-msix.zip
├── IntelligentTerminalDev.cer                    # Dev signing certificate
├── CascadiaPackage_<version>_<arch>.msix         # Signed Terminal MSIX
├── Dependencies/
│   └── Microsoft.UI.Xaml.2.8.appx                # XAML framework dependency
├── Install-Msix.ps1                              # Imports cert + installs packages
└── fre-test-reset.ps1                            # Resets First Run Experience for repeat testing
```

### Prerequisites

- Visual Studio 2022 Enterprise with C++ desktop & UWP workloads
- Windows 10 SDK (10.0.22621.0+)
- Rust toolchain (`cargo`, `rustup`) with both targets:
  ```
  rustup target add x86_64-pc-windows-msvc
  rustup target add aarch64-pc-windows-msvc
  ```

---

### TL;DR (typical version bump + ship)

Five lines, in order. Step details below.

```powershell
# 0. Bump manifest + _sign_msix.cmd to the new version
# 1. (skipped — cert is committed)
# 2. cargo build --release --target {x86_64,aarch64}-pc-windows-msvc --manifest-path tools/wta/Cargo.toml
# 3. .\_build_msix_x64.cmd   AND THEN   .\_build_msix_arm64.cmd      # serial — see note
# 4. .\_sign_msix.cmd
# 5. powershell -File build\scripts\assemble-msix-zip.ps1 -Version 0.7.0.X -Arch x64
#    powershell -File build\scripts\assemble-msix-zip.ps1 -Version 0.7.0.X -Arch ARM64
```

The driver scripts ([`_build_msix_x64.cmd`](../_build_msix_x64.cmd), [`_build_msix_arm64.cmd`](../_build_msix_arm64.cmd), [`_sign_msix.cmd`](../_sign_msix.cmd)) live at the repo root and encode workarounds the bare MSBuild invocation doesn't handle — see Step 3 for what they actually do.

---

### Step 0: Bump the version

**Edit two files**, both with the same version string:

1. `src\cascadia\CascadiaPackage\Package-Dev.appxmanifest`:
   ```xml
   <Identity Name="IntelligentTerminal" Publisher="CN=Intelligent Terminal Dev" Version="0.7.0.X" />
   ```
2. [`_sign_msix.cmd`](../_sign_msix.cmd) — the version is hardcoded in two signtool path lines. Search-replace the old version → new.

> The script doesn't read the manifest. If you forget to bump it, signtool will look for a nonexistent MSIX path and fail.

### Step 1: Dev signing certificate

We use [`cert\IntelligentTerminalDev.pfx`](../cert/IntelligentTerminalDev.pfx), committed in the repo. **Skip this step unless that file is missing.**

To regenerate from scratch (e.g., cert expired — they're valid 3 years):

```powershell
powershell -ExecutionPolicy Bypass -File build\scripts\New-DevSigningCert.ps1
```

That script produces `CascadiaPackage_TemporaryKey.pfx` (gitignored) — you'd then need to update [`_sign_msix.cmd`](../_sign_msix.cmd) to point at it, or copy/rename into `cert\IntelligentTerminalDev.pfx`. Keeping the cert committed is the simpler convention here.

### Step 2: Build `wta.exe`

```powershell
# x64
cargo build --release --target x86_64-pc-windows-msvc --manifest-path tools/wta/Cargo.toml

# ARM64 (cross-compile)
cargo build --release --target aarch64-pc-windows-msvc --manifest-path tools/wta/Cargo.toml
```

These two are independent — run them in parallel if you want.

MSBuild picks up `wta.exe` automatically from the Cargo output via a `<Content>` rule in `CascadiaPackage.wapproj`:
- x64: `tools\wta\target\x86_64-pc-windows-msvc\release\wta.exe`
- ARM64: `tools\wta\target\aarch64-pc-windows-msvc\release\wta.exe`

> **Always pass `--target` explicitly.** The wapproj's `<Content>` items prefer `tools\wta\target\<triple>\release\wta.exe` over the bare `tools\wta\target\release\wta.exe` fallback. If a stale binary exists at the explicit-target path (e.g., from a previous `--target` build), a bare `cargo build --release` writes to a different directory and MSBuild silently packages the stale one. Using `--target` for both arches keeps the two paths symmetric and avoids the trap. 0.7.0.0 and 0.7.0.1 were burned by this.

> **Always re-run cargo even if you think source didn't change.** Cargo's incremental check is fast (~seconds for a no-op) and serves as cheap insurance against the "wta source did change but I forgot" footgun that bit 0.7.0.12.

### Step 3: Build the Terminal MSIX

Use the wrapper scripts — **not** the bare MSBuild command:

```powershell
.\_build_msix_x64.cmd        # x64 must finish first
.\_build_msix_arm64.cmd      # then ARM64
```

> **Run them serially.** x64 and ARM64 share `Generated Files\Profiles_Advanced.xaml` (and other generated XAML files) under `src\cascadia\TerminalSettingsEditor\`. Parallel builds race on those files and one of them dies with `WMC9999 — being used by another process`.

[`_build_msix_x64.cmd`](../_build_msix_x64.cmd) and [`_build_msix_arm64.cmd`](../_build_msix_arm64.cmd) do four things the bare MSBuild invocation doesn't:

1. **Wipe `obj\<Platform>\Release\` and `bin\<Platform>\Release\AppX\`** before building. The wapproj has a glob-based `<Content Include="...\wt-agent-hooks\**">` rule (for the agent hook bundle); incremental MSBuild caches the resolved file list and silently drops freshly-added files. 0.7.0.5 and 0.7.0.6 shipped without `wt-agent-hooks\` because of this — every "Install hooks" click failed until we figured it out.
2. **Pre-build `Microsoft.Terminal.Settings.ModelLib.vcxproj`** so its `Microsoft.Terminal.Settings.Model.winmd` is the source of truth before any consumer (`TerminalSettingsAppAdapterLib`, etc.) calls `cppwinrt` to regenerate its WinRT projection headers. Without this, `cppwinrt` can scan a stale winmd from `bin\<Platform>\Release\<OtherProject>\` and emit projections missing newer members (e.g. `DragDropDelimiter` → `C2039` in `TerminalSettings.cpp`).
3. **Pre-build `Microsoft.Terminal.Settings.Editor.vcxproj`** to generate XBF files. Otherwise, `TerminalAppLib` starts before `AIAgents.xaml.g.h` exists and fails with `MSB3030: file not found`.
4. **`exit /b %BUILD_EXIT%`** at the end. The previous `echo Exit code: %ERRORLEVEL%` made the shell return 0 even when MSBuild failed, masking real errors as silent "successful" runs. 0.7.0.10 wasted a round on this.

#### ARM64 quirks

- **First-pass `ITerminalHandoff.h` not found**: parallel build race — `OpenConsoleProxy` generates it but `TerminalConnection` may start before it's ready. Re-run [`_build_msix_arm64.cmd`](../_build_msix_arm64.cmd) immediately and it succeeds.
- **`APPX1204: SignTool Error: The file is being used by another process`**: MSBuild's built-in auto-sign (kicked off by `<AppxPackageSigningEnabled>true</AppxPackageSigningEnabled>` inferred from PFX presence) sometimes loses a race with AV/indexer locking the freshly-produced MSIX. The MSIX is still written to disk; just run [`_sign_msix.cmd`](../_sign_msix.cmd) in Step 4 to sign it explicitly. 0.7.0.14 ARM64 hit this.
- **Missing `Dependencies\` folder**: when MSBuild's auto-sign fails as above, it also skips staging the XAML dependency. Copy it manually from a prior successful build:
  ```powershell
  Copy-Item -Recurse src\cascadia\CascadiaPackage\AppPackages\CascadiaPackage_<PRIOR>_ARM64_Test\Dependencies `
                     src\cascadia\CascadiaPackage\AppPackages\CascadiaPackage_<NEW>_ARM64_Test\Dependencies
  ```
  The XAML appx is identical across our builds.

MSBuild outputs to:
```
src\cascadia\CascadiaPackage\AppPackages\CascadiaPackage_<version>_<arch>_Test\
├── CascadiaPackage_<version>_<arch>.msix      # may be unsigned if auto-sign raced
└── Dependencies\<arch>\Microsoft.UI.Xaml.2.8.appx
```

#### Bare MSBuild commands (for reference)

If you ever need to skip the wrapper:

```powershell
$env:MSBUILD = "C:\Program Files\Microsoft Visual Studio\2022\Enterprise\MSBuild\Current\Bin\MSBuild.exe"
$env:REPO = (Get-Location).Path

& $env:MSBUILD src\cascadia\CascadiaPackage\CascadiaPackage.wapproj `
    /p:Platform=x64 /p:Configuration=Release /p:WindowsTerminalBranding=Dev `
    /p:GenerateAppxPackageOnBuild=true /p:AppxBundle=Never `
    /p:SolutionDir="$env:REPO\" /m /nologo
```

You will hit the issues listed above. Use the wrappers.

### Step 4: Sign the MSIXs

```powershell
.\_sign_msix.cmd
```

[`_sign_msix.cmd`](../_sign_msix.cmd) signs both x64 and ARM64 with `cert\IntelligentTerminalDev.pfx` (SHA256, empty password). Use this even if MSBuild's auto-sign succeeded — it's idempotent and ensures both arches end up with our cert specifically.

### Step 5: Assemble the ZIPs

```powershell
powershell -File build\scripts\assemble-msix-zip.ps1 -Version 0.7.0.X -Arch x64
powershell -File build\scripts\assemble-msix-zip.ps1 -Version 0.7.0.X -Arch ARM64
```

Output: `artifacts\local-installer\intelligent-terminal-<version>-<arch>-msix.zip`

The script ([`build\scripts\assemble-msix-zip.ps1`](../build/scripts/assemble-msix-zip.ps1)) copies five things into each ZIP:
- Signed MSIX from `src\cascadia\CascadiaPackage\AppPackages\CascadiaPackage_<version>_<arch>_Test\`
- `Dependencies\<arch>\Microsoft.UI.Xaml.2.8.appx`
- `artifacts\local-installer\IntelligentTerminalDev.cer`
- [`installer\Install-Msix.ps1`](../installer/Install-Msix.ps1)
- [`tools\fre-test-reset.ps1`](../tools/fre-test-reset.ps1) — FRE reset helper for repeat testing

### Install on target machine

```powershell
# Extract the ZIP, then run (no admin needed if cert is already trusted):
powershell -ExecutionPolicy Bypass -File .\Install-Msix.ps1
```

`Install-Msix.ps1` does three things:
1. Removes any old unpackaged install (`%LOCALAPPDATA%\Programs\IntelligentTerminal`)
2. Imports `IntelligentTerminalDev.cer` into the Trusted People store — **only if not already trusted** (this step requires admin; subsequent installs skip it)
3. Installs the XAML dependency and the Terminal MSIX via `Add-AppxPackage` (per-user, no elevation needed)

To repeat-test the FRE, run [`fre-test-reset.ps1`](../tools/fre-test-reset.ps1) from the same extracted ZIP and pick `[1]` (just the FRE flag) or `[A]` (full reset including Copilot CLI uninstall).

### Certificate notes

- `cert\IntelligentTerminalDev.pfx` is **committed** in this repo. Don't rotate it casually — installed builds on every dev machine trust this specific cert.
- The same cert signs both x64 and ARM64 MSIXs; no need to regenerate per-arch.
- Valid for 3 years from creation. Regenerate with [`build\scripts\New-DevSigningCert.ps1`](../build/scripts/New-DevSigningCert.ps1) when it expires and re-commit.

---

## 2. Self-Extracting EXE Installer (Unpackaged / Portable)

Built by [`build\scripts\New-WtaLocalInstaller.ps1`](../build/scripts/New-WtaLocalInstaller.ps1). Creates a portable distribution with `WindowsTerminal.exe`, `wta.exe`, `wtcli.exe`, and prompt templates — no MSIX, no package identity.

### Prerequisites

- Everything from the MSIX build above
- Rust toolchain (`cargo`, `rustup`) with the target platform installed
- A pre-built Terminal MSIX (from Step 3 above, or from a prior VS F5)

### Build command

```powershell
# Full build (builds both Terminal MSIX and WTA from source):
.\build\scripts\New-WtaLocalInstaller.ps1 -Platform x64 -Configuration Release -BuildTerminal

# Using an existing MSIX (skips Terminal rebuild):
.\build\scripts\New-WtaLocalInstaller.ps1 -Platform x64 -Configuration Release

# Skip WTA rebuild too (use a pre-built wta.exe):
.\build\scripts\New-WtaLocalInstaller.ps1 -Platform x64 -Configuration Release `
    -SkipWtaBuild -WtaExePath tools\wta\target\x86_64-pc-windows-msvc\release\wta.exe
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
2. Runs [`New-UnpackagedTerminalDistribution.ps1`](../build/scripts/New-UnpackagedTerminalDistribution.ps1) to extract the MSIX into a portable layout
3. Builds `wta.exe` (Rust, release, static CRT) for the target platform
4. Injects `wta.exe`, `wtcli.exe`, and prompt templates into the layout
5. Creates `payload.zip` from the layout
6. Builds the Rust bootstrap EXE ([`installer\bootstrap\`](../installer/bootstrap/))
7. Assembles a self-extracting EXE: bootstrap + [`install.cmd`](../installer/install.cmd) + [`install-local-terminal.ps1`](../installer/install-local-terminal.ps1) + `payload.zip`

### Output

```
artifacts\local-installer\intelligent-terminal-<version>-<arch>-<config>-setup.exe
```

### Install on target machine

Just run the `.exe`. It self-extracts and launches `install.cmd`, which calls `install-local-terminal.ps1`.

Install location: `%LOCALAPPDATA%\Programs\IntelligentTerminal`

Options (pass to `install.cmd`): `/quiet`, `/nopath`, `/noshortcuts`

---

## Quick reference

| Goal | Command |
|------|---------|
| Generate dev cert (one-time / expired) | [`powershell -File build\scripts\New-DevSigningCert.ps1`](../build/scripts/New-DevSigningCert.ps1) |
| Build wta (x64) | `cargo build --release --target x86_64-pc-windows-msvc --manifest-path tools/wta/Cargo.toml` |
| Build wta (ARM64) | `cargo build --release --target aarch64-pc-windows-msvc --manifest-path tools/wta/Cargo.toml` |
| Build MSIX (x64) | [`.\_build_msix_x64.cmd`](../_build_msix_x64.cmd) |
| Build MSIX (ARM64) | [`.\_build_msix_arm64.cmd`](../_build_msix_arm64.cmd) |
| Sign both MSIXs | [`.\_sign_msix.cmd`](../_sign_msix.cmd) |
| Assemble MSIX ZIP | [`powershell -File build\scripts\assemble-msix-zip.ps1 -Version X.X.X.X -Arch x64`](../build/scripts/assemble-msix-zip.ps1) |
| Build self-extracting EXE | [`.\build\scripts\New-WtaLocalInstaller.ps1 -Platform x64 -Configuration Release`](../build/scripts/New-WtaLocalInstaller.ps1) |

## Key files

| File | Purpose |
|------|---------|
| [`_build_msix_x64.cmd`](../_build_msix_x64.cmd) | Wrapper around MSBuild for x64 with all the workarounds |
| [`_build_msix_arm64.cmd`](../_build_msix_arm64.cmd) | Same for ARM64 |
| [`_sign_msix.cmd`](../_sign_msix.cmd) | Signs both arches with the committed `cert\IntelligentTerminalDev.pfx` |
| [`cert\IntelligentTerminalDev.pfx`](../cert/) | Committed dev signing cert (3-year validity) |
| [`build\scripts\New-DevSigningCert.ps1`](../build/scripts/New-DevSigningCert.ps1) | Generates PFX + CER for dev signing (only when expired) |
| [`build\scripts\assemble-msix-zip.ps1`](../build/scripts/assemble-msix-zip.ps1) | Assembles the MSIX ZIP from build outputs |
| [`installer\Install-Msix.ps1`](../installer/Install-Msix.ps1) | Install script included in the MSIX ZIP |
| [`tools\fre-test-reset.ps1`](../tools/fre-test-reset.ps1) | FRE reset helper bundled in the ZIP |
| [`build\scripts\New-WtaLocalInstaller.ps1`](../build/scripts/New-WtaLocalInstaller.ps1) | Self-extracting EXE builder |
| [`build\scripts\New-UnpackagedTerminalDistribution.ps1`](../build/scripts/New-UnpackagedTerminalDistribution.ps1) | Extracts MSIX into portable layout |
| [`installer\bootstrap\`](../installer/bootstrap/) | Rust self-extracting bootstrap |
| [`installer\install-local-terminal.ps1`](../installer/install-local-terminal.ps1) | Unpackaged installer script |
| [`installer\install.cmd`](../installer/install.cmd) | CMD wrapper for the unpackaged installer |
| `artifacts\local-installer\` | Build output (gitignored) |
