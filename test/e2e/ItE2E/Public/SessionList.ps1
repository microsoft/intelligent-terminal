# SessionList.ps1 — the agent-pane "session management" view (/sessions).
# Opened via the bottom-bar SessionToggleButton; it renders a TUI list of agent sessions
# (live + historical) in the agent pane. Navigate ↑/↓, Enter launches/resumes, Esc exits.
# Like all agent-pane UI, it's TEXT captured via Get-AgentPaneText.

function Get-SessionViewRenderRegex {
    <#
    .SYNOPSIS
        Regex that detects the rendered session view, robust across locales AND pane widths. The
        view's footer hint (agents.footer_hint) is drawn in BOTH the loading and populated branches
        (ui/agents_view.rs render_footer_hint), so it's the reliable "view is open" signal — but it
        is end-truncated to the pane width (render_footer_hint → trunc), so the full localized line
        may not appear. EVERY bundled locale leads the hint with the invariant nav arrows "↑ ↓"
        (e.g. en "(↑ ↓ to navigate …)", zh "(↑ ↓ 导航 …)"), and being at the start they survive
        truncation — so match those. The en-US footer words are an extra fallback.
    #>
    [CmdletBinding()] param()
    '↑\s*↓|to launch session|to exit|No sessions|navigate'
}

function Open-SessionList {
    <#
    .SYNOPSIS
        Open the session-management view in the agent pane (via SessionToggleButton) and
        wait until the list renders. Returns the agent pane session object.
    .NOTES
        SessionToggleButton is a TOGGLE: if the view is already open (or a click lands during a
        transition) a single press can CLOSE it, leaving us in chat view. So we verify the list
        actually rendered and re-toggle up to a few times instead of trusting one click.
    #>
    [CmdletBinding()] param([Parameter(Mandatory, ValueFromPipeline)]$App, [int]$TimeoutSec = 20)
    process {
        Open-AgentPane -App $App | Out-Null
        $sess = Get-AgentPaneSession -App $App
        $renderRe = Get-SessionViewRenderRegex
        $shownNow = { (Get-AgentPaneText -App $App -MaxLines 50) -match $renderRe }
        $perTry = [Math]::Max(3, [int]($TimeoutSec / 3))
        for ($try = 0; $try -lt 3; $try++) {
            # Already open? Don't toggle (that would CLOSE it). Otherwise press and verify.
            if (& $shownNow) { return $sess }
            Invoke-UiElement -App $App -Selector 'SessionToggleButton' -TimeoutSec 10 | Out-Null
            if (Test-Until -TimeoutSec $perTry -IntervalSec 0.5 -Condition $shownNow) { return $sess }
        }
        throw "Open-SessionList: the session view did not render after 3 toggles (${TimeoutSec}s)."
    }
}

function Close-SessionList {
    <# Exit the session view (Esc) back to chat. #>
    [CmdletBinding()] param([Parameter(Mandatory, ValueFromPipeline)]$App)
    process {
        Send-AgentKey -App $App -Key Escape | Out-Null
        Start-Sleep -Milliseconds 400
        $App
    }
}

function Test-SessionListShown {
    <# Is the session-management view currently displayed? (locale-robust footer-hint match) #>
    [CmdletBinding()] param([Parameter(Mandatory, ValueFromPipeline)]$App, [int]$TimeoutSec = 5)
    process {
        $renderRe = Get-SessionViewRenderRegex
        Test-Until -TimeoutSec $TimeoutSec -IntervalSec 0.4 -Condition {
            (Get-AgentPaneText -App $App -MaxLines 50) -match $renderRe
        }
    }
}

function Get-SessionRows {
    <#
    .SYNOPSIS
        Parse the session rows from the rendered list. Each row: @{ Selected; Title; Meta }.
        Title is the session label; Meta is the trailing "· <cli>   <when>" tail.
    #>
    [CmdletBinding()] param([Parameter(Mandatory, ValueFromPipeline)]$App)
    process {
        $lines = (Get-AgentPaneText -App $App -MaxLines 60) -split "`n"
        $rows = @()
        foreach ($l in $lines) {
            # Rows are bordered with ┃ and start with > (selected) or spaces.
            if ($l -match '^[┃│]\s*(>?)\s{0,3}(\S.*?)\s{2,}(.*?)\s*[┃│]?\s*$') {
                $sel = $Matches[1] -eq '>'
                $title = $Matches[2].Trim()
                $meta = $Matches[3].Trim()
                # Skip hint/footer lines.
                if ($title -match 'navigate|to launch|to exit') { continue }
                $rows += [pscustomobject]@{ Selected = $sel; Title = $title; Meta = $meta }
            }
        }
        $rows
    }
}

function Get-SessionListSelection {
    <# The currently selected (>) session row, or $null. #>
    [CmdletBinding()] param([Parameter(Mandatory, ValueFromPipeline)]$App)
    process { Get-SessionRows -App $App | Where-Object Selected | Select-Object -First 1 }
}

function Select-SessionRow {
    <#
    .SYNOPSIS
        Move the selection (↓/↑) until the selected row's Title matches -Match, then leave
        it selected (does not press Enter). Returns the selected row.
    #>
    [CmdletBinding()]
    param([Parameter(Mandatory, ValueFromPipeline)]$App, [Parameter(Mandatory)][string]$Match, [int]$MaxMoves = 20)
    process {
        for ($i = 0; $i -le $MaxMoves; $i++) {
            $sel = Get-SessionListSelection -App $App
            if ($sel -and $sel.Title -match $Match) { return $sel }
            Send-AgentKey -App $App -Key Down | Out-Null
        }
        throw "Select-SessionRow: no row matching /$Match/ found."
    }
}

function Resume-Session {
    <#
    .SYNOPSIS
        In the session list, select the row matching -Match (or use the current selection)
        and press Enter to launch/resume it.
    #>
    [CmdletBinding()]
    param([Parameter(Mandatory, ValueFromPipeline)]$App, [string]$Match)
    process {
        if ($Match) { Select-SessionRow -App $App -Match $Match | Out-Null }
        Send-AgentKey -App $App -Key Enter | Out-Null
        Start-Sleep -Milliseconds 800
        $App
    }
}

function Get-SessionListJson {
    <# The out-of-band registry view via `wta sessions list --json` (needs runnable wta). #>
    [CmdletBinding()]
    param([Parameter(Mandatory, ValueFromPipeline)]$App, [ValidateSet('shell', 'agent-pane', 'all')][string]$Origin = 'all')
    process { Get-WtSessions -App $App -Origin $Origin }
}
