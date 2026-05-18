@echo off
cd /d "%~dp0"

rem Wipe ARM64 Release intermediates so glob Content items (wt-agent-hooks\**)
rem get re-evaluated; otherwise an incremental MSIX build silently drops
rem freshly-added files. See _build_msix_x64.cmd for the long-form note.
if exist "src\cascadia\CascadiaPackage\obj\ARM64\Release" rmdir /s /q "src\cascadia\CascadiaPackage\obj\ARM64\Release"
if exist "src\cascadia\CascadiaPackage\bin\ARM64\Release\AppX" rmdir /s /q "src\cascadia\CascadiaPackage\bin\ARM64\Release\AppX"

"C:\Program Files\Microsoft Visual Studio\2022\Enterprise\MSBuild\Current\Bin\MSBuild.exe" src\cascadia\CascadiaPackage\CascadiaPackage.wapproj /p:Platform=ARM64 /p:Configuration=Release /p:WindowsTerminalBranding=Dev /p:GenerateAppxPackageOnBuild=true /p:AppxBundle=Never /p:SolutionDir=%CD%\ /m /nologo > _build_msix_arm64.log 2>&1
set BUILD_EXIT=%ERRORLEVEL%
echo Exit code: %BUILD_EXIT%
exit /b %BUILD_EXIT%
