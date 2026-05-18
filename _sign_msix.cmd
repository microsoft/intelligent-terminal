@echo off
cd /d "%~dp0"
set SIGNTOOL="C:\Program Files (x86)\Windows Kits\10\bin\10.0.26100.0\x64\signtool.exe"
%SIGNTOOL% sign /fd SHA256 /p "" /f cert\IntelligentTerminalDev.pfx "src\cascadia\CascadiaPackage\AppPackages\CascadiaPackage_0.7.0.16_x64_Test\CascadiaPackage_0.7.0.16_x64.msix"
if %ERRORLEVEL% NEQ 0 exit /b %ERRORLEVEL%
%SIGNTOOL% sign /fd SHA256 /p "" /f cert\IntelligentTerminalDev.pfx "src\cascadia\CascadiaPackage\AppPackages\CascadiaPackage_0.7.0.16_ARM64_Test\CascadiaPackage_0.7.0.16_ARM64.msix"
echo Exit code: %ERRORLEVEL%
