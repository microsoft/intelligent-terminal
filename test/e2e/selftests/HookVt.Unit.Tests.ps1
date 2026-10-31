#Requires -Version 7.0
#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }

Describe 'VT-first hook transport' -Tag Unit {
    BeforeAll {
        $script:root = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..\..\..'))
        $script:writer = Join-Path $script:root 'tools\wta\wt-agent-hooks\emit-hook.ps1'
        $script:artifacts = Join-Path $script:root ('Generated Files\hook-vt-' + [guid]::NewGuid().ToString('N'))
        New-Item -ItemType Directory -Path $script:artifacts | Out-Null
        . (Join-Path $PSScriptRoot '..\tests\helpers\HookInvocation.ps1')
        function Record-HookArguments {
            [pscustomobject]@{ Arguments = @($args); Input = ($input -join "`n") }
        }
        Set-Alias -Name pwsh -Value Record-HookArguments -Scope Local
        Set-Alias -Name powershell.exe -Value Record-HookArguments -Scope Local

        function Invoke-HookWriter {
            param(
                [string]$Payload,
                [string]$Event = 'agent.prompt.submit',
                [string]$CliSource = 'claude',
                [bool]$ConsoleAvailable = $true,
                [switch]$NativeConsole,
                [switch]$MissingIdentity
            )
            $id = [guid]::NewGuid().ToString('N')
            $sequencePath = Join-Path $script:artifacts "$id.sequence"
            $fallbackPath = Join-Path $script:artifacts "$id.fallback.json"
            $command = @'
function Record-HookFallback {
    $record = @{ arguments = @($args); input = ($input -join "`n") }
    [IO.File]::WriteAllText($env:HOOK_FALLBACK, ($record | ConvertTo-Json -Compress))
}
if ($env:HOOK_MOCK_CONSOLE -eq '1') {
    Add-Type -TypeDefinition @"
using System;
using System.IO;
public static class AgentHookConsole {
    public static bool Write(string value) {
        if (Environment.GetEnvironmentVariable("HOOK_CONSOLE_AVAILABLE") != "1") return false;
        File.WriteAllText(Environment.GetEnvironmentVariable("HOOK_SEQUENCE"), value);
        return true;
    }
}
"@
}
else {
    Add-Type -TypeDefinition @"
using System.Runtime.InteropServices;
public static class HookFixtureConsole {
    [DllImport("kernel32.dll")] public static extern bool FreeConsole();
    [DllImport("kernel32.dll")] public static extern uint GetConsoleCP();
}
"@
    [HookFixtureConsole]::FreeConsole() | Out-Null
    if ([HookFixtureConsole]::GetConsoleCP() -ne 0) { throw 'Fixture is still attached to a console.' }
}
& $env:HOOK_WRITER -CliSource $env:HOOK_CLI_SOURCE -Event $env:HOOK_EVENT
[Console]::Out.WriteLine('HOOK_RETURNED')
'@
            $info = [Diagnostics.ProcessStartInfo]::new()
            $info.FileName = 'powershell.exe'
            foreach ($argument in @('-NoLogo', '-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-Command', $command)) {
                $info.ArgumentList.Add($argument)
            }
            $info.UseShellExecute = $false
            $info.CreateNoWindow = $true
            $info.RedirectStandardInput = $true
            $info.RedirectStandardOutput = $true
            $info.RedirectStandardError = $true
            $info.StandardInputEncoding = [Text.UTF8Encoding]::new($false)
            $info.Environment['HOOK_WRITER'] = $script:writer
            $info.Environment['HOOK_EVENT'] = $Event
            $info.Environment['HOOK_CLI_SOURCE'] = $CliSource
            $info.Environment['HOOK_SEQUENCE'] = $sequencePath
            $info.Environment['HOOK_FALLBACK'] = $fallbackPath
            $info.Environment['HOOK_MOCK_CONSOLE'] = if ($NativeConsole) { '0' } else { '1' }
            $info.Environment['HOOK_CONSOLE_AVAILABLE'] = if ($ConsoleAvailable) { '1' } else { '0' }
            $info.Environment['INTELLIGENT_TERMINAL'] = '1'
            $info.Environment['WT_SESSION'] = if ($MissingIdentity) { '' } else { '00000000-0000-0000-0000-000000000001' }
            $info.Environment['WT_COM_CLSID'] = 'fixture-existing-server'
            $info.Environment['WTCLI_PATH'] = 'Record-HookFallback'
            $info.Environment['WTA_HOOK_LOG_DIR'] = $script:artifacts
            $process = [Diagnostics.Process]::new()
            $process.StartInfo = $info
            try {
                $process.Start() | Out-Null
                $output = $process.StandardOutput.ReadToEndAsync()
                $errorOutput = $process.StandardError.ReadToEndAsync()
                $process.StandardInput.Write($Payload)
                $process.StandardInput.Close()
                if (-not $process.WaitForExit(15000)) {
                    $process.Kill($true)
                    throw 'Hook fixture did not exit.'
                }
                [pscustomobject]@{
                    ExitCode = $process.ExitCode
                    Output = $output.GetAwaiter().GetResult()
                    Error = $errorOutput.GetAwaiter().GetResult()
                    Sequence = if (Test-Path -LiteralPath $sequencePath) { [IO.File]::ReadAllText($sequencePath) } else { $null }
                    Fallback = if (Test-Path -LiteralPath $fallbackPath) { [IO.File]::ReadAllText($fallbackPath) | ConvertFrom-Json } else { $null }
                    Trace = if (Test-Path -LiteralPath (Join-Path $script:artifacts 'hook-trace.log')) { [IO.File]::ReadAllText((Join-Path $script:artifacts 'hook-trace.log')) } else { '' }
                }
            }
            finally {
                $process.Dispose()
            }
        }
    }

    AfterAll {
        Remove-Item -LiteralPath $script:artifacts -Recurse -Force
    }

    It 'writes one encoded VT event to the console rather than captured stdout' {
        $payload = '{"session_id":"fixture","cwd":"C:\\quoted path","message":"' + [char]0x4E2D + '","prompt":"private"}'
        $result = Invoke-HookWriter -Payload $payload
        $result.ExitCode | Should -Be 0
        $result.Error | Should -BeNullOrEmpty
        $result.Output.Trim() | Should -BeExactly 'HOOK_RETURNED'
        $result.Fallback | Should -BeNullOrEmpty
        $result.Sequence | Should -Match '^\x1b\]9001;AgentEvent64;[A-Za-z0-9+/=]+\x07$'
        $encoded = $result.Sequence.Substring(20).TrimEnd([char]7)
        $event = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String($encoded)) | ConvertFrom-Json
        $event.event | Should -BeExactly 'agent.prompt.submit'
        $event.cli_source | Should -BeExactly 'claude'
        $event.payload.session_id | Should -BeExactly 'fixture'
        $event.payload.cwd | Should -BeExactly 'C:\quoted path'
        $event.payload.message | Should -BeExactly ([string][char]0x4E2D)
        $event.payload.prompt | Should -BeExactly 'private'
    }

    It 'preserves UTF-8 input and routing through the real no-console fallback' {
        $payload = '{"session_id":"fixture","message":"' + [char]0x4E2D + '"}'
        $result = Invoke-HookWriter -Payload $payload -NativeConsole
        $result.ExitCode | Should -Be 0
        $result.Error | Should -BeNullOrEmpty
        $result.Sequence | Should -BeNullOrEmpty
        $result.Fallback | Should -Not -BeNullOrEmpty -Because $result.Trace
        $result.Fallback.input | Should -BeExactly $payload
        $result.Fallback.arguments | Should -Be @('agent-hook', '--cli-source', 'claude', '--event', 'agent.prompt.submit')
    }

    It 'omits unavailable environment routing for <CliSource>' -TestCases @(
        @{ CliSource = 'antigravity' },
        @{ CliSource = 'custom:fixture' }
    ) {
        param($CliSource)
        $result = Invoke-HookWriter -CliSource $CliSource -Payload '{"session_id":"fixture"}'
        $result.ExitCode | Should -Be 0
        $result.Error | Should -BeNullOrEmpty
        $result.Fallback | Should -BeNullOrEmpty
        $encoded = $result.Sequence.Substring(20).TrimEnd([char]7)
        $json = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String($encoded))
        $event = $json | ConvertFrom-Json
        $event.PSObject.Properties.Name | Should -Not -Contain 'agent_session_id'
        $event.payload.session_id | Should -BeExactly 'fixture'
        $event.cli_source | Should -BeExactly $CliSource
    }

    It 'does not emit or bind ACP hooks without an owning pane identity' {
        $result = Invoke-HookWriter -Payload '{"session_id":"fixture"}' -MissingIdentity
        $result.ExitCode | Should -Be 0
        $result.Error | Should -BeNullOrEmpty
        $result.Sequence | Should -BeNullOrEmpty
        $result.Fallback | Should -BeNullOrEmpty
    }

    It 'falls back without leaking VT bytes into a provider hook response' {
        $result = Invoke-HookWriter -Payload '{"session_id":"fixture"}' -ConsoleAvailable $false
        $result.ExitCode | Should -Be 0
        $result.Error | Should -BeNullOrEmpty
        $result.Output.Trim() | Should -BeExactly 'HOOK_RETURNED'
        $result.Sequence | Should -BeNullOrEmpty
        $result.Fallback.input | Should -BeExactly '{"session_id":"fixture"}'
    }

    It 'preserves the shipped <Cli> invocation and quoted payload path' -TestCases @(
        @{ Cli = 'copilot' },
        @{ Cli = 'gemini' },
        @{ Cli = 'codex' },
        @{ Cli = 'claude' }
    ) {
        param($Cli)
        $bundle = Join-Path $script:root 'tools\wta\wt-agent-hooks'
        $payloadFile = Join-Path $script:artifacts "payload 'quoted'.json"
        $payload = '{"session_id":"fixture"}'
        [IO.File]::WriteAllText($payloadFile, $payload)
        $hook = Get-ShippedAgentHook -BundleRoot $bundle -Cli $Cli -Event 'agent.prompt.submit'
        $invocation = New-AgentHookInvocation -BundleRoot $bundle -Cli $Cli -Event 'agent.prompt.submit' `
            -PayloadFile $payloadFile -GitBash 'Record-HookArguments'
        $actual = & ([scriptblock]::Create($invocation))
        $actual.Input | Should -BeExactly $payload
        if ($Cli -eq 'copilot') {
            $actual.Arguments | Should -Be @($hook.args)
        }
        elseif ($Cli -eq 'claude') {
            $actual.Arguments | Should -Be @('-c', $hook.command)
        }
        else {
            $decoded = [Text.Encoding]::Unicode.GetString([Convert]::FromBase64String($actual.Arguments[-1]))
            $expected = if ($Cli -eq 'codex') { $hook.commandWindows } else { $hook.command }
            $decoded | Should -BeExactly $expected
        }
    }

    It 'executes the Codex Windows primary writer from <Shell> with a spaced path' -TestCases @(
        @{ Shell = 'PowerShell' },
        @{ Shell = 'Cmd' }
    ) {
        param($Shell)
        $support = Join-Path $script:artifacts "codex $Shell bundle 'quoted'"
        New-Item -ItemType Directory -Path $support | Out-Null
        [IO.File]::WriteAllText((Join-Path $support 'emit-hook.ps1'), @'
param([string]$CliSource, [string]$Event)
[IO.File]::WriteAllText($env:HOOK_QUOTE_RECORD, ($CliSource + ' ' + $Event))
'@)
        $manifest = Get-Content -LiteralPath (Join-Path $script:root 'tools\wta\wt-agent-hooks\codex\wt-agent-hooks\hooks\hooks.json') -Raw |
            ConvertFrom-Json
        foreach ($topic in $manifest.hooks.PSObject.Properties) {
            $command = $topic.Value[0].hooks[0].commandWindows
            if ($command -notmatch '--event ([a-z.]+)') { throw 'Codex hook lost its event routing argument.' }
            $expected = 'codex ' + $Matches[1]
            $record = Join-Path $support ($topic.Name + '.record')
            $info = [Diagnostics.ProcessStartInfo]::new()
            $info.UseShellExecute = $false
            $info.CreateNoWindow = $true
            $info.RedirectStandardOutput = $true
            $info.RedirectStandardError = $true
            $info.Environment['WT_AGENT_HOOK_DIR'] = $support
            $info.Environment['HOOK_QUOTE_RECORD'] = $record
            $info.Environment.Remove('WT_SESSION') | Out-Null
            $info.Environment.Remove('WT_COM_CLSID') | Out-Null
            if ($Shell -eq 'Cmd') {
                $info.FileName = 'cmd.exe'
                $info.Arguments = '/d /s /c "' + $command + '"'
            }
            else {
                $info.FileName = 'powershell.exe'
                foreach ($argument in @('-NoLogo', '-NoProfile', '-NonInteractive', '-Command', $command)) {
                    $info.ArgumentList.Add($argument)
                }
            }
            $process = [Diagnostics.Process]::new()
            $process.StartInfo = $info
            try {
                $process.Start() | Out-Null
                $output = $process.StandardOutput.ReadToEndAsync()
                $errors = $process.StandardError.ReadToEndAsync()
                if (-not $process.WaitForExit(15000)) {
                    $process.Kill($true)
                    throw 'Codex primary hook fixture did not exit.'
                }
                $process.ExitCode | Should -Be 0
                $output.GetAwaiter().GetResult() | Should -BeNullOrEmpty
                $errors.GetAwaiter().GetResult() | Should -BeNullOrEmpty
                Test-Path -LiteralPath $record | Should -BeTrue -Because "$Shell must execute $($topic.Name)'s primary writer"
                [IO.File]::ReadAllText($record) | Should -BeExactly $expected
            }
            finally { $process.Dispose() }
        }
    }
}
