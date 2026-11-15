# Copyright (c) Microsoft Corporation.
# Licensed under the MIT license.
$ErrorActionPreference = 'Stop'
$wtcli = (Get-Command wtcli.exe -ErrorAction Stop | Select-Object -First 1).Source
$wta = (Get-Command wta.exe -ErrorAction Stop | Select-Object -First 1).Source
$query = & $wtcli --json active-pane
if ($LASTEXITCODE -ne 0) { throw 'The fixture could not query its isolated Terminal.' }
$proof = @{
    PaneSessionId = $env:WT_SESSION; ComClsid = $env:WT_COM_CLSID
    WtcliPath = $wtcli; WtaPath = $wta
    Query = ($query -join "`n" | ConvertFrom-Json)
    WorkingDirectory = [Environment]::CurrentDirectory
}
$path = Join-Path $env:ITE2E_NATIVE_PROFILE_PROOF_DIR ($env:WT_SESSION.Trim('{}') + '.json')
[IO.File]::WriteAllText($path, ($proof | ConvertTo-Json -Depth 6), [Text.UTF8Encoding]::new($false))
