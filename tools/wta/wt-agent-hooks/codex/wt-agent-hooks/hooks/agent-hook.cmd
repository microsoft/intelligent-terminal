@echo off
if not defined WT_COM_CLSID exit /b 0
if not defined WT_SESSION exit /b 0
wtcli.exe agent-hook %* >nul 2>nul
exit /b 0
