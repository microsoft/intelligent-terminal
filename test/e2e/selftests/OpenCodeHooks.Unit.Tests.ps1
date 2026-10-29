#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }

BeforeAll {
    Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
    $suite = [Management.Automation.Language.Parser]::ParseFile(
        (Join-Path $PSScriptRoot '..\tests\Feature.OpenCodeHookLifecycle.Tests.ps1'), [ref]$null, [ref]$null)
    $ownershipGuard = $suite.Find({
        param($node)
        $node -is [Management.Automation.Language.FunctionDefinitionAst] -and
            $node.Name -eq 'Assert-DisposableHookFixture'
    }, $true)
    . ([scriptblock]::Create($ownershipGuard.Extent.Text))
    $script:nativeCases = @{}
    foreach ($command in $suite.FindAll({
        param($node)
        $node -is [Management.Automation.Language.CommandAst] -and $node.GetCommandName() -eq 'It'
    }, $true)) {
        $script:nativeCases[$command.CommandElements[1].Value] = $command.CommandElements[2].ScriptBlock.GetScriptBlock()
    }
    $self = [Management.Automation.Language.Parser]::ParseFile(
        (Join-Path $PSScriptRoot 'OpenCodeHooks.Unit.Tests.ps1'), [ref]$null, [ref]$null)
    $componentCase = $self.Find({
        param($node)
        $node -is [Management.Automation.Language.CommandAst] -and $node.GetCommandName() -eq 'It' -and
            $node.CommandElements[1].Value -eq 'executes both plugin entrypoints and lifecycle regressions'
    }, $true)
    $script:nativeCases['Node component wrapper'] = $componentCase.CommandElements[2].ScriptBlock.GetScriptBlock()
    $script:nativeLoaderCase = 'OpenCode CLI loads copied hooks and native V2 TUI binds a viewed session'
    $script:nativeCases[$script:nativeLoaderCase] | Should -Not -BeNullOrEmpty -Because 'the native loader regression must track its current E2E title'
    $tracking = [Management.Automation.Language.Parser]::ParseFile(
        (Join-Path $PSScriptRoot '..\tests\Feature.OpenCodeHooks.Tests.ps1'), [ref]$null, [ref]$null)
    foreach ($name in @('BeforeAll', 'AfterAll')) {
        $block = $tracking.Find({
            param($node)
            $node -is [Management.Automation.Language.CommandAst] -and $node.GetCommandName() -eq $name
        }, $true)
        Set-Variable -Name "tracking$name" -Scope Script -Value $block.CommandElements[1].ScriptBlock.GetScriptBlock()
    }
    $bridge = [Management.Automation.Language.Parser]::ParseFile(
        (Join-Path $PSScriptRoot '..\tests\Feature.OpenCodeHookBridge.Tests.ps1'), [ref]$null, [ref]$null)
    $bridgeCleanup = $bridge.Find({
        param($node)
        $node -is [Management.Automation.Language.CommandAst] -and $node.GetCommandName() -eq 'AfterAll'
    }, $true)
    $script:bridgeAfterAll = $bridgeCleanup.CommandElements[1].ScriptBlock.GetScriptBlock()
    $events = $bridge.Find({
        param($node)
        $node -is [Management.Automation.Language.CommandAst] -and $node.GetCommandName() -eq 'Get-WtEvents' -and
            $node.Extent.Text -match 'agent\.session\.start'
    }, $true)
    $script:bridgePredicate = $events.CommandElements[-1].ScriptBlock.GetScriptBlock()
}

Describe 'Unit: OpenCode bridge teardown' -Tag 'Unit' {
    BeforeAll {
        function opencode { throw 'The real OpenCode CLI must never run in teardown selftests' }
    }

    BeforeEach {
        $script:previousBridgeApp = $script:app
        $script:previousBridgeSession = $script:fixtureSessionId
        $script:previousBridgeDir = $script:fixtureDir
        $script:previousBridgeExitCode = $global:LASTEXITCODE
        $script:app = [pscustomobject]@{ Pid = 123 }
        $script:fixtureSessionId = 'fixture-session'
        $script:fixtureDir = 'fixture-directory'
        Mock opencode { $global:LASTEXITCODE = 0 }
        Mock Test-Path { $true } -ParameterFilter { $Path -eq 'fixture-directory' }
        Mock Remove-Item {} -ParameterFilter { $LiteralPath -eq 'fixture-directory' }
        Mock Stop-Terminal {}
    }

    AfterEach {
        $script:app = $script:previousBridgeApp
        $script:fixtureSessionId = $script:previousBridgeSession
        $script:fixtureDir = $script:previousBridgeDir
        $global:LASTEXITCODE = $script:previousBridgeExitCode
    }

    It 'stops Terminal and surfaces a failed fixture session removal' {
        Mock opencode { $global:LASTEXITCODE = 1 }
        { & $script:bridgeAfterAll } | Should -Throw '*only the fixture session should be removed*'
        Should -Invoke Stop-Terminal -Times 1 -Exactly -ParameterFilter { $App -eq $script:app }
        Should -Invoke Remove-Item -Times 0 -Exactly -ParameterFilter { $LiteralPath -eq 'fixture-directory' }
    }

    It 'stops Terminal and preserves a fixture directory cleanup failure' {
        Mock Remove-Item { throw 'Fixture directory cleanup failed' } -ParameterFilter { $LiteralPath -eq 'fixture-directory' }
        { & $script:bridgeAfterAll } | Should -Throw '*Fixture directory cleanup failed*'
        Should -Invoke Stop-Terminal -Times 1 -Exactly -ParameterFilter { $App -eq $script:app }
    }

    It 'preserves successful cleanup with fixture session present: <HasSession>' -TestCases @(
        @{ HasSession = $true },
        @{ HasSession = $false }
    ) {
        param($HasSession)
        if (-not $HasSession) { $script:fixtureSessionId = $null }
        { & $script:bridgeAfterAll } | Should -Not -Throw
        Should -Invoke opencode -Times ([int]$HasSession) -Exactly
        Should -Invoke Remove-Item -Times 1 -Exactly -ParameterFilter { $LiteralPath -eq 'fixture-directory' -and $Recurse -and $Force }
        Should -Invoke Stop-Terminal -Times 1 -Exactly -ParameterFilter { $App -eq $script:app }
    }
}

Describe 'Unit: OpenCode hook live-test isolation' -Tag 'Unit' {
    It 'refuses an existing user instance before injecting <Case>' -TestCases @(
        @{ Case = 'OpenCode hook entrypoints deliver lifecycle to WTA' },
        @{ Case = 'OpenCode hook suppression prevents ghost sessions' }
    ) {
        param($Case)
        $previousApp = $script:app
        $previousNode = $script:node
        try {
            $script:app = [pscustomobject]@{ Pid = 123; Launched = $false }
            $script:node = 'fixture-node.exe'
            { & $script:nativeCases[$Case] } | Should -Throw '*harness-owned disposable Terminal*'
        }
        finally {
            $script:app = $previousApp
            $script:node = $previousNode
        }
    }

    It 'does not treat a launched flag without a creation-owned process as disposable' {
        $app = [pscustomobject]@{ Pid = 123; Launched = $true }
        Mock Get-Process { throw 'An unproven process must not be inspected or adopted' }
        { Assert-DisposableHookFixture $app } | Should -Throw '*harness-owned disposable Terminal*'
        Should -Invoke Get-Process -Times 0 -Exactly
    }

    It 'rejects a recycled process before allowing hook injection' {
        $start = [DateTime]'2026-01-01T00:00:00Z'
        $app = [pscustomobject]@{
            Pid = 123
            Launched = $true
            InstallLocation = $TestDrive
            OwnedProcess = [pscustomobject]@{ Id = 123; HasExited = $false; StartTime = $start }
        }
        Mock Get-Process {
            [pscustomobject]@{
                StartTime = $start.AddSeconds(1)
                Path = Join-Path $TestDrive 'WindowsTerminal.exe'
            }
        }
        { Assert-DisposableHookFixture $app } | Should -Throw '*harness-owned disposable Terminal*'
    }

    It 'restores all V2 plugin files after the V1 installer removes them' {
        $previousConfig = $env:XDG_CONFIG_HOME
        $previousApp = $script:app
        $env:XDG_CONFIG_HOME = Join-Path $TestDrive 'config'
        $plugins = Join-Path $env:XDG_CONFIG_HOME 'opencode\plugins'
        $original = @{}
        foreach ($relative in @(
            'wt-agent-hooks.js', 'wt-agent-hooks\bridge.js', 'wt-agent-hooks\plugin.json',
            'wt-agent-hooks\send-event.ps1', 'wt-agent-hooks\wt-agent-hooks.js',
            'wt-agent-hooks\server.js', 'wt-agent-hooks\rpc.js', 'wt-agent-hooks\tui.js',
            'wt-agent-hooks\package.json', 'wt-agent-hooks\wt-agent-hooks\rpc.js'
        )) {
            $path = Join-Path $plugins $relative
            New-Item -ItemType Directory -Path (Split-Path $path) -Force | Out-Null
            $original[$path] = [Text.Encoding]::UTF8.GetBytes("original $relative`r`n")
            [IO.File]::WriteAllBytes($path, $original[$path])
        }
        $pwsh = (Get-Command pwsh).Source
        Mock Get-Command { [pscustomobject]@{ Source = $pwsh } } -ParameterFilter { $Name -eq 'opencode' }
        try {
            & $script:trackingBeforeAll
            foreach ($path in $original.Keys) { Remove-Item -LiteralPath $path }
            & $script:trackingAfterAll
            foreach ($path in $original.Keys) {
                Test-Path -LiteralPath $path -PathType Leaf | Should -BeTrue
                [Convert]::ToBase64String([IO.File]::ReadAllBytes($path)) |
                    Should -Be ([Convert]::ToBase64String($original[$path]))
            }
        }
        finally {
            $script:app = $previousApp
            if ($null -eq $previousConfig) { Remove-Item Env:\XDG_CONFIG_HOME -ErrorAction SilentlyContinue }
            else { $env:XDG_CONFIG_HOME = $previousConfig }
        }
    }

    It 'requires the fixture session and pane for a V2 bridge event (<SessionId>, <Pane>)' -TestCases @(
        @{ SessionId = 'fixture-session'; Pane = 'fixture-pane'; Expected = 1 },
        @{ SessionId = 'restored-session'; Pane = 'fixture-pane'; Expected = 0 },
        @{ SessionId = 'fixture-session'; Pane = 'other-pane'; Expected = 0 }
    ) {
        param($SessionId, $Pane, $Expected)
        $script:fixtureSessionId = 'fixture-session'
        $paneId = 'fixture-pane'
        $event = [pscustomobject]@{
            method = 'agent_event'
            params = [pscustomobject]@{
                agent_session_id = $SessionId
                pane_id = $Pane
                cli_source = 'opencode'
                event = 'agent.session.start'
            }
        }
        @($event | Where-Object -FilterScript $script:bridgePredicate).Count | Should -Be $Expected
    }
}

Describe 'Unit: OpenCode hook component behavior' -Tag 'Unit' {
    It 'executes both plugin entrypoints and lifecycle regressions' {
        $node = Get-Command node -ErrorAction SilentlyContinue
        if (-not $node) {
            Set-ItResult -Skipped -Because 'Node.js is not installed for the JavaScript component regressions'
            return
        }
        $result = Invoke-Native -FilePath $node.Source -Arguments @(
            '--experimental-vm-modules', '--test', '--test-reporter=tap', (Join-Path $PSScriptRoot 'OpenCodeHooks.test.mjs')
        ) -TimeoutSec 30
        $result.TimedOut | Should -BeFalse
        $result.ExitCode | Should -Be 0 -Because "$($result.StdOut)`n$($result.StdErr)"
        $result.StdOut | Should -Match '(?m)^# pass [1-9][0-9]*\s*$'
        $result.StdOut | Should -Match '# skipped 0'
    }

    It 'skips only the Node-dependent <Case> when Node.js is absent' -TestCases @(
        @{ Case = 'Node component wrapper' },
        @{ Case = 'OpenCode hook entrypoints deliver lifecycle to WTA' },
        @{ Case = 'OpenCode hook suppression prevents ghost sessions' }
    ) {
        param($Case)
        $previous = $script:node
        try {
            $script:node = $null
            Mock Get-Command { $null } -ParameterFilter { $Name -eq 'node' }
            Mock Set-ItResult {}
            Mock Invoke-Native { throw 'Node-dependent execution must not start' }
            & $script:nativeCases[$Case]
            Should -Invoke Set-ItResult -Times 1 -Exactly -ParameterFilter { $Skipped -and $Because -match 'Node.js is not installed' }
            Should -Invoke Invoke-Native -Times 0 -Exactly
        }
        finally { $script:node = $previous }
    }

    It 'skips the <Case> case when the optional native CLI is absent' -TestCases @(
        @{ Case = 'OpenCode copied hooks upgrade without force' },
        @{ Case = 'OpenCode CLI loads copied hooks and native V2 TUI binds a viewed session' }
    ) {
        param($Case)
        $previous = $env:ITE2E_OPENCODE_EXE
        try {
            Remove-Item Env:\ITE2E_OPENCODE_EXE -ErrorAction SilentlyContinue
            Mock Get-Command { $null } -ParameterFilter { $Name -eq 'opencode' }
            Mock Set-ItResult {}
            Mock Invoke-Native { throw 'Native execution must not start without the prerequisite' }
            & $script:nativeCases[$Case]
            Should -Invoke Set-ItResult -Times 1 -Exactly -ParameterFilter { $Skipped -and $Because -match 'opencode CLI is not installed' }
            Should -Invoke Invoke-Native -Times 0 -Exactly
        }
        finally {
            if ($null -eq $previous) { Remove-Item Env:\ITE2E_OPENCODE_EXE -ErrorAction SilentlyContinue }
            else { $env:ITE2E_OPENCODE_EXE = $previous }
        }
    }

    It 'uses an explicit native executable without requiring an OpenCode PATH command' {
        $previous = $env:ITE2E_OPENCODE_EXE
        $override = Join-Path $TestDrive 'opencode-override.exe'
        Set-Content -LiteralPath $override -Value ''
        try {
            $env:ITE2E_OPENCODE_EXE = $override
            Mock Get-Command { throw 'PATH discovery must not precede the override' } -ParameterFilter { $Name -eq 'opencode' }
            Mock Invoke-Native { throw 'Reached selected native executable' }
            { & $script:nativeCases[$script:nativeLoaderCase] } | Should -Throw '*Reached selected native executable*'
            Should -Invoke Get-Command -Times 0 -Exactly -ParameterFilter { $Name -eq 'opencode' }
            Should -Invoke Invoke-Native -Times 1 -Exactly -ParameterFilter { $FilePath -eq $override }
        }
        finally {
            if ($null -eq $previous) { Remove-Item Env:\ITE2E_OPENCODE_EXE -ErrorAction SilentlyContinue }
            else { $env:ITE2E_OPENCODE_EXE = $previous }
        }
    }

    It 'copies the selected packaged native adapter without changing default OpenCode discovery' {
        $previousOverride = $env:ITE2E_OPENCODE_EXE
        $previousPath = $env:PATH
        $previousApp = $script:app
        $previousRoots = $script:nativeRoots
        $pathDirectory = Join-Path $TestDrive 'path-v1'
        New-Item -ItemType Directory -Path $pathDirectory | Out-Null
        Set-Content -LiteralPath (Join-Path $pathDirectory 'opencode.cmd') -Value '@echo 1.0.0'
        $override = (Get-Command pwsh).Source
        try {
            $env:PATH = "$pathDirectory$([IO.Path]::PathSeparator)$previousPath"
            $env:ITE2E_OPENCODE_EXE = $override
            $script:app = [pscustomobject]@{
                WtaPath = 'fixture-wta.exe'
                InstallLocation = $TestDrive
                ComClsid = 'fixture-clsid'
            }
            $script:nativeRoots = [Collections.Generic.List[string]]::new()
            $oldBundle = $script:bundlePath
            $script:bundlePath = Join-Path $TestDrive 'bundle'
            foreach ($relative in @('shared\bridge.js', 'plugin.json', 'v2\wt-agent-hooks.js', 'v2\server.js', 'v2\rpc.js', 'v2\tui.js', 'v2\package.json')) {
                $path = Join-Path $script:bundlePath $relative
                New-Item -ItemType Directory -Path (Split-Path $path) -Force | Out-Null
                Set-Content -LiteralPath $path -Value '// copied payload'
            }
            Mock Invoke-Native { [pscustomobject]@{ ExitCode = 0; StdOut = '2.0.0'; StdErr = '' } }
            Mock Get-FileHash {
                $support = Join-Path $script:nativeRoots[0] 'config\opencode\plugins\wt-agent-hooks'
                Test-Path -LiteralPath (Join-Path $support 'wt-agent-hooks.js') | Should -BeTrue
                throw 'Reached copied V2 payload'
            }
            { & $script:nativeCases[$script:nativeLoaderCase] } | Should -Throw '*Reached copied V2 payload*'
            Should -Invoke Invoke-Native -Times 1 -Exactly -ParameterFilter { $FilePath -eq $override }
            Should -Invoke Invoke-Native -Times 0 -Exactly -ParameterFilter { $FilePath -eq 'fixture-wta.exe' }
        }
        finally {
            foreach ($root in $script:nativeRoots) {
                if (Test-Path -LiteralPath $root) { Remove-Item -LiteralPath $root -Recurse -Force }
            }
            $script:app = $previousApp
            $script:nativeRoots = $previousRoots
            $script:bundlePath = $oldBundle
            $env:PATH = $previousPath
            if ($null -eq $previousOverride) { Remove-Item Env:\ITE2E_OPENCODE_EXE -ErrorAction SilentlyContinue }
            else { $env:ITE2E_OPENCODE_EXE = $previousOverride }
        }
    }

    It 'fails rather than skips an invalid explicit native executable' {
        $previous = $env:ITE2E_OPENCODE_EXE
        try {
            $env:ITE2E_OPENCODE_EXE = Join-Path $TestDrive 'missing-native.exe'
            Mock Get-Command { throw 'An explicit invalid path must not fall back to PATH' } -ParameterFilter { $Name -eq 'opencode' }
            Mock Set-ItResult {}
            Mock Invoke-Native { throw 'Invalid executables must not run' }
            { & $script:nativeCases[$script:nativeLoaderCase] } | Should -Throw '*set ITE2E_OPENCODE_EXE*'
            Should -Invoke Set-ItResult -Times 0 -Exactly
            Should -Invoke Invoke-Native -Times 0 -Exactly
        }
        finally {
            if ($null -eq $previous) { Remove-Item Env:\ITE2E_OPENCODE_EXE -ErrorAction SilentlyContinue }
            else { $env:ITE2E_OPENCODE_EXE = $previous }
        }
    }

    It 'rejects invalid native versions before copying either adapter' {
        $previous = $env:ITE2E_OPENCODE_EXE
        $override = Join-Path $TestDrive 'opencode-version-probe.exe'
        Set-Content -LiteralPath $override -Value ''
        try {
            $env:ITE2E_OPENCODE_EXE = $override
            Mock Write-ItLog {}
            Mock Set-ItResult {}
            Mock Invoke-Native { @{ ExitCode = 0; StdOut = 'unknown'; StdErr = '' } }
            { & $script:nativeCases[$script:nativeLoaderCase] } | Should -Throw
            Should -Invoke Set-ItResult -Times 0 -Exactly
        }
        finally {
            if ($null -eq $previous) { Remove-Item Env:\ITE2E_OPENCODE_EXE -ErrorAction SilentlyContinue }
            else { $env:ITE2E_OPENCODE_EXE = $previous }
        }
    }
}
