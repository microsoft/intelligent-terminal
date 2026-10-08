function Open-TestSidebarPersistenceLock {
    [CmdletBinding()]
    param([Parameter(Mandatory)]$App, [Parameter(Mandatory)][string]$Path)
    $full = [IO.Path]::GetFullPath($Path)
    $owned = @($App.SettingsPath, $App.StatePath | Where-Object { $_ } |
        ForEach-Object { [IO.Path]::GetFullPath($_) })
    if (-not $App.ConfigBackupOwned -or $full -notin $owned -or
        (-not (Test-Path -LiteralPath "$full.e2ebak") -and
            -not (Test-Path -LiteralPath "$full.e2ebak.missing")) -or -not (Test-Path -LiteralPath $full)) {
        throw 'Persistence fault requires an existing fixture file and its owned byte backup.'
    }
    # Reads remain possible; both overwrite and atomic replacement are denied.
    [IO.File]::Open($full, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read)
}
