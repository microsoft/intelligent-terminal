@echo off
if "%WTA_COPILOT_ACP%"=="1" exit /b 0
powershell -NoProfile -NonInteractive -ExecutionPolicy Bypass -File "%~dp0send-event.ps1" %*
exit /b %ERRORLEVEL%
