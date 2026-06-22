# Settings.ps1 — settings.json read/patch primitives.
# The AI settings (acpAgent, autoFixEnabled, agentPanePosition,
# aiIntegration.coordinator.enabled, ...) are TOP-LEVEL keys whose NAMES contain dots
# (MTSMSettings.h), so they are set as single literal properties, not nested paths.

function ConvertFrom-JsonC {
    <# Parse JSON-with-comments (strip // and /* */), tolerant of trailing commas. #>
    [CmdletBinding()] param([Parameter(ValueFromPipeline)][string]$Text)
    process {
        if ([string]::IsNullOrWhiteSpace($Text)) { return $null }
        $noBlock = [regex]::Replace($Text, '/\*[\s\S]*?\*/', '')
        $noLine = [regex]::Replace($noBlock, '(?m)^\s*//.*$', '')
        $noTrailing = [regex]::Replace($noLine, ',\s*([}\]])', '$1')
        try { $noTrailing | ConvertFrom-Json -Depth 64 } catch { Write-ItLog -Level WARN -Message "ConvertFrom-JsonC failed: $_"; $null }
    }
}

function Get-WtSettingsObject {
    <# Read the package's settings.json as an object (or $null if absent/unparseable). #>
    [CmdletBinding()] param([Parameter(Mandatory, ValueFromPipeline)]$App)
    process {
        if (-not (Test-Path $App.SettingsPath)) { return $null }
        (Get-Content -LiteralPath $App.SettingsPath -Raw -Encoding utf8) | ConvertFrom-JsonC
    }
}

function Set-WtSetting {
    <#
    .SYNOPSIS
        Set a top-level settings.json property and wait for the on-disk write to confirm.
        Self-verifies by re-reading the file.
    #>
    [CmdletBinding()]
    param(
        [Parameter(Mandatory, ValueFromPipeline)]$App,
        [Parameter(Mandatory)][string]$Key,
        [Parameter(Mandatory)][AllowNull()]$Value
    )
    process {
        $obj = Get-WtSettingsObject -App $App
        if (-not $obj) { $obj = [pscustomobject]@{ '$schema' = 'https://aka.ms/terminal-profiles-schema' } }
        if ($obj.PSObject.Properties.Name -contains $Key) { $obj.$Key = $Value }
        else { $obj | Add-Member -NotePropertyName $Key -NotePropertyValue $Value -Force }
        $json = $obj | ConvertTo-Json -Depth 64
        Set-Content -LiteralPath $App.SettingsPath -Value $json -Encoding utf8
        Write-ItLog -Level INFO -Message "Set setting '$Key' = '$Value'"
        Wait-Until -TimeoutSec 5 -IntervalSec 0.3 -Because "settings.json to reflect $Key" -Condition {
            $cur = Get-WtSetting -App $App -Key $Key
            "$cur" -eq "$Value"
        } | Out-Null
        $App
    }
}

function Get-WtSetting {
    <# Read a top-level settings.json property value (or $null). #>
    [CmdletBinding()]
    param([Parameter(Mandatory, ValueFromPipeline)]$App, [Parameter(Mandatory)][string]$Key)
    process {
        $obj = Get-WtSettingsObject -App $App
        if ($obj -and ($obj.PSObject.Properties.Name -contains $Key)) { return $obj.$Key }
        $null
    }
}

# ── Typed convenience wrappers ──
function Set-WtAgent { param([Parameter(Mandatory, ValueFromPipeline)]$App, [Parameter(Mandatory)][string]$Agent) process { Set-WtSetting -App $App -Key 'acpAgent' -Value $Agent } }
function Set-WtDelegateAgent { param([Parameter(Mandatory, ValueFromPipeline)]$App, [Parameter(Mandatory)][string]$Agent) process { Set-WtSetting -App $App -Key 'delegateAgent' -Value $Agent } }
function Set-WtAutofix { param([Parameter(Mandatory, ValueFromPipeline)]$App, [Parameter(Mandatory)][bool]$Enabled) process { Set-WtSetting -App $App -Key 'autoFixEnabled' -Value $Enabled } }
function Set-WtPanePosition { param([Parameter(Mandatory, ValueFromPipeline)]$App, [ValidateSet('bottom', 'right', 'top', 'left')][string]$Position) process { Set-WtSetting -App $App -Key 'agentPanePosition' -Value $Position } }

function Set-WtSettings {
    <# Apply a hashtable of top-level key/value settings in one call. #>
    [CmdletBinding()]
    param([Parameter(Mandatory, ValueFromPipeline)]$App, [Parameter(Mandatory)][hashtable]$Settings)
    process { foreach ($k in $Settings.Keys) { Set-WtSetting -App $App -Key $k -Value $Settings[$k] | Out-Null }; $App }
}
