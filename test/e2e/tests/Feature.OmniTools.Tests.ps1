#Requires -Version 7.0
#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# Real deployed WTA -> Copilot SDK -> host callback boundary. Host receipts are
# synthetic: these tests do not claim native pane mutation or UI acceptance.

BeforeDiscovery {
    Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
    $script:Ready = $null -ne (Resolve-ItApp -Package (Get-ItTestPackage) -IfInstalled)
}

Describe 'Feature: Omni dedicated tool bridge' -Tag 'Feature', 'OmniTools' -Skip:(-not $script:Ready) {
    BeforeAll {
        $script:app = Resolve-ItApp -Package (Get-ItTestPackage)
        $script:wta = Join-Path $script:app.InstallLocation 'wta.exe'
        $env:ITE2E_EXPECTED_WTA_SHA256 | Should -Not -BeNullOrEmpty
        (Get-FileHash -LiteralPath $script:wta).Hash | Should -Be $env:ITE2E_EXPECTED_WTA_SHA256
        Get-Command copilot -ErrorAction Stop | Should -Not -BeNullOrEmpty
        $header = Get-Content -LiteralPath (Join-Path $PSScriptRoot '..\..\..\src\cascadia\inc\OmniToolDefinitions.h') -Raw
        $catalog = [regex]::Match($header, '(?s)R"json\((.*?)\)json"').Groups[1].Value | ConvertFrom-Json -AsHashtable
        $script:tools = @($catalog)
        foreach ($tool in $script:tools) {
            $tool.hasHandler = $true
            $tool.skipPermission = $true
        }

        function New-OmniContext([string]$Prompt) {
            @{
                prompt = $Prompt; run_id = [guid]::NewGuid().ToString()
                tool_protocol = 1; capabilities = $script:tools
                current_content_id = 7; current_tab_id = 'tab-one'
                current_pane_kind = 'terminal'
                panes = @(@{ content_id = 7; title = 'Fixture pane'; index = 1 })
                tabs = @(@{ id = 'tab-one'; title = 'Fixture tab'; index = 1 })
                profiles = @(
                    @{ id = '{00000000-0000-0000-0000-000000000001}'; name = 'PowerShell' },
                    @{ id = '{00000000-0000-0000-0000-000000000002}'; name = 'Copilot' }
                )
                default_profile_id = '{00000000-0000-0000-0000-000000000001}'
                current_appearance = @{ opacity = 80; acrylic = $false; font_size = 12 }
                schemes = @('Campbell'); directory_panes = @()
                directory_context_complete = $true
            }
        }

        function Start-OmniBridge {
            $info = [Diagnostics.ProcessStartInfo]::new($script:wta)
            $info.ArgumentList.Add('omni-plan')
            $info.ArgumentList.Add('--serve')
            $info.UseShellExecute = $false
            $info.CreateNoWindow = $true
            $info.RedirectStandardInput = $true
            $info.RedirectStandardOutput = $true
            $info.RedirectStandardError = $true
            $process = [Diagnostics.Process]::Start($info)
            @{ Process = $process; Error = $process.StandardError.ReadToEndAsync() }
        }

        function Send-OmniFrame($Bridge, $Frame) {
            $Bridge.Process.StandardInput.WriteLine(($Frame | ConvertTo-Json -Depth 40 -Compress))
            $Bridge.Process.StandardInput.Flush()
        }

        function Read-OmniFrame($Bridge) {
            $read = $Bridge.Process.StandardOutput.ReadLineAsync()
            $read.Wait(100000) | Should -BeTrue -Because 'the owned bridge has a bounded response time'
            $read.Result | Should -Not -BeNullOrEmpty
            $read.Result | ConvertFrom-Json -AsHashtable
        }

        function Read-OmniAction($Bridge, $Context) {
            for ($attempt = 0; $attempt -lt 3; $attempt++) {
                $call = Read-OmniFrame $Bridge
                $call.run_id | Should -Be $Context.run_id
                if ($call.name -ne 'terminal_get_context') { return $call }
                $call.arguments.Count | Should -Be 0
                $snapshot = $Context.Clone()
                $snapshot.Remove('capabilities')
                Send-OmniFrame $Bridge @{
                    kind = 'tool_result'; run_id = $Context.run_id; call_id = $call.call_id
                    result = @{ status = 'succeeded'; data = $snapshot }
                }
            }
            throw 'The agent queried context repeatedly instead of selecting the requested action.'
        }

        function Stop-OmniBridge($Bridge) {
            $Bridge.Process.StandardInput.Close()
            if (-not $Bridge.Process.WaitForExit(15000)) {
                # This is the exact process created above; its descendant job is
                # installed by WTA, never a package-wide process-name shutdown.
                Stop-Process -Id $Bridge.Process.Id -Force
                $Bridge.Process.WaitForExit(5000) | Should -BeTrue
            }
            $Bridge.Process.Dispose()
        }
    }

    It 'Omni tool results return to the dedicated agent without exposing shell tools' {
        $bridge = Start-OmniBridge
        try {
            foreach ($iteration in 1..2) {
                $context = New-OmniContext 'Call terminal_get_context exactly once and answer with its returned fixture title.'
                $nonce = "OmniReceipt$([guid]::NewGuid().ToString('N'))"
                Send-OmniFrame $bridge $context
                $call = Read-OmniFrame $bridge
                $call.kind | Should -Be 'tool_call'
                $call.run_id | Should -Be $context.run_id
                $call.name | Should -Be 'terminal_get_context'
                $call.arguments.Count | Should -Be 0
                Send-OmniFrame $bridge @{
                    kind = 'tool_result'; run_id = $context.run_id; call_id = $call.call_id
                    result = @{ status = 'succeeded'; data = @{ title = $nonce } }
                }
                $complete = Read-OmniFrame $bridge
                $complete.kind | Should -Be 'tool_complete'
                $complete.run_id | Should -Be $context.run_id
                $complete.message | Should -Match ([regex]::Escape($nonce))
            }
        }
        finally { Stop-OmniBridge $bridge }
    }

    It 'Omni cancelled close receipts finish without a second mutation' {
        $bridge = Start-OmniBridge
        try {
            $context = New-OmniContext 'Close the current pane.'
            Send-OmniFrame $bridge $context
            $call = Read-OmniAction $bridge $context
            $call.kind | Should -Be 'tool_call'
            $call.run_id | Should -Be $context.run_id
            $call.name | Should -Be 'terminal_close_pane'
            $call.arguments.content_id | Should -Be 7
            Send-OmniFrame $bridge @{
                kind = 'tool_result'; run_id = $context.run_id; call_id = $call.call_id
                result = @{ status = 'cancelled'; reason = 'native_confirmation_declined' }
            }
            $complete = Read-OmniFrame $bridge
            $complete.kind | Should -Be 'tool_complete' -Because 'cancellation must not produce another native mutation'
            $complete.run_id | Should -Be $context.run_id
        }
        finally { Stop-OmniBridge $bridge }
    }

    It 'Omni appearance uses refreshed context without retargeting the request' {
        $bridge = Start-OmniBridge
        try {
            $context = New-OmniContext 'First call terminal_get_context to read the latest opacity, then increase that value by 10 percentage points for the original current pane. Keep all other appearance fields unchanged.'
            Send-OmniFrame $bridge $context
            $read = Read-OmniFrame $bridge
            $read.name | Should -Be 'terminal_get_context'
            $read.run_id | Should -Be $context.run_id
            $live = $context.Clone()
            $live.Remove('capabilities')
            $live.current_appearance = @{ opacity = 40; acrylic = $false; font_size = 12 }
            $live.snapshot = $false
            $live.targets_bound_to_request = $true
            $live.active_tab_id = 'another-tab'
            Send-OmniFrame $bridge @{
                kind = 'tool_result'; run_id = $context.run_id; call_id = $read.call_id
                result = @{ status = 'succeeded'; effect = 'not_applied'; data = $live }
            }
            $action = Read-OmniFrame $bridge
            $action.name | Should -Be 'terminal_appearance'
            $action.run_id | Should -Be $context.run_id
            $action.arguments.scope | Should -Be 'pane'
            $action.arguments.opacity | Should -Be 50
            $action.arguments.Count | Should -Be 2
            Send-OmniFrame $bridge @{
                kind = 'tool_result'; run_id = $context.run_id; call_id = $action.call_id
                result = @{ status = 'succeeded'; effect = 'applied'; data = @{ content_id = 7; opacity = 50 } }
            }
            (Read-OmniFrame $bridge).kind | Should -Be 'tool_complete'
        }
        finally { Stop-OmniBridge $bridge }
    }

    It 'Omni partial failure receipts finish without another mutation' {
        $bridge = Start-OmniBridge
        try {
            $context = New-OmniContext 'Set this pane opacity to 70. If it fails, do not retry and report the exact reason code.'
            Send-OmniFrame $bridge $context
            $action = Read-OmniAction $bridge $context
            $action.name | Should -Be 'terminal_appearance'
            $action.arguments.opacity | Should -Be 70
            Send-OmniFrame $bridge @{
                kind = 'tool_result'; run_id = $context.run_id; call_id = $action.call_id
                result = @{
                    status = 'failed'; effect = 'partial_or_unknown'; reason = 'appearance_readback_mismatch'
                    data = @{ panes = @(@{ content_id = 7; opacity = 65 }) }
                }
            }
            $complete = Read-OmniFrame $bridge
            $complete.kind | Should -Be 'tool_complete'
            $complete.run_id | Should -Be $context.run_id
            $complete.message | Should -Match 'appearance_readback_mismatch'
        }
        finally { Stop-OmniBridge $bridge }
    }

    It 'Omni foreign tool receipts terminate the isolated run' {
        $bridge = Start-OmniBridge
        try {
            $context = New-OmniContext 'Call terminal_get_context exactly once.'
            Send-OmniFrame $bridge $context
            $call = Read-OmniFrame $bridge
            $call.kind | Should -Be 'tool_call'
            Send-OmniFrame $bridge @{
                kind = 'tool_result'; run_id = [guid]::NewGuid().ToString(); call_id = $call.call_id
                result = @{ status = 'succeeded'; data = @{ title = 'must-not-be-consumed' } }
            }
            $failure = Read-OmniFrame $bridge
            $failure.error | Should -Not -BeNullOrEmpty
            $failure.kind | Should -BeNullOrEmpty
            $bridge.Process.WaitForExit(10000) | Should -BeTrue
        }
        finally { Stop-OmniBridge $bridge }
    }

    It 'Omni runtime appearance and split requests use dedicated tools: <Scenario>' -ForEach @(
        @{ Scenario = 'acrylic preserves unspecified fields'; Prompt = 'Enable acrylic for this tab without changing its opacity or font size.'; Tool = 'terminal_appearance'; Arguments = @{ scope = 'tab'; acrylic = $true } },
        @{ Scenario = 'split resolves a profile'; Prompt = 'split pane right with Copilot profile'; Tool = 'terminal_split'; Arguments = @{ direction = 'right'; profile_id = '{00000000-0000-0000-0000-000000000002}' } }
    ) {
        $bridge = Start-OmniBridge
        try {
            $context = New-OmniContext $Prompt
            Send-OmniFrame $bridge $context
            $call = Read-OmniAction $bridge $context
            $call.kind | Should -Be 'tool_call'
            $call.name | Should -Be $Tool
            $call.arguments.Count | Should -Be $Arguments.Count
            foreach ($key in $Arguments.Keys) { $call.arguments[$key] | Should -Be $Arguments[$key] }
            Send-OmniFrame $bridge @{
                kind = 'tool_result'; run_id = $context.run_id; call_id = $call.call_id
                result = @{ status = 'succeeded'; data = $Arguments }
            }
            (Read-OmniFrame $bridge).kind | Should -Be 'tool_complete'
        }
        finally { Stop-OmniBridge $bridge }
    }

    It 'Omni metadata clues drive buffer search and reveal tool calls' {
        $bridge = Start-OmniBridge
        try {
            $context = New-OmniContext 'Go to the tab discussing what my Rust app does.'
            $context.directory_panes = @(
                @{ tab_id = 'tab-one'; content_id = 7; cwd = 'C:\src\OtherProject'; title = 'PowerShell'; profile_name = 'PowerShell' },
                @{ tab_id = 'tab-two'; content_id = 14; cwd = 'C:\src\MyRustApp'; title = 'PowerShell'; profile_name = 'PowerShell' }
            )
            $context.tabs += @{ id = 'tab-two'; title = 'PowerShell'; index = 2 }
            Send-OmniFrame $bridge $context
            $search = Read-OmniAction $bridge $context
            $search.kind | Should -Be 'tool_call'
            $search.name | Should -Be 'terminal_find_content'
            $search.arguments.metadata_pane_ids | Should -Contain 14
            Send-OmniFrame $bridge @{
                kind = 'tool_result'; run_id = $context.run_id; call_id = $search.call_id
                result = @{
                    status = 'succeeded'; data = @{
                        search_truncated = $false; pane_metadata = $context.directory_panes
                        content_matches = @(
                            @{ id = 0; tab_id = 'tab-two'; content_id = 14; anchor_text = 'What does this program do?'; text = 'User: What does this program do? Assistant: It reads CSV data, computes grouped totals, and writes a report.' },
                            @{ id = 1; tab_id = 'tab-one'; content_id = 7; anchor_text = 'PS C:\src\OtherProject>'; text = 'PS C:\src\OtherProject>' }
                        )
                    }
                }
            }
            $reveal = Read-OmniAction $bridge $context
            $reveal.kind | Should -Be 'tool_call'
            $reveal.name | Should -Be 'terminal_reveal_content'
            $reveal.arguments.match_id | Should -Be 0
            Send-OmniFrame $bridge @{
                kind = 'tool_result'; run_id = $context.run_id; call_id = $reveal.call_id
                result = @{ status = 'succeeded'; data = @{ content_id = 14; tab_id = 'tab-two' } }
            }
            (Read-OmniFrame $bridge).kind | Should -Be 'tool_complete'
        }
        finally { Stop-OmniBridge $bridge }
    }
}
