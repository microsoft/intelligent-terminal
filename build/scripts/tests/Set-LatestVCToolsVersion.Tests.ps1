Describe 'Set-LatestVCToolsVersion fallback selection' {
    BeforeAll {
        $scriptPath = Join-Path $PSScriptRoot '..\Set-LatestVCToolsVersion.ps1'
        $content = Get-Content -LiteralPath $scriptPath -Raw
        $tokens = $null
        $errors = $null
        $ast = [System.Management.Automation.Language.Parser]::ParseInput($content, [ref]$tokens, [ref]$errors)
        if ($errors.Count -ne 0)
        {
            throw 'The toolset selection script failed to parse.'
        }

        # Exercise the production selection logic with synthetic VS discovery inputs.
        $selectionStart = $ast.Find({
            param($node)
            $node -is [System.Management.Automation.Language.AssignmentStatementAst] -and
            $node.Left.Extent.Text -eq '$PackageVCToolPath'
        }, $true).Extent.StartOffset
        $selection = [scriptblock]::Create($content.Substring($selectionStart))

        function Add-TestToolset([string]$Version, [switch]$Empty)
        {
            $bin = Join-Path $VCToolsRoot "$Version\bin"
            New-Item -ItemType Directory -Path $bin -Force | Out-Null
            if (-not $Empty)
            {
                Set-Content -LiteralPath (Join-Path $bin 'cl.exe') -Value 'test fixture'
            }
        }
    }

    BeforeEach {
        $VCToolsRoot = Join-Path $TestDrive ([guid]::NewGuid().ToString('N'))
        New-Item -ItemType Directory -Path $VCToolsRoot | Out-Null
        $LatestVCToolsVersion = '14.51.36244'
        $ErrorActionPreference = 'Stop'
    }

    It 'does not select the installed 14.52 preview when VS reports 14.51' {
        Add-TestToolset '14.51.36231'
        Add-TestToolset '14.52.36725'
        $output = & $selection
        $output | Should -Contain 'Latest VCToolsVersion: 14.51.36231'
        $output | Should -Contain '##vso[task.setvariable variable=VCToolsVersion]14.51.36231'
    }

    It 'keeps a populated exact package version' {
        Add-TestToolset '14.51.36244'
        Add-TestToolset '14.51.36245'
        $output = & $selection
        $output | Should -Contain 'Latest VCToolsVersion: 14.51.36244'
    }

    It 'selects the highest populated patch within the reported compiler family' {
        Add-TestToolset '14.51.36231'
        Add-TestToolset '14.51.36240'
        Add-TestToolset '14.51.36245' -Empty
        Add-TestToolset '14.52.36725'
        $output = & $selection
        $output | Should -Contain 'Latest VCToolsVersion: 14.51.36240'
    }

    It 'rejects an empty exact package directory and uses a populated matching version' {
        Add-TestToolset '14.51.36244' -Empty
        Add-TestToolset '14.51.36231'
        $output = & $selection
        $output | Should -Contain 'Latest VCToolsVersion: 14.51.36231'
    }

    It 'fails rather than crossing compiler families when only preview tools exist' {
        Add-TestToolset '14.52.36725'
        { & $selection } | Should -Throw '*No usable VC Tools installation matching 14.51*'
    }

    It 'fails when all matching compiler directories are empty' {
        Add-TestToolset '14.51.36231' -Empty
        { & $selection } | Should -Throw '*No usable VC Tools installation matching 14.51*'
    }
}
