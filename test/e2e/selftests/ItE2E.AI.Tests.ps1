#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }
# AI oracle self-tests. Require an authenticated agent CLI (copilot by default).
#   Invoke-Pester test/e2e/selftests -Tag AI
# Override the judge agent with $env:ITE2E_AI_AGENT (copilot|claude|codex|gemini).

BeforeDiscovery {
    $agent = if ($env:ITE2E_AI_AGENT) { $env:ITE2E_AI_AGENT } else { 'copilot' }
    $script:HasAgent = [bool](Get-Command $agent -ErrorAction SilentlyContinue)
}

Describe 'AI oracle (agent CLI judge)' -Tag 'AI' -Skip:(-not $script:HasAgent) {
    BeforeAll { Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force }

    It 'extracts a JSON verdict from noisy CLI output' {
        $noisy = "blah`n{`"pass`": true, `"confidence`": 0.9, `"reason`": `"ok`"}`n`nTokens 5"
        (Get-JsonObjectFromText -Text $noisy).pass | Should -BeTrue
    }

    It 'returns pass=true for a supported claim' {
        $ctx = "PS C:\repo> git status`nOn branch main`nnothing to commit, working tree clean"
        $r = Test-AIClaim -Claim 'The output shows the current git branch is main.' -Context $ctx
        $r.pass | Should -BeTrue
    }

    It 'returns pass=false for an unsupported claim' {
        $ctx = "PS C:\repo> echo hello`nhello`nPS C:\repo>"
        $r = Test-AIClaim -Claim 'An autofix suggestion was offered for a failed command.' -Context $ctx
        $r.pass | Should -BeFalse
    }

    It 'Assert-AI throws on a false claim' {
        $ctx = "PS C:\repo> echo hi`nhi"
        { Assert-AI -Claim 'The terminal is currently displaying an error dialog.' -Context $ctx } | Should -Throw
    }
}
