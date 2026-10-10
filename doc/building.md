
# How to build OpenConsole

This repository's dependencies come from NuGet, vcpkg, and the vendored `oss/` tree, and are restored automatically by the build.

OpenConsole.slnx may be built from within Visual Studio or from the command-line using a set of convenience scripts & tools in the **/tools** directory:

When using Visual Studio, be sure to set up the path for code formatting. To download the required clang-format.exe file, follow one of the building instructions below and run:
```powershell
Import-Module .\tools\OpenConsole.psm1
Set-MsBuildDevEnvironment
Get-Format
```
After, go to Tools > Options > Text Editor > C++ > Formatting and check "Use custom clang-format.exe file" in Visual Studio and choose the clang-format.exe in the repository at /packages/clang-format.win-x86.10.0.0/tools/clang-format.exe by clicking "browse" right under the check box.

### Building in PowerShell

```powershell
Import-Module .\tools\OpenConsole.psm1
Set-MsBuildDevEnvironment
Invoke-OpenConsoleBuild
```

There are a few additional exported functions (look at their documentation for further details):

- `Invoke-OpenConsoleBuild` - builds the solution. Can be passed msbuild arguments.
- `Invoke-OpenConsoleTests` - runs the various tests. Will run the unit tests by default.
- `Start-OpenConsole` - starts Openconsole.exe from the output directory. x64 is run by default.
- `Debug-OpenConsole` - starts Openconsole.exe and attaches it to the default debugger. x64 is run by default.
- `Invoke-CodeFormat` - uses clang-format to format all c++ files to match our coding style.

### Building in Cmd

```shell
.\tools\razzle.cmd
bcz
```

There are also scripts for running the tests:
- `runut.cmd` - run the unit tests
- `runft.cmd` - run the feature tests
- `runuia.cmd` - run the UIA tests
- `runformat` - uses clang-format to format all c++ files to match our coding style.

## Running & Debugging

To debug the Windows Terminal in VS, right click on `CascadiaPackage` (in the Solution Explorer) and go to properties. In the Debug menu, change "Application process" and "Background task process" to "Native Only".

You should then be able to build & debug the Terminal project by hitting <kbd>F5</kbd>.

> 👉 You will _not_ be able to launch the Terminal directly by running the WindowsTerminal.exe. For more details on why, see [#926](https://github.com/microsoft/terminal/issues/926), [#4043](https://github.com/microsoft/terminal/issues/4043)

## Configuration Types

Openconsole has three configuration types:

- Debug
- Release
- AuditMode

AuditMode is an experimental mode that enables some additional static analysis from CppCoreCheck.

## Updating Nuget package references - Globally versioned
Most Nuget package references in this project are centralized in a single configuration so that there is a single canonical version for everything.  This canonical version is restored before builds by the build pipeline, environment initialization scripts, or Visual Studio (as appropriate).

The canonical version numbers are defined in dep/nuget/packages.config.  That defines what will be downloaded by nuget.exe.  Most Nuget packages also have a .props and/or .targets file that must be imported by every project that consumes it.  Those import statements are consolidated in:
- src/common.nugetversions.props
- src/common.nugetversions.targets

When a globally managed version changes all three of those files must be changed in unison.

## Updating Nuget package references - Locally versioned
Certain Nuget package references in this project, like `Microsoft.UI.Xaml`, must be updated outside of the Visual Studio NuGet package manager. This can be done using the snippet below.
> Note that to run this snippet, you need to use WSL as the command uses `sed`.
To update the version of a given package, use the following snippet

`git grep -z -l $PackageName | xargs -0 sed -i -e 's/$OldVersionNumber/$NewVersionNumber/g'`

where:
- `$PackageName` is the name of the package, e.g. Microsoft.UI.Xaml
- `$OldVersionNumber` is the version number currently used, e.g. 2.4.0-prerelease.200506002
- `$NewVersionNumber` is the version number you want to migrate to, e.g. 2.5.0-prerelease.200812002

Example usage:

`git grep -z -l Microsoft.UI.Xaml | xargs -0 sed -i -e 's/2.4.0-prerelease.200506002/2.5.0-prerelease.200812002/g'`

## Using .nupkg files instead of downloaded Nuget packages
If you want to use .nupkg files instead of the downloaded Nuget package, you can do this with the following steps:

1. Open the Nuget.config file and uncomment line 8 ("Static Package Dependencies")
2. Create the folder /dep/packages
3. Put your .nupkg files in /dep/packages
4. If you are using different versions than those already being used, you need to update the references as well. How to do that is explained under "Updating Nuget package references".


## Building the Terminal package from the commandline

The Terminal is bundled as an `.msix`, which is produced by the `CascadiaPackage.wapproj` project. To build that project from the commandline, you can run the following (from a window you've already run `tools\razzle.cmd` in):

```cmd
"%msbuild%" "%OPENCON%\OpenConsole.slnx" /p:Configuration=%_LAST_BUILD_CONF% /p:Platform=%ARCH% /p:AppxSymbolPackageEnabled=false /t:Terminal\CascadiaPackage /m
```

This generates an `msix`; it does not install it. For repeatable same-version
Debug/Dev deployment, use [Loose Debug/Dev deployment](#loose-debugdev-deployment)
below rather than uninstalling and unpacking the MSIX into another layout.
For signing and distributing an MSIX, see [Building Installers](building-installer.md);
that distribution workflow is separate from the loose Dev inner loop.

### Loose Debug and Dev deployment

After building the package, deploy the canonical Intelligent Terminal Dev layout:

```powershell
.\build\scripts\Invoke-IntelligentTerminalDebugDeployment.ps1 `
    -AppxRecipePath src\cascadia\CascadiaPackage\bin\x64\Debug\CascadiaPackage.build.appxrecipe
```

The script uses Visual Studio's `DeployAppRecipe.exe`. With the **same identity,
version, and registered layout**, it updates changed binaries and can re-register
manifest changes without uninstalling. Do not automatically bump the version or
remove the package between iterations; settings are retained. Downgrades remain
blocked. Verify the deployed binary hashes before testing.

Plain `Add-AppxPackage -Register` is not equivalent: Windows can reject a changed
same-version development manifest. This behavior is for loose Dev deployment,
not Store/MSIX upgrades. The script refuses another registered layout; follow
[the worktree guide](dev-worktree-package.md) for isolated packages.


### Elevated Intelligent Terminal agent integration

The Terminal protocol uses classic COM interfaces marshaled by
`OpenConsoleProxy.dll`. Both normal and elevated `WindowsTerminal.exe` and
`wtcli.exe` explicitly load their local DLL and register its factory for all
eight protocol and handoff interfaces inside their own processes,
without writing global COM registry entries. Keep `OpenConsoleProxy.dll` beside both executables;
the loader verifies the loaded path and restricts dependency searches to its
directory and system DLL directories. Packaged execution requires the current
package's original installation path. Unpackaged Dev and actually elevated
processes may load their own executable's sibling DLL; other package API errors
do not enable that exception. No additional DLL signing requirement applies to Dev.

An elevated shell outside the package can use the already-running Terminal
factory when packaged class activation returns `REGDB_E_CLASSNOTREG`. This
lookup does not launch another Terminal and does not bypass COM integrity-level
isolation. Agent hooks continue to use the existing-only connection path.

If the agent bar stays at its default title while chat works, inspect helper
logs for `wtcli publish failed` and event-listener errors. A proxy/stub
initialization error from `wtcli` identifies a missing or unusable adjacent DLL;
`E_NOINTERFACE` during connection indicates custom-interface marshaling failed.

The `ProtocolMarshalingTests` unit tests launch isolated native probes to test
all eight interface registrations and missing adjacent DLL failures without requiring
elevation. The scoped facade also retains its normal-process no-op gate; production
uses the shared all-mode registration instead. After building `TerminalApp.UnitTests.vcxproj`, run:

```powershell
& .\bin\x64\Debug\UnitTests_TerminalApp\TE.exe `
    .\bin\x64\Debug\UnitTests_TerminalApp\Terminal.App.Unit.Tests.dll `
    '/name:*ProtocolMarshalingTests*'
```

### Are you seeing `DEP0700: Registration of the app failed`?

Once in a blue moon, I get a `DEP0700: Registration of the app failed.
[0x80073CF6] error 0x80070020: Windows cannot register the package because of an
internal error or low memory.` when trying to deploy in VS. For us, that can
happen if the `OpenConsoleProxy.dll` gets locked up, in use by some other
terminal package.

Doing the equivalent command in powershell can give us more info:

```pwsh
Add-AppxPackage -register "Z:\dev\public\OpenConsole\src\cascadia\CascadiaPackage\bin\x64\Debug\AppX\AppxManifest.xml"
```

That'll suggest `NOTE: For additional information, look for [ActivityId]
dbf551f1-83d0-0007-43e7-9cded083da01 in the Event Log or use the command line
Get-AppPackageLog -ActivityID dbf551f1-83d0-0007-43e7-9cded083da01`. So do that:

```pwsh
Get-AppPackageLog -ActivityID dbf551f1-83d0-0007-43e7-9cded083da01
```

which will give you a lot of info. In my case, that revealed that the platform
couldn't delete the packaged com entries. The key line was: `AppX Deployment
operation failed with error 0x0 from API Logging data because access was denied
for file:
C:\ProgramData\Microsoft\Windows\AppRepository\Packages\WindowsTerminalDev_0.0.1.0_x64__8wekyb3d8bbwe,
user SID: S-1-5-18`

Take that path, and
```pwsh
sudo start C:\ProgramData\Microsoft\Windows\AppRepository\Packages\WindowsTerminalDev_0.0.1.0_x64__8wekyb3d8bbwe
```

(use `sudo`, since the path is otherwise locked down). From there, go into the
`PackagedCom` folder, and open [File
Locksmith](https://learn.microsoft.com/en-us/windows/powertoys/file-locksmith)
(or Process Explorer, if you're more familiar with that) on
`OpenConsoleProxy.dll`. Just go ahead and immediately re-launch it as admin,
too. That should list off a couple terminal processes that are just hanging
around. Go ahead and end them all. You should be good to deploy again after
that.
