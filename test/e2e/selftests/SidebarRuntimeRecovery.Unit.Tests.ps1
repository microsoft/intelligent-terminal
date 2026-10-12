#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }

BeforeDiscovery {
    $modes = foreach ($strict in @('Off', 'Latest')) {
        foreach ($primary in @($false, $true)) {
            @{ Strict = $strict; Primary = $primary }
        }
    }
    $cases = @(
        @{ Fault = 'missing'; Json = ''; Message = 'receipt is missing'; Reads = 0 }
        @{ Fault = 'no-evidence'; Json = ''; Message = 'receipt is missing'; Reads = 0 }
        @{ Fault = 'null'; Json = 'null'; Message = 'not an object'; Reads = 1 }
        @{ Fault = 'empty-document'; Json = ''; Message = 'not an object'; Reads = 1 }
        @{ Fault = 'empty'; Json = '{}'; Message = 'boolean successful'; Reads = 1 }
        @{ Fault = 'empty-array'; Json = '[]'; Message = 'boolean successful'; Reads = 1 }
        @{ Fault = 'single-array'; Json = '[{"settings_preserved":true,"state_preserved":true}]'; Message = 'boolean successful'; Reads = 1 }
        @{ Fault = 'multiple-array'; Json = '[{"settings_preserved":true,"state_preserved":true},{}]'; Message = 'boolean successful'; Reads = 1 }
        @{ Fault = 'scalar'; Json = '"receipt"'; Message = 'boolean successful'; Reads = 1 }
        @{ Fault = 'numeric-scalar'; Json = '1'; Message = 'boolean successful'; Reads = 1 }
        @{ Fault = 'boolean-scalar'; Json = 'true'; Message = 'boolean successful'; Reads = 1 }
        @{ Fault = 'missing-settings'; Json = '{"state_preserved":true}'; Message = 'boolean successful'; Reads = 1 }
        @{ Fault = 'missing-state'; Json = '{"settings_preserved":true}'; Message = 'boolean successful'; Reads = 1 }
        @{ Fault = 'string-settings'; Json = '{"settings_preserved":"false","state_preserved":true}'; Message = 'boolean successful'; Reads = 1 }
        @{ Fault = 'string-state'; Json = '{"settings_preserved":true,"state_preserved":"true"}'; Message = 'boolean successful'; Reads = 1 }
        @{ Fault = 'numeric-settings'; Json = '{"settings_preserved":1,"state_preserved":true}'; Message = 'boolean successful'; Reads = 1 }
        @{ Fault = 'numeric-state'; Json = '{"settings_preserved":true,"state_preserved":1}'; Message = 'boolean successful'; Reads = 1 }
        @{ Fault = 'false-settings'; Json = '{"settings_preserved":false,"state_preserved":true}'; Message = 'boolean successful'; Reads = 1 }
        @{ Fault = 'false-state'; Json = '{"settings_preserved":true,"state_preserved":false}'; Message = 'boolean successful'; Reads = 1 }
        @{ Fault = 'false-both'; Json = '{"settings_preserved":false,"state_preserved":false}'; Message = 'boolean successful'; Reads = 1 }
        @{ Fault = 'null-settings'; Json = '{"settings_preserved":null,"state_preserved":true}'; Message = 'boolean successful'; Reads = 1 }
        @{ Fault = 'null-state'; Json = '{"settings_preserved":true,"state_preserved":null}'; Message = 'boolean successful'; Reads = 1 }
        @{ Fault = 'unreadable'; Json = ''; Message = 'receipt read sentinel'; Reads = 1 }
        @{ Fault = 'invalid-json'; Json = '{broken'; Message = 'Conversion from JSON failed'; Reads = 1 }
        @{ Fault = 'active'; Message = 'Dev active' }
        @{ Fault = 'discovery'; Message = 'discovery sentinel' }
        @{ Fault = 'backup-hash-error'; Message = 'backup hash sentinel' }
        @{ Fault = 'backup-hash-mismatch'; Message = 'Expected'; }
        @{ Fault = 'remove'; Message = 'remove sentinel' }
        @{ Fault = 'copy'; Message = 'copy sentinel' }
        @{ Fault = 'restored-hash-error'; Message = 'restored hash sentinel' }
        @{ Fault = 'restored-hash-mismatch'; Message = 'Expected' }
        @{ Fault = 'output'; Message = 'output sentinel' }
        @{ Fault = 'backup-hash-nonterminating'; Message = 'nonterminating backup-hash sentinel' }
        @{ Fault = 'remove-nonterminating'; Message = 'nonterminating remove sentinel' }
        @{ Fault = 'copy-nonterminating'; Message = 'nonterminating copy sentinel' }
        @{ Fault = 'restored-hash-nonterminating'; Message = 'nonterminating restored-hash sentinel' }
        @{ Fault = 'output-nonterminating'; Message = 'nonterminating output sentinel' }
        @{ Fault = 'valid'; Message = '' }
    )
    $startupCases = @(
        @{ Fault = ''; Runtime = 'missing' }
        @{ Fault = ''; Runtime = 'empty' }
        @{ Fault = ''; Runtime = 'nonempty' }
        @{ Fault = 'evidence-path'; Runtime = 'nonempty' }
        @{ Fault = 'new-item'; Runtime = 'nonempty' }
        @{ Fault = 'runtime-path'; Runtime = 'nonempty' }
        @{ Fault = 'enumeration'; Runtime = 'empty' }
        @{ Fault = 'enumeration'; Runtime = 'nonempty' }
        @{ Fault = 'original-hash'; Runtime = 'nonempty' }
        @{ Fault = 'copy'; Runtime = 'empty' }
        @{ Fault = 'copy'; Runtime = 'nonempty' }
        @{ Fault = 'backup-hash'; Runtime = 'nonempty' }
        @{ Fault = 'manifest'; Runtime = 'missing' }
        @{ Fault = 'manifest'; Runtime = 'empty' }
        @{ Fault = 'manifest'; Runtime = 'nonempty' }
    )
}

BeforeAll {
    $tokens = $null
    $parseErrors = $null
    $suite = Join-Path $PSScriptRoot '..\tests\Feature.SidebarRowAlignment.Tests.ps1'
    $ast = [Management.Automation.Language.Parser]::ParseFile($suite, [ref]$tokens, [ref]$parseErrors)
    if ($parseErrors.Count) { throw 'Cannot parse the actual alignment suite.' }
    $blocks = @($ast.FindAll({
        param($node)
        $node -is [Management.Automation.Language.CommandAst] -and $node.GetCommandName() -eq 'AfterAll'
    }, $true))
    if ($blocks.Count -ne 1) { throw 'Expected exactly one actual alignment AfterAll.' }
    $body = $blocks[0].CommandElements[1].ScriptBlock.Extent.Text
    $script:actualAfterAll = [scriptblock]::Create($body.Substring(1, $body.Length - 2))
    $script:afterAllText = $body
    $startupBlocks = @($ast.FindAll({
        param($node)
        $node -is [Management.Automation.Language.CommandAst] -and $node.GetCommandName() -eq 'BeforeAll'
    }, $true))
    if ($startupBlocks.Count -ne 1) { throw 'Expected exactly one actual alignment BeforeAll.' }
    $statements = @($startupBlocks[0].CommandElements[1].ScriptBlock.EndBlock.Statements)
    $starts = @($statements | Where-Object {
        $_ -is [Management.Automation.Language.IfStatementAst] -and
            $_.Extent.Text -match '^if \(Test-Path -LiteralPath \$script:evidence'
    })
    $ends = @($statements | Where-Object {
        $_.Extent.Text -match '^\$script:runtimeBackedUp = \$true$|original-hashes\.json'
    } | Sort-Object { $_.Extent.EndOffset })
    if ($starts.Count -ne 1 -or $ends.Count -ne 2) { throw 'Cannot isolate the actual startup snapshot statements.' }
    $snapshotStatements = @($statements | Where-Object {
        $_.Extent.StartOffset -ge $starts[0].Extent.StartOffset -and
            $_.Extent.EndOffset -le $ends[-1].Extent.EndOffset
    })
    $script:actualSnapshot = [scriptblock]::Create(
        ($snapshotStatements | ForEach-Object { $_.Extent.Text }) -join "`n")
    $snapshotCommands = @($script:actualSnapshot.Ast.FindAll({
        param($node)
        $node -is [Management.Automation.Language.CommandAst]
    }, $true))
    foreach ($command in $snapshotCommands) {
        if ($command.GetCommandName() -notin @('Test-Path', 'New-Item', 'Out-Null', 'Join-Path',
            'Get-ChildItem', 'Get-FileHash', 'Copy-Item', 'Should', 'ConvertTo-Json', 'Set-Content')) {
            throw "Unexpected command in startup snapshot: $($command.GetCommandName())"
        }
    }

    # Only these two commands are unavailable without the live framework.
    function Invoke-SidebarSessionCleanup {
        param($App, $Target, $OwnsConfigBackup, $SettingsHash, $StateHash, $Evidence)
        throw 'Unmocked cleanup is forbidden.'
    }
    function Get-WtProcessesForApp {
        param($App, [switch]$IncludePackageExecutables)
        throw 'Unmocked process discovery is forbidden.'
    }
    function Join-Path {
        param($Path, $ChildPath)
        throw 'Unmocked path resolution is forbidden.'
    }
    function Test-Path {
        [CmdletBinding()]
        param($LiteralPath)
        throw 'Unmocked filesystem probe is forbidden.'
    }
    function Get-Content {
        [CmdletBinding()]
        param($LiteralPath, [switch]$Raw)
        throw 'Unmocked filesystem read is forbidden.'
    }
    function Get-FileHash {
        [CmdletBinding()]
        param($LiteralPath)
        throw 'Unmocked filesystem hash is forbidden.'
    }
    function Remove-Item {
        [CmdletBinding()]
        param($LiteralPath, [switch]$Recurse, [switch]$Force)
        throw 'Unmocked filesystem removal is forbidden.'
    }
    function Copy-Item {
        [CmdletBinding()]
        param($LiteralPath, $Destination, [switch]$Recurse)
        throw 'Unmocked filesystem copy is forbidden.'
    }
    function Set-Content {
        [CmdletBinding()]
        param($LiteralPath, [Parameter(ValueFromPipeline)]$Value)
        process { throw 'Unmocked filesystem write is forbidden.' }
    }
    function New-Item {
        [CmdletBinding()]
        param($ItemType, $Path)
        throw 'Unmocked filesystem creation is forbidden.'
    }
    function Get-ChildItem {
        [CmdletBinding()]
        param($LiteralPath, [switch]$File, [switch]$Recurse, [switch]$Force)
        throw 'Unmocked filesystem enumeration is forbidden.'
    }
    function Invoke-ActualSnapshot {
        param([string]$Strict)
        try {
            $ErrorActionPreference = 'Continue'
            if ($Strict -eq 'Off') { Set-StrictMode -Off }
            else { Set-StrictMode -Version Latest }
            & $script:actualSnapshot
            $null
        }
        catch { $_ }
        finally { Set-StrictMode -Off }
    }
    function Invoke-ActualRecovery {
        param([string]$Strict)
        try {
            $ErrorActionPreference = 'Continue'
            if ($Strict -eq 'Off') { Set-StrictMode -Off }
            else { Set-StrictMode -Version Latest }
            & $script:actualAfterAll
            $null
        }
        catch { $_ }
        finally { Set-StrictMode -Off }
    }
}

Describe 'Actual sidebar startup snapshot: StrictMode <Strict>' -Tag Unit -ForEach @(
    @{ Strict = 'Off' }; @{ Strict = 'Latest' }
) {
    BeforeEach {
        $script:evidence = 'Z:\fictitious-owned\evidence'
        $script:target = [pscustomobject]@{ LocalStateDir = 'Z:\fictitious-owned\state' }
        $script:runtimePath = 'Z:\fictitious-owned\state\IntelligentTerminal'
        $script:runtimeBackup = 'Z:\fictitious-owned\evidence\original-runtime'
        $script:runtimeBackedUp = $false
        $script:runtimeExisted = $false
        $script:runtimeHashes = @{}
        $script:settingsHash = 'SETTINGS-SENTINEL'
        $script:stateHash = 'STATE-SENTINEL'
        $script:startupFault = ''
        $script:startupRuntime = 'nonempty'
        $script:startupEvents = [Collections.Generic.List[string]]::new()
        $script:startupBindings = @{}
        function Write-StartupFault {
            param($Phase)
            if ($script:startupFault -eq $Phase) {
                Write-Error "startup $Phase sentinel"
                $script:startupEvents.Add('continued-after-fault')
            }
        }
        Mock Join-Path { param($Path, $ChildPath) "$Path\$ChildPath" }
        Mock Test-Path {
            param($LiteralPath, $ErrorAction)
            $phase = if ($LiteralPath -eq $script:evidence) { 'evidence-path' }
                elseif ($LiteralPath -eq $script:runtimePath) { 'runtime-path' }
                else { throw "Unexpected startup probe: $LiteralPath" }
            $script:startupBindings[$phase] = $ErrorAction
            $script:startupEvents.Add($phase)
            Write-StartupFault $phase
            if ($phase -eq 'evidence-path') { $false } else { $script:startupRuntime -ne 'missing' }
        }
        Mock New-Item {
            param($ItemType, $Path, $ErrorAction)
            $ItemType | Should -Be 'Directory'
            $Path | Should -Be $script:evidence
            $script:startupBindings['new-item'] = $ErrorAction
            $script:startupEvents.Add('new-item')
            Write-StartupFault 'new-item'
        }
        Mock Get-ChildItem {
            param($LiteralPath, [switch]$File, [switch]$Recurse, [switch]$Force, $ErrorAction)
            $LiteralPath | Should -Be $script:runtimePath
            $File | Should -BeTrue
            $Recurse | Should -BeTrue
            $Force | Should -BeTrue
            $script:startupBindings['enumeration'] = $ErrorAction
            $script:startupEvents.Add('enumeration')
            Write-StartupFault 'enumeration'
            if ($script:startupRuntime -eq 'nonempty') {
                [pscustomobject]@{ FullName = "$script:runtimePath\original.json" }
            }
        }
        Mock Get-FileHash {
            param($LiteralPath, $ErrorAction)
            $phase = if ($LiteralPath -eq "$script:runtimePath\original.json") { 'original-hash' }
                elseif ($LiteralPath -eq "$script:runtimeBackup\original.json") { 'backup-hash' }
                else { throw "Unexpected startup hash: $LiteralPath" }
            $script:startupBindings[$phase] = $ErrorAction
            $script:startupEvents.Add($phase)
            Write-StartupFault $phase
            [pscustomobject]@{ Hash = 'NONEMPTY-HASH-SENTINEL' }
        }
        Mock Copy-Item {
            param($LiteralPath, $Destination, [switch]$Recurse, $ErrorAction)
            $LiteralPath | Should -Be $script:runtimePath
            $Destination | Should -Be $script:runtimeBackup
            $Recurse | Should -BeTrue
            $script:startupBindings['copy'] = $ErrorAction
            $script:startupEvents.Add('copy')
            Write-StartupFault 'copy'
        }
        Mock Set-Content {
            param($LiteralPath, $Value, $ErrorAction)
            $LiteralPath | Should -Be "$script:evidence\original-hashes.json"
            $script:runtimeBackedUp | Should -BeFalse
            $script:startupBindings['manifest'] = $ErrorAction
            $script:startupEvents.Add('manifest')
            Write-StartupFault 'manifest'
            $manifest = $Value | ConvertFrom-Json
            $manifest.settings | Should -Be $script:settingsHash
            $manifest.state | Should -Be $script:stateHash
            $script:startupEvents.Add('manifest-written')
        }
    }

    It 'keeps eligibility fail-closed for fault <Fault> and runtime <Runtime>' -ForEach $startupCases {
        $script:startupFault = $Fault
        $script:startupRuntime = $Runtime
        $failure = Invoke-ActualSnapshot $Strict
        if ($Fault) {
            $failure.Exception.Message | Should -Match "startup $Fault sentinel"
            $script:runtimeBackedUp | Should -BeFalse
            $script:startupEvents | Should -Not -Contain 'manifest-written'
            $script:startupEvents[-1] | Should -Be $Fault
            $script:startupBindings[$Fault] | Should -Be 'Stop'
        }
        else {
            $failure | Should -BeNullOrEmpty
            $script:runtimeBackedUp | Should -BeTrue
            $script:startupEvents[-1] | Should -Be 'manifest-written'
            $expected = 'evidence-path,new-item,runtime-path'
            if ($Runtime -eq 'empty') { $expected += ',enumeration,copy' }
            if ($Runtime -eq 'nonempty') { $expected += ',enumeration,original-hash,copy,backup-hash' }
            ($script:startupEvents -join ',') | Should -Be "$expected,manifest,manifest-written"
            $script:runtimeExisted | Should -Be ($Runtime -ne 'missing')
            $script:runtimeHashes.Count | Should -Be $(if ($Runtime -eq 'nonempty') { 1 } else { 0 })
        }
        $script:startupEvents | Should -Not -Contain 'continued-after-fault'
        foreach ($bound in $script:startupBindings.Values) { $bound | Should -Be 'Stop' }
    }
}

Describe 'Actual sidebar runtime AfterAll: StrictMode <Strict>, primary <Primary>' -Tag Unit -ForEach $modes {
    BeforeEach {
        $script:app = [pscustomobject]@{ Pid = 123; Hwnd = 456 }
        $script:target = [pscustomobject]@{ Name = 'fictitious-owned-Dev' }
        $script:ownsConfigBackup = $true
        $script:runtimeBackedUp = $true
        $script:settingsHash = 'SETTINGS-SENTINEL'
        $script:stateHash = 'STATE-SENTINEL'
        $script:evidence = 'Z:\fictitious-owned\evidence'
        $script:runtimeBackup = 'Z:\fictitious-owned\snapshot'
        $script:runtimePath = 'Z:\fictitious-owned\runtime'
        $script:runtimeHashes = @{ 'sessions\original.json' = 'NONEMPTY-HASH-SENTINEL' }
        $script:runtimeExisted = $true
        $script:fault = 'valid'
        $script:json = '{"settings_preserved":true,"state_preserved":true}'
        $script:primary = $Primary
        $script:originalException = [InvalidOperationException]::new('original screenshot sentinel')
        $script:readException = [IO.IOException]::new('receipt read sentinel')
        $script:events = [Collections.Generic.List[string]]::new()
        $script:boundErrorActions = @{}

        Mock Invoke-SidebarSessionCleanup {
            $script:events.Add('cleanup')
            if ($script:primary) { throw $script:originalException }
        }
        Mock Join-Path { param($Path, $ChildPath) "$Path\$ChildPath" }
        Mock Test-Path {
            param($LiteralPath)
            if ($LiteralPath -eq "$script:evidence\cleanup.json") { return $script:fault -ne 'missing' }
            if ($LiteralPath -eq $script:runtimePath) { return $true }
            throw "Unexpected path probe: $LiteralPath"
        }
        Mock Get-Content {
            param($LiteralPath, [switch]$Raw, $ErrorAction)
            $LiteralPath | Should -Be "$script:evidence\cleanup.json"
            $Raw | Should -BeTrue
            $ErrorAction | Should -Be 'Stop'
            $script:events.Add('read')
            if ($script:fault -eq 'unreadable') { throw $script:readException }
            $script:json
        }
        Mock Get-WtProcessesForApp {
            param($App, [switch]$IncludePackageExecutables)
            $App | Should -Be $script:target
            $IncludePackageExecutables | Should -BeTrue
            $script:events.Add('discovery')
            if ($script:fault -eq 'discovery') { throw 'discovery sentinel' }
            if ($script:fault -eq 'active') { [pscustomobject]@{ Id = 123 } }
        }
        Mock Get-FileHash {
            param($LiteralPath, $ErrorAction)
            $phase = if ($LiteralPath -eq "$script:runtimeBackup\sessions\original.json") { 'backup' }
                elseif ($LiteralPath -eq "$script:runtimePath\sessions\original.json") { 'restored' }
                else { throw "Unexpected hash path: $LiteralPath" }
            $script:events.Add("$phase-hash")
            $script:boundErrorActions["$phase-hash"] = $ErrorAction
            if ($script:fault -eq "$phase-hash-nonterminating") {
                Write-Error "nonterminating $phase-hash sentinel"
                $script:events.Add('continued-after-fault')
            }
            if ($script:fault -eq "$phase-hash-error") { throw "$phase hash sentinel" }
            $hash = if ($script:fault -eq "$phase-hash-mismatch") { 'CORRUPT' } else { 'NONEMPTY-HASH-SENTINEL' }
            [pscustomobject]@{ Hash = $hash }
        }
        Mock Remove-Item {
            param($LiteralPath, [switch]$Recurse, [switch]$Force, $ErrorAction)
            $LiteralPath | Should -Be $script:runtimePath
            $Recurse | Should -BeTrue
            $Force | Should -BeTrue
            $script:events.Add('remove')
            $script:boundErrorActions['remove'] = $ErrorAction
            if ($script:fault -eq 'remove-nonterminating') {
                Write-Error 'nonterminating remove sentinel'
                $script:events.Add('continued-after-fault')
            }
            if ($script:fault -eq 'remove') { throw 'remove sentinel' }
        }
        Mock Copy-Item {
            param($LiteralPath, $Destination, [switch]$Recurse, $ErrorAction)
            $LiteralPath | Should -Be $script:runtimeBackup
            $Destination | Should -Be $script:runtimePath
            $Recurse | Should -BeTrue
            $script:events.Add('copy')
            $script:boundErrorActions['copy'] = $ErrorAction
            if ($script:fault -eq 'copy-nonterminating') {
                Write-Error 'nonterminating copy sentinel'
                $script:events.Add('continued-after-fault')
            }
            if ($script:fault -eq 'copy') { throw 'copy sentinel' }
        }
        Mock Set-Content {
            param($LiteralPath, $Value, $ErrorAction)
            $LiteralPath | Should -Be "$script:evidence\runtime-cleanup.json"
            $receipt = $Value | ConvertFrom-Json
            $receipt.restored | Should -BeTrue
            $receipt.hashes.'sessions\original.json' | Should -Be 'NONEMPTY-HASH-SENTINEL'
            $script:events.Add('output')
            $script:boundErrorActions['output'] = $ErrorAction
            if ($script:fault -eq 'output-nonterminating') {
                Write-Error 'nonterminating output sentinel'
                $script:events.Add('continued-after-fault')
            }
            if ($script:fault -eq 'output') { throw 'output sentinel' }
            $script:events.Add('receipt-written')
        }
    }

    AfterEach {
        Should -Invoke Invoke-SidebarSessionCleanup -Exactly -Times 1
        Should -Invoke Remove-Item -Exactly -Times 0 -ParameterFilter { $LiteralPath -ne $script:runtimePath }
        Should -Invoke Copy-Item -Exactly -Times 0 -ParameterFilter {
            $LiteralPath -ne $script:runtimeBackup -or $Destination -ne $script:runtimePath
        }
        Should -Invoke Set-Content -Exactly -Times 0 -ParameterFilter {
            $LiteralPath -ne "$script:evidence\runtime-cleanup.json"
        }
    }

    It 'preserves errors and the snapshot for <Fault>' -ForEach $cases {
        $script:fault = $Fault
        if ($_.ContainsKey('Json')) { $script:json = $Json }
        if ($Fault -eq 'no-evidence') { $script:evidence = '' }
        $failure = Invoke-ActualRecovery $Strict
        if ($Fault -like '*-nonterminating') {
            $phase = $Fault -replace '-nonterminating$', ''
            $script:boundErrorActions[$phase] | Should -Be 'Stop' -Because (
                'actual recovery events: ' + ($script:events -join ','))
        }

        if ($Fault -eq 'valid') {
            if ($Primary) {
                [object]::ReferenceEquals($failure.Exception, $script:originalException) | Should -BeTrue
            }
            else { $failure | Should -BeNullOrEmpty }
        }
        else {
            $failure | Should -Not -BeNullOrEmpty
            $additional = $failure.Exception
            if ($Primary) {
                $failure.Exception | Should -BeOfType ([AggregateException])
                $failure.Exception.InnerExceptions.Count | Should -Be 2
                [object]::ReferenceEquals($failure.Exception.InnerExceptions[0], $script:originalException) | Should -BeTrue
                $additional = $failure.Exception.InnerExceptions[1]
            }
            $additional.Message | Should -Match $Message
            if ($Fault -eq 'unreadable') {
                [object]::ReferenceEquals($additional, $script:readException) | Should -BeTrue
            }
            if ($Fault -eq 'invalid-json') {
                $additional | Should -BeOfType ([ArgumentException])
            }
        }

        $receiptRejected = $_.ContainsKey('Reads')
        $discover = if ($receiptRejected) { 0 } else { 1 }
        $backupHash = if ($receiptRejected -or $Fault -in @('active', 'discovery')) { 0 } else { 1 }
        $remove = if (-not $backupHash -or $Fault -in @('backup-hash-error', 'backup-hash-mismatch', 'backup-hash-nonterminating')) { 0 } else { 1 }
        $copy = if (-not $remove -or $Fault -in @('remove', 'remove-nonterminating')) { 0 } else { 1 }
        $restoredHash = if (-not $copy -or $Fault -in @('copy', 'copy-nonterminating')) { 0 } else { 1 }
        $output = if (-not $restoredHash -or $Fault -in @('restored-hash-error', 'restored-hash-mismatch', 'restored-hash-nonterminating')) { 0 } else { 1 }
        $reads = if ($receiptRejected) { $Reads } else { 1 }
        Should -Invoke Get-Content -Exactly -Times $reads
        Should -Invoke Get-WtProcessesForApp -Exactly -Times $discover
        Should -Invoke Get-FileHash -Exactly -Times $backupHash -ParameterFilter {
            $LiteralPath -eq "$script:runtimeBackup\sessions\original.json"
        }
        Should -Invoke Get-FileHash -Exactly -Times $restoredHash -ParameterFilter {
            $LiteralPath -eq "$script:runtimePath\sessions\original.json"
        }
        Should -Invoke Remove-Item -Exactly -Times $remove
        Should -Invoke Copy-Item -Exactly -Times $copy
        Should -Invoke Set-Content -Exactly -Times $output
        $script:events | Should -Not -Contain 'continued-after-fault'
        if ($Fault -ne 'valid') { $script:events | Should -Not -Contain 'receipt-written' }
        if ($Fault -eq 'valid') {
            ($script:events -join ',') | Should -Be 'cleanup,read,discovery,backup-hash,remove,copy,restored-hash,output,receipt-written'
        }
    }

    It 'fails closed on a nonterminating <Probe> recovery path probe' -TestCases @(
        @{ Probe = 'receipt' },
        @{ Probe = 'runtime' }
    ) {
        param($Probe)
        $script:failingProbe = if ($Probe -eq 'receipt') { "$script:evidence\cleanup.json" } else { $script:runtimePath }
        $script:probeReturned = $false
        $script:probeErrorAction = $null
        Mock Test-Path {
            param($LiteralPath, $ErrorAction)
            if ($LiteralPath -eq $script:failingProbe) {
                $script:probeErrorAction = $ErrorAction
                Write-Error 'recovery path probe sentinel'
                $script:probeReturned = $true
                return $false
            }
            $true
        }
        $failure = Invoke-ActualRecovery $Strict
        $failure | Should -Not -BeNullOrEmpty
        if ($Primary) {
            [object]::ReferenceEquals($failure.Exception.InnerExceptions[0], $script:originalException) | Should -BeTrue
            $failure.Exception.InnerExceptions[1].Message | Should -Match 'recovery path probe sentinel'
        }
        else { $failure.Exception.Message | Should -Match 'recovery path probe sentinel' }
        $script:probeReturned | Should -BeFalse
        $script:probeErrorAction | Should -Be 'Stop'
        Should -Invoke Remove-Item -Exactly -Times 0
        Should -Invoke Copy-Item -Exactly -Times 0
        Should -Invoke Set-Content -Exactly -Times 0
    }

    It 'ignores receipts for unowned or incomplete backups (<Owned>, <Backed>)' -ForEach @(
        @{ Owned = $false; Backed = $true }
        @{ Owned = $true; Backed = $false }
        @{ Owned = $false; Backed = $false }
    ) {
        $script:ownsConfigBackup = $Owned
        $script:runtimeBackedUp = $Backed
        $script:fault = 'unreadable'
        $failure = Invoke-ActualRecovery $Strict
        if ($Primary) {
            [object]::ReferenceEquals($failure.Exception, $script:originalException) | Should -BeTrue
        }
        else { $failure | Should -BeNullOrEmpty }
        foreach ($command in @('Join-Path', 'Test-Path', 'Get-Content', 'Get-WtProcessesForApp',
            'Get-FileHash', 'Remove-Item', 'Copy-Item', 'Set-Content')) {
            Should -Invoke $command -Exactly -Times 0
        }
        ($script:events -join ',') | Should -Be 'cleanup'
    }

    It 'uses the real JSON parser with NoEnumerate to reject even a singleton array' {
        $script:afterAllText | Should -Match 'ConvertFrom-Json -NoEnumerate -ErrorAction Stop'
        $object = '{"settings_preserved":true,"state_preserved":true}' | ConvertFrom-Json -NoEnumerate
        $array = '[{"settings_preserved":true,"state_preserved":true}]' | ConvertFrom-Json -NoEnumerate
        $object -is [pscustomobject] | Should -BeTrue
        $array -is [array] | Should -BeTrue
        $array | Should -HaveCount 1
        $unwrapped = '[{"settings_preserved":true,"state_preserved":true}]' | ConvertFrom-Json
        $unwrapped -is [array] | Should -BeFalse
        $unwrapped.settings_preserved | Should -BeTrue
        $unwrapped.state_preserved | Should -BeTrue
        $script:json = '[{"settings_preserved":true,"state_preserved":true}]'
        $failure = Invoke-ActualRecovery $Strict
        $failure.Exception.Message | Should -Match 'boolean successful'
        Should -Invoke Get-WtProcessesForApp -Exactly -Times 0
        Should -Invoke Remove-Item -Exactly -Times 0
        Should -Invoke Copy-Item -Exactly -Times 0
    }
}
