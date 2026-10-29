#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }

BeforeAll {
    Set-StrictMode -Version Latest
    $verifier = Join-Path $PSScriptRoot '..\..\..\build\scripts\Verify-AgentHooks.ps1'
    $errors = $null
    $ast = [Management.Automation.Language.Parser]::ParseFile($verifier, [ref]$null, [ref]$errors)
    if ($errors) { throw ($errors -join "`n") }
    $schemaVersion = $ast.Find({
        param($node)
        $node -is [Management.Automation.Language.AssignmentStatementAst] -and
            $node.Left.Extent.Text -eq '$script:SupportedStatusSchemaVersion'
    }, $true)
    . ([scriptblock]::Create($schemaVersion.Extent.Text))
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
        }
    }
    function New-StatusReport {
        param($Cli)
        [pscustomobject]@{ bundle_source = @{ kind = 'embedded' }; clis = @($Cli) }
    }
}

Describe 'Unit: agent hook verifier OpenCode compatibility' -Tag 'Unit' {
    It 'validates shared status schema <Version>' -TestCases @(
        @{ Version = 5; Accepted = $true },
        @{ Version = 6; Accepted = $false }
    ) {
        param($Version, $Accepted)
        $report = New-StatusReport (New-CliStatus)
        $report | Add-Member schema_version $Version
        $json = $report | ConvertTo-Json -Depth 5
        if ($Accepted) {
            (Get-AgentHooksStatus -Json $json).schema_version | Should -Be 5
        } else {
            { Get-AgentHooksStatus -Json $json } | Should -Throw '*Unsupported schema_version 6*'
        }
    }

    It 'reports healthy hooks using only shared status fields' {
        $cli = New-CliStatus
        $row = Get-AgentHookCliRow -Cli $cli
        $row.State | Should -Be 'installed'
        $row.Detail | Should -BeNullOrEmpty
        (Test-AgentHooksConsistent -Report (New-StatusReport $cli)).Ok | Should -BeTrue
    }

    It 'reports an API mismatch rather than filesystem fallback' {
        $cli = New-CliStatus
        $cli.plugin_enabled = $false
        $cli.detection_fallback = 'opencode-api-variant-mismatch'
        $row = Get-AgentHookCliRow -Cli $cli
        $row.Detail | Should -Match 'OpenCode API variant mismatch'
        $result = Test-AgentHooksConsistent -Report (New-StatusReport $cli)
        $result.Ok | Should -BeFalse
        ($result.Reasons -join "`n") | Should -Match 'OpenCode API variant mismatch'
        ($result.Reasons -join "`n") | Should -Not -Match 'fs detection|fs fallback'
    }

    It 'surfaces an unknown CLI version through the shared detection reason' {
        $cli = New-CliStatus
        $cli.plugin_enabled = $false
        $cli.detection_fallback = 'opencode-version-unknown'
        $row = Get-AgentHookCliRow -Cli $cli
        $row.Detail | Should -Match 'OpenCode CLI version unknown'
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
