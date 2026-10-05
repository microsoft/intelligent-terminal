# Copyright (c) Microsoft Corporation. All rights reserved.
# Trusted entrypoint. Repository and dependency inputs must be read-only mounts.
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

if ($env:CARGO_HOME -ne 'C:\cargo-cache' -or $env:RUSTUP_HOME -ne 'C:\rustup' -or
    $env:RUSTUP_TOOLCHAIN -ne '1.93.0' -or $env:CARGO_NET_OFFLINE -ne 'true') {
    throw 'Unexpected validation environment.'
}
foreach ($name in @('GITHUB_TOKEN', 'GH_TOKEN', 'COPILOT_GITHUB_TOKEN', 'ACTIONS_RUNTIME_TOKEN')) {
    if (Test-Path "Env:$name") { throw "Unexpected credential environment: $name" }
}
if (-not (Test-Path -LiteralPath 'C:\workspace\.cargo\config.toml') -or
    -not (Test-Path -LiteralPath 'C:\workspace\tools\wta\Cargo.lock')) {
    throw 'Repository-root configuration or locked WTA manifest is missing.'
}
$version = & C:\cargo-cache\bin\rustc.exe --version --verbose
if ($LASTEXITCODE -ne 0 -or $version -notcontains 'release: 1.93.0' -or
    $version -notcontains 'host: x86_64-pc-windows-msvc') {
    throw 'Public Windows Rust 1.93.0 is required.'
}
$version
$powerShell = & 'C:\Program Files\PowerShell\7\pwsh.exe' -NoProfile -NonInteractive -Command '$PSVersionTable.PSVersion.ToString()'
if ($LASTEXITCODE -ne 0 -or $powerShell -ne '7.6.6') {
    throw 'PowerShell is required so shell-launch regression tests do not silently skip.'
}
$env:CARGO_TARGET_DIR = 'C:\validation-output'
$env:CARGO_TERM_COLOR = 'never'
Set-Location -LiteralPath 'C:\workspace'

# Keep VsDevCmd and Cargo in one CMD process; cwd preserves the static CRT config.
$command = 'call "C:\BuildTools\Common7\Tools\VsDevCmd.bat" -arch=amd64 -host_arch=amd64 -winsdk=10.0.26100.0 >nul && C:\cargo-cache\bin\cargo.exe test --locked --offline --target x86_64-pc-windows-msvc --manifest-path tools\wta\Cargo.toml'
& $env:ComSpec /d /s /c $command
exit $LASTEXITCODE
