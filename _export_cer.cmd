@echo off
cd /d C:\Users\kaitao\codes\agentic-terminal
if not exist artifacts\local-installer mkdir artifacts\local-installer
powershell -NoProfile -Command "$pfx = [System.Security.Cryptography.X509Certificates.X509Certificate2]::new('cert\IntelligentTerminalDev.pfx'); $cer = $pfx.Export([System.Security.Cryptography.X509Certificates.X509ContentType]::Cert); [System.IO.File]::WriteAllBytes('artifacts\local-installer\IntelligentTerminalDev.cer', $cer); Write-Host 'CER exported'"
