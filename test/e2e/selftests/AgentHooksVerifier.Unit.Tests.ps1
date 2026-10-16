#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }

BeforeAll {
    Set-StrictMode -Version Latest
    $verifier = Join-Path $PSScriptRoot '..\..\..\build\scripts\Verify-AgentHooks.ps1'
    $errors = $null
    $ast = [Management.Automation.Language.Parser]::ParseFile($verifier, [ref]$null, [ref]$errors)
    if ($errors) { throw ($errors -join "`n") }
    foreach ($function in $ast.FindAll({
        param($node)
        $node -is [Management.Automation.Language.FunctionDefinitionAst]
    }, $false)) {
        . ([scriptblock]::Create($function.Extent.Text))
    }
    function New-CliStatus {
        param([string]$Name = 'opencode')
        [pscustomobject]@{
            name = $Name
            binary_on_path = $true
            marketplace_registered = $true
            plugin_installed = $true
            plugin_enabled = $true
            detection_fallback = $null
            opencode = [pscustomobject]@{
                cli_version = '2.0.10'
                api_variant = 'v2'
                installed_api_variant = 'v2'
            }
        }
    }
    function New-StatusReport {
        param($Cli)
        [pscustomobject]@{ bundle_source = @{ kind = 'embedded' }; clis = @($Cli) }
    }
}

Describe 'Unit: agent hook verifier OpenCode compatibility' -Tag 'Unit' {
    It 'shows the detected CLI version and matching API variants without reporting an inconsistency' {
        $cli = New-CliStatus
        $row = Get-AgentHookCliRow -Cli $cli
        $row.Detail | Should -Be 'CLI version=2.0.10; required API=v2; installed API=v2'
        (Test-AgentHooksConsistent -Report (New-StatusReport $cli)).Ok | Should -BeTrue
    }

    It 'reports an API mismatch rather than filesystem fallback' {
        $cli = New-CliStatus
        $cli.plugin_enabled = $false
        $cli.opencode.installed_api_variant = 'v1'
        $cli.detection_fallback = 'opencode-api-variant-mismatch'
        $row = Get-AgentHookCliRow -Cli $cli
        $row.Detail | Should -Match 'required API=v2; installed API=v1'
        $row.Detail | Should -Match 'OpenCode API variant mismatch'
        $result = Test-AgentHooksConsistent -Report (New-StatusReport $cli)
        $result.Ok | Should -BeFalse
        ($result.Reasons -join "`n") | Should -Match 'OpenCode API variant mismatch'
        ($result.Reasons -join "`n") | Should -Not -Match 'fs detection|fs fallback'
    }

    It 'surfaces unknown CLI versions, missing variants, and the probe error' {
        $cli = New-CliStatus
        $cli.plugin_enabled = $false
        $cli.opencode.cli_version = $null
        $cli.opencode.api_variant = $null
        $cli.opencode.installed_api_variant = $null
        $cli.opencode | Add-Member error 'version probe timed out'
        $cli.detection_fallback = 'opencode-version-unknown'
        $row = Get-AgentHookCliRow -Cli $cli
        $row.Detail | Should -Match 'CLI version=unknown; required API=unknown; installed API=none'
        $row.Detail | Should -Match 'error=version probe timed out'
        $result = Test-AgentHooksConsistent -Report (New-StatusReport $cli)
        $result.Ok | Should -BeFalse
        ($result.Reasons -join "`n") | Should -Match 'OpenCode CLI version unknown'
        ($result.Reasons -join "`n") | Should -Not -Match 'fs detection|fs fallback'
    }

    It 'preserves filesystem-fallback diagnostics for existing consumers' {
        $cli = New-CliStatus -Name 'claude'
        $cli.detection_fallback = 'plugin-list-failed'
        (Get-AgentHookCliRow -Cli $cli).Detail | Should -Be 'fs fallback'
        $result = Test-AgentHooksConsistent -Report (New-StatusReport $cli)
        $result.Ok | Should -BeFalse
        $result.Reasons | Should -Contain 'claude: wta fell back to fs detection (plugin-list-failed)'
    }
}
