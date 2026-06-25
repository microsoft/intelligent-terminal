# AgentInput.ps1 — agent pane INPUT, OUTPUT, and POPUP/MENU interaction.
#
# The agent pane is a ratatui TUI inside a conpty. Consequences (all verified live):
#   * OUTPUT  : everything the user sees (replies, popups, permission cards, selection
#               lists) is rendered as TEXT in the pane buffer -> capture via Get-AgentPaneText.
#   * POPUPS  : there are NO native Windows/UIA dialogs. A "popup" (the `/` command menu,
#               permission card, model picker, ask-user) is TUI text -> assert on the text.
#   * SELECT  : the highlighted item is marked with `>` in the buffer. Navigate with ARROW
#               keys, confirm with Enter. wtcli's tmux tokens do NOT include arrows, so
#               arrows must be sent as raw ANSI escapes (Up=ESC[A Down=ESC[B Right=ESC[C
#               Left=ESC[D) via `send-keys --raw`. Enter/Tab/Escape/C-x DO have tokens.

$script:ItEsc = [char]27
$script:ItArrow = @{
    Up    = "$([char]27)[A"
    Down  = "$([char]27)[B"
    Right = "$([char]27)[C"
    Left  = "$([char]27)[D"
}

function Send-AgentKey {
    <#
    .SYNOPSIS
        Send a navigation key to the agent pane. -Key accepts Up/Down/Left/Right (sent as
        raw ANSI escapes) and Enter/Tab/Escape/Space/BSpace/C-c… (tmux tokens). Repeat with
        -Count. The agent pane is resolved automatically.
    #>
    [CmdletBinding()]
    param(
        [Parameter(Mandatory, ValueFromPipeline)]$App,
        [Parameter(Mandatory)][string]$Key,
        [int]$Count = 1,
        [string]$PaneSessionId
    )
    process {
        if (-not $PaneSessionId) {
            $PaneSessionId = (Wait-Until -TimeoutSec 20 -Because "agent pane session id" -Condition { Get-AgentPaneSession -App $App }).PaneSessionId
        }
        for ($i = 0; $i -lt $Count; $i++) {
            if ($script:ItArrow.ContainsKey($Key)) {
                # Arrow keys: raw ANSI escape (wtcli has no arrow token).
                Invoke-WtCli -App $App -Arguments @('send-keys', '--raw', '-t', $PaneSessionId, '--', $script:ItArrow[$Key]) | Out-Null
            }
            else {
                # Enter/Tab/Escape/Space/BSpace/C-x: tmux token translation.
                Invoke-WtCli -App $App -Arguments @('send-keys', '-t', $PaneSessionId, '--', $Key) | Out-Null
            }
            Start-Sleep -Milliseconds 120
        }
        $App
    }
}

function Send-AgentShiftEnter {
    <#
    .SYNOPSIS
        Send a REAL Shift+Enter to the agent pane. tmux send-keys tokens cannot express a
        modified Enter, so this writes the raw win32-input-mode sequence:
        `ESC[Vk;Sc;Uc;Kd;Cs;Rc_` (key-down then key-up) with Vk=13 (VK_RETURN), Sc=28,
        Uc=13 ('\r'), Cs=16 (SHIFT_PRESSED). WT's conpty honors win32 input mode, so this
        delivers KeyCode::Enter + KeyModifiers::SHIFT to the helper's ratatui app
        (app.rs handles `KeyCode::Enter if modifiers.contains(SHIFT)`). Verified live: in the
        chat input it inserts a newline (multi-line input) instead of submitting, and kitty
        (`ESC[13;2u`) / CSI-27 (`ESC[27;2;13~`) encodings are NOT honored (typed literally).
    #>
    [CmdletBinding()] param([Parameter(Mandatory, ValueFromPipeline)]$App, [string]$PaneSessionId)
    process {
        if (-not $PaneSessionId) {
            $PaneSessionId = (Wait-Until -TimeoutSec 20 -Because "agent pane session id" -Condition { Get-AgentPaneSession -App $App }).PaneSessionId
        }
        $esc = [char]27
        $seq = "$esc[13;28;13;1;16;1_$esc[13;28;13;0;16;1_"
        Invoke-WtCli -App $App -Arguments @('send-keys', '--raw', '-t', $PaneSessionId, '--', $seq) | Out-Null
        Start-Sleep -Milliseconds 150
        $App
    }
}

function Clear-AgentInput {
    <# Clear the agent pane input line / dismiss any popup (Escape resets to the hint). #>
    [CmdletBinding()] param([Parameter(Mandatory, ValueFromPipeline)]$App)
    process {
        Send-AgentKey -App $App -Key Escape | Out-Null
        Start-Sleep -Milliseconds 200
        $App
    }
}

function Send-AgentWin32Key {
    <#
    .SYNOPSIS
        Inject a single MODIFIED key (e.g. Alt+V, Shift+Enter) into the agent pane via
        win32-input-mode raw key-event sequences — the only way to express a modifier, since
        wtcli's tmux tokens and bare ANSI escapes carry no modifier state.
    .DESCRIPTION
        Emits a keydown then keyup as `ESC[Vk;Sc;Uc;Kd;Cs;Rc_`, where:
          Vk = virtual-key code, Sc = scan code, Uc = UTF-16 unit (0 if none),
          Kd = 1 down / 0 up, Cs = control-key-state bitmask, Rc = repeat count.
        ConPTY decodes these back into INPUT_RECORD key events for the helper's console, so
        crossterm sees a real KeyEvent with the modifier — exactly like a physical keypress.
    .PARAMETER Modifiers
        Control-key-state bitmask: 0x02 = LEFT_ALT_PRESSED, 0x10 = SHIFT_PRESSED,
        0x08 = LEFT_CTRL_PRESSED (combine with -bor).
    #>
    [CmdletBinding()]
    param(
        [Parameter(Mandatory, ValueFromPipeline)]$App,
        [Parameter(Mandatory)][int]$Vk,
        [int]$Sc = 0,
        [int]$Uc = 0,
        [int]$Modifiers = 0,
        [string]$PaneSessionId
    )
    process {
        if (-not $PaneSessionId) {
            $PaneSessionId = (Wait-Until -TimeoutSec 20 -Because "agent pane session id" -Condition { Get-AgentPaneSession -App $App }).PaneSessionId
        }
        $e = $script:ItEsc
        $down = "$e[$Vk;$Sc;$Uc;1;$Modifiers;1_"
        $up = "$e[$Vk;$Sc;$Uc;0;$Modifiers;1_"
        Invoke-WtCli -App $App -Arguments @('send-keys', '--raw', '-t', $PaneSessionId, '--', "$down$up") | Out-Null
        Start-Sleep -Milliseconds 150
        $App
    }
}

function Send-AgentAltV {
    <#
    .SYNOPSIS
        Simulate Alt+V (clipboard image paste) in the agent pane. VK_V=0x56, scan=0x2F, no
        character (Alt+letter is a menu accelerator, so Uc=0 — matching real hardware),
        LEFT_ALT_PRESSED=0x02. The agent pane session is resolved automatically.
    #>
    [CmdletBinding()] param([Parameter(Mandatory, ValueFromPipeline)]$App, [string]$PaneSessionId)
    process { Send-AgentWin32Key -App $App -Vk 0x56 -Sc 0x2F -Uc 0 -Modifiers 0x02 -PaneSessionId $PaneSessionId }
}

function Open-AgentCommandMenu {
    <#
    .SYNOPSIS
        Open the agent pane's `/` slash-command popup and wait until it renders.
        Clears any existing input first (Escape) so a stale char can't suppress the menu.
        Returns the agent pane session object.
    #>
    [CmdletBinding()] param([Parameter(Mandatory, ValueFromPipeline)]$App, [int]$TimeoutSec = 15)
    process {
        Clear-AgentInput -App $App | Out-Null
        $sess = Send-AgentPrompt -App $App -Text '/' -NoSubmit
        Wait-Until -TimeoutSec $TimeoutSec -Because "the / command popup to render" -Condition {
            (Get-AgentPaneText -App $App -MaxLines 40) -match '/help|/clear|/new'
        } | Out-Null
        $sess
    }
}

function Get-AgentMenuSelection {
    <# Return the menu row currently marked selected (the `>` line), trimmed. #>
    [CmdletBinding()] param([Parameter(Mandatory, ValueFromPipeline)]$App)
    process {
        $line = (Get-AgentPaneText -App $App -MaxLines 40) -split "`n" |
            Where-Object { $_ -match '>\s*/\w' } | Select-Object -First 1
        if ($line) { ($line -replace '[│┌┐└┘>]', '').Trim() } else { $null }
    }
}

function Invoke-AgentMenuItem {
    <#
    .SYNOPSIS
        Open the `/` menu, navigate (Down/Up) to the item whose row matches -Name, and
        confirm with Enter. Self-verifies the target row is selected before pressing Enter.
    .PARAMETER Name   A regex matched against the menu rows (e.g. '/new', 'clear').
    #>
    [CmdletBinding()]
    param([Parameter(Mandatory, ValueFromPipeline)]$App, [Parameter(Mandatory)][string]$Name, [int]$MaxMoves = 12)
    process {
        Open-AgentCommandMenu -App $App | Out-Null
        $reached = $false
        for ($i = 0; $i -le $MaxMoves; $i++) {
            $sel = Get-AgentMenuSelection -App $App
            if ($sel -and $sel -match $Name) { $reached = $true; break }
            Send-AgentKey -App $App -Key Down | Out-Null
        }
        if (-not $reached) { throw "Invoke-AgentMenuItem: could not select a menu row matching /$Name/." }
        Send-AgentKey -App $App -Key Enter | Out-Null
        $App
    }
}

function Test-AgentPopupShown {
    <#
    .SYNOPSIS
        Assert/await that a popup or card is shown in the agent pane by matching its
        rendered text (e.g. 'Allow', 'permission', '/help', a model name). Returns bool.
    #>
    [CmdletBinding()]
    param([Parameter(Mandatory, ValueFromPipeline)]$App, [Parameter(Mandatory)][string]$Pattern, [int]$TimeoutSec = 15)
    process {
        Test-Until -TimeoutSec $TimeoutSec -IntervalSec 0.5 -Condition {
            (Get-AgentPaneText -App $App -MaxLines 60) -match $Pattern
        }
    }
}

function Wait-AgentPermission {
    <#
    .SYNOPSIS
        Wait for a permission card to appear in the agent pane (agent asked to run a tool).
        Detect via the rendered card text. Returns bool.
    #>
    [CmdletBinding()] param([Parameter(Mandatory, ValueFromPipeline)]$App, [int]$TimeoutSec = 60)
    process { Test-AgentPopupShown -App $App -Pattern '(?i)allow|reject|permission|approve|deny|yes.*no' -TimeoutSec $TimeoutSec }
}

function Resolve-AgentPermission {
    <#
    .SYNOPSIS
        Answer a permission card. -Decision Allow/Reject navigates with arrows to the
        matching option and presses Enter (the card highlights with `>`/selection).
    #>
    [CmdletBinding()]
    param([Parameter(Mandatory, ValueFromPipeline)]$App, [ValidateSet('Allow', 'Reject')][string]$Decision = 'Allow')
    process {
        if (-not (Wait-AgentPermission -App $App -TimeoutSec 30)) { throw "Resolve-AgentPermission: no permission card shown." }
        # Allow is the leftmost (default) option; Reject is to the right.
        if ($Decision -eq 'Reject') { Send-AgentKey -App $App -Key Right -Count 3 | Out-Null }
        else { Send-AgentKey -App $App -Key Left -Count 3 | Out-Null }
        Send-AgentKey -App $App -Key Enter | Out-Null
        $App
    }
}

function Assert-AgentPaneText {
    <# Assert the agent pane buffer matches a regex within the timeout. #>
    [CmdletBinding()]
    param([Parameter(Mandatory)]$App, [Parameter(Mandatory)][string]$Pattern, [int]$TimeoutSec = 30)
    if (-not (Test-AgentPopupShown -App $App -Pattern $Pattern -TimeoutSec $TimeoutSec)) {
        $shot = Save-UiScreenshot -App $App -Path (Join-Path $env:TEMP 'ite2e-agentpane-fail.png')
        throw "Assert-AgentPaneText: agent pane never matched /$Pattern/ within ${TimeoutSec}s. Screenshot: $shot"
    }
}
