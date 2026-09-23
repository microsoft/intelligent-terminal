#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
Describe 'Constrained telemetry policy broker' -Tag Unit {
    BeforeAll {
        . (Join-Path $PSScriptRoot '..\tests\helpers\TelemetryPolicy.ps1')
        $script:policyTestRoot = Join-Path $PSScriptRoot ('..\artifacts\policy-unit-' + [guid]::NewGuid().ToString('N'))
        New-Item -ItemType Directory -Path $script:policyTestRoot -Force | Out-Null
    }
    AfterAll { Remove-Item -LiteralPath $script:policyTestRoot -Recurse -Force }

    It 'Requires explicit approval and the original Windows SID' {
        $sid = [Security.Principal.WindowsIdentity]::GetCurrent().User.Value
        { Assert-TelemetryPolicyBrokerIdentity -ExpectedSid $sid -Approved $true } | Should -Not -Throw
        { Assert-TelemetryPolicyBrokerIdentity -ExpectedSid $sid -Approved $false } | Should -Throw '*approval*'
        { Assert-TelemetryPolicyBrokerIdentity -ExpectedSid 'S-1-5-18' -Approved $true } | Should -Throw '*same Windows user*'
    }

    It 'Accepts only the three approved policy names and bounded values' {
        foreach ($name in @('AllowAutoFix', 'AllowCustomAgents')) {
            foreach ($value in @(0, 1, $null)) {
                { Assert-TelemetryPolicyRequest @{ Id = 'a' * 32; Operation = 'Set'; Name = $name; Value = $value } } | Should -Not -Throw
            }
        }
        foreach ($value in @(@(), @('copilot'), @('claude', 'custom_agent'), $null)) {
            { Assert-TelemetryPolicyRequest @{ Id = 'a' * 32; Operation = 'Set'; Name = 'AllowedAgents'; Value = $value } } | Should -Not -Throw
        }
        { Assert-TelemetryPolicyRequest @{ Id = 'a' * 32; Operation = 'Restore' } } | Should -Not -Throw
    }

    It 'Rejects commands paths extra fields invalid IDs and out-of-range policy values before dispatch' {
        Mock Set-TelemetryPolicy { throw 'must not dispatch' }
        $invalid = @(
            @{ Operation = 'Execute'; Name = 'AllowAutoFix'; Value = 1 }
            @{ Operation = 'Set'; Name = 'HKLM\AllowAutoFix'; Value = 1 }
            @{ Operation = 'Set'; Name = 'AllowAutoFix'; Value = 2 }
            @{ Operation = 'Set'; Name = 'AllowAutoFix'; Value = $true }
            @{ Operation = 'Set'; Name = 'AllowAutoFix'; Value = '1' }
            @{ Operation = 'Set'; Name = 'AllowedAgents'; Value = 'copilot' }
            @{ Operation = 'Set'; Name = 'AllowedAgents'; Value = @('cmd.exe /c whoami') }
            @{ Operation = 'Set'; Name = 'AllowedAgents'; Value = @('x' * 33) }
            @{ Operation = 'Set'; Name = 'AllowedAgents'; Value = @('copilot') * 17 }
            @{ Operation = 'Set'; Name = 'AllowedAgents'; Value = @(); Path = 'HKLM' }
            @{ Operation = 'Restore'; Name = 'AllowAutoFix' }
        )
        foreach ($request in $invalid) {
            $request.Id = 'b' * 32
            { Invoke-TelemetryPolicyBrokerRequest -Transaction @{} -Request $request } | Should -Throw
        }
        { Assert-TelemetryPolicyRequest @{ Id = '..\outside'; Operation = 'Restore' } } | Should -Throw
        Should -Invoke Set-TelemetryPolicy -Times 0 -Exactly
    }

    It 'Dispatches only validated Set and Restore operations and rejects writes after restoration' {
        Mock Set-TelemetryPolicy {}
        Mock Restore-TelemetryPolicy {}
        Invoke-TelemetryPolicyBrokerRequest -Transaction @{ Restored = $false } -Request @{
            Id = 'c' * 32; Operation = 'Set'; Name = 'AllowAutoFix'; Value = 0
        }
        Invoke-TelemetryPolicyBrokerRequest -Transaction @{} -Request @{ Id = 'd' * 32; Operation = 'Restore' }
        { Invoke-TelemetryPolicyBrokerRequest -Transaction @{ Restored = $true } -Request @{
            Id = 'e' * 32; Operation = 'Set'; Name = 'AllowAutoFix'; Value = 1
        } } | Should -Throw '*already been restored*'
        Should -Invoke Set-TelemetryPolicy -Times 1 -Exactly
        Should -Invoke Restore-TelemetryPolicy -Times 1 -Exactly
    }

    It 'Persists original and pending values without executing registry changes' {
        $transaction = [pscustomobject]@{
            Original = @{ AllowAutoFix = @{ Exists = $false; Kind = $null; Value = $null } }
            LastWritten = [ordered]@{}; KeyExisted = $false; KeyCreated = $false; Restored = $false
            Backup = Join-Path $script:policyTestRoot 'hkcu-policy-original.clixml'
            BrokerDirectory = $null
        }
        Save-TelemetryPolicyJournal -Transaction $transaction -Pending @{ Name = 'AllowAutoFix'; Value = 1 }
        $journal = Get-Content (Join-Path $script:policyTestRoot 'hkcu-policy-journal.json') -Raw | ConvertFrom-Json
        $journal.Pending.Name | Should -Be 'AllowAutoFix'
        $journal.Pending.Value | Should -Be 1
        $journal.Original.AllowAutoFix.Exists | Should -BeFalse
        Restore-TelemetryPolicy -Transaction $transaction
        $transaction.Restored | Should -BeTrue
        Test-Path (Join-Path $script:policyTestRoot 'hkcu-policy-restored.clixml') | Should -BeTrue
    }

    It 'Requires worker restoration evidence after the bounded collector exits' {
        Mock Send-TelemetryPolicyBrokerRequest { throw 'must not contact an exited worker' }
        Mock Assert-TelemetryPolicyRestoration {
            if (-not $Receipt.Restored) { throw 'unverified receipt' }
        }
        $broker = Join-Path $script:policyTestRoot 'broker'
        New-Item -ItemType Directory -Path $broker | Out-Null
        New-Item -ItemType File -Path (Join-Path $broker 'done') | Out-Null
        $transaction = [pscustomobject]@{ BrokerDirectory = $broker; Restored = $false }
        { Restore-TelemetryPolicy -Transaction $transaction } | Should -Throw '*did not verify restoration*'
        @{ Restored = $true } | Export-Clixml (Join-Path $broker 'hkcu-policy-restored.clixml')
        { Restore-TelemetryPolicy -Transaction $transaction } | Should -Not -Throw
        $transaction.Restored | Should -BeTrue
        Should -Invoke Send-TelemetryPolicyBrokerRequest -Times 0 -Exactly
        Should -Invoke Assert-TelemetryPolicyRestoration -Times 1 -Exactly
    }

    It 'Rejects unverified restoration receipts before opening the registry' {
        { Assert-TelemetryPolicyRestoration -Transaction @{} -Receipt @{ Restored = $false } } |
            Should -Throw '*does not confirm restoration*'
        { Assert-TelemetryPolicyRestoration -Transaction @{} -Receipt @{ Restored = $true } } |
            Should -Throw '*does not confirm restoration*'
    }

    It 'Verifies deserialized originals against read-only HKCU state and rejects mismatched readback' {
        $approval = $env:ITE2E_TELEMETRY_POLICY_APPROVED
        try {
            $env:ITE2E_TELEMETRY_POLICY_APPROVED = '1'
            $transaction = Initialize-TelemetryPolicyTransaction -Directory $script:policyTestRoot
            $transaction.Restored = $true
            $path = Join-Path $script:policyTestRoot 'readback-receipt.clixml'
            $transaction | Export-Clixml -LiteralPath $path -Depth 10
            $receipt = Import-Clixml -LiteralPath $path
            { Assert-TelemetryPolicyRestoration -Transaction $transaction -Receipt $receipt } | Should -Not -Throw
            $transaction.Original.AllowAutoFix.Exists = -not $transaction.Original.AllowAutoFix.Exists
            $receipt.Original.AllowAutoFix.Exists = $transaction.Original.AllowAutoFix.Exists
            { Assert-TelemetryPolicyRestoration -Transaction $transaction -Receipt $receipt } | Should -Throw '*readback failed*'
        }
        finally { $env:ITE2E_TELEMETRY_POLICY_APPROVED = $approval }
    }

    It 'Sends only JSON requests and surfaces explicit worker errors without local registry writes' {
        $broker = Join-Path $script:policyTestRoot 'ipc'
        New-Item -ItemType Directory -Path $broker | Out-Null
        $transaction = [pscustomobject]@{ BrokerDirectory = $broker; Restored = $false }
        $request = @{ Id = 'f' * 32; Operation = 'Set'; Name = 'AllowAutoFix'; Value = 0 }
        $responsePath = Join-Path $broker "policy-response-$($request.Id).json"
        @{ Id = 'e' * 32; Success = $false; Error = 'unrelated response' } |
            ConvertTo-Json | Set-Content (Join-Path $broker ('policy-response-' + ('e' * 32) + '.json'))
        @{ Id = $request.Id; Success = $true } | ConvertTo-Json | Set-Content $responsePath
        Send-TelemetryPolicyBrokerRequest -Transaction $transaction -Request $request
        $sent = Get-Content (Join-Path $broker 'policy-request.json') -Raw | ConvertFrom-Json
        $sent.Name | Should -Be 'AllowAutoFix'
        $sent.Value | Should -Be 0
        Remove-Item (Join-Path $broker 'policy-request.json')
        @{ Id = $request.Id; Success = $false; Error = 'deliberate worker refusal' } |
            ConvertTo-Json | Set-Content $responsePath
        { Send-TelemetryPolicyBrokerRequest -Transaction $transaction -Request $request } | Should -Throw '*deliberate worker refusal*'
    }

    It 'Routes configured medium-client writes through the broker instead of opening writable registry keys' {
        $savedApproval = $env:ITE2E_TELEMETRY_POLICY_APPROVED
        $savedBroker = $script:telemetryPolicyBroker
        Mock Assert-TelemetryNoMachinePolicy { throw 'local registry write path must not run' }
        Mock Send-TelemetryPolicyBrokerRequest {}
        try {
            $env:ITE2E_TELEMETRY_POLICY_APPROVED = '1'
            $script:telemetryPolicyBroker = $script:policyTestRoot
            $transaction = [pscustomobject]@{ BrokerDirectory = $null }
            Set-TelemetryPolicy -Transaction $transaction -Name AllowedAgents -Value ([string[]]@('copilot'))
            $transaction.BrokerDirectory | Should -Be $script:policyTestRoot
            Should -Invoke Send-TelemetryPolicyBrokerRequest -Times 1 -Exactly
            Should -Invoke Assert-TelemetryNoMachinePolicy -Times 0 -Exactly
        }
        finally {
            $env:ITE2E_TELEMETRY_POLICY_APPROVED = $savedApproval
            $script:telemetryPolicyBroker = $savedBroker
        }
    }
}
