# Copyright (c) Microsoft Corporation.
# Licensed under the MIT license.

# Run from an x64 Visual Studio developer shell after building wtcli in Debug.
[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
$null = Get-Command cl.exe -ErrorAction Stop
$repo = (Resolve-Path (Join-Path $PSScriptRoot '..\..\..\..')).Path
$output = Join-Path $repo 'obj\x64\Debug\ProtocolProxyTests'
$null = New-Item -ItemType Directory -Path $output -Force
$wil = Join-Path $repo 'packages\Microsoft.Windows.ImplementationLibrary.1.0.250325.1\include'
$idl = Join-Path $repo 'obj\x64\Debug\OpenConsoleProxy'
$proxy = (Resolve-Path (Join-Path $repo 'bin\x64\Debug\OpenConsoleProxy.dll')).Path
$null = Copy-Item -LiteralPath $proxy -Destination (Join-Path $output 'OpenConsoleProxy.dll') -Force
$source = Join-Path $PSScriptRoot 'ProtocolProxyRegistrationTests.cpp'

foreach ($brand in @('DEV', 'RELEASE', 'PREVIEW', 'CANARY')) {
    $exe = Join-Path $output "$brand.exe"
    $obj = Join-Path $output "$brand.obj"
    $buildLog = Join-Path $output "$brand-build.log"
    $arguments = "/nologo /std:c++20 /EHsc /W4 /WX /DUNICODE /D_UNICODE /DWT_BRANDING_$brand /I`"$wil`" /I`"$idl`" `"$source`" /Fe:`"$exe`" /Fo:`"$obj`" /link ole32.lib oleaut32.lib runtimeobject.lib user32.lib"
    $process = Start-Process cl.exe -ArgumentList $arguments -PassThru -NoNewWindow -RedirectStandardOutput $buildLog -RedirectStandardError "$buildLog.err"
    if (!$process.WaitForExit(120000)) {
        $process.Kill($true)
        throw "Compile timeout 120s: PID=$($process.Id), command=cl.exe $arguments; launched process tree terminated."
    }
    Get-Content $buildLog
    Get-Content "$buildLog.err"
    if ($process.ExitCode -ne 0) { throw "$brand compile failed: $($process.ExitCode)" }

    $testLog = Join-Path $output "$brand-test.log"
    $process = Start-Process $exe -ArgumentList "`"$proxy`"" -PassThru -NoNewWindow -RedirectStandardOutput $testLog -RedirectStandardError "$testLog.err"
    if (!$process.WaitForExit(30000)) {
        $process.Kill($true)
        throw "Test timeout 30s: PID=$($process.Id), command=$exe $proxy; launched process tree terminated."
    }
    Get-Content $testLog
    Get-Content "$testLog.err"
    if ($process.ExitCode -ne 0) { throw "$brand test failed: $($process.ExitCode)" }
    Write-Output "$brand PASS (exit 0)"
}
