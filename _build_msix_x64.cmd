@echo off
cd /d "%~dp0"
set MSBUILD="C:\Program Files\Microsoft Visual Studio\2022\Enterprise\MSBuild\Current\Bin\MSBuild.exe"
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
%MSBUILD% src\cascadia\TerminalSettingsModel\Microsoft.Terminal.Settings.ModelLib.vcxproj %COMMON% >> _build_msix_x64.log 2>&1
if %ERRORLEVEL% NEQ 0 (
    echo Settings Model build failed: %ERRORLEVEL%
    exit /b %ERRORLEVEL%
)

rem Build Settings Editor next (generates XBF files)
%MSBUILD% src\cascadia\TerminalSettingsEditor\Microsoft.Terminal.Settings.Editor.vcxproj %COMMON% >> _build_msix_x64.log 2>&1
if %ERRORLEVEL% NEQ 0 (
    echo Settings Editor build failed: %ERRORLEVEL%
    exit /b %ERRORLEVEL%
)

rem Now build the full package
%MSBUILD% src\cascadia\CascadiaPackage\CascadiaPackage.wapproj %COMMON% /p:GenerateAppxPackageOnBuild=true /p:AppxBundle=Never >> _build_msix_x64.log 2>&1
set BUILD_EXIT=%ERRORLEVEL%
echo Exit code: %BUILD_EXIT%
exit /b %BUILD_EXIT%
