#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# No package, terminal, UI or elevation is accessed by this suite.
Describe 'Harness: process-owned WTA master selection' -Tag 'Harness' {
    BeforeAll {
        . (Join-Path $PSScriptRoot '..\ItE2E\Public\Sessions.ps1')
        function Wait-Until {
            param([scriptblock]$Condition, [int]$TimeoutSec, [string]$Because)
            $value = & $Condition
            if (-not $value) { throw "Missing $Because" }
            $value
        }
    }
    BeforeEach {
        $script:app = [pscustomobject]@{
            Pid = 100
            WindowsTerminal = 'C:\fixture\WindowsTerminal.exe'
            WtaPath = 'C:\fixture\wta.exe'
        }
        $script:root = [pscustomobject]@{
            ExecutablePath = $script:app.WindowsTerminal
            CreationDate = [datetime]'2026-10-20T01:00:00Z'
        }
        $script:masters = @([pscustomobject]@{
            ExecutablePath = $script:app.WtaPath
            CreationDate = [datetime]'2026-10-20T01:00:01Z'
            CommandLine = '"C:\fixture\wta.exe" --master "\\.\pipe\wta-master-normal" --agent copilot'
        })
        Mock Get-CimInstance {
            if ($Filter -eq 'ProcessId=100') { return $script:root }
            if ($Filter -eq "ParentProcessId=100 AND Name='wta.exe'") { return $script:masters }
            throw "Unscoped process lookup: $Filter"
        }
    }
    It 'selects only the chosen Terminal process child without package-wide discovery' {
        Get-WtaMasterPipe -App $script:app | Should -Be '\\.\pipe\wta-master-normal'
        Should -Invoke Get-CimInstance -Times 1 -Exactly -ParameterFilter {
            $Filter -eq "ParentProcessId=100 AND Name='wta.exe'"
        }
    }
    It 'ignores session and delegate CLI children that also carry the owning master argument' {
        $script:masters += @(
            '"C:\fixture\wta.exe" sessions list --master "\\.\pipe\wta-master-normal" --json',
            '"C:\fixture\wta.exe" delegate --master "\\.\pipe\wta-master-normal" "fix this build"'
        ) | ForEach-Object {
            [pscustomobject]@{
                ExecutablePath = $script:app.WtaPath
                CreationDate = [datetime]'2026-10-20T01:00:02Z'
                CommandLine = $_
            }
        }
        Get-WtaMasterPipe -App $script:app | Should -Be '\\.\pipe\wta-master-normal'
    }
    It 'rejects missing or ambiguous owners instead of choosing another master' {
        $script:masters = @()
        { Get-WtaMasterPipe -App $script:app } | Should -Throw
        $script:masters = @(1, 2 | ForEach-Object {
            [pscustomobject]@{
                ExecutablePath = $script:app.WtaPath
                CreationDate = [datetime]'2026-10-20T01:00:01Z'
                CommandLine = "wta.exe --master \\.\pipe\wta-master-$_"
            }
        })
        { Get-WtaMasterPipe -App $script:app } | Should -Throw '*multiple WTA masters*'
    }
    It 'rejects a different host image or a master predating its apparent parent' {
        $script:root.ExecutablePath = 'C:\other\WindowsTerminal.exe'
        { Get-WtaMasterPipe -App $script:app } | Should -Throw '*Cannot verify*'
        $script:root.ExecutablePath = $script:app.WindowsTerminal
        $script:masters[0].CreationDate = [datetime]'2026-10-19T01:00:00Z'
        { Get-WtaMasterPipe -App $script:app } | Should -Throw
    }
}
