#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# PR #468: keep OSC 133 prompt boundaries intact when a user's
# PROMPT_COMMAND rebuilds PS1, and keep the user-wide script inert outside IT.

BeforeDiscovery {
    $script:GitBash = 'C:\Program Files\Git\bin\bash.exe'
    $script:Ready = [bool](
        (Test-Path $script:GitBash) -and
        (Get-AppxPackage | Where-Object { $_.PackageFamilyName -like 'IntelligentTerminal_*' }) -and
        (Get-Command winapp -ErrorAction SilentlyContinue)
    )
}

Describe 'Feature: Bash semantic prompt integration' -Tag 'Feature' -Skip:(-not $script:Ready) {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        $script:GitBash = 'C:\Program Files\Git\bin\bash.exe'
        $script:app = $null
        $script:bashProfile = Join-Path $HOME '.bashrc'
        $script:integrationDir = Join-Path $HOME '.intelligent-terminal'
        $script:integrationScript = Join-Path $script:integrationDir 'shell-integration_v3.sh'
        $script:profileExisted = Test-Path $script:bashProfile
        $script:profileBytes = if ($script:profileExisted) { [IO.File]::ReadAllBytes($script:bashProfile) } else { $null }
        $script:scriptExisted = Test-Path $script:integrationScript
        $script:scriptBytes = if ($script:scriptExisted) { [IO.File]::ReadAllBytes($script:integrationScript) } else { $null }
        $script:existingBackups = @(
            Get-ChildItem "$($script:bashProfile).bak.*" -File -ErrorAction SilentlyContinue |
                Select-Object -ExpandProperty FullName
        )

        $profile = [pscustomobject][ordered]@{
            guid        = '{e2e46800-4680-4680-8680-000000000001}'
            name        = 'ITE2E PR468 Git Bash'
            commandline = "`"$script:GitBash`" --noprofile --norc -i"
            hidden      = $false
        }
        $profiles = [pscustomobject][ordered]@{
            defaults = [pscustomobject]@{}
            list     = @($profile)
        }
        $script:app = Start-Terminal -Package Dev -PassFre $true -Settings @{
            autoErrorDetectionEnabled = $true
            profiles                  = $profiles
        }
    }

    AfterAll {
        if ($script:app) {
            Stop-Terminal -App $script:app
        }

        if ($script:profileExisted) {
            [IO.File]::WriteAllBytes($script:bashProfile, $script:profileBytes)
        }
        elseif (Test-Path $script:bashProfile) {
            Remove-Item -LiteralPath $script:bashProfile -Force
        }

        if ($script:scriptExisted) {
            [IO.File]::WriteAllBytes($script:integrationScript, $script:scriptBytes)
        }
        elseif (Test-Path $script:integrationScript) {
            Remove-Item -LiteralPath $script:integrationScript -Force
        }

        Get-ChildItem "$($script:bashProfile).bak.*" -File -ErrorAction SilentlyContinue |
            Where-Object FullName -NotIn $script:existingBackups |
            Remove-Item -Force
        if ((Test-Path $script:integrationDir) -and -not (Get-ChildItem $script:integrationDir -Force)) {
            Remove-Item -LiteralPath $script:integrationDir
        }
    }

    It 'Bash PROMPT_COMMAND rewrites preserve semantic prompt boundaries' {
        (Test-Until -TimeoutSec 90 -IntervalSec 1 -Condition {
                Test-Path $script:integrationScript
            }) | Should -BeTrue -Because 'the explicit auto-fix setting must install the packaged Bash integration'

        $posixScript = $script:integrationScript.Replace('\', '/')
        $outside = Invoke-Native -FilePath $script:GitBash -Arguments @(
            '--noprofile',
            '--norc',
            '-ic',
            "unset INTELLIGENT_TERMINAL; PROMPT_COMMAND=':'; source '$posixScript'; if [ -z `"`${__IT_SHELLINTEG_INSTALLED:-}`" ] && [ `"`${PROMPT_COMMAND:-}`" = ':' ]; then printf HOST_GATE_OK; else printf HOST_GATE_FAILED; fi"
        ) -TimeoutSec 20
        $outside.ExitCode | Should -Be 0
        $outside.StdOut | Should -Match 'HOST_GATE_OK' -Because 'the user-wide script must not activate outside Intelligent Terminal'
        $outside.StdOut | Should -Not -Match ([regex]::Escape("`e]133;")) -Because 'a non-IT host must receive no semantic prompt marks'

        $tab = New-WtTab -App $script:app -Command "`"$script:GitBash`" --noprofile --norc -i" -Title 'bash-prompt-rewrite'
        $sid = $tab.session_id
        try {
            Assert-Pane -App $script:app -SessionId $sid -Match 'bash-[0-9.]+\$' -TimeoutSec 15
            $setup = "PROMPT_COMMAND='__ITE2E_PC_COUNT=`$(( `${__ITE2E_PC_COUNT:-0} + 1 )); PS1=`"ITE2E-`${__ITE2E_PC_COUNT}> `"'`; source '$posixScript'"
            Send-WtInput -App $script:app -SessionId $sid -Text $setup
            Send-WtKeys -App $script:app -SessionId $sid -Keys @('Enter')
            $promptReady = Test-Until -TimeoutSec 15 -IntervalSec 0.5 -Condition {
                (Get-WtCapture -App $script:app -SessionId $sid -MaxLines 80) -match 'ITE2E-1>'
            }
            $capture = Get-WtCapture -App $script:app -SessionId $sid -MaxLines 80
            $promptReady | Should -BeTrue -Because "the user PROMPT_COMMAND must run after sourcing the integration; pane:`n$capture"

            foreach ($cycle in @(
                    @{ Command = 'false'; ExitCode = '1'; Prompt = 'ITE2E-2>' },
                    @{ Command = 'true'; ExitCode = '0'; Prompt = 'ITE2E-3>' }
                )) {
                $listener = Start-WtEventListener -App $script:app -SessionId $sid
                try {
                    Send-WtInput -App $script:app -SessionId $sid -Text $cycle.Command
                    Send-WtKeys -App $script:app -SessionId $sid -Keys @('Enter')
                    Assert-Pane -App $script:app -SessionId $sid -Match ([regex]::Escape($cycle.Prompt)) -TimeoutSec 15

                    $complete = Test-Until -TimeoutSec 15 -IntervalSec 0.4 -Condition {
                        $sequences = @(
                            Get-WtEvents -Listener $listener -Predicate {
                                $_.method -eq 'vt_sequence' -and "$($_.params.pane_id)" -eq "$sid"
                            } | ForEach-Object { "$($_.params.sequence)" }
                        )
                        ($sequences -match "(?i)osc:133;D;$($cycle.ExitCode)(\b|;|$)").Count -eq 1 -and
                        ($sequences -match '(?i)osc:133;A(\b|;|$)').Count -eq 1 -and
                        ($sequences -match '(?i)osc:133;B(\b|;|$)').Count -eq 1
                    }
                    $complete | Should -BeTrue -Because "$($cycle.Command) must produce exactly one complete D/A/B semantic prompt cycle"

                    $sequences = @(
                        Get-WtEvents -Listener $listener -Predicate {
                            $_.method -eq 'vt_sequence' -and "$($_.params.pane_id)" -eq "$sid"
                        } | ForEach-Object { "$($_.params.sequence)" }
                    )
                    $d = 0..($sequences.Count - 1) | Where-Object { $sequences[$_] -match "(?i)osc:133;D;$($cycle.ExitCode)(\b|;|$)" } | Select-Object -First 1
                    $a = 0..($sequences.Count - 1) | Where-Object { $sequences[$_] -match '(?i)osc:133;A(\b|;|$)' } | Select-Object -First 1
                    $b = 0..($sequences.Count - 1) | Where-Object { $sequences[$_] -match '(?i)osc:133;B(\b|;|$)' } | Select-Object -First 1
                    $d | Should -BeLessThan $a
                    $a | Should -BeLessThan $b
                }
                finally {
                    Stop-WtEventListener -Listener $listener
                }
            }
        }
        finally {
            Close-WtPane -App $script:app -SessionId $sid
        }
    }
}
