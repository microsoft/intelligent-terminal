BeforeAll {
    Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
}

Describe 'Process deadlines without interactive input' {
    It 'expires on UTC after a simulated suspend even if active elapsed time is short' {
        $start = [datetimeoffset]::Parse('2026-10-04T02:57:57Z')
        Test-ItProcessDeadline -DeadlineUtc $start.AddSeconds(900) -ElapsedSeconds 612 `
            -TimeoutSeconds 900 -NowUtc $start.AddSeconds(13370) | Should -BeTrue
    }
    It 'expires monotonically despite a backward UTC adjustment' {
        $start = [datetimeoffset]::Parse('2026-10-04T02:57:57Z')
        Test-ItProcessDeadline -DeadlineUtc $start.AddSeconds(900) -ElapsedSeconds 901 `
            -TimeoutSeconds 900 -NowUtc $start.AddSeconds(-60) | Should -BeTrue
    }
    It 'accepts only an exit observed before both deadlines' {
        $start = [datetimeoffset]::Parse('2026-10-04T02:57:57Z')
        Test-ItProcessDeadline -DeadlineUtc $start.AddSeconds(900) -ElapsedSeconds 612 `
            -TimeoutSeconds 900 -NowUtc $start.AddSeconds(612) | Should -BeFalse
    }
    It 'rejects an already expired worker without issuing another wait' {
        $process = [pscustomobject]@{ HasExited = $true }
        Wait-ItProcessDeadline -Process $process -TimeoutSec 1 `
            -StartedUtc ([datetimeoffset]::UtcNow.AddSeconds(-2)) | Should -BeFalse
    }
    It 'accepts an immediately completed worker before the deadline' {
        Wait-ItProcessDeadline -Process ([pscustomobject]@{ HasExited = $true }) -TimeoutSec 1 |
            Should -BeTrue
    }
    It 'polls no longer than 500 milliseconds per process wait' {
        $process = [pscustomobject]@{ HasExited = $false; LastWait = 0 }
        $process | Add-Member ScriptMethod WaitForExit {
            param($milliseconds)
            $this.LastWait = $milliseconds
            $this.HasExited = $true
            $true
        }
        Wait-ItProcessDeadline -Process $process -TimeoutSec 2 | Should -BeTrue
        $process.LastWait | Should -Be 500
    }
    It 'exports the UI action helper into the importing BeforeAll scope' {
        (Get-Command Invoke-WinAppUi -Module ItE2E -ErrorAction Stop).Name | Should -Be 'Invoke-WinAppUi'
    }
}
