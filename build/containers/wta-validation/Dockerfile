# escape=`
# Process isolation shares the host kernel; this is not hardware isolation.
FROM mcr.microsoft.com/windows/servercore@sha256:22505496dd4229dba63453ba0c6dc31c06fd3e11810e5b8e429aa6b16dab2457

SHELL ["powershell.exe", "-NoLogo", "-NoProfile", "-NonInteractive", "-Command"]
ENV CARGO_HOME="C:\cargo-cache" RUSTUP_HOME="C:\rustup" RUSTUP_TOOLCHAIN="1.93.0"
ENV PATH="C:\cargo-cache\bin;${PATH}"

# Official VS2026 Stable link: https://learn.microsoft.com/visualstudio/install/create-a-network-installation-of-visual-studio?view=visualstudio
# The bootstrapper digest was verified with MZ, Valid Microsoft Authenticode, and VS2026 product metadata.
RUN $ErrorActionPreference = 'Stop'; `
    New-Item -ItemType Directory -Path C:\setup | Out-Null; `
    Invoke-WebRequest -UseBasicParsing -TimeoutSec 180 -Uri 'https://aka.ms/vs/stable/vs_buildtools.exe' -OutFile C:\setup\vs_buildtools.exe; `
    $bytes = [IO.File]::ReadAllBytes('C:\setup\vs_buildtools.exe'); `
    if ($bytes.Length -lt 2 -or $bytes[0] -ne 0x4d -or $bytes[1] -ne 0x5a) { throw 'VS bootstrapper is not a Windows executable.' }; `
    if ((Get-FileHash C:\setup\vs_buildtools.exe -Algorithm SHA256).Hash -ne '9203cc1fa53bc4c6254723cdd4bc35e6a658253b98c90c863b0b4a962808254e') { throw 'VS bootstrapper differs from verified public download.' }; `
    $signature = Get-AuthenticodeSignature C:\setup\vs_buildtools.exe; `
    if ($signature.Status -ne 'Valid' -or $signature.SignerCertificate.Subject -notmatch '(^|,\s*)O=Microsoft Corporation(,|$)') { throw 'Invalid Microsoft installer signature.' }; `
    if ([Diagnostics.FileVersionInfo]::GetVersionInfo('C:\setup\vs_buildtools.exe').ProductVersion -ne 'Visual Studio 2026') { throw 'Unexpected Visual Studio generation.' }; `
    $process = Start-Process C:\setup\vs_buildtools.exe -ArgumentList '--quiet --wait --norestart --nocache --installPath C:\BuildTools --add Microsoft.VisualStudio.Component.VC.Tools.x86.x64 --add Microsoft.VisualStudio.Component.Windows11SDK.26100' -PassThru; `
    if (-not $process.WaitForExit(1800000)) { & taskkill.exe /PID $process.Id /T /F; throw 'VS installation exceeded 1800 seconds.' }; `
    if ($process.ExitCode -notin @(0, 3010)) { throw ('VS installation failed: ' + $process.ExitCode) }; `
    if (-not (Test-Path 'C:\BuildTools\Common7\Tools\VsDevCmd.bat') -or -not (Test-Path 'C:\Program Files (x86)\Windows Kits\10\Include\10.0.26100.0\um\Windows.h')) { throw 'Required MSVC/SDK installation is missing.' }; `
    Remove-Item C:\setup\vs_buildtools.exe

RUN $ErrorActionPreference = 'Stop'; `
    Invoke-WebRequest -UseBasicParsing -TimeoutSec 180 -Uri 'https://static.rust-lang.org/rustup/archive/1.29.1/x86_64-pc-windows-msvc/rustup-init.exe' -OutFile C:\setup\rustup-init.exe; `
    if ((Get-FileHash C:\setup\rustup-init.exe -Algorithm SHA256).Hash -ne '6f4bef66261261fcb43131be8720bab817d403a09edec7455c371974b90bdb7e') { throw 'Rustup checksum mismatch.' }; `
    $process = Start-Process C:\setup\rustup-init.exe -ArgumentList '-y --no-modify-path --profile minimal --default-host x86_64-pc-windows-msvc --default-toolchain 1.93.0' -PassThru; `
    if (-not $process.WaitForExit(900000)) { & taskkill.exe /PID $process.Id /T /F; throw 'Rust installation exceeded 900 seconds.' }; `
    if ($process.ExitCode -ne 0) { throw ('Rust installation failed: ' + $process.ExitCode) }; `
    & C:\cargo-cache\bin\rustc.exe --version --verbose; `
    if ($LASTEXITCODE -ne 0) { throw 'Rust toolchain verification failed.' }; `
    Remove-Item C:\setup -Recurse -Force

COPY Invoke-WtaContainerTests.ps1 C:\validation\Invoke-WtaContainerTests.ps1
RUN $ErrorActionPreference = 'Stop'; `
    New-Item -ItemType Directory -Path C:\validation-output | Out-Null; `
    & icacls.exe C:\validation-output /grant '*S-1-5-32-545:(OI)(CI)M'; `
    if ($LASTEXITCODE -ne 0) { throw 'Could not grant guest output access.' }; `
    & icacls.exe C:\cargo-cache /grant '*S-1-5-32-545:(OI)(CI)M'; `
    if ($LASTEXITCODE -ne 0) { throw 'Could not grant Cargo cache-root access.' }; `
    & icacls.exe C:\rustup /grant '*S-1-5-32-545:(OI)(CI)RX' /T; `
    if ($LASTEXITCODE -ne 0) { throw 'Could not grant toolchain read access.' }
WORKDIR C:\workspace
USER ContainerUser
ENTRYPOINT ["powershell.exe", "-NoLogo", "-NoProfile", "-NonInteractive", "-File", "C:\\validation\\Invoke-WtaContainerTests.ps1"]
