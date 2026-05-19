@echo off
cd /d "%~dp0"
call tools\razzle.cmd
if errorlevel 1 (
  echo RAZZLE_FAILED
  exit /b 1
)
call bcz no_clean
exit /b %errorlevel%
