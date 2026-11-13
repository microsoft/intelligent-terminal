@echo off
rem Copyright (c) Microsoft Corporation.
rem Licensed under the MIT license.
echo PROFILE-RESUME-ARGS %*
pwsh.exe -NoLogo -NoProfile -File "%~dp0Mock-NativeProfileResume.ps1"
pause
