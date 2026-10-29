#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }

Describe 'Feature: sidebar metadata belongs to its window' -Tag @('Feature', 'SidebarMetadata') {
    BeforeAll {
        Import-Module (Join-Path $PSScriptRoot '..\ItE2E\ItE2E.psd1') -Force
        Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes
        if ($env:ITE2E_PACKAGE -ne 'Dev' -or
            -not $env:ITE2E_EXPECTED_APP_SHA256 -or -not $env:ITE2E_EXPECTED_WTA_SHA256) {
            throw 'Select the exact-source Dev package and provide its App and WTA hashes.'
        }
        $script:target = Resolve-ItApp -Package Dev
        Stop-StaleItInstances -App $script:target
        (Get-FileHash (Join-Path $script:target.InstallLocation 'TerminalApp.dll')).Hash |
            Should -Be $env:ITE2E_EXPECTED_APP_SHA256
        (Get-FileHash $script:target.WtaPath).Hash | Should -Be $env:ITE2E_EXPECTED_WTA_SHA256
        $script:app = $null
        $script:repository = $null

        function Get-RenderedMetadata {
            param($Window)
            $root = [Windows.Automation.AutomationElement]::FromHandle([IntPtr][long]$Window.Hwnd)
            $condition = [Windows.Automation.PropertyCondition]::new(
                [Windows.Automation.AutomationElement]::AutomationIdProperty, 'HeaderMetadataTextBlock')
            @($root.FindAll([Windows.Automation.TreeScope]::Descendants, $condition) | Where-Object {
                $_.Current.ProcessId -eq $Window.Pid -and -not $_.Current.IsOffscreen -and
                    $_.Current.BoundingRectangle.Width -gt 0
            } | ForEach-Object { $_.Current.Name })
        }

        function Set-OnlyMetadataField {
            param($Window, [string]$Field)
            Set-WtWindowForeground -App $Window | Should -BeTrue
            foreach ($selector in @('RichTabAgentStatusVisibleItem', 'RichTabWorkingDirectoryVisibleItem', $Field)) {
                Invoke-UiElement -App $Window -Selector FilterTabsButton | Out-Null
                Invoke-UiElement -App $Window -Selector $selector | Out-Null
                Send-WtWindowKey -App $Window -Vk 0x1B -RequireForeground | Out-Null
            }
        }

        function Report-ShellWorkingDirectory {
            param($Window)
            $pane = Wait-Until -TimeoutSec 15 -Because 'the window shell to start' -Condition {
                @(Get-WtPanes -App $Window -WindowId $Window.WindowId | Where-Object { $_.session_id }) |
                    Select-Object -First 1
            }
            Invoke-RunCommand -App $Window -SessionId $pane.session_id -Command `
                'Write-Host -NoNewline ([char]27 + '']9;9;"'' + (Get-Location).Path + ''"'' + [char]7)' | Out-Null
        }
    }

    AfterAll {
        try {
            if ($script:app) { Stop-Terminal -App $script:app }
        }
        finally {
            if ($script:repository -and (Test-Path -LiteralPath $script:repository)) {
                Remove-Item -LiteralPath $script:repository -Recurse -Force
            }
        }
    }

    It 'keeps Repository in window one while window two displays Branch' {
        $script:repository = Join-Path $PSScriptRoot ('..\artifacts\metadata-window-' + [guid]::NewGuid().ToString('N'))
        New-Item -ItemType Directory -Path $script:repository -Force | Out-Null
        & git -C $script:repository init --initial-branch fixture-branch --quiet
        $LASTEXITCODE | Should -Be 0
        & git -C $script:repository -c user.name='Metadata test' -c user.email='metadata-test@example.invalid' `
            -c "core.hooksPath=$script:repository" commit --allow-empty --quiet -m 'Create metadata fixture'
        $LASTEXITCODE | Should -Be 0
        $profile = '{' + [guid]::NewGuid() + '}'
        $script:app = Start-Terminal -Package Dev -PassFre $true -State @{
            sidebarLayoutMigrationCompleted = $true; sidebarIntroductionShown = $true
        } -Settings @{
            language = 'en-US'; tabLayout = 'vertical'; defaultProfile = $profile; startupActions = ''
            firstWindowPreference = 'defaultProfile'; windowingBehavior = 'useNew'
            profiles = @{ list = @(@{
                guid = $profile; name = 'Metadata window shell'
                commandline = 'pwsh -NoLogo -NoProfile'; startingDirectory = $script:repository
            }) }
        }
        $firstId = [string]$script:app.WindowId
        $previousHwnds = @(Get-WtWindowHwnds -App $script:app | ForEach-Object { [string]$_.hwnd })
        Report-ShellWorkingDirectory -Window $script:app
        Set-OnlyMetadataField -Window $script:app -Field RichTabRepositoryVisibleItem
        $expectedRepository = [regex]::Escape((Split-Path $script:repository -Leaf))
        Wait-Until -TimeoutSec 20 -Because 'the first window renders its selected repository' -Condition {
            @(Get-RenderedMetadata -Window $script:app | Where-Object { $_ -match "Repository: $expectedRepository" }).Count -eq 1
        } | Out-Null

        Send-WtWindowKey -App $script:app -Vk 0x4E -Ctrl -Shift -RequireForeground | Out-Null
        $secondId = Wait-Until -TimeoutSec 20 -Because 'the app opens a second window' -Condition {
            Get-WtWindows -App $script:app | Where-Object { [string]$_.window_id -ne $firstId } |
                Select-Object -First 1 -ExpandProperty window_id
        }
        $secondHwnd = Wait-Until -TimeoutSec 15 -Because 'the second window uses the same process' -Condition {
            Get-WtWindowHwnds -App $script:app | Where-Object {
                $_.pid -eq $script:app.Pid -and [string]$_.hwnd -notin $previousHwnds
            } | Select-Object -First 1 -ExpandProperty hwnd
        }
        $second = $script:app.PSObject.Copy()
        $second.WindowId = [string]$secondId
        $second.Hwnd = $secondHwnd
        Report-ShellWorkingDirectory -Window $second
        Set-OnlyMetadataField -Window $second -Field RichTabBranchVisibleItem
        $branch = 'fixture-branch'
        Wait-Until -TimeoutSec 20 -Because 'each window renders its own selection' -Condition {
            @(Get-RenderedMetadata -Window $script:app | Where-Object {
                $_ -match "Repository: $expectedRepository" -and $_ -notmatch 'Branch:'
            }).Count -eq 1 -and
            @(Get-RenderedMetadata -Window $second | Where-Object {
                $_ -match ([regex]::Escape("Branch: $branch")) -and $_ -notmatch 'Repository:'
            }).Count -eq 1
        } | Out-Null
        foreach ($window in @($script:app, $second)) {
            Invoke-UiElement -App $window -Selector FilterTabsButton | Out-Null
            (Get-UiElement -App $window -Selector RichTabRepositoryVisibleItem).toggleState |
                Should -Be $(if ($window.WindowId -eq $firstId) { 'on' } else { 'off' })
            (Get-UiElement -App $window -Selector RichTabBranchVisibleItem).toggleState |
                Should -Be $(if ($window.WindowId -eq $firstId) { 'off' } else { 'on' })
            Send-WtWindowKey -App $window -Vk 0x1B -RequireForeground | Out-Null
        }
    }
}
