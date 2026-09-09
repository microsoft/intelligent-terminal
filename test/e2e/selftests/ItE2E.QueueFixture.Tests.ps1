#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# Real redirected ACP pipes, but no Terminal, WTA, credentials, or network.

BeforeAll {
    $script:queueFixturePath = Join-Path $PSScriptRoot '..\fixtures\Mock-AcpQueueAgent.ps1'

    function Start-QueueFixture {
        param([switch]$HoldInitialize, [switch]$HoldSession)

        $root = Join-Path $PSScriptRoot "..\artifacts\queue-fixture-$([guid]::NewGuid().ToString('N'))"
        $control = Join-Path $root 'control'
        $records = Join-Path $root 'records'
        $null = New-Item -ItemType Directory -Path $control, $records
        $process = [System.Diagnostics.Process]::new()
        $process.StartInfo.FileName = (Get-Command pwsh -CommandType Application).Source
        $process.StartInfo.UseShellExecute = $false
        $process.StartInfo.CreateNoWindow = $true
        $process.StartInfo.RedirectStandardInput = $true
        $process.StartInfo.RedirectStandardOutput = $true
        $process.StartInfo.RedirectStandardError = $true
        foreach ($argument in @('-NoLogo', '-NoProfile', '-NonInteractive', '-File',
                $script:queueFixturePath, '-ControlDirectory', $control, '-RecordDirectory', $records)) {
            $process.StartInfo.ArgumentList.Add($argument)
        }
        if ($HoldInitialize) { $process.StartInfo.ArgumentList.Add('-HoldInitialize') }
        if ($HoldSession) { $process.StartInfo.ArgumentList.Add('-HoldSession') }
        $started = $false
        try {
            if (-not $process.Start()) { throw 'Queue fixture process did not start' }
            $started = $true
            return @{
                Process = $process
                Root = $root
                Control = $control
                Records = $records
                ReadTask = $process.StandardOutput.ReadLineAsync()
                ErrorTask = $process.StandardError.ReadToEndAsync()
            }
        }
        finally {
            if (-not $started) {
                $process.Dispose()
                Remove-Item -LiteralPath $root -Recurse -Force
            }
        }
    }

    function Stop-QueueFixture {
        param([hashtable]$Fixture)

        try {
            if (-not $Fixture.Process.HasExited) {
                $Fixture.Process.StandardInput.Close()
                if (-not $Fixture.Process.WaitForExit(5000)) {
                    $childPid = $Fixture.Process.Id
                    $Fixture.Process.Kill($true)
                    $Fixture.Process.WaitForExit()
                    throw "Queue fixture PID $childPid did not exit after stdin EOF"
                }
            }
            $stderr = $Fixture.ErrorTask.GetAwaiter().GetResult()
            $Fixture.Process.ExitCode | Should -Be 0 -Because $stderr
            $stderr | Should -BeNullOrEmpty
        }
        finally {
            $Fixture.Process.Dispose()
            Remove-Item -LiteralPath $Fixture.Root -Recurse -Force
        }
    }

    function Send-QueueRequest {
        param([hashtable]$Fixture, [hashtable]$Request)

        $Request.jsonrpc = '2.0'
        $Fixture.Process.StandardInput.WriteLine(($Request | ConvertTo-Json -Depth 20 -Compress))
        $Fixture.Process.StandardInput.Flush()
    }

    function Read-QueueMessage {
        param([hashtable]$Fixture)

        if (-not $Fixture.ReadTask.Wait(10000)) {
            throw "Timed out reading ACP stdout from PID $($Fixture.Process.Id)"
        }
        $line = $Fixture.ReadTask.GetAwaiter().GetResult()
        if ($null -eq $line) {
            throw "Unexpected ACP EOF: $($Fixture.ErrorTask.GetAwaiter().GetResult())"
        }
        $message = ConvertFrom-Json -InputObject $line
        $message.jsonrpc | Should -Be '2.0'
        $Fixture.ReadTask = $Fixture.Process.StandardOutput.ReadLineAsync()
        return $message
    }

    function Assert-QueueQuiet {
        param([hashtable]$Fixture)

        $Fixture.ReadTask.Wait(250) | Should -BeFalse -Because 'a held request must not settle or answer a notification'
        $Fixture.Process.HasExited | Should -BeFalse
    }

    function Get-QueueRecords {
        param([hashtable]$Fixture)

        Get-ChildItem -LiteralPath $Fixture.Records -Filter '*.json' |
            Sort-Object Name |
            ForEach-Object { Get-Content -LiteralPath $_.FullName -Raw | ConvertFrom-Json }
    }

    function Wait-QueueRecord {
        param([hashtable]$Fixture, [string]$Kind, [string]$Marker, [Nullable[int]]$RequestId)

        $clock = [System.Diagnostics.Stopwatch]::StartNew()
        do {
            $records = @(Get-QueueRecords $Fixture | Where-Object {
                $_.kind -eq $Kind -and (-not $Marker -or $_.marker -eq $Marker) -and
                    ($null -eq $RequestId -or $_.requestId -eq $RequestId)
            })
            if ($records.Count -gt 0) { return $records[-1] }
            if ($Fixture.Process.HasExited) {
                throw "Queue fixture exited waiting for $Kind/$Marker`: $($Fixture.ErrorTask.GetAwaiter().GetResult())"
            }
            Start-Sleep -Milliseconds 20
        } while ($clock.Elapsed.TotalSeconds -lt 10)
        throw "Timed out waiting for queue record $Kind/$Marker"
    }

    function Set-QueueGate {
        param([hashtable]$Fixture, [string]$Name)

        [System.IO.File]::WriteAllText((Join-Path $Fixture.Control $Name), '')
    }

    function Initialize-QueueFixture {
        param([hashtable]$Fixture)

        Send-QueueRequest $Fixture @{ id = 0; method = 'initialize'; params = @{ protocolVersion = 1 } }
        $message = Read-QueueMessage $Fixture
        $message.id | Should -Be 0
        $message.result.protocolVersion | Should -Be 1
        $null = Wait-QueueRecord $Fixture initialized
    }

    function New-QueueSession {
        param([hashtable]$Fixture, [int]$RequestId = 1, [hashtable]$Parameters = @{ cwd = 'C:\queue-work'; mcpServers = @() })

        Send-QueueRequest $Fixture @{ id = $RequestId; method = 'session/new'; params = $Parameters }
        $message = Read-QueueMessage $Fixture
        $message.id | Should -Be $RequestId
        $message.result.sessionId | Should -Match "^queue-$($Fixture.Process.Id)-\d+$"
        return $message.result.sessionId
    }

    function Send-QueuePrompt {
        param([hashtable]$Fixture, [string]$SessionId, [int]$RequestId, [string]$Text)

        Send-QueueRequest $Fixture @{
            id = $RequestId
            method = 'session/prompt'
            params = @{ sessionId = $SessionId; prompt = @(@{ type = 'text'; text = $Text }) }
        }
    }

    function Assert-QueueText {
        param([hashtable]$Fixture, [string]$SessionId, [string]$Text)

        $message = Read-QueueMessage $Fixture
        $message.method | Should -Be 'session/update'
        $message.params.sessionId | Should -Be $SessionId
        $message.params.update.sessionUpdate | Should -Be 'agent_message_chunk'
        $message.params.update.content.type | Should -Be 'text'
        $message.params.update.content.text | Should -BeExactly $Text
    }
}

Describe 'ACP queue fixture' -Tag 'Unit' {
    It 'gates session creation after initialize without blocking control-file polling' {
        $fixture = Start-QueueFixture -HoldSession
        try {
            Initialize-QueueFixture $fixture
            Send-QueueRequest $fixture @{ id = 1; method = 'session/new'; params = @{ cwd = 'C:\queue-work'; mcpServers = @() } }
            $session = (Wait-QueueRecord $fixture session).sessionId
            Assert-QueueQuiet $fixture
            @(Get-QueueRecords $fixture | Where-Object kind -eq session_ready) | Should -HaveCount 0
            Set-QueueGate $fixture 'session.release'
            $message = Read-QueueMessage $fixture
            $message.id | Should -Be 1
            $message.result.sessionId | Should -Be $session
            (Wait-QueueRecord $fixture session_ready).sessionId | Should -Be $session
        }
        finally { Stop-QueueFixture $fixture }
    }

    It 'gates initialize without blocking stdin or control-file polling' {
        $fixture = Start-QueueFixture -HoldInitialize
        try {
            Send-QueueRequest $fixture @{ id = 0; method = 'initialize'; params = @{ protocolVersion = 1 } }
            (Wait-QueueRecord $fixture initialize).requestId | Should -Be 0
            Assert-QueueQuiet $fixture
            @(Get-QueueRecords $fixture | Where-Object kind -eq initialized) | Should -HaveCount 0

            Send-QueueRequest $fixture @{ method = 'session/cancel'; params = @{ sessionId = 'not-yet-created' } }
            (Wait-QueueRecord $fixture cancel).sessionId | Should -Be 'not-yet-created'
            Assert-QueueQuiet $fixture
            Set-QueueGate $fixture 'initialize.release'
            $message = Read-QueueMessage $fixture
            $message.id | Should -Be 0
            $message.result.protocolVersion | Should -Be 1
            $message.result.agentInfo.name | Should -Be 'Queue Fixture'
            $message.result.agentCapabilities.mcpCapabilities.http | Should -BeTrue
            $message.result.agentCapabilities.sessionCapabilities.PSObject.Properties.Name | Should -Contain close
            (Wait-QueueRecord $fixture initialized).requestId | Should -Be 0
            Assert-QueueQuiet $fixture
        }
        finally { Stop-QueueFixture $fixture }
    }

    It 'records combined text, safe markers and cwd without persisting MCP credentials' {
        $fixture = Start-QueueFixture
        try {
            Initialize-QueueFixture $fixture
            $session = New-QueueSession $fixture -Parameters @{
                cwd = 'C:\queue-work space'
                mcpServers = @(@{
                    name = 'PRIVATE_MCP_NAME'; type = 'http'
                    url = 'http://localhost:1234/PRIVATE_MCP_URL'
                    headers = @(@{ name = 'Authorization'; value = 'Bearer PRIVATE_MCP_SECRET' })
                })
            }
            Send-QueueRequest $fixture @{
                id = 2; method = 'session/prompt'
                params = @{
                    sessionId = $session
                    prompt = @(
                        @{ type = 'text'; text = 'Autofix QUEUE_ERROR_FIRST' }
                        @{ type = 'image'; data = 'IGNORED_IMAGE_DATA'; mimeType = 'image/png' }
                        @{ type = 'text'; text = 'QUEUE_SUCCESS_SECOND' }
                    )
                }
            }
            Assert-QueueText $fixture $session 'ACK_QUEUE_ERROR_FIRST'
            (Read-QueueMessage $fixture).result.stopReason | Should -Be 'end_turn'
            $prompt = Wait-QueueRecord $fixture prompt QUEUE_ERROR_FIRST
            $prompt.text | Should -BeExactly "Autofix QUEUE_ERROR_FIRST`nQUEUE_SUCCESS_SECOND"
            $prompt.requestId | Should -Be 2
            $prompt.images | Should -HaveCount 1
            $prompt.images[0].decodeError | Should -BeExactly 'Invalid base64 image data'
            (Wait-QueueRecord $fixture session).cwd | Should -BeExactly 'C:\queue-work space'
            (Wait-QueueRecord $fixture completion QUEUE_ERROR_FIRST).reason | Should -Be 'end_turn'

            Send-QueuePrompt $fixture $session 3 'Autofix QUEUE_SUCCESS_ONLY'
            Assert-QueueText $fixture $session 'ACK_QUEUE_SUCCESS_ONLY'
            (Read-QueueMessage $fixture).result.stopReason | Should -Be 'end_turn'
            Send-QueuePrompt $fixture $session 4 'no explicit marker'
            $generated = (Wait-QueueRecord $fixture prompt -RequestId 4).marker
            $generated | Should -Match '^QUEUE_GENERATED_[A-Z0-9_]+$'
            Assert-QueueText $fixture $session "ACK_$generated"
            (Read-QueueMessage $fixture).id | Should -Be 4
            $null = Wait-QueueRecord $fixture completion $generated

            $records = @(Get-QueueRecords $fixture)
            $files = @(Get-ChildItem -LiteralPath $fixture.Records -Filter '*.json' | Sort-Object Name)
            for ($index = 0; $index -lt $records.Count; $index++) {
                $records[$index].sequence | Should -Be ($index + 1)
                $records[$index].pid | Should -Be $fixture.Process.Id
                $files[$index].Name | Should -Be ('{0:D10}-{1}.json' -f ($index + 1), $fixture.Process.Id)
            }
            $persisted = ($files | ForEach-Object { Get-Content -LiteralPath $_.FullName -Raw }) -join "`n"
            $persisted | Should -Not -Match 'PRIVATE_MCP|mcpServers|Authorization|IGNORED_IMAGE_DATA'
            @(Get-ChildItem -LiteralPath $fixture.Records -Filter '*.pending') | Should -HaveCount 0
        }
        finally { Stop-QueueFixture $fixture }
    }

    It 'consumes cancellation while a prompt is held and remains usable afterward' {
        $fixture = Start-QueueFixture
        try {
            Initialize-QueueFixture $fixture
            $session = New-QueueSession $fixture
            Send-QueuePrompt $fixture $session 2 QUEUE_HOLD_CANCEL
            Assert-QueueText $fixture $session START_QUEUE_HOLD_CANCEL
            Assert-QueueQuiet $fixture
            Send-QueueRequest $fixture @{ method = 'session/cancel'; params = @{ sessionId = $session } }
            $cancel = Wait-QueueRecord $fixture cancel QUEUE_HOLD_CANCEL
            $cancel.sessionId | Should -Be $session
            $cancel.requestId | Should -BeNullOrEmpty
            $message = Read-QueueMessage $fixture
            $message.id | Should -Be 2
            $message.error.code | Should -Be -32800
            $message.error.message | Should -Be 'cancelled by fixture'
            (Wait-QueueRecord $fixture completion QUEUE_HOLD_CANCEL).reason | Should -Be cancelled
            Assert-QueueQuiet $fixture

            Send-QueuePrompt $fixture $session 3 QUEUE_RECOVER
            Assert-QueueText $fixture $session ACK_QUEUE_RECOVER
            (Read-QueueMessage $fixture).result.stopReason | Should -Be end_turn
        }
        finally { Stop-QueueFixture $fixture }
    }

    It 'defers cancellation settlement while still serving another session' {
        $fixture = Start-QueueFixture
        try {
            Initialize-QueueFixture $fixture
            $session = New-QueueSession $fixture
            Set-QueueGate $fixture hold-cancel
            Send-QueuePrompt $fixture $session 2 QUEUE_HOLD_DEFER
            Assert-QueueText $fixture $session START_QUEUE_HOLD_DEFER
            Send-QueueRequest $fixture @{ method = 'session/cancel'; params = @{ sessionId = $session } }
            $null = Wait-QueueRecord $fixture cancel QUEUE_HOLD_DEFER
            Set-QueueGate $fixture QUEUE_HOLD_DEFER.release
            Set-QueueGate $fixture QUEUE_HOLD_DEFER.fail
            Assert-QueueQuiet $fixture
            @(Get-QueueRecords $fixture | Where-Object kind -eq completion) | Should -HaveCount 0

            $otherSession = New-QueueSession $fixture -RequestId 3
            $otherSession | Should -Not -Be $session
            Send-QueuePrompt $fixture $otherSession 4 QUEUE_OTHER_SESSION
            Assert-QueueText $fixture $otherSession ACK_QUEUE_OTHER_SESSION
            (Read-QueueMessage $fixture).id | Should -Be 4
            Assert-QueueQuiet $fixture

            Set-QueueGate $fixture release-cancel
            $message = Read-QueueMessage $fixture
            $message.id | Should -Be 2
            $message.error.code | Should -Be -32800
            (Wait-QueueRecord $fixture completion QUEUE_HOLD_DEFER).reason | Should -Be cancelled
            @(Get-QueueRecords $fixture | Where-Object { $_.kind -eq 'completion' -and $_.marker -eq 'QUEUE_HOLD_DEFER' }) |
                Should -HaveCount 1
            Assert-QueueQuiet $fixture
        }
        finally { Stop-QueueFixture $fixture }
    }

    It 'settles a held prompt through its <Gate> file without more stdin' -TestCases @(
        @{ Gate = 'release'; Reason = 'end_turn' }
        @{ Gate = 'fail'; Reason = 'error' }
    ) {
        param($Gate, $Reason)

        $fixture = Start-QueueFixture
        try {
            Initialize-QueueFixture $fixture
            $session = New-QueueSession $fixture
            Send-QueuePrompt $fixture $session 2 QUEUE_HOLD_CONTROL
            Assert-QueueText $fixture $session START_QUEUE_HOLD_CONTROL
            Assert-QueueQuiet $fixture
            Set-QueueGate $fixture "QUEUE_HOLD_CONTROL.$Gate"
            if ($Reason -eq 'end_turn') {
                Assert-QueueText $fixture $session ACK_QUEUE_HOLD_CONTROL
            }
            $message = Read-QueueMessage $fixture
            $message.id | Should -Be 2
            if ($Reason -eq 'end_turn') {
                $message.result.stopReason | Should -Be end_turn
            }
            else {
                $message.error.code | Should -Be -32603
                $message.error.message | Should -Be QUEUE_FIXTURE_FAILURE
            }
            (Wait-QueueRecord $fixture completion QUEUE_HOLD_CONTROL).reason | Should -Be $Reason
            Send-QueuePrompt $fixture $session 3 QUEUE_AFTER_SETTLEMENT
            Assert-QueueText $fixture $session ACK_QUEUE_AFTER_SETTLEMENT
            (Read-QueueMessage $fixture).result.stopReason | Should -Be end_turn
            $null = Wait-QueueRecord $fixture completion QUEUE_AFTER_SETTLEMENT
            @(Get-QueueRecords $fixture | Where-Object { $_.kind -eq 'completion' -and $_.marker -eq 'QUEUE_HOLD_CONTROL' }) |
                Should -HaveCount 1
        }
        finally { Stop-QueueFixture $fixture }
    }

    It 'uses the hold marker for control even when source context has another marker' {
        $fixture = Start-QueueFixture
        try {
            Initialize-QueueFixture $fixture
            $session = New-QueueSession $fixture
            Send-QueuePrompt $fixture $session 2 "Source: QUEUE_ERROR_CONTEXT`nRequest: QUEUE_HOLD_CURRENT"
            Assert-QueueText $fixture $session START_QUEUE_HOLD_CURRENT
            Assert-QueueQuiet $fixture
            Set-QueueGate $fixture QUEUE_HOLD_CURRENT.release
            Assert-QueueText $fixture $session ACK_QUEUE_HOLD_CURRENT
            (Read-QueueMessage $fixture).id | Should -Be 2
            (Wait-QueueRecord $fixture completion QUEUE_HOLD_CURRENT).reason | Should -Be end_turn
        }
        finally { Stop-QueueFixture $fixture }
    }

    It 'rejects same-session overlap rather than hiding a product queue failure' {
        $fixture = Start-QueueFixture
        try {
            Initialize-QueueFixture $fixture
            $session = New-QueueSession $fixture
            Send-QueuePrompt $fixture $session 2 QUEUE_HOLD_ORIGINAL
            Assert-QueueText $fixture $session START_QUEUE_HOLD_ORIGINAL
            Send-QueuePrompt $fixture $session 3 QUEUE_OVERLAP
            $message = Read-QueueMessage $fixture
            $message.id | Should -Be 3
            $message.error.code | Should -Be -32603
            $message.error.message | Should -Be QUEUE_FIXTURE_OVERLAP
            (Wait-QueueRecord $fixture overlap QUEUE_OVERLAP).sessionId | Should -Be $session
            (Wait-QueueRecord $fixture completion QUEUE_OVERLAP).reason | Should -Be error
            Assert-QueueQuiet $fixture
            Set-QueueGate $fixture QUEUE_HOLD_ORIGINAL.release
            Assert-QueueText $fixture $session ACK_QUEUE_HOLD_ORIGINAL
            (Read-QueueMessage $fixture).id | Should -Be 2
        }
        finally { Stop-QueueFixture $fixture }
    }

    It 'handles close and unknown notifications without unsolicited responses' {
        $fixture = Start-QueueFixture
        try {
            Initialize-QueueFixture $fixture
            $session = New-QueueSession $fixture
            Send-QueuePrompt $fixture $session 2 QUEUE_HOLD_CLOSE
            Assert-QueueText $fixture $session START_QUEUE_HOLD_CLOSE
            Send-QueueRequest $fixture @{ id = 3; method = 'session/close'; params = @{ sessionId = $session } }
            $cancelled = Read-QueueMessage $fixture
            $cancelled.id | Should -Be 2
            $cancelled.error.code | Should -Be -32800
            (Read-QueueMessage $fixture).id | Should -Be 3
            (Wait-QueueRecord $fixture completion QUEUE_HOLD_CLOSE).reason | Should -Be cancelled
            Send-QueuePrompt $fixture $session 4 QUEUE_CLOSED
            (Read-QueueMessage $fixture).error.code | Should -Be -32602

            Send-QueueRequest $fixture @{ method = 'session/close'; params = @{ sessionId = $session } }
            Send-QueueRequest $fixture @{ method = 'unknown/notification'; params = @{} }
            Send-QueueRequest $fixture @{ id = 5; method = 'unknown/request'; params = @{} }
            $barrier = Read-QueueMessage $fixture
            $barrier.id | Should -Be 5
            $barrier.error.code | Should -Be -32601
            Assert-QueueQuiet $fixture
        }
        finally { Stop-QueueFixture $fixture }
    }
}
