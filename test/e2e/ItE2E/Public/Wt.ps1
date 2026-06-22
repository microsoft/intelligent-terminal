# Wt.ps1 — terminal/pane control over wtcli (COM IProtocolServer). The central
# control brick. Every function self-verifies its postcondition where practical.

function Invoke-WtCli {
    <#
    .SYNOPSIS
        Run a wtcli subcommand with package identity (WT_COM_CLSID set), returning parsed
        JSON. Throws on non-zero exit with stderr. `--json` is a global flag placed first.
    #>
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)]$App,
        [Parameter(Mandatory)][string[]]$Arguments,
        [int]$TimeoutSec = 20,
        [switch]$NoThrow
    )
    if (-not $App.ComClsid) { Resolve-WtComClsid -App $App | Out-Null }
    $r = Invoke-Native -FilePath $App.WtcliPath -Arguments (@('--json') + $Arguments) `
        -TimeoutSec $TimeoutSec -Environment @{ WT_COM_CLSID = $App.ComClsid }
    if ($r.ExitCode -ne 0) {
        $msg = "wtcli $($Arguments -join ' ') failed (exit $($r.ExitCode)): $($r.StdErr.Trim())"
        if ($NoThrow) { Write-ItLog -Level WARN -Message $msg; return $null }
        throw $msg
    }
    $r.StdOut | ConvertFrom-JsonSafe
}

function Invoke-WtCliRaw {
    <# Run a wtcli subcommand returning raw stdout text (e.g. capture-pane content). #>
    [CmdletBinding()]
    param([Parameter(Mandatory)]$App, [Parameter(Mandatory)][string[]]$Arguments, [int]$TimeoutSec = 20)
    if (-not $App.ComClsid) { Resolve-WtComClsid -App $App | Out-Null }
    $r = Invoke-Native -FilePath $App.WtcliPath -Arguments $Arguments `
        -TimeoutSec $TimeoutSec -Environment @{ WT_COM_CLSID = $App.ComClsid }
    if ($r.ExitCode -ne 0) { throw "wtcli $($Arguments -join ' ') failed (exit $($r.ExitCode)): $($r.StdErr.Trim())" }
    $r.StdOut
}

function Get-WtWindows {
    [CmdletBinding()] param([Parameter(Mandatory, ValueFromPipeline)]$App)
    process { (Invoke-WtCli -App $App -Arguments @('list-windows')).windows }
}

function Get-WtTabs {
    [CmdletBinding()] param([Parameter(Mandatory, ValueFromPipeline)]$App, [string]$WindowId)
    process {
        $a = @('list-tabs'); if ($WindowId) { $a += @('-w', $WindowId) }
        (Invoke-WtCli -App $App -Arguments $a).tabs
    }
}

function Get-WtPanes {
    [CmdletBinding()] param([Parameter(Mandatory, ValueFromPipeline)]$App, [string]$TabId, [string]$WindowId)
    process {
        $a = @('list-panes'); if ($TabId) { $a += @('-t', $TabId) }; if ($WindowId) { $a += @('-w', $WindowId) }
        (Invoke-WtCli -App $App -Arguments $a).panes
    }
}

function Get-ActivePane {
    <# Returns @{ session_id; tab_id; window_id }. #>
    [CmdletBinding()] param([Parameter(Mandatory, ValueFromPipeline)]$App)
    process { Invoke-WtCli -App $App -Arguments @('active-pane') }
}

function Get-WtPaneStatus {
    <# Returns @{ state; pid; exit_code }. #>
    [CmdletBinding()] param([Parameter(Mandatory, ValueFromPipeline)]$App, [Parameter(Mandatory)][string]$SessionId)
    process { Invoke-WtCli -App $App -Arguments @('pane-status', '-t', $SessionId) }
}

function New-WtTab {
    <# Create a tab. Returns @{ tab_id; session_id }. Verifies the tab appears. #>
    [CmdletBinding()]
    param([Parameter(Mandatory, ValueFromPipeline)]$App, [string]$Command, [string]$Cwd, [string]$Title)
    process {
        $a = @('new-tab'); if ($Command) { $a += @('-c', $Command) }; if ($Cwd) { $a += @('-d', $Cwd) }; if ($Title) { $a += @('-n', $Title) }
        $res = Invoke-WtCli -App $App -Arguments $a
        if (-not $res.session_id) { throw "new-tab did not return a session_id." }
        # Robust existence check: pane-status resolves the pane regardless of which
        # tab/window it landed in (list-panes is active-tab scoped).
        Wait-Until -TimeoutSec 10 -Because "new tab $($res.session_id) to appear" -Condition {
            try { [bool](Get-WtPaneStatus -App $App -SessionId $res.session_id) } catch { $false }
        } | Out-Null
        $res
    }
}

function Split-WtPane {
    <# Split a pane. Returns the new pane info. #>
    [CmdletBinding()]
    param(
        [Parameter(Mandatory, ValueFromPipeline)]$App,
        [Parameter(Mandatory)][string]$SessionId,
        [ValidateSet('right', 'left', 'up', 'down', 'auto')][string]$Direction = 'auto',
        [double]$Size, [string]$Command
    )
    process {
        $a = @('split-pane', '-t', $SessionId, '-d', $Direction)
        if ($Size) { $a += @('-s', $Size) }; if ($Command) { $a += @('-c', $Command) }
        Invoke-WtCli -App $App -Arguments $a
    }
}

function Close-WtPane {
    [CmdletBinding()]
    param([Parameter(Mandatory, ValueFromPipeline)]$App, [Parameter(Mandatory)][string]$SessionId)
    process {
        Invoke-WtCli -App $App -Arguments @('kill-pane', '-t', $SessionId) -NoThrow | Out-Null
        Wait-Until -TimeoutSec 10 -Because "pane $SessionId to be gone" -Condition {
            try { $st = Get-WtPaneStatus -App $App -SessionId $SessionId; (-not $st) -or ($st.state -match 'exit|closed|terminated|gone') } catch { $true }
        } | Out-Null
    }
}

function Set-WtPaneFocus {
    [CmdletBinding()]
    param([Parameter(Mandatory, ValueFromPipeline)]$App, [Parameter(Mandatory)][string]$SessionId)
    process {
        Invoke-WtCli -App $App -Arguments @('focus-pane', '-t', $SessionId) | Out-Null
        Wait-Until -TimeoutSec 8 -Because "pane $SessionId to become active" -Condition {
            (Get-ActivePane -App $App).session_id -eq $SessionId
        } | Out-Null
    }
}

function Send-WtInput {
    <# Send literal UTF-8 text to a pane (no token translation). #>
    [CmdletBinding()]
    param([Parameter(Mandatory, ValueFromPipeline)]$App, [Parameter(Mandatory)][string]$SessionId, [Parameter(Mandatory)][string]$Text)
    process { Invoke-WtCli -App $App -Arguments @('send-keys', '--raw', '-t', $SessionId, '--', $Text) | Out-Null }
}

function Send-WtKeys {
    <# Send tmux-style key tokens (e.g. 'Enter', 'C-c', 'Tab') to a pane. #>
    [CmdletBinding()]
    param([Parameter(Mandatory, ValueFromPipeline)]$App, [Parameter(Mandatory)][string]$SessionId, [Parameter(Mandatory)][string[]]$Keys)
    process { Invoke-WtCli -App $App -Arguments (@('send-keys', '-t', $SessionId, '--') + $Keys) | Out-Null }
}

function Get-WtCapture {
    <# Capture pane buffer text. -LastPrompt requires OSC 133 shell integration. #>
    [CmdletBinding()]
    param(
        [Parameter(Mandatory, ValueFromPipeline)]$App, [Parameter(Mandatory)][string]$SessionId,
        [int]$MaxLines, [switch]$LastPrompt
    )
    process {
        $a = @('capture-pane', '-t', $SessionId)
        if ($MaxLines) { $a += @('-l', $MaxLines) }; if ($LastPrompt) { $a += '--last-prompt' }
        Invoke-WtCliRaw -App $App -Arguments $a
    }
}

function Wait-WtPaneExit {
    [CmdletBinding()]
    param([Parameter(Mandatory, ValueFromPipeline)]$App, [Parameter(Mandatory)][string]$SessionId, [int]$TimeoutSec = 30)
    process {
        Invoke-WtCli -App $App -Arguments @('wait-for', '-t', $SessionId, '--timeout', $TimeoutSec) -TimeoutSec ($TimeoutSec + 5) | Out-Null
    }
}

function Invoke-RunCommand {
    <#
    .SYNOPSIS
        Type a command into a pane, press Enter, wait for the prompt to settle, and
        return the captured last-prompt output. Best with PowerShell shell integration.
    #>
    [CmdletBinding()]
    param(
        [Parameter(Mandatory, ValueFromPipeline)]$App,
        [Parameter(Mandatory)][string]$SessionId,
        [Parameter(Mandatory)][string]$Command,
        [int]$SettleSec = 10
    )
    process {
        Send-WtInput -App $App -SessionId $SessionId -Text $Command
        Send-WtKeys -App $App -SessionId $SessionId -Keys @('Enter')
        # Best-effort settle: wait until the captured buffer stops changing.
        $prev = ''
        Wait-Until -TimeoutSec $SettleSec -IntervalSec 0.75 -Quiet -Condition {
            $cur = Get-WtCapture -App $App -SessionId $SessionId -MaxLines 50
            $stable = ($cur -eq $prev) -and ($cur -match [regex]::Escape($Command))
            $script:__rc_prev = $cur; $prev = $cur
            $stable
        } | Out-Null
        Get-WtCapture -App $App -SessionId $SessionId -LastPrompt
    }
}

function Send-WtEvent {
    <# Inject an event to all listeners (e.g. autofix_state) via wtcli send-event. #>
    [CmdletBinding()]
    param(
        [Parameter(Mandatory, ValueFromPipeline)]$App,
        [Parameter(Mandatory)][string]$EventType,
        [string]$ParamsJson = '{}',
        [string]$SourcePane
    )
    process {
        $a = @('send-event', '-e', $EventType)
        if ($SourcePane) { $a += @('-p', $SourcePane) }
        $a += $ParamsJson
        Invoke-WtCli -App $App -Arguments $a | Out-Null
    }
}
