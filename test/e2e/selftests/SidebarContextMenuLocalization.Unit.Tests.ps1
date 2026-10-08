#Requires -Modules @{ ModuleName='Pester'; ModuleVersion='5.0.0' }

BeforeDiscovery {
    $resourceRoot = Join-Path (Resolve-Path (Join-Path $PSScriptRoot '..\..\..')).Path 'src\cascadia\TerminalApp\Resources'
    $localeCases = Get-ChildItem -LiteralPath $resourceRoot -Directory |
        Where-Object { Test-Path -LiteralPath (Join-Path $_.FullName 'Resources.resw') } |
        ForEach-Object { @{ Locale = $_.Name; ResourceFile = Join-Path $_.FullName 'Resources.resw' } }
    $menuCases = @(
        @{ Key = 'TabColorChoose'; Arabic = 'تغيير لون علامة التبويب' }
        @{ Key = 'RenameTabText'; Arabic = 'إعادة تسمية علامة التبويب' }
        @{ Key = 'DuplicateTabText'; Arabic = 'تكرار علامة التبويب' }
        @{ Key = 'SplitTabText'; Arabic = 'تقسيم علامة التبويب' }
        @{ Key = 'TabMoveSubMenu'; Arabic = 'نقل علامة التبويب' }
        @{ Key = 'ExportTabText'; Arabic = 'تصدير النص' }
        @{ Key = 'FindText'; Arabic = 'بحث' }
        @{ Key = 'RestartConnectionText'; Arabic = 'إعادة تشغيل الجلسة' }
        @{ Key = 'TabCloseSubMenu'; Arabic = 'إغلاق' }
        @{ Key = 'TabClose'; Arabic = 'إغلاق علامة التبويب' }
        @{ Key = 'MoveTabToNewWindowText'; Arabic = 'نقل علامة التبويب إلى نافذة جديدة' }
        @{ Key = 'TabMoveLeft'; Arabic = 'نقل إلى اليسار' }
        @{ Key = 'TabMoveRight'; Arabic = 'نقل إلى اليمين' }
        @{ Key = 'TabMoveUp'; Arabic = 'نقل إلى أعلى' }
        @{ Key = 'TabMoveDown'; Arabic = 'نقل إلى أسفل' }
        @{ Key = 'TabCloseAfter'; Arabic = 'إغلاق علامات التبويب إلى اليمين' }
        @{ Key = 'TabCloseBelow'; Arabic = 'إغلاق علامات التبويب أدناه' }
        @{ Key = 'TabCloseOther'; Arabic = 'إغلاق علامات التبويب الأخرى' }
        @{ Key = 'ClosePaneText'; Arabic = 'إغلاق اللوحة' }
    )
}

Describe 'Shipped tab context menu resource category parity' -Tag Unit {
    BeforeAll {
        $root = (Resolve-Path (Join-Path $PSScriptRoot '..\..\..')).Path
        [xml]$sourceResources = Get-Content -LiteralPath (Join-Path $root 'src\cascadia\TerminalApp\Resources\en-US\Resources.resw') -Raw
        $categoryKeys = @(
            'TabColorChoose', 'RenameTabText', 'DuplicateTabText', 'SplitTabText',
            'TabMoveSubMenu', 'ExportTabText', 'FindText', 'RestartConnectionText',
            'TabCloseSubMenu', 'TabClose', 'MoveTabToNewWindowText', 'TabMoveLeft',
            'TabMoveRight', 'TabMoveUp', 'TabMoveDown', 'TabCloseAfter',
            'TabCloseBelow', 'TabCloseOther', 'ClosePaneText'
        )
    }

    It 'provides unique localized canonical labels and source comments in <Locale>' -TestCases $localeCases {
        param($Locale, $ResourceFile)

        [xml]$targetResources = Get-Content -LiteralPath $ResourceFile -Raw
        foreach ($key in $categoryKeys) {
            $sourceEntry = @($sourceResources.root.data | Where-Object name -eq $key)
            $targetEntry = @($targetResources.root.data | Where-Object name -eq $key)
            $sourceEntry | Should -HaveCount 1 -Because "$key must use one canonical source entry"
            $targetEntry | Should -HaveCount 1 -Because "$Locale must not fall back for $key"
            [string]::IsNullOrWhiteSpace($targetEntry[0].value) | Should -BeFalse
            $targetEntry[0].comment | Should -BeExactly $sourceEntry[0].comment
            # Danish "Find" is the native imperative, not an English fallback.
            $identicalNativeTerm = $Locale -eq 'da-DK' -and $key -eq 'FindText'
            if ($Locale -notlike 'en-*' -and $Locale -notlike 'qps-*' -and -not $identicalNativeTerm) {
                $targetEntry[0].value | Should -Not -BeExactly $sourceEntry[0].value
            }
        }
    }
}

Describe 'Arabic tab context menu canonical resources' -Tag Unit {
    BeforeAll {
        $root = (Resolve-Path (Join-Path $PSScriptRoot '..\..\..')).Path
        $resourceRoot = Join-Path $root 'src\cascadia\TerminalApp\Resources'
        [xml]$english = Get-Content -LiteralPath (Join-Path $resourceRoot 'en-US\Resources.resw') -Raw
        [xml]$arabicResources = Get-Content -LiteralPath (Join-Path $resourceRoot 'ar-SA\Resources.resw') -Raw
        $tabSource = Get-Content -LiteralPath (Join-Path $root 'src\cascadia\TerminalApp\Tab.cpp') -Raw
    }

    It 'localizes <Key> using the existing shared horizontal/vertical menu lookup' -TestCases $menuCases {
        param($Key, $Arabic)

        $sourceEntry = @($english.root.data | Where-Object name -eq $Key)
        $targetEntry = @($arabicResources.root.data | Where-Object name -eq $Key)
        $sourceEntry | Should -HaveCount 1
        $targetEntry | Should -HaveCount 1
        $targetEntry[0].value | Should -BeExactly $Arabic
        $targetEntry[0].value | Should -Not -BeExactly $sourceEntry[0].value
        $targetEntry[0].comment | Should -BeExactly $sourceEntry[0].comment
        $tabSource | Should -Match ('RS_\(L"' + [regex]::Escape($Key) + '"\)')
    }

    It 'uses plain resource IDs rather than inventing XAML property or action-name aliases' {
        foreach ($key in @('TabColorChoose', 'RenameTabText', 'TabMoveSubMenu', 'TabCloseSubMenu')) {
            @($arabicResources.root.data | Where-Object name -eq "$key.Text") | Should -HaveCount 0
            $tabSource | Should -Not -Match ('RS_\(L"' + [regex]::Escape("$key/Text") + '"\)')
        }
    }
}
