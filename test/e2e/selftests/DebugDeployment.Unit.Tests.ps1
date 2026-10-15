Describe 'Debug deployment version guard' -Tag Unit {
    BeforeAll {
        $scriptPath = (Resolve-Path (Join-Path $PSScriptRoot '..\..\..\build\scripts\Invoke-IntelligentTerminalDebugDeployment.ps1')).Path
        $tokens = $null
        $errors = $null
        $ast = [System.Management.Automation.Language.Parser]::ParseFile($scriptPath, [ref]$tokens, [ref]$errors)
        if ($errors.Count) { throw 'Deployment script did not parse.' }
        $guards = @($ast.FindAll({
            param($node)
            $node -is [System.Management.Automation.Language.IfStatementAst] -and
                $node.Extent.Text.Contains('Refusing to deploy Dev version')
        }, $true))
        if ($guards.Count -ne 1) { throw 'Expected the single product downgrade guard.' }
        # Evaluate the actual guard without deployment, package or process side effects.
        $script:guard = [scriptblock]::Create(
            'param($package, [version]$buildVersion) ' + $guards[0].Clauses[0].Item1.Extent.Text)
    }

    It 'Compares Appx string versions numerically (<Installed> to <Build>)' -TestCases @(
        @{ Installed = '0.8.0.2'; Build = '0.8.0.11'; Downgrade = $false }
        @{ Installed = '0.8.0.10'; Build = '0.8.0.11'; Downgrade = $false }
        @{ Installed = '0.8.0.11'; Build = '0.8.0.11'; Downgrade = $false }
        @{ Installed = '0.8.0.11'; Build = '0.8.0.2'; Downgrade = $true }
    ) {
        param($Installed, $Build, $Downgrade)
        & $script:guard ([pscustomobject]@{ Version = [string]$Installed }) ([version]$Build) |
            Should -Be $Downgrade
    }
}
