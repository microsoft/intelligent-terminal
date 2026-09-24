$script:telemetryPolicyPath = 'Software\Policies\Microsoft\IntelligentTerminal'
$script:telemetryPolicyNames = @('AllowAutoFix', 'AllowedAgents', 'AllowCustomAgents')

function Assert-TelemetryPolicyBrokerIdentity {
    param([string]$ExpectedSid, [bool]$Approved)
    if (-not $Approved) { throw 'Policy broker requires explicit approval.' }
    if (-not $ExpectedSid -or [Security.Principal.WindowsIdentity]::GetCurrent().User.Value -cne $ExpectedSid) {
        throw 'Policy broker must run as the same Windows user as the medium-integrity test.'
    }
}

function Assert-TelemetryPolicyRequest {
    param([Parameter(Mandatory)][System.Collections.IDictionary]$Request)
    if (@($Request.Keys | Where-Object { $_ -notin @('Id', 'Operation', 'Name', 'Value') }).Count) {
        throw 'Unknown policy request fields.'
    }
    if ($Request.Id -isnot [string] -or $Request.Id -cnotmatch '^[a-f0-9]{32}$') { throw 'Invalid policy request ID.' }
    if ($Request.Operation -ceq 'Restore') {
        if ($Request.Contains('Name') -or $Request.Contains('Value')) { throw 'Restore accepts no policy arguments.' }
        return
    }
    if ($Request.Operation -cne 'Set' -or $Request.Name -cnotin $script:telemetryPolicyNames -or -not $Request.Contains('Value')) {
        throw 'Only Set of an approved policy name, or Restore, is permitted.'
    }
    if ($null -eq $Request.Value) { return }
    if ($Request.Name -ceq 'AllowedAgents') {
        if ($Request.Value -isnot [array] -or $Request.Value.Count -gt 16) { throw 'AllowedAgents must be an array of at most 16 bounded agent IDs.' }
        foreach ($value in $Request.Value) {
            if ($value -isnot [string] -or $value -cnotmatch '^[a-z][a-z0-9_-]{0,31}$') { throw 'Invalid bounded agent ID.' }
        }
    }
    elseif (($Request.Value -isnot [int] -and $Request.Value -isnot [long]) -or $Request.Value -notin @(0, 1)) {
        throw 'Boolean policies accept only integer 0, integer 1, or null.'
    }
}

function Invoke-TelemetryPolicyBrokerRequest {
    param([Parameter(Mandatory)]$Transaction, [Parameter(Mandatory)][System.Collections.IDictionary]$Request)
    Assert-TelemetryPolicyRequest -Request $Request
    if ($Request.Operation -ceq 'Restore') { Restore-TelemetryPolicy -Transaction $Transaction }
    else {
        if ($Transaction.Restored) { throw 'The policy transaction has already been restored.' }
        Set-TelemetryPolicy -Transaction $Transaction -Name $Request.Name -Value $Request.Value
    }
}

function Send-TelemetryPolicyBrokerRequest {
    param([Parameter(Mandatory)]$Transaction, [Parameter(Mandatory)][System.Collections.IDictionary]$Request)
    Assert-TelemetryPolicyRequest -Request $Request
    $directory = $Transaction.BrokerDirectory
    if (Test-Path -LiteralPath (Join-Path $directory 'done')) { throw 'The bounded policy broker has already stopped.' }
    $requestPath = Join-Path $directory 'policy-request.json'
    if (Test-Path -LiteralPath $requestPath) { throw 'Another policy request is already pending.' }
    $pending = Join-Path $directory 'policy-request.pending'
    $Request | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath $pending
    Move-Item -LiteralPath $pending -Destination $requestPath
    $deadline = [DateTime]::UtcNow.AddSeconds(15)
    do {
        $responsePath = Join-Path $directory ("policy-response-$($Request.Id).json")
        if (Test-Path -LiteralPath $responsePath) {
            $response = Get-Content -LiteralPath $responsePath -Raw | ConvertFrom-Json
            if ($response.Id -ceq $Request.Id) {
                if (-not $response.Success) { throw "Policy broker rejected request: $($response.Error)" }
                return
            }
        }
        if (Test-Path -LiteralPath (Join-Path $directory 'done')) { throw 'Policy broker stopped before acknowledging the request.' }
        Start-Sleep -Milliseconds 100
    } while ([DateTime]::UtcNow -lt $deadline)
    throw 'Timed out waiting for the bounded policy broker.'
}

function Save-TelemetryPolicyJournal {
    param([Parameter(Mandatory)]$Transaction, $Pending = $null)
    $journal = [ordered]@{
        UserSid = [Security.Principal.WindowsIdentity]::GetCurrent().User.Value
        RegistryPath = "HKCU\$script:telemetryPolicyPath"
        Original = $Transaction.Original; KeyExisted = $Transaction.KeyExisted; KeyCreated = $Transaction.KeyCreated
        LastWritten = $Transaction.LastWritten; Pending = $Pending; Restored = $Transaction.Restored
    }
    $path = Join-Path (Split-Path $Transaction.Backup) 'hkcu-policy-journal.json'
    $journal | ConvertTo-Json -Depth 10 | Set-Content -LiteralPath "$path.pending"
    Move-Item -LiteralPath "$path.pending" -Destination $path -Force
}

function Assert-TelemetryPolicyRestoration {
    param([Parameter(Mandatory)]$Transaction, [Parameter(Mandatory)]$Receipt)
    if (-not $Receipt.Restored -or -not $Receipt.Original) { throw 'Policy worker receipt does not confirm restoration.' }
    $key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey($script:telemetryPolicyPath)
    try {
        foreach ($name in $script:telemetryPolicyNames) {
            $saved = $Receipt.Original[$name]
            if ($null -eq $saved) { throw "Policy restoration receipt omits $name." }
            if (($saved | ConvertTo-Json -Depth 5 -Compress) -cne ($Transaction.Original[$name] | ConvertTo-Json -Depth 5 -Compress)) {
                throw "Policy $name changed between client and worker snapshots; inspect both original backups."
            }
            $actual = if ($key) { Get-TelemetryPolicyValue -Key $key -Name $name } else {
                [ordered]@{ Exists = $false; Kind = $null; Value = $null }
            }
            if (($actual | ConvertTo-Json -Depth 5 -Compress) -cne ($saved | ConvertTo-Json -Depth 5 -Compress)) {
                throw "Medium-client HKCU restoration readback failed for $name."
            }
        }
    }
    finally { if ($key) { $key.Dispose() } }
}

function Get-TelemetryPolicyValue {
    param([Parameter(Mandatory)]$Key, [Parameter(Mandatory)][string]$Name)
    if ($Name -notin $Key.GetValueNames()) { return [ordered]@{ Exists = $false; Kind = $null; Value = $null } }
    [ordered]@{
        Exists = $true
        Kind = $Key.GetValueKind($Name).ToString()
        Value = $Key.GetValue($Name, $null, [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
    }
}

function Assert-TelemetryNoMachinePolicy {
    $key = [Microsoft.Win32.Registry]::LocalMachine.OpenSubKey($script:telemetryPolicyPath)
    try {
        if ($key) {
            foreach ($name in $script:telemetryPolicyNames) {
                if ($name -in $key.GetValueNames()) {
                    throw "Machine policy $name takes precedence; this suite will not change or circumvent HKLM policy."
                }
            }
        }
    }
    finally { if ($key) { $key.Dispose() } }
}

function Initialize-TelemetryPolicyTransaction {
    param([Parameter(Mandatory)][string]$Directory)
    if ($env:ITE2E_TELEMETRY_POLICY_APPROVED -ne '1') {
        throw 'Full telemetry acceptance needs explicit approval for temporary HKCU AllowAutoFix/AllowedAgents/AllowCustomAgents changes. Set ITE2E_TELEMETRY_POLICY_APPROVED=1 only after approval.'
    }
    Assert-TelemetryNoMachinePolicy
    $original = [ordered]@{}
    $key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey($script:telemetryPolicyPath)
    $keyExisted = $null -ne $key
    try {
        foreach ($name in $script:telemetryPolicyNames) {
            $original[$name] = if ($key) { Get-TelemetryPolicyValue -Key $key -Name $name } else {
                [ordered]@{ Exists = $false; Kind = $null; Value = $null }
            }
        }
    }
    finally { if ($key) { $key.Dispose() } }
    $transaction = [pscustomobject]@{
        Original = $original
        LastWritten = [ordered]@{}
        KeyExisted = $keyExisted
        KeyCreated = $false
        Backup = Join-Path $Directory 'hkcu-policy-original.clixml'
        Restored = $false
        BrokerDirectory = $null
    }
    $transaction | Export-Clixml -LiteralPath $transaction.Backup -Depth 10
    $transaction
}

function Set-TelemetryPolicy {
    param(
        [Parameter(Mandatory)]$Transaction,
        [Parameter(Mandatory)][ValidateSet('AllowAutoFix', 'AllowedAgents', 'AllowCustomAgents')][string]$Name,
        [AllowNull()]$Value
    )
    if ($env:ITE2E_TELEMETRY_POLICY_APPROVED -ne '1') { throw 'HKCU telemetry policy modification is not approved.' }
    $request = @{ Id = [guid]::NewGuid().ToString('N'); Operation = 'Set'; Name = $Name; Value = $Value }
    Assert-TelemetryPolicyRequest -Request $request
    if ($script:telemetryPolicyBroker) {
        $Transaction.BrokerDirectory = $script:telemetryPolicyBroker
        Send-TelemetryPolicyBrokerRequest -Transaction $Transaction -Request $request
        return
    }
    Assert-TelemetryNoMachinePolicy
    Save-TelemetryPolicyJournal -Transaction $Transaction -Pending @{ Name = $Name; Value = $Value }
    $expected = if ($Transaction.LastWritten.Contains($Name)) { $Transaction.LastWritten[$Name] } else { $Transaction.Original[$Name] }
    $key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey($script:telemetryPolicyPath, $true)
    if (-not $key) {
        if ($expected.Exists) { throw "HKCU $Name disappeared before the test write; retained original policy backup." }
        if ($null -eq $Value) {
            Save-TelemetryPolicyJournal -Transaction $Transaction
            return
        }
        $key = [Microsoft.Win32.Registry]::CurrentUser.CreateSubKey($script:telemetryPolicyPath)
        $Transaction.KeyCreated = $true
    }
    try {
        $current = Get-TelemetryPolicyValue -Key $key -Name $Name
        if (($current | ConvertTo-Json -Depth 5 -Compress) -cne ($expected | ConvertTo-Json -Depth 5 -Compress)) {
            throw "Concurrent modification of HKCU $Name; retained original policy backup rather than overwriting it."
        }
        Save-TelemetryPolicyJournal -Transaction $Transaction -Pending @{ Name = $Name; Value = $Value }
        if ($null -eq $Value) { $key.DeleteValue($Name, $false) }
        elseif ($Name -eq 'AllowedAgents') { $key.SetValue($Name, [string[]]$Value, [Microsoft.Win32.RegistryValueKind]::MultiString) }
        else { $key.SetValue($Name, [int]$Value, [Microsoft.Win32.RegistryValueKind]::DWord) }
        $Transaction.LastWritten[$Name] = Get-TelemetryPolicyValue -Key $key -Name $Name
        Save-TelemetryPolicyJournal -Transaction $Transaction
    }
    finally { $key.Dispose() }
}

function Restore-TelemetryPolicy {
    param([Parameter(Mandatory)]$Transaction)
    if ($Transaction.BrokerDirectory) {
        if (-not (Test-Path -LiteralPath (Join-Path $Transaction.BrokerDirectory 'done'))) {
            Send-TelemetryPolicyBrokerRequest -Transaction $Transaction -Request @{
                Id = [guid]::NewGuid().ToString('N'); Operation = 'Restore'
            }
        }
        $receipt = Join-Path $Transaction.BrokerDirectory 'hkcu-policy-restored.clixml'
        if (-not (Test-Path -LiteralPath $receipt)) { throw "Policy broker did not verify restoration; inspect $($Transaction.BrokerDirectory)." }
        Assert-TelemetryPolicyRestoration -Transaction $Transaction -Receipt (Import-Clixml -LiteralPath $receipt)
        $Transaction.Restored = $true
        return
    }
    if ($Transaction.Restored) { return }
    if (-not $Transaction.LastWritten.Count -and -not $Transaction.KeyCreated) {
        $Transaction.Restored = $true
        $Transaction | Export-Clixml -LiteralPath ($Transaction.Backup -replace 'original', 'restored') -Depth 10
        Save-TelemetryPolicyJournal -Transaction $Transaction
        return
    }
    $key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey($script:telemetryPolicyPath, $true)
    if (-not $key) { throw "Telemetry policy key disappeared; recover retained backup $($Transaction.Backup)." }
    try {
        foreach ($name in $Transaction.LastWritten.Keys) {
            $current = Get-TelemetryPolicyValue -Key $key -Name $name
            if (($current | ConvertTo-Json -Depth 5 -Compress) -cne ($Transaction.LastWritten[$name] | ConvertTo-Json -Depth 5 -Compress)) {
                throw "Concurrent modification of HKCU $name; recover retained backup $($Transaction.Backup)."
            }
        }
        foreach ($name in $Transaction.LastWritten.Keys) {
            $saved = $Transaction.Original[$name]
            if ($saved.Exists) { $key.SetValue($name, $saved.Value, [Microsoft.Win32.RegistryValueKind]$saved.Kind) }
            else { $key.DeleteValue($name, $false) }
            $actual = Get-TelemetryPolicyValue -Key $key -Name $name
            if (($actual | ConvertTo-Json -Depth 5 -Compress) -cne ($saved | ConvertTo-Json -Depth 5 -Compress)) {
                throw "HKCU policy restoration verification failed for $name."
            }
        }
        $emptyCreatedKey = $Transaction.KeyCreated -and $key.ValueCount -eq 0 -and $key.SubKeyCount -eq 0
    }
    finally { $key.Dispose() }
    if ($emptyCreatedKey) { [Microsoft.Win32.Registry]::CurrentUser.DeleteSubKey($script:telemetryPolicyPath, $false) }
    $Transaction.Restored = $true
    Save-TelemetryPolicyJournal -Transaction $Transaction
    $Transaction | Export-Clixml -LiteralPath ($Transaction.Backup -replace 'original', 'restored') -Depth 10
}
