#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# Regression coverage for WTA -> wtcli -> COM SendEvent payloads that exceed
# Windows' 32,767-character process command-line limit.
#
#   Invoke-Pester test/e2e/tests/Feature.WtcliPublishStdin.Tests.ps1 -Tag Feature

BeforeDiscovery { $script:Ready = [bool](Get-AppxPackage | Where-Object { $_.Name -like '*IntelligentTerminal*' }) }

Describe 'Feature: wtcli publish stdin transport' -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $script:app = Start-Terminal -Package (Get-ItTestPackage) -PassFre $true
        if ($env:ITE2E_WTCLI_PATH) {
            $script:app.WtcliPath = (Resolve-Path $env:ITE2E_WTCLI_PATH).Path
        }

        function script:Invoke-PublishProcess {
            param(
                [Parameter(Mandatory)][string[]]$Arguments,
                [AllowNull()][string]$StdIn,
                [switch]$PipeStdin
            )

            $psi = [System.Diagnostics.ProcessStartInfo]::new()
            $psi.FileName = $script:app.WtcliPath
            foreach ($argument in $Arguments) { [void]$psi.ArgumentList.Add($argument) }
            $psi.RedirectStandardInput = $PipeStdin
            $psi.RedirectStandardOutput = $true
            $psi.RedirectStandardError = $true
            $psi.UseShellExecute = $false
            $psi.CreateNoWindow = $true
            $psi.Environment['WT_COM_CLSID'] = $script:app.ComClsid

            $process = [System.Diagnostics.Process]::new()
            $process.StartInfo = $psi
            $started = $false
            try {
                [void]$process.Start()
                $started = $true
                $stdoutTask = $process.StandardOutput.ReadToEndAsync()
                $stderrTask = $process.StandardError.ReadToEndAsync()
                if ($PipeStdin) {
                    $writeTask = $process.StandardInput.WriteAsync($StdIn)
                    $writeCompleted = $writeTask.Wait(30000)
                    if (-not $writeCompleted) {
                        try { if (-not $process.HasExited) { $process.Kill($true) } } catch { }
                        $process.WaitForExit(2000) | Out-Null
                    }
                    $writeCompleted | Should -BeTrue -Because "wtcli publish process $($process.Id) must drain stdin within 30 seconds"
                    $writeTask.GetAwaiter().GetResult()
                    $process.StandardInput.Close()
                }
                $exited = $process.WaitForExit(30000)
                if (-not $exited) {
                    try { if (-not $process.HasExited) { $process.Kill($true) } } catch { }
                    $process.WaitForExit(2000) | Out-Null
                }
                $exited | Should -BeTrue -Because "wtcli publish process $($process.Id) must exit within 30 seconds"
                [pscustomobject]@{
                    ExitCode = $process.ExitCode
                    StdOut   = $stdoutTask.GetAwaiter().GetResult()
                    StdErr   = $stderrTask.GetAwaiter().GetResult()
                }
            }
            finally {
                if ($started) {
                    try {
                        if (-not $process.HasExited) {
                            $process.Kill($true)
                            $process.WaitForExit(2000) | Out-Null
                        }
                    }
                    catch { }
                }
                $process.Dispose()
            }
        }

        function script:Start-ReadyListener {
            $listener = Start-WtEventListener -App $script:app
            $marker = "listener-ready-$([guid]::NewGuid())"
            Wait-Until -TimeoutSec 20 -Because 'wtcli listen to subscribe before publishing' -Condition {
                try {
                    $control = @{ type = 'event'; method = 'agent_event'; params = @{ event = $marker } } | ConvertTo-Json -Compress
                    Invoke-WtCli -App $script:app -Arguments @('publish', $control) | Out-Null
                }
                catch { }
                @(Get-WtEvents -Listener $listener -Predicate { $_.params.event -eq $marker }).Count -gt 0
            } | Out-Null
            $listener
        }
    }
    AfterAll { if ($script:app) { Stop-Terminal -App $script:app } }

    It 'delivers a 64 KB JSON payload intact through SendEvent' {
        $listener = script:Start-ReadyListener
        try {
            $marker = "large-publish-$([guid]::NewGuid())"
            $body = 'x' * (64KB)
            $payload = @{
                type   = 'event'
                method = 'agent_event'
                params = @{ event = $marker; body = $body }
            } | ConvertTo-Json -Compress

            $result = script:Invoke-PublishProcess -Arguments @('publish', '--stdin') -StdIn $payload -PipeStdin
            $result.ExitCode | Should -Be 0 -Because $result.StdErr

            $event = Wait-WtEvent -Listener $listener -TimeoutSec 30 -Predicate { $_.params.event -eq $marker }
            $event.params.body.Length | Should -Be $body.Length
            $event.params.body | Should -BeExactly $body -Because 'SendEvent must receive every byte written to stdin'
        }
        finally {
            Stop-WtEventListener -Listener $listener
        }
    }

    It 'keeps positional JSON publish compatibility' {
        $listener = script:Start-ReadyListener
        try {
            $marker = "positional-publish-$([guid]::NewGuid())"
            $payload = @{ type = 'event'; method = 'agent_event'; params = @{ event = $marker; ok = $true } } | ConvertTo-Json -Compress
            Invoke-WtCli -App $script:app -Arguments @('publish', $payload) | Out-Null

            $event = Wait-WtEvent -Listener $listener -TimeoutSec 30 -Predicate { $_.params.event -eq $marker }
            $event.params.ok | Should -BeTrue
        }
        finally {
            Stop-WtEventListener -Listener $listener
        }
    }

    It 'rejects empty stdin and rejects selecting both input forms' {
        $empty = script:Invoke-PublishProcess -Arguments @('publish', '--stdin') -StdIn '' -PipeStdin
        $empty.ExitCode | Should -Not -Be 0
        $empty.StdErr | Should -Match 'must not be empty'

        $both = script:Invoke-PublishProcess -Arguments @('publish', '{}', '--stdin') -StdIn '{}' -PipeStdin
        $both.ExitCode | Should -Not -Be 0
    }
}
