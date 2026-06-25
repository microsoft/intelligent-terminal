[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$PayloadZip,

    [string]$InstallDir = "$env:LOCALAPPDATA\Programs\IntelligentTerminal",

    [switch]$NoPathUpdate,

    [switch]$NoShortcuts,

    [string]$StartMenuDir = "$env:APPDATA\Microsoft\Windows\Start Menu\Programs\Intelligent Terminal",

    [switch]$Quiet
)

$ErrorActionPreference = 'Stop'

$PromptConfigDir = Join-Path $env:LOCALAPPDATA 'IntelligentTerminal\prompts'
$PromptUserPath = Join-Path $PromptConfigDir 'terminal-agent.md'
$PromptDefaultPath = Join-Path $PromptConfigDir 'terminal-agent.default.md'
$InstallMetadataFileName = 'intelligent-terminal-install-metadata.json'

# Legacy paths from the prior "Agentic Terminal" name — cleaned up on install.
$LegacyInstallDir = Join-Path $env:LOCALAPPDATA 'Programs\AgenticTerminal'
$LegacyStartMenuDir = Join-Path $env:APPDATA 'Microsoft\Windows\Start Menu\Programs\Agentic Terminal'
$LegacyPromptConfigDir = Join-Path $env:LOCALAPPDATA 'AgenticTerminal\prompts'

function Write-Status {
    param([string]$Message)

    if (-not $Quiet) {
        Write-Host $Message
    }
}

function Ensure-Directory {
    param([string]$Path)

    if (-not (Test-Path $Path -PathType Container)) {
        New-Item -ItemType Directory -Path $Path | Out-Null
    }
}

function Remove-DirectoryContents {
    param([string]$Path)

    if (Test-Path $Path -PathType Container) {
        Get-ChildItem $Path -Force | Remove-Item -Recurse -Force
    }
}

function Add-InstallDirToUserPath {
    param([string]$PathToAdd)

    $current = [Environment]::GetEnvironmentVariable('Path', 'User')
    $parts = @()
    if (-not [string]::IsNullOrWhiteSpace($current)) {
        $parts = $current.Split(';') | Where-Object { -not [string]::IsNullOrWhiteSpace($_) }
    }

    if ($parts -contains $PathToAdd) {
        return
    }

    $updated = @($parts + $PathToAdd) -join ';'
    [Environment]::SetEnvironmentVariable('Path', $updated, 'User')
}

function Remove-PathFromUserPath {
    param([string]$PathToRemove)

    $current = [Environment]::GetEnvironmentVariable('Path', 'User')
    if ([string]::IsNullOrWhiteSpace($current)) {
        return
    }

    $remaining = @(
        $current.Split(';') |
            Where-Object { $_ -and ($_ -ne $PathToRemove) }
    )
    $updated = $remaining -join ';'
    if ($updated -ne $current) {
        [Environment]::SetEnvironmentVariable('Path', $updated, 'User')
    }
}

function Migrate-LegacyPrompts {
    $legacyUserPrompt = Join-Path $LegacyPromptConfigDir 'terminal-agent.md'
    if (-not (Test-Path $legacyUserPrompt -PathType Leaf)) {
        return
    }

    if (Test-Path $PromptUserPath -PathType Leaf) {
        return
    }

    Ensure-Directory $PromptConfigDir
    Copy-Item -Path $legacyUserPrompt -Destination $PromptUserPath -Force
    Write-Status "Migrated customized planner prompt from $LegacyPromptConfigDir."
}

function Remove-LegacyAgenticInstall {
    if (Test-Path $LegacyInstallDir -PathType Container) {
        Write-Status "Removing legacy AgenticTerminal install at $LegacyInstallDir ..."
        try {
            Stop-RunningInstalledProcesses -InstallRoot $LegacyInstallDir
        } catch {
            Write-Status "  Warning: failed to stop legacy processes: $_"
        }
        Remove-Item $LegacyInstallDir -Recurse -Force -ErrorAction SilentlyContinue
    }

    if (Test-Path $LegacyStartMenuDir -PathType Container) {
        Write-Status "Removing legacy Start menu folder $LegacyStartMenuDir ..."
        Remove-Item $LegacyStartMenuDir -Recurse -Force -ErrorAction SilentlyContinue
    }

    Remove-PathFromUserPath -PathToRemove $LegacyInstallDir
}

function Read-InstallMetadata {
    param([string]$RootPath)

    $metadataPath = Join-Path $RootPath $InstallMetadataFileName
    if (-not (Test-Path $metadataPath -PathType Leaf)) {
        return $null
    }

    return Get-Content $metadataPath -Raw | ConvertFrom-Json
}

function Get-MetadataVersionLabel {
    param($Metadata)

    if ($null -eq $Metadata) {
        return $null
    }

    $parts = @()
    if (-not [string]::IsNullOrWhiteSpace($Metadata.productName)) {
        $parts += [string]$Metadata.productName
    }
    if (-not [string]::IsNullOrWhiteSpace($Metadata.version)) {
        $parts += [string]$Metadata.version
    }

    $qualifiers = @()
    if (-not [string]::IsNullOrWhiteSpace($Metadata.platform)) {
        $qualifiers += [string]$Metadata.platform
    }
    if (-not [string]::IsNullOrWhiteSpace($Metadata.configuration)) {
        $qualifiers += [string]$Metadata.configuration
    }

    $label = $parts -join ' '
    if ($qualifiers.Count -gt 0) {
        $label = '{0} ({1})' -f $label, ($qualifiers -join ' ')
    }

    return $label
}

function Get-ExecutablePathWithinInstallDir {
    param(
        [Parameter(Mandatory = $true)]
        [string]$ExecutablePath,

        [Parameter(Mandatory = $true)]
        [string]$InstallRoot
    )

    if ([string]::IsNullOrWhiteSpace($ExecutablePath)) {
        return $null
    }

    $normalizedInstallRoot = [System.IO.Path]::GetFullPath($InstallRoot).TrimEnd('\') + '\'
    $normalizedExecutablePath = [System.IO.Path]::GetFullPath($ExecutablePath)
    if (-not $normalizedExecutablePath.StartsWith($normalizedInstallRoot, [System.StringComparison]::OrdinalIgnoreCase)) {
        return $null
    }

    return $normalizedExecutablePath
}

function Get-RunningInstalledProcesses {
    param([string]$InstallRoot)

    $candidates = Get-CimInstance Win32_Process -Filter "Name = 'IntelligentTerminal.exe' OR Name = 'wta.exe'" -ErrorAction SilentlyContinue
    $running = @()

    foreach ($candidate in $candidates) {
        $matchedExecutablePath = Get-ExecutablePathWithinInstallDir -ExecutablePath $candidate.ExecutablePath -InstallRoot $InstallRoot
        if ($matchedExecutablePath) {
            $running += [pscustomobject]@{
                ProcessId = [int]$candidate.ProcessId
                Name = [string]$candidate.Name
                ExecutablePath = $matchedExecutablePath
            }
        }
    }

    return @($running | Sort-Object Name, ProcessId -Unique)
}

function Stop-RunningInstalledProcesses {
    param([string]$InstallRoot)

    $running = @(Get-RunningInstalledProcesses -InstallRoot $InstallRoot)
    if ($running.Count -eq 0) {
        return
    }

    Write-Status "Stopping running Intelligent Terminal processes ..."
    foreach ($processInfo in $running) {
        Write-Status ("  Stopping {0} (PID {1})" -f $processInfo.Name, $processInfo.ProcessId)
        Stop-Process -Id $processInfo.ProcessId -Force -ErrorAction SilentlyContinue
    }

    $deadline = (Get-Date).AddSeconds(10)
    do {
        Start-Sleep -Milliseconds 200
        $remaining = @(Get-RunningInstalledProcesses -InstallRoot $InstallRoot)
        if ($remaining.Count -eq 0) {
            return
        }
    } while ((Get-Date) -lt $deadline)

    $remainingSummary = ($remaining | ForEach-Object { "{0} (PID {1})" -f $_.Name, $_.ProcessId }) -join ', '
    throw "Timed out waiting for installed Intelligent Terminal processes to exit: $remainingSummary"
}

function New-Shortcut {
    param(
        [Parameter(Mandatory = $true)]
        [string]$ShortcutPath,

        [Parameter(Mandatory = $true)]
        [string]$TargetPath,

        [string]$WorkingDirectory
    )

    $shell = New-Object -ComObject WScript.Shell
    $shortcut = $shell.CreateShortcut($ShortcutPath)
    $shortcut.TargetPath = $TargetPath
    if ($WorkingDirectory) {
        $shortcut.WorkingDirectory = $WorkingDirectory
    }
    $shortcut.Save()
}

function Seed-PlannerPromptFiles {
    param(
        [Parameter(Mandatory = $true)]
        [string]$InstallRoot
    )

    $bundledPromptPath = Join-Path $InstallRoot 'prompts\terminal-agent.default.md'
    if (-not (Test-Path $bundledPromptPath -PathType Leaf)) {
        Write-Status "Bundled planner prompt template not found; skipping prompt seeding."
        return
    }

    Ensure-Directory $PromptConfigDir
    $existingDefaultContent = $null
    $existingUserContent = $null

    if (Test-Path $PromptDefaultPath -PathType Leaf) {
        $existingDefaultContent = Get-Content $PromptDefaultPath -Raw
    }
    if (Test-Path $PromptUserPath -PathType Leaf) {
        $existingUserContent = Get-Content $PromptUserPath -Raw
    }

    Copy-Item -Path $bundledPromptPath -Destination $PromptDefaultPath -Force

    if (-not (Test-Path $PromptUserPath -PathType Leaf)) {
        Copy-Item -Path $bundledPromptPath -Destination $PromptUserPath -Force
    } elseif ($null -ne $existingDefaultContent -and $existingUserContent -eq $existingDefaultContent) {
        Copy-Item -Path $bundledPromptPath -Destination $PromptUserPath -Force
    }
}

if (-not (Test-Path $PayloadZip -PathType Leaf)) {
    throw "Payload zip not found: $PayloadZip"
}

$payloadRoot = Join-Path ([System.IO.Path]::GetTempPath()) ("intelligent-terminal-install-" + [Guid]::NewGuid().ToString("N"))
$expandedRoot = Join-Path $payloadRoot 'expanded'

try {
    Ensure-Directory $payloadRoot
    Ensure-Directory $expandedRoot

    Write-Status "Extracting installer payload..."
    Expand-Archive -Path $PayloadZip -DestinationPath $expandedRoot -Force

    $sourceRoot = $expandedRoot
    $children = @(Get-ChildItem $expandedRoot)
    if ($children.Count -eq 1 -and $children[0].PSIsContainer) {
        $sourceRoot = $children[0].FullName
    }

    $incomingMetadata = Read-InstallMetadata -RootPath $sourceRoot
    $incomingVersionLabel = Get-MetadataVersionLabel -Metadata $incomingMetadata
    if ($incomingVersionLabel) {
        Write-Status "Preparing to install $incomingVersionLabel"
    }

    $installedMetadata = Read-InstallMetadata -RootPath $InstallDir
    $installedVersionLabel = Get-MetadataVersionLabel -Metadata $installedMetadata
    if ($installedVersionLabel) {
        Write-Status "Existing install detected: $installedVersionLabel"
    }

    Remove-LegacyAgenticInstall

    Ensure-Directory $InstallDir
    Stop-RunningInstalledProcesses -InstallRoot $InstallDir
    Write-Status "Installing to $InstallDir ..."

    # Preserve user settings across upgrades.
    $settingsDir = Join-Path $InstallDir 'settings'
    $settingsBackup = $null
    if (Test-Path $settingsDir -PathType Container) {
        $settingsBackup = Join-Path ([System.IO.Path]::GetTempPath()) "intelligent-terminal-settings-backup-$([System.IO.Path]::GetRandomFileName())"
        Copy-Item -Path $settingsDir -Destination $settingsBackup -Recurse -Force
        Write-Status "Backed up settings to $settingsBackup"
    }

    Remove-DirectoryContents $InstallDir
    Copy-Item -Path (Join-Path $sourceRoot '*') -Destination $InstallDir -Recurse -Force

    # Restore preserved settings.
    if ($settingsBackup -and (Test-Path $settingsBackup -PathType Container)) {
        Ensure-Directory $settingsDir
        Copy-Item -Path (Join-Path $settingsBackup '*') -Destination $settingsDir -Recurse -Force
        Remove-Item $settingsBackup -Recurse -Force -ErrorAction SilentlyContinue
        Write-Status "Restored user settings"
    }

    $terminalExe = Join-Path $InstallDir 'IntelligentTerminal.exe'
    $wtaExe = Join-Path $InstallDir 'wta.exe'

    if (-not $NoShortcuts) {
        Ensure-Directory $StartMenuDir

        if (Test-Path $terminalExe -PathType Leaf) {
            New-Shortcut -ShortcutPath (Join-Path $StartMenuDir 'Intelligent Terminal.lnk') -TargetPath $terminalExe -WorkingDirectory $InstallDir
        }
        if (Test-Path $wtaExe -PathType Leaf) {
            New-Shortcut -ShortcutPath (Join-Path $StartMenuDir 'WTA.lnk') -TargetPath $wtaExe -WorkingDirectory $InstallDir
        }
    }

    if (-not $NoPathUpdate) {
        Write-Status "Adding install directory to user PATH ..."
        Add-InstallDirToUserPath -PathToAdd $InstallDir
    }

    Migrate-LegacyPrompts

    Write-Status "Seeding planner prompt files in $PromptConfigDir ..."
    Seed-PlannerPromptFiles -InstallRoot $InstallDir

    Write-Status "Installation complete."
}
finally {
    if (Test-Path $payloadRoot -PathType Container) {
        Remove-Item $payloadRoot -Recurse -Force -ErrorAction SilentlyContinue
    }
}
