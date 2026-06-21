@echo off
cd /d "%~dp0"

rem Resolve MSBuild robustly instead of hard-coding one VS edition/drive:
rem   1. honor an externally supplied MSBUILD env var (CI / custom installs);
rem   2. otherwise locate it via vswhere, which works across Community /
rem      Professional / Enterprise / Build Tools and non-default install drives.
rem The value is stored UNQUOTED and quoted at each call site below.
rem Guard the vswhere call with `if exist`: on minimal/custom installs that
rem lack vswhere.exe, calling it would print a confusing error and leave
rem MSBUILD unset; skipping it lets the clear message below handle the case.
if not defined MSBUILD (
    if exist "%ProgramFiles(x86)%\Microsoft Visual Studio\Installer\vswhere.exe" (
        for /f "usebackq delims=" %%i in (`"%ProgramFiles(x86)%\Microsoft Visual Studio\Installer\vswhere.exe" -latest -products * -requires Microsoft.Component.MSBuild -find MSBuild\**\Bin\MSBuild.exe`) do set "MSBUILD=%%i"
    )
)
if not defined MSBUILD (
    echo Could not locate MSBuild. Install the "MSBuild" VS component, or set the MSBUILD env var to your MSBuild.exe path.
    exit /b 1
)
if not exist "%MSBUILD%" (
    echo MSBUILD points to a missing file: "%MSBUILD%"
    exit /b 1
)

set SOLUTION_DIR=%CD%\
set COMMON=/p:Platform=x64 /p:Configuration=Release /p:WindowsTerminalBranding=Dev /p:SolutionDir=%SOLUTION_DIR% /m /nologo

rem Wipe the wapproj's Release intermediates so glob-based Content items
rem (like wt-agent-hooks\**) get re-evaluated. Without this, an incremental
rem MSIX build keeps the cached file list and silently drops freshly-added
rem files from the package.
if exist "src\cascadia\CascadiaPackage\obj\x64\Release" rmdir /s /q "src\cascadia\CascadiaPackage\obj\x64\Release"
if exist "src\cascadia\CascadiaPackage\bin\x64\Release\AppX" rmdir /s /q "src\cascadia\CascadiaPackage\bin\x64\Release\AppX"

rem Build Settings Model first. Its winmd is the source-of-truth for the
rem Profile / Globals WinRT projection. If we don't pin its build ahead
rem of consumer projects, cppwinrt can scan a stale older winmd elsewhere
rem and generate consumer projections missing newer members (e.g.
rem DragDropDelimiter), producing C2039 in TerminalSettingsAppAdapterLib.
"%MSBUILD%" src\cascadia\TerminalSettingsModel\Microsoft.Terminal.Settings.ModelLib.vcxproj %COMMON% >> _build_msix_x64.log 2>&1
if %ERRORLEVEL% NEQ 0 (
    echo Settings Model build failed: %ERRORLEVEL%
    exit /b %ERRORLEVEL%
)

rem Build Settings Editor next (generates XBF files)
"%MSBUILD%" src\cascadia\TerminalSettingsEditor\Microsoft.Terminal.Settings.Editor.vcxproj %COMMON% >> _build_msix_x64.log 2>&1
if %ERRORLEVEL% NEQ 0 (
    echo Settings Editor build failed: %ERRORLEVEL%
    exit /b %ERRORLEVEL%
)

rem Now build the full package
"%MSBUILD%" src\cascadia\CascadiaPackage\CascadiaPackage.wapproj %COMMON% /p:GenerateAppxPackageOnBuild=true /p:AppxBundle=Never >> _build_msix_x64.log 2>&1
set BUILD_EXIT=%ERRORLEVEL%
echo Exit code: %BUILD_EXIT%
exit /b %BUILD_EXIT%
