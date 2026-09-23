function Start-TestTelemetryTrace {
    param([Parameter(Mandatory)][string]$Directory)
    New-Item -ItemType Directory -Path $Directory -Force | Out-Null
    $directoryPath = (Resolve-Path -LiteralPath $Directory).Path
    $collector = (Resolve-Path (Join-Path $PSScriptRoot '..\..\tools\Collect-TelemetryTrace.ps1')).Path
    $script:telemetryPolicyBroker = $null
    $sid = [Security.Principal.WindowsIdentity]::GetCurrent().User.Value
    $policyArgument = if ($env:ITE2E_TELEMETRY_POLICY_APPROVED -eq '1') { " -PolicyApproved -ExpectedUserSid `"$sid`"" } else { '' }
    $process = Start-Process -FilePath (Get-Command pwsh).Source -Verb RunAs -PassThru `
        -ArgumentList "-NoProfile -File `"$collector`" -OutputDirectory `"$directoryPath`"$policyArgument"
    $deadline = [DateTime]::UtcNow.AddSeconds(45)
    while (-not (Test-Path -LiteralPath (Join-Path $directoryPath 'ready.json'))) {
        if ((Test-Path -LiteralPath (Join-Path $directoryPath 'error.txt')) -or $process.HasExited) {
            throw "Elevated capture failed; inspect $directoryPath"
        }
        if ([DateTime]::UtcNow -ge $deadline) {
            New-Item -ItemType File -Path (Join-Path $directoryPath 'stop') -Force | Out-Null
            throw 'Timed out waiting for elevated ETW readiness.'
        }
        Start-Sleep -Milliseconds 200
    }
    $ready = Get-Content -LiteralPath (Join-Path $directoryPath 'ready.json') -Raw | ConvertFrom-Json
    if ($policyArgument) {
        if (-not $ready.policyBroker -or $ready.userSid -cne $sid) {
            New-Item -ItemType File -Path (Join-Path $directoryPath 'stop') -Force | Out-Null
            throw 'The policy broker did not confirm the approved same-user transaction.'
        }
        $script:telemetryPolicyBroker = $directoryPath
    }
    [pscustomobject]@{ Directory = $directoryPath; Process = $process }
}

function Stop-TestTelemetryTrace {
    param([Parameter(Mandatory)]$Trace)
    New-Item -ItemType File -Path (Join-Path $Trace.Directory 'stop') -Force | Out-Null
    $deadline = [DateTime]::UtcNow.AddSeconds(90)
    while (-not (Test-Path -LiteralPath (Join-Path $Trace.Directory 'done'))) {
        if ([DateTime]::UtcNow -ge $deadline) { throw "ETW finalization timed out: $($Trace.Directory)" }
        Start-Sleep -Milliseconds 200
    }
    if (-not $Trace.Process.WaitForExit(10000)) { throw 'The bounded elevated collector did not exit after finalization.' }
    $errorFile = Join-Path $Trace.Directory 'error.txt'
    if (Test-Path -LiteralPath $errorFile) { throw (Get-Content -LiteralPath $errorFile -Raw) }
}

function Select-TestTelemetrySchema {
    param(
        [object[]]$Schemas,
        [string]$Provider,
        [int]$ProcessId,
        [string]$Name,
        [string[]]$FieldNames
    )
    $matches = @($Schemas | Where-Object {
        $_.Provider -eq $Provider -and $_.ProcessId -eq $ProcessId -and $_.Name -ceq $Name -and
        (($_.Types.Keys -join '|') -ceq ($FieldNames -join '|'))
    })
    if ($matches.Count -gt 1) { throw "Ambiguous TDH types for $Provider/$Name in process $ProcessId" }
    if ($matches.Count -eq 1) { return $matches[0] }
}

function Read-TestTelemetryTrace {
    param(
        [Parameter(Mandatory)][string]$Directory,
        [Parameter(Mandatory)][int[]]$ProcessIds
    )
    [xml]$events = Get-Content -LiteralPath (Join-Path $Directory 'events.xml') -Raw
    foreach ($lost in $events.SelectNodes("//*[local-name()='EventData']/*[local-name()='Data'][@Name='EventsLost' or @Name='BuffersLost']")) {
        if ([long]$lost.InnerText -ne 0) { throw "ETW capture lost data: $($lost.GetAttribute('Name'))=$($lost.InnerText)" }
    }
    [xml]$schema = Get-Content -LiteralPath (Join-Path $Directory 'schema.xml') -Raw
    $tlgSchemas = @()
    if (Test-Path -LiteralPath (Join-Path $Directory 'telemetry.etl')) {
        if (-not ('ItE2E.TraceLoggingSchema' -as [type])) {
            Add-Type -Path (Join-Path $PSScriptRoot 'TraceLoggingSchema.cs')
        }
        $tlgSchemas = @([ItE2E.TraceLoggingSchema]::Read((Join-Path $Directory 'telemetry.etl')))
        ConvertTo-Json -InputObject $tlgSchemas -Depth 8 | Set-Content -LiteralPath (Join-Path $Directory 'tdh-schema.json')
    }
    foreach ($event in $events.SelectNodes("//*[local-name()='Event']")) {
        $system = $event.SelectSingleNode("*[local-name()='System']")
        if (-not $system) { continue }
        $execution = $system.SelectSingleNode("*[local-name()='Execution']")
        if ([int]$execution.GetAttribute('ProcessID') -notin $ProcessIds) { continue }
        $provider = $system.SelectSingleNode("*[local-name()='Provider']")
        $guid = $provider.GetAttribute('Guid').Trim('{}')
        if ($guid -notin @('56c06166-2e2e-5f4d-7ff3-74f4b78c87d6', '24a1622f-7da7-5c77-3303-d850bd1ab2ed', '4cfcff80-4e6b-5bfd-8ea1-d38e1226f70b', 'be579944-4d33-5202-e5d6-a7a57f1935cb')) { continue }
        $data = @($event.SelectNodes("*[local-name()='EventData']/*[local-name()='Data']"))
        $task = $event.SelectSingleNode("*[local-name()='RenderingInfo']/*[local-name()='Task']")
        if ($guid -eq '56c06166-2e2e-5f4d-7ff3-74f4b78c87d6' -and $task -and $task.InnerText -cne 'SessionBecameInteractive') { continue }
        $tlg = Select-TestTelemetrySchema -Schemas $tlgSchemas -Provider $guid `
            -ProcessId ([int]$execution.GetAttribute('ProcessID')) -Name $task.InnerText `
            -FieldNames @($data | ForEach-Object { $_.GetAttribute('Name') })
        if ($tlg) {
            $values = [ordered]@{}
            foreach ($field in $data) { $values[$field.GetAttribute('Name')] = $field.InnerText.Trim() }
            [pscustomobject]@{
                Provider = $guid; Name = $tlg.Name; ProcessId = [int]$execution.GetAttribute('ProcessID')
                Timestamp = $system.SelectSingleNode("*[local-name()='TimeCreated']").GetAttribute('SystemTime')
                Fields = $values; Types = $tlg.Types
            }
            continue
        }
        if ($tlgSchemas.Count -and $task) {
            throw "No matching process-scoped TDH schema for $guid/$($task.InnerText) in process $($execution.GetAttribute('ProcessID'))"
        }
        $providerSchema = @($schema.SelectNodes("//*[local-name()='provider']") | Where-Object {
            $_.GetAttribute('guid').Trim('{}') -ieq $guid
        }) | Select-Object -First 1
        if (-not $providerSchema) { throw "No exported schema for captured provider $guid" }
        $id = $system.SelectSingleNode("*[local-name()='EventID']").InnerText
        $versionNode = $system.SelectSingleNode("*[local-name()='Version']")
        $definitions = @($providerSchema.SelectNodes(".//*[local-name()='event']") | Where-Object {
            $_.GetAttribute('value') -eq $id -and
            (-not $_.HasAttribute('version') -or $_.GetAttribute('version') -eq $versionNode.InnerText)
        })
        # TraceLogging schemas may reuse event IDs. Match the complete property
        # sequence too; never guess the schema from the event ID alone.
        $matches = @(
            foreach ($definition in $definitions) {
                $templateId = $definition.GetAttribute('template')
                $template = @($providerSchema.SelectNodes(".//*[local-name()='template']") | Where-Object {
                    $_.GetAttribute('tid') -eq $templateId
                }) | Select-Object -First 1
                if (-not $template) { continue }
                $fields = @($template.SelectNodes("*[local-name()='data']"))
                $schemaNames = ($fields | ForEach-Object { $_.GetAttribute('name') }) -join '|'
                $eventNames = ($data | ForEach-Object { $_.GetAttribute('Name') }) -join '|'
                if ($schemaNames -ceq $eventNames) {
                    [pscustomobject]@{ Definition = $definition; Fields = $fields }
                }
            }
        )
        if ($matches.Count -ne 1) { throw "No unambiguous typed event schema for $guid/$id; retain raw ETL for TDH decoding." }
        $definition = $matches[0].Definition
        $name = $definition.GetAttribute('symbol')
        if (-not $name) { $name = $definition.GetAttribute('task') }
        if (-not $name) { throw "Decoded event has no self-describing name: $guid/$id" }
        $values = [ordered]@{}
        $types = [ordered]@{}
        for ($i = 0; $i -lt $data.Count; $i++) {
            $field = $data[$i].GetAttribute('Name')
            $types[$field] = $matches[0].Fields[$i].GetAttribute('inType')
            if (-not $types[$field]) { throw "Missing wire type for $name.$field" }
            $values[$field] = $data[$i].InnerText
        }
        [pscustomobject]@{
            Provider = $guid; Name = $name; ProcessId = [int]$execution.GetAttribute('ProcessID')
            Timestamp = $system.SelectSingleNode("*[local-name()='TimeCreated']").GetAttribute('SystemTime')
            Fields = $values; Types = $types
        }
    }
}
