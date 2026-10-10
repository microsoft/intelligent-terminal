# Test a worktree with its own Dev package

Use this opt-in workflow when another worktree owns the normal Dev package.
It gives your worktree a separate installed app and settings folder.
**Ordinary Dev/Store builds and pipelines do not change.**

Changing only the package name is not enough: Dev apps also share a
single-instance window name. This workflow gives each app its own window name
and Terminal server CLSID. The existing package-local proxy DLL fix is unchanged;
using the same server CLSID across these apps was not tested.

## 1. Choose two values

Keep these stable for this worktree and save them in session artifacts:

| Value | Choose |
| --- | --- |
| `WORKTREE_TOKEN` | 1-21 ASCII letters/digits, starting with a letter; for example `wt8e31a94d0c22` |
| `PROTOCOL_CLSID` | A GUID from `[guid]::NewGuid().ToString('D')` |

The package name is `IntelligentTerminal.Worktree.<WORKTREE_TOKEN>`.
Its 29-character prefix leaves 21 characters within Windows' 50-character limit.
The package family name (PFN) is `<package name>_rd9vj3e6a2mbr`.
If that PFN is already registered elsewhere, choose a different token.

## 2. Apply local edits

Back up the original files first, preserving any existing feature edits.
**Never commit these substitutions. Restore only your edits, even if testing fails.**

Expand `build\templates\Package-DevWorktree.appxmanifest.template` into
`src\cascadia\CascadiaPackage\Package-Dev.appxmanifest`:

| Placeholder | Replace with |
| --- | --- |
| `__WORKTREE_TOKEN__` | Your token |
| `__PROTOCOL_CLSID__` | Your GUID, without braces |
| `__DEV_PACKAGE_VERSION__` | The original Dev manifest version |
| `__DEV_PACKAGE_RESOURCES__` | The original `<Resources>` element's inner XML |

Parse the result as XML; no placeholders may remain. For redeployment, use a
version newer than this worktree's installed version, not an uninstall.

Make these additional **Dev-only** edits:

| File | Local substitution |
| --- | --- |
| `src\cascadia\WindowsTerminal\WindowEmperor.cpp` | Change the window prefix `Windows Terminal Dev` to `Windows Terminal Dev <WORKTREE_TOKEN>`; leave the unpackaged AUMID unchanged |
| `src\cascadia\WindowsTerminal\TerminalProtocolComServer.h` | Set the Dev `__CLSID_TerminalProtocolServer` to your unbraced GUID |
| `test\e2e\ItE2E\Private\Paths.ps1` | Set `ItKnownFamilies.Dev` to your PFN. In `ItFamilyBrand`, replace the key `IntelligentTerminal_rd9vj3e6a2mbr` with your PFN, keeping its value `'Dev'`. Set `ItBrandClsids.Dev` to your braced GUID |
| `test\e2e\ItE2E\Public\Harness.ps1` | In `Start-ItCreatedDevTerminal`, use your PFN guard and `wtai-<WORKTREE_TOKEN>.exe` alias |

Do not remove ownership checks or change Release/Preview/Canary branches.

## 3. Build and install

Build Debug with Dev branding from this worktree using the
[normal build instructions](../AGENTS.md#terminal). Build WTA when required.
The local edits must be present during compilation and packaging.

Extract the **exact MSIX reported by the build** into a fresh private directory
using Windows SDK `MakeAppx.exe`. Keep that directory outside mutable build output:

```powershell
& $makeappx unpack /p $builtMsix /d $privateLayout
if ($LASTEXITCODE -ne 0) { throw 'Package extraction failed.' }
# Verify the extracted identity, publisher, version, architecture and GUID first.
Add-AppxPackage -Register (Join-Path $privateLayout 'AppxManifest.xml')
```

Use bounded waits. Do not edit the extracted manifest or use
`-DisableDevelopmentMode` for an unsigned Dev layout. For replacement, require an
inactive package, extract into another fresh directory, and keep the old layout
until registration succeeds. Leave the ordinary deployment script untouched.

Verify the exact PFN's installed directory and binary hashes against this build.
Never uninstall normal Dev/Store, close another worktree's processes, or copy its settings.

## 4. Pin the test and CLI paths

Use the existing E2E harness with your exact PFN and a separate artifact folder:

```powershell
$env:ITE2E_PACKAGE = '<your PFN>'
$env:ITE2E_ARTIFACT_ROOT = '<this run artifact folder>'
Import-Module .\test\e2e\ItE2E\ItE2E.psd1 -Force
$app = Resolve-ItApp -Package $env:ITE2E_PACKAGE
$env:PATH = "$($app.InstallLocation);$env:PATH"
$env:WT_WTCLI_PATH = $app.WtcliPath # WTA's client override
$env:WTCLI_PATH = $app.WtcliPath   # Hook bridge path
$env:WT_COM_CLSID = '{<PROTOCOL_CLSID>}'
```

Also put `PATH`, `WT_WTCLI_PATH`, and `WTCLI_PATH` in the test profile's
`environment` settings before launch: app activation can use a different
environment from the runner. The host injects its own `WT_COM_CLSID` into panes.
Binary location is not package identity. For runner-side session queries, pass
the selected package's master pipe explicitly:

```powershell
$pipeFile = Join-Path $app.LocalStateDir 'IntelligentTerminal\master-pipe.txt'
$masterPipe = (Get-Content -LiteralPath $pipeFile -Raw -ErrorAction Stop).Trim()
if (-not $masterPipe) { throw 'Sandbox master pipe is missing.' }
Invoke-Wta -App $app -Arguments @('sessions', 'list', '--json',
    '--master', $masterPipe, '--include-status', '--origin', 'all')
```

Read that file only after the owned app starts; verify the pipe belongs to its
WTA master. For suites using `Get-WtSessions` / `Get-SessionListJson`, temporarily
add this `--master` argument to the `sessions list` call in
`test\e2e\ItE2E\Public\Sessions.ps1`, and restore it afterward. `sessions refresh`
also accepts `--master`. Other stateful commands need a verified identity-aware
launch; do not claim they work from a direct `$app.WtaPath` invocation.
C++ already launches its co-located `wta.exe`.
Do not change user-wide PATH, the registry, or normal executable aliases.

Before accepting a result, verify the app's package/path, its HWND's owning PID,
and the pane's injected GUID. In both runner and pane, the first `Get-Command
wtcli.exe` / `Get-Command wta.exe` result must point into this package. This checks
binary discovery, not identity: explicitly route stateful session queries. Use
`Select-Object -First 1`, run both CLIs, and check their responses against the
selected pane. Give evidence a fresh run ID; do not accept stale result files.

## 5. Restore and keep the boundaries clear

Restore your local substitutions before committing. Refresh the last-write
timestamps of **all restored build inputs**: `Package-Dev.appxmanifest`,
`WindowEmperor.cpp`, and `TerminalProtocolComServer.h`. Copying old backups can
otherwise leave the sandbox manifest or compiled window name/CLSID cached.
Run the normal full incremental build with project dependencies enabled, then
verify the generated ordinary Dev identity and the host's injected Dev CLSID.
Inspect the staged diff for your token/GUID. Keep the registered private layout
intact; uninstalling this sandbox and preserving its useful data is a separate
cleanup decision. Stop only processes whose test ownership was established.

**Parallelize per operation.** HWND-scoped UIA reads and supported invoke/value/
selection actions can target different apps. Shared foreground keyboard/mouse,
clipboard, policy, and agent configuration still need serialization. Inspect
each suite's package guards and shared-state use before running it.

The sandbox does not support jump-list launching: internal shell links still
expect the normal `wtai.exe` alias, not this template's tokenized alias.
The template also omits Explorer menus, startup tasks, default-terminal handoff,
and global proxy registrations. Validate these with ordinary Dev, not this sandbox.
Windows PATH checks do not prove WSL discovery or full-suite parallel safety.
