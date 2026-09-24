#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
Describe 'Telemetry typed decoding' -Tag Unit {
    BeforeAll {
        . (Join-Path $PSScriptRoot '..\tests\helpers\TelemetryTrace.ps1')
        $script:directory = Join-Path $PSScriptRoot ('..\artifacts\telemetry-decoder-' + [guid]::NewGuid().ToString('N'))
        New-Item -ItemType Directory -Path $script:directory -Force | Out-Null
        @'
<Events><Event><System><Provider Guid="{4cfcff80-4e6b-5bfd-8ea1-d38e1226f70b}" /><EventID>0</EventID><Version>0</Version><Execution ProcessID="42"/><TimeCreated SystemTime="2026-09-23T00:00:00Z"/></System><EventData><Data Name="command">config</Data></EventData></Event></Events>
'@ | Set-Content -LiteralPath (Join-Path $script:directory 'events.xml')
        @'
<instrumentationManifest><provider guid="{4cfcff80-4e6b-5bfd-8ea1-d38e1226f70b}"><events><event value="0" version="0" symbol="AgentSlashCommandUsed" template="T1" /></events><templates><template tid="T1"><data name="command" inType="win:AnsiString" /></template></templates></provider></instrumentationManifest>
'@ | Set-Content -LiteralPath (Join-Path $script:directory 'schema.xml')
    }
    AfterAll { Remove-Item -LiteralPath $script:directory -Recurse -Force }
    It 'Retains self-describing event names, wire types and business values' {
        $records = @(Read-TestTelemetryTrace -Directory $script:directory -ProcessIds @(42))
        $records | Should -HaveCount 1
        $records[0].Name | Should -Be 'AgentSlashCommandUsed'
        $records[0].Fields.command | Should -Be 'config'
        $records[0].Types.command | Should -Be 'win:AnsiString'
    }
    It 'Excludes events from unrelated processes' {
        @(Read-TestTelemetryTrace -Directory $script:directory -ProcessIds @(99)) | Should -HaveCount 0
    }
    It 'Includes the Win32Host interaction source used by retention' {
        $eventsPath = Join-Path $script:directory 'events.xml'
        $schemaPath = Join-Path $script:directory 'schema.xml'
        $originalEvents = Get-Content -LiteralPath $eventsPath -Raw
        $originalSchema = Get-Content -LiteralPath $schemaPath -Raw
        try {
            @'
<Events><Event><System><Provider Guid="{56c06166-2e2e-5f4d-7ff3-74f4b78c87d6}" /><EventID>0</EventID><Version>0</Version><Execution ProcessID="42"/><TimeCreated SystemTime="2026-09-23T00:00:00Z"/></System><EventData><Data Name="Branding">0</Data><Data Name="Distribution">2</Data></EventData></Event></Events>
'@ | Set-Content -LiteralPath $eventsPath
            @'
<instrumentationManifest><provider guid="{56c06166-2e2e-5f4d-7ff3-74f4b78c87d6}"><events><event value="0" version="0" symbol="SessionBecameInteractive" template="T1" /></events><templates><template tid="T1"><data name="Branding" inType="win:UInt8" /><data name="Distribution" inType="win:UInt8" /></template></templates></provider></instrumentationManifest>
'@ | Set-Content -LiteralPath $schemaPath
            $records = @(Read-TestTelemetryTrace -Directory $script:directory -ProcessIds @(42))
            $records | Should -HaveCount 1
            $records[0].Name | Should -Be 'SessionBecameInteractive'
            $records[0].Types.Branding | Should -Be 'win:UInt8'
            $records[0].Fields.Distribution | Should -Be '2'
        }
        finally {
            Set-Content -LiteralPath $eventsPath -Value $originalEvents -NoNewline
            Set-Content -LiteralPath $schemaPath -Value $originalSchema -NoNewline
        }
    }
    It 'Rejects absent typed schemas instead of guessing from text' {
        [xml]$schema = Get-Content -LiteralPath (Join-Path $script:directory 'schema.xml') -Raw
        $schema.instrumentationManifest.provider.templates.template.data.SetAttribute('name', 'other')
        $schema.Save((Join-Path $script:directory 'schema.xml'))
        { Read-TestTelemetryTrace -Directory $script:directory -ProcessIds @(42) } | Should -Throw '*unambiguous typed event schema*'
    }
    It 'Does not borrow another process schema for the same provider and event' {
        $schemas = @(
            [pscustomobject]@{ Provider = 'provider'; Name = 'event'; ProcessId = 42; Types = [ordered]@{ flag = 'win:Boolean' } }
            [pscustomobject]@{ Provider = 'provider'; Name = 'event'; ProcessId = 99; Types = [ordered]@{ flag = 'win:UInt32' } }
        )
        $match = Select-TestTelemetrySchema -Schemas $schemas -Provider provider -Name event -ProcessId 42 -FieldNames flag
        $match.ProcessId | Should -Be 42
        $match.Types.flag | Should -Be 'win:Boolean'
        Select-TestTelemetrySchema -Schemas $schemas -Provider provider -Name event -ProcessId 123 -FieldNames flag |
            Should -BeNullOrEmpty
    }
    It 'Rejects conflicting same-process types instead of selecting the first schema' {
        $schemas = @(
            [pscustomobject]@{ Provider = 'provider'; Name = 'event'; ProcessId = 42; Types = [ordered]@{ flag = 'win:Boolean' } }
            [pscustomobject]@{ Provider = 'provider'; Name = 'event'; ProcessId = 42; Types = [ordered]@{ flag = 'win:UInt32' } }
        )
        { Select-TestTelemetrySchema -Schemas $schemas -Provider provider -Name event -ProcessId 42 -FieldNames flag } |
            Should -Throw '*Ambiguous TDH types*'
    }
    It 'Registers an already-ready policy broker before returning the capture handle' {
        $saved = $env:ITE2E_TELEMETRY_POLICY_APPROVED
        $directory = Join-Path $script:directory 'ready-broker'
        New-Item -ItemType Directory -Path $directory -Force | Out-Null
        @{
            policyBroker = $true
            userSid = [Security.Principal.WindowsIdentity]::GetCurrent().User.Value
        } | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $directory 'ready.json')
        Mock Start-Process { [pscustomobject]@{ HasExited = $false } }
        try {
            $env:ITE2E_TELEMETRY_POLICY_APPROVED = '1'
            $trace = Start-TestTelemetryTrace -Directory $directory
            $script:telemetryPolicyBroker | Should -Be $trace.Directory
            Should -Invoke Start-Process -Times 1 -Exactly
        }
        finally {
            $env:ITE2E_TELEMETRY_POLICY_APPROVED = $saved
            $script:telemetryPolicyBroker = $null
        }
    }
}
