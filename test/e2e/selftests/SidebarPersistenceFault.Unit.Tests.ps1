BeforeAll {
    . (Join-Path $PSScriptRoot '..\tests\helpers\SidebarPersistenceFault.ps1')
    $script:root = Join-Path $PSScriptRoot ('..\artifacts\sidebar-sharing-selftest-' + [guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $script:root | Out-Null
    $script:path = Join-Path $script:root 'state.json'
    $script:app = [pscustomobject]@{ StatePath = $script:path
        SettingsPath = Join-Path $script:root 'settings.json'; ConfigBackupOwned = $true }
}
AfterAll { Remove-Item -LiteralPath $script:root -Recurse -Force }
Describe 'Sidebar real file-sharing fault delivery without Terminal activation' {
    BeforeEach {
        '{"sidebarLayoutMigrationCompleted":false}' | Set-Content -LiteralPath $script:path
        Copy-Item -LiteralPath $script:path -Destination "$script:path.e2ebak" -Force
    }
    It 'denies real writes and replacement while retaining readable bytes, then permits one retry' {
        $before = [IO.File]::ReadAllText($script:path)
        $lock = Open-TestSidebarPersistenceLock -App $script:app -Path $script:path
        $replacement = Join-Path $script:root 'replacement.json'
        [IO.File]::WriteAllText($replacement, '{"sidebarLayoutMigrationCompleted":true}')
        try {
            [IO.File]::ReadAllText($script:path) | Should -Be $before
            { [IO.File]::WriteAllText($script:path, 'invalid') } | Should -Throw
            { [IO.File]::Move($replacement, $script:path, $true) } | Should -Throw
            [IO.File]::ReadAllText($script:path) | Should -Be $before
        }
        finally { $lock.Dispose() }
        [IO.File]::Move($replacement, $script:path, $true)
        ([IO.File]::ReadAllText($script:path) | ConvertFrom-Json).sidebarLayoutMigrationCompleted |
            Should -BeTrue
    }
    It 'refuses an unowned lease and refuses files outside the protected settings/state pair' {
        $unowned = [pscustomobject]@{ StatePath = $script:path; SettingsPath = $script:app.SettingsPath
            ConfigBackupOwned = $false }
        { Open-TestSidebarPersistenceLock -App $unowned -Path $script:path } | Should -Throw
        { Open-TestSidebarPersistenceLock -App $script:app -Path "$script:path.e2ebak" } | Should -Throw
    }
}
