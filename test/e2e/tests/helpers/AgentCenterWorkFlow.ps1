. (Join-Path $PSScriptRoot 'AgentCenterDraft.ps1')

function Get-WorkFlowNavigationLogFilter {
    'warn,agent_center::navigation=debug'
}

function Invoke-WorkFlowUi {
    param([Parameter(Mandatory)][string]$WtaPath, [switch]$NavigationDiagnostics)
    $priorLog = $env:WTA_LOG
    try {
        if ($NavigationDiagnostics) { $env:WTA_LOG = Get-WorkFlowNavigationLogFilter }
        & $WtaPath --language en-US ui
    }
    finally { $env:WTA_LOG = $priorLog }
}

function Get-WorkFlowNavigationObservation {
    param([Parameter(Mandatory)][hashtable]$Context)
    $record = [ordered]@{ startedUtc = [datetime]::UtcNow; errors = @(); focusedChildHwnd = $null }
    try {
        & (Get-Module ItE2E -ErrorAction Stop) { Initialize-WtWin32Input }
        $foreground = [ItE2E.ItWtWin32Input]::GetForegroundWindow()
        $foregroundPid = [uint32]0
        $ownerPid = [uint32]0
        $record.foregroundHwnd = $foreground.ToInt64()
        $record.foregroundThreadId = [ItE2E.ItWtWin32Input]::GetWindowThreadProcessId($foreground, [ref]$foregroundPid)
        $record.foregroundPid = $foregroundPid
        $record.expectedWindowThreadId = [ItE2E.ItWtWin32Input]::GetWindowThreadProcessId([IntPtr][int64]$Context.App.Hwnd, [ref]$ownerPid)
        $record.expectedWindowOwnerPid = $ownerPid
        $record.observerThreadId = [ItE2E.ItWtWin32Input]::GetCurrentThreadId()
        $record.observedModifiers = @{
            ctrl = [ItE2E.ItWtWin32Input]::IsKeyDown(0x11)
            shift = [ItE2E.ItWtWin32Input]::IsKeyDown(0x10)
            alt = [ItE2E.ItWtWin32Input]::IsKeyDown(0x12)
        }
    }
    catch {
        $record.errors += "Win32 observation: $($_.Exception.Message)"
        Write-Warning $record.errors[-1]
    }
    try {
        $pane = Get-ActivePane -App $Context.App
        $record.activePane = @{ sessionId = $pane.session_id; tabId = $pane.tab_id; windowId = $pane.window_id }
    }
    catch {
        $record.errors += "Active-pane observation: $($_.Exception.Message)"
        Write-Warning $record.errors[-1]
    }
    $record.focusEvidence = 'Foreground HWND/thread and protocol active pane; focused child HWND is not exposed by the existing helper.'
    $record.completedUtc = [datetime]::UtcNow
    return $record
}

function Write-WorkFlowNavigationRecord {
    param([Parameter(Mandatory)][hashtable]$Context, [Parameter(Mandatory)]$Record)
    $Record | ConvertTo-Json -Depth 12 -Compress |
        Add-Content -LiteralPath (Join-Path $Context.Root 'navigation-input.jsonl') -Encoding utf8NoBOM
}

function Send-WorkFlowNavigationKey {
    param([Parameter(Mandatory)][hashtable]$Context, [ValidateSet(0x21, 0x22, 0x7B)][int]$Vk,
        [switch]$Ctrl, [switch]$Shift)
    $Context.NavigationKeyCount = [int]$Context.NavigationKeyCount + 1
    $Context.LastNavigationId = [guid]::NewGuid().ToString('N')
    $record = [ordered]@{
        id = $Context.LastNavigationId; phase = 'Requested'; requestedUtc = [datetime]::UtcNow
        vk = $Vk; ctrl = [bool]$Ctrl; shift = [bool]$Shift; alt = $false; requestedRepeat = 1
        expectedHwnd = $Context.App.Hwnd; expectedHostPid = $Context.App.Pid
        expectedPaneId = $Context.Pane.session_id; uiIdentity = $Context.UiIdentity
        capturePath = $Context.CapturedActionEvidencePath; precedingFrameSequence = $Context.Sequence
        lastDecodedCommandId = $Context.LastDecodedCommandId
        injectionApi = 'keybd_event'; reportedSendCount = $null
        deliveryEvidence = 'Unavailable: keybd_event returns void; framework returns App, not an input-delivery receipt.'
        before = Get-WorkFlowNavigationObservation -Context $Context
    }
    Write-WorkFlowNavigationRecord -Context $Context -Record $record
    try {
        $record.sendStartedUtc = [datetime]::UtcNow
        $returned = Send-WtWindowKey -App $Context.App -Vk $Vk -Ctrl:$Ctrl -Shift:$Shift -RequireForeground
        $record.sendFinishedUtc = [datetime]::UtcNow
        $record.phase = 'FrameworkReturned'
        $record.returnedApp = @{ hwnd = $returned.Hwnd; pid = $returned.Pid }
        $Context.NativeInputSent = $true
    }
    catch {
        $record.sendFinishedUtc = [datetime]::UtcNow
        $record.phase = 'FrameworkThrew'
        $record.error = $_.Exception.Message
        throw
    }
    finally {
        $record.after = Get-WorkFlowNavigationObservation -Context $Context
        $record.completedUtc = [datetime]::UtcNow
        Write-WorkFlowNavigationRecord -Context $Context -Record $record
    }
}

function Save-WorkFlowNavigationLogProvenance {
    param([Parameter(Mandatory)][hashtable]$Context, [switch]$Final)
    if (-not $Context.Runtime.navigationDiagnostics) { return }
    $files = @(Get-ChildItem -LiteralPath $Context.Runtime.localAppData -Recurse -File -Filter 'wta-center-ui*.log' -ErrorAction Stop |
        Where-Object Name -Match '^wta-center-ui(?:\.\d{4}-\d{2}-\d{2})?\.log$')
    [ordered]@{
        observedUtc = [datetime]::UtcNow; final = [bool]$Final
        uiIdentity = $Context.UiIdentity; wtaSha256 = $Context.Runtime.sha256
        filter = $Context.Runtime.uiLogFilter; searchRoot = $Context.Runtime.localAppData
        status = $(if ($files.Count) { 'Observed' } else { 'NotObserved' })
        files = @($files | ForEach-Object { @{
            path = $_.FullName; bytes = $_.Length
            sha256 = $(if ($Final) { (Get-FileHash -LiteralPath $_.FullName).Hash } else { $null })
        } })
    } | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath (Join-Path $Context.Root 'navigation-log-provenance.json') -Encoding utf8NoBOM
    if ($Final -and $Context.NavigationKeyCount -gt 0 -and $files.Count -eq 0) {
        throw 'No test-owned center-ui log was retained for requested navigation diagnostics.'
    }
}

function Get-WorkFlowAdapterConfiguration {
    param([string]$IntakeScriptPath, [string]$EvidenceDirectory)
    $configuration = @{
        capabilities = @(@{
            id = 'ite2e-never-run'
            adapter = @{
                kind = 'ACP'
                executable = 'cmd.exe'
                args = @('/d', '/c', 'exit 77')
                approvedModelDestination = 'Local fail-closed fixture only; no model service'
            }
        })
    }
    if ($IntakeScriptPath) {
        if (-not $EvidenceDirectory) { throw 'The scripted intake adapter requires an isolated evidence directory.' }
        $configuration.capabilities += @{
            id = 'ite2e-local-intake'
            adapter = @{
                kind = 'ACP'; executable = (Get-Command node.exe -ErrorAction Stop).Source
                args = @('--no-warnings', $IntakeScriptPath, $EvidenceDirectory)
                approvedModelDestination = 'Local scripted intake protocol fixture; no external model or worker execution'
            }
        }
        $configuration.capabilities += @{
            id = 'ite2e-local-report'
            adapter = @{
                kind = 'ACP'; executable = (Get-Command node.exe -ErrorAction Stop).Source
                args = @('--no-warnings', $IntakeScriptPath, $EvidenceDirectory, 'report')
                approvedModelDestination = 'Local scripted single-report adapter; no external model or code/OS qualification'
            }
        }
        $configuration.capabilities += @{
            id = 'ite2e-local-decision'
            adapter = @{
                kind = 'ACP'; executable = (Get-Command node.exe -ErrorAction Stop).Source
                args = @('--no-warnings', $IntakeScriptPath, $EvidenceDirectory, 'decision')
                approvedModelDestination = 'Local scripted initial brief and human TaskInput continuation; no external model'
            }
        }
        $configuration.conversationCapabilityId = 'ite2e-local-global'
        $configuration.conversationLimits = @{
            concurrency = 1; executionAttempts = 1; evaluationAttempts = 1
            coordinationTurns = 24; contextRounds = 3; executionSeconds = 180; coordinationSeconds = 90
        }
        $configuration.capabilities += @{
            id = 'ite2e-local-global'
            adapter = @{
                kind = 'ACP'; executable = (Get-Command node.exe -ErrorAction Stop).Source
                args = @('--no-warnings', $IntakeScriptPath, $EvidenceDirectory, 'global')
                approvedModelDestination = 'Local scripted global conversation only; no external model or autonomous interpretation'
            }
        }
    }
    return $configuration
}

function Get-WorkFlowPipeName {
    param([Parameter(Mandatory)][string]$StateRoot)
    $path = [IO.Path]::GetFullPath($StateRoot)
    if (-not $path.StartsWith('\\?\')) { $path = '\\?\' + $path }
    $hash = [Security.Cryptography.SHA256]::HashData([Text.Encoding]::UTF8.GetBytes($path.ToLowerInvariant()))
    'IntelligentTerminal-AgentCenter-' + [Convert]::ToHexString($hash).ToLowerInvariant()
}

function Get-WorkFlowBodyRows {
    param([Parameter(Mandatory)][string]$Frame)
    $lines = $Frame -split '\r?\n'
    $modals = @(for ($i = 0; $i -lt $lines.Count; $i++) {
        if ($lines[$i] -cmatch '^┌(?:Diagnostic protocol details \(read-only\)|Confirm the captured action|Answer)─*┐\s*$') { $i }
    })
    if ($modals.Count -gt 1) { throw 'Ambiguous full-width work-flow modal bodies.' }
    if ($modals.Count -eq 1) {
        $rows = [Collections.Generic.List[string]]::new()
        for ($i = $modals[0] + 1; $i -lt $lines.Count; $i++) {
            if ($lines[$i] -cmatch '^│(.*)│\s*$') { $rows.Add($Matches[1].TrimEnd([char]' ')) }
            elseif ($lines[$i] -cmatch '^└─*┘\s*$') { return ,$rows.ToArray() }
            else { throw 'The full-width work-flow modal has malformed body rows.' }
        }
        throw 'The full-width work-flow modal has no closing border.'
    }
    if ($Frame -cmatch '(?m)(?:^|│)(?:Global conversation\s*$|Message for: |New work · )') {
        $box = Get-NativePasteInputBox -Frame $Frame
        if ($box.scope -cnotmatch '^(?:Global conversation$|Message for: |New work · )') { throw 'The scoped composer is missing its work-flow heading.' }
        $bodyStart = if ($box.scope -ceq 'Global conversation') { 2 } else { 3 }
        $rows = @(for ($i = $bodyStart; $i -lt $box.titleRow - 1; $i++) {
            $line = $lines[$i]
            if ($box.hasRail) {
                if ($line -cnotmatch '^[^│]*│') { throw 'The work-flow main region has a missing rail boundary.' }
                $line = $line.Substring($line.IndexOf('│') + 1)
            }
            $line.TrimEnd([char]' ')
        })
        return ,$rows
    }
    $rows = foreach ($line in ($Frame -split '\r?\n')) {
        $divider = $line.IndexOf('││', [StringComparison]::Ordinal)
        if ($divider -ge 0) { $line.Substring($divider + 2).TrimEnd([char[]]@('│', ' ')) }
    }
    return ,@($rows)
}

function Get-WorkFlowSelectedMenuLabel {
    param([Parameter(Mandatory)][string]$Frame, [switch]$IncludeWrapped)
    if ($Frame -cmatch '(?m)(?:^|│)(?:Global conversation\s*$|Message for: |New work · )' -and
        $Frame -cnotmatch '(?m)^F4 actions · Up/Down select · Enter open') {
        throw 'Expected one visibly selected work-flow menu action, not a focused card or draft.'
    }
    $rows = Get-WorkFlowBodyRows $Frame
    $selected = @(for ($i = 0; $i -lt $rows.Count; $i++) {
        if ($rows[$i] -cmatch '^>\s+(.+)$') { $i }
    })
    if ($selected.Count -ne 1) { throw 'Expected one visibly selected work-flow menu action.' }
    $label = $rows[$selected[0]] -creplace '^>\s+', ''
    if ($IncludeWrapped) {
        for ($i = $selected[0] + 1; $i -lt $rows.Count; $i++) {
            if (-not $rows[$i] -or $rows[$i].StartsWith('  ', [StringComparison]::Ordinal)) { break }
            $label += ' ' + $rows[$i]
        }
        $label = $label -replace '[ \t]+', ' '
    }
    return $label
}

function Test-WorkFlowRenderedSelection {
    param([string]$Frame, [string]$Goal, [Parameter(Mandatory)][string]$ProjectName)
    $box = Get-NativePasteInputBox -Frame $Frame
    $header = @(($Frame -split '\r?\n' | Select-Object -First 2) | ForEach-Object {
        $line = $_
        if ($box.hasRail) {
            if ($line -cnotmatch '^[^│]*│') { throw 'The selected-work header has a missing rail boundary.' }
            $line = $line.Substring($line.IndexOf('│') + 1)
        }
        $line.TrimEnd([char]' ')
    })
    if ($box.scope -ceq 'Global conversation') {
        if ($header.Count -ne 2 -or $header[0] -cne 'Global conversation') { return $false }
        $hint = if ($Goal) { "$Goal · $ProjectName" } else { $ProjectName }
        return $header[1] -ceq "Context hint: $hint"
    }
    if ($header.Count -ne 2 -or -not $header[1].StartsWith("$ProjectName ·", [StringComparison]::Ordinal)) { return $false }
    if ($Goal) { return $header[0] -ceq $Goal -and $box.scope -ceq "Message for: $Goal · $ProjectName" }
    return $header[0] -cin @('Overview', 'Needs your attention') -and $box.scope -ceq "New work · $ProjectName"
}

function Get-WorkFlowNativeCaret {
    param([Parameter(Mandatory)]$App, [Parameter(Mandatory)][string]$ExpectedScope)
    Add-Type -AssemblyName UIAutomationClient
    Add-Type -AssemblyName UIAutomationTypes
    $window = [System.Windows.Automation.AutomationElement]::FromHandle([IntPtr][int64]$App.Hwnd)
    $condition = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::IsTextPatternAvailableProperty, $true)
    $matches = @(foreach ($element in $window.FindAll([System.Windows.Automation.TreeScope]::Descendants, $condition)) {
        if ($element.Current.IsOffscreen) { continue }
        $pattern = $element.GetCurrentPattern([System.Windows.Automation.TextPattern]::Pattern)
        $document = $pattern.DocumentRange.GetText(-1)
        if (-not $document.Contains($ExpectedScope)) { continue }
        $box = Get-NativePasteInputBox -Frame $document
        if ($box.scope -cne $ExpectedScope) { continue }
        @{ pattern = $pattern; box = $box }
    })
    if ($matches.Count -ne 1) { throw 'Expected one visible test-window terminal exposing the exact composer through TextPattern.' }
    $pattern = $matches[0].pattern
    $ranges = @($pattern.GetSelection())
    if ($ranges.Count -ne 1 -or $ranges[0].CompareEndpoints(
        [System.Windows.Automation.Text.TextPatternRangeEndpoint]::Start, $ranges[0],
        [System.Windows.Automation.Text.TextPatternRangeEndpoint]::End) -ne 0) {
        throw 'Expected the degenerate native terminal caret, not host text selection.'
    }
    $before = $pattern.DocumentRange.Clone()
    $before.MoveEndpointByRange([System.Windows.Automation.Text.TextPatternRangeEndpoint]::End,
        $ranges[0], [System.Windows.Automation.Text.TextPatternRangeEndpoint]::Start)
    return Get-NativePasteCaretPrefix -DocumentBeforeCaret ($before.GetText(-1)) -InputBox $matches[0].box
}

function Assert-WorkFlowVisualOwnership {
    param([Parameter(Mandatory)][hashtable]$Context)
    if ($Context.App.Hwnd -le 0 -or $Context.App.Pid -le 0 -or $Context.UiIdentity.pid -le 0 -or
        -not $Context.UiIdentity.created -or -not $Context.Pane.session_id) {
        throw 'Visual evidence requires the original owned HWND, process and pane identities.'
    }
    $owner = [ItE2E.ItWtWin32Input]::GetWindowProcessId([IntPtr][int64]$Context.App.Hwnd)
    if ($owner -ne $Context.App.Pid) { throw 'The visual evidence HWND no longer belongs to the test-owned host.' }
    $ui = Get-CimInstance Win32_Process -Filter "ProcessId=$($Context.UiIdentity.pid)"
    if (-not $ui -or $ui.CreationDate -ne $Context.UiIdentity.created) { throw 'The original owned Agent Center UI process is no longer present.' }
    if ((Get-ActivePane -App $Context.App).session_id -cne $Context.Pane.session_id) {
        throw 'The owned window is no longer displaying the intended test pane; no screenshot will be taken.'
    }
}

function Save-WorkFlowVisualEvidence {
    param(
        [Parameter(Mandatory)][hashtable]$Context,
        [Parameter(Mandatory)][ValidateSet('overview', 'current-work', 'typed-intake', 'fixed-report', 'post-start-question')][string]$Name,
        [Parameter(Mandatory)][string]$Frame
    )
    Assert-WorkFlowVisualOwnership -Context $Context
    if (-not [IO.Path]::IsPathFullyQualified($Context.Root) -or -not (Test-Path -LiteralPath $Context.Root -PathType Container)) {
        throw 'Visual evidence requires the existing absolute test-fixture directory.'
    }
    $image = Join-Path $Context.Root "visual-$Name.png"
    $text = Join-Path $Context.Root "visual-$Name.txt"
    $metadata = Join-Path $Context.Root "visual-$Name.json"
    foreach ($file in @($image, $text, $metadata)) {
        if (Test-Path -LiteralPath $file) { throw "Visual evidence must not reuse a previous artifact: $file" }
    }
    $record = @{
        boundary = $Name; hwnd = $Context.App.Hwnd; hostPid = $Context.App.Pid
        uiIdentity = $Context.UiIdentity; paneSessionId = $Context.Pane.session_id
        wtaSha256 = $Context.Runtime.sha256; captureScreen = $false
        frameRecordedUtc = [datetime]::UtcNow; frame = $text; screenshot = $image
        qualification = 'Actual test-owned window evidence for human visual inspection; not automatic design acceptance'
        screenshotStatus = 'Pending'
    }
    $Frame | Set-Content -LiteralPath $text -Encoding utf8NoBOM -NoNewline
    try {
        Save-UiScreenshot -App $Context.App -Path $image -CaptureScreen:$false -RequireSuccess | Out-Null
        if (-not (Test-Path -LiteralPath $image)) { throw 'The HWND-targeted screenshot command did not produce a new image.' }
        $bytes = [IO.File]::ReadAllBytes($image)
        if ($bytes.Length -lt 24 -or [Convert]::ToHexString($bytes[0..7]) -cne '89504E470D0A1A0A') {
            throw 'The HWND-targeted screenshot is not a nonempty PNG.'
        }
        $record.screenshotStatus = 'Captured'
        $record.screenshotSha256 = (Get-FileHash -LiteralPath $image -Algorithm SHA256).Hash
        $record.frameSha256 = (Get-FileHash -LiteralPath $text -Algorithm SHA256).Hash
        $record.screenshotCapturedUtc = [datetime]::UtcNow
    }
    catch {
        $record.screenshotStatus = 'Failed'
        $record.error = $_.Exception.Message
        throw
    }
    finally { $record | ConvertTo-Json -Depth 10 | Set-Content -LiteralPath $metadata -Encoding utf8NoBOM }
}

function Get-WorkFlowEncodedRequestJson {
    param([Parameter(Mandatory)][string]$Body, [switch]$RequireEncoding)
    $members = [Collections.Generic.List[int]]::new()
    $boundaries = [Collections.Generic.Stack[char]]::new()
    $depth = 0
    $quoted = $false
    $escaped = $false
    $closed = $false
    for ($i = 0; $i -lt $Body.Length; $i++) {
        $character = $Body[$i]
        if ($quoted) {
            # Physical line breaks do not change quote/escape state. Readable values
            # are scanned only for structure, never reconstructed or returned.
            if ($character -in @("`r", "`n")) { continue }
            if ($escaped) { $escaped = $false }
            elseif ($character -eq '\') { $escaped = $true }
            elseif ($character -eq '"') { $quoted = $false }
            continue
        }
        if ($character -eq '"') {
            $lineStart = $Body.LastIndexOf("`n", $i) + 1
            if ($depth -eq 1 -and $Body.Substring($lineStart, $i - $lineStart) -ceq '  ' -and
                $Body.Substring($i).StartsWith('"requestUtf8Hex"', [StringComparison]::Ordinal)) {
                $members.Add($i)
            }
            $quoted = $true
        }
        elseif ($character -in @('{', '[')) {
            $boundaries.Push($character)
            $depth++
        }
        elseif ($character -in @('}', ']')) {
            $opening = if ($character -eq '}') { [char]'{' } else { [char]'[' }
            if (-not $boundaries.Count -or $boundaries.Pop() -ne $opening) {
                throw [IO.InvalidDataException]::new('Malformed diagnostic root boundaries.')
            }
            $depth--
            if ($depth -lt 0) { throw [IO.InvalidDataException]::new('Malformed diagnostic root boundaries.') }
            if ($depth -eq 0) {
                $closed = $true
                if ($Body.Substring($i + 1).Trim()) { throw [IO.InvalidDataException]::new('Unexpected content after the diagnostic root.') }
                break
            }
        }
    }
    if ($members.Count -gt 1) { throw [IO.InvalidDataException]::new('Duplicate root requestUtf8Hex fields in diagnostics.') }
    if (($RequireEncoding -or $members.Count) -and -not $Body.TrimStart().StartsWith('{')) {
        throw [IO.InvalidDataException]::new('Encoded diagnostics require the captured root object boundary.')
    }
    if ($members.Count -eq 0) {
        if ($RequireEncoding -or $Body -cmatch '(?m)^  "requestUtf8') {
            if ($closed) { throw [IO.InvalidDataException]::new('The pending diagnostic root is missing requestUtf8Hex.') }
            throw [IO.EndOfStreamException]::new('The root requestUtf8Hex field is not yet fully visible.')
        }
        return $null
    }
    $value = $Body.Substring($members[0] + '"requestUtf8Hex"'.Length)
    if ($value -cnotmatch '^[ \r\n]*:[ \r\n]*"') {
        if (-not $closed -and $value -cmatch '^[ \r\n]*:?[ \r\n]*$') {
            throw [IO.EndOfStreamException]::new('The requestUtf8Hex value is not yet visible.')
        }
        throw [IO.InvalidDataException]::new('requestUtf8Hex must be a JSON string.')
    }
    $value = $value.Substring($Matches[0].Length)
    $end = $value.IndexOf('"')
    if ($end -lt 0) {
        if ($value -cmatch '[^0-9a-f \r\n]') { throw [IO.InvalidDataException]::new('Invalid canonical requestUtf8Hex characters.') }
        throw [IO.EndOfStreamException]::new('The requestUtf8Hex string is incomplete.')
    }
    $hex = $value.Substring(0, $end) -creplace '[ \r\n]', ''
    if ($hex -cnotmatch '^[0-9a-f]+$' -or $hex.Length % 2 -ne 0) {
        throw [IO.InvalidDataException]::new('requestUtf8Hex must contain nonempty even-length lowercase hex.')
    }
    if ($value.Substring($end + 1) -cnotmatch '^[ \r\n]*(?:,|})') {
        if (-not $closed -and $value.Substring($end + 1) -cmatch '^[ \r\n]*$') {
            throw [IO.EndOfStreamException]::new('The requestUtf8Hex member terminator is incomplete.')
        }
        throw [IO.InvalidDataException]::new('Invalid requestUtf8Hex member terminator.')
    }
    if (-not $closed) { throw [IO.EndOfStreamException]::new('The diagnostic root must finish before duplicate encoding can be excluded.') }
    try { return [Text.UTF8Encoding]::new($false, $true).GetString([Convert]::FromHexString($hex)) }
    catch [Text.DecoderFallbackException] {
        throw [IO.InvalidDataException]::new('requestUtf8Hex does not encode strict UTF-8.', $_.Exception)
    }
}

function ConvertFrom-WorkFlowJsonElement {
    param([System.Text.Json.JsonElement]$Element)
    switch ($Element.ValueKind) {
        Object {
            $result = [Collections.Specialized.OrderedDictionary]::new([StringComparer]::Ordinal)
            foreach ($property in $Element.EnumerateObject()) { $result.Add($property.Name, (ConvertFrom-WorkFlowJsonElement $property.Value)) }
            return ,$result
        }
        Array {
            $result = [Collections.Generic.List[object]]::new()
            foreach ($value in $Element.EnumerateArray()) { $result.Add((ConvertFrom-WorkFlowJsonElement $value)) }
            return ,$result.ToArray()
        }
        String { return $Element.GetString() }
        Number { return ($Element.GetRawText() | ConvertFrom-Json) }
        True { return $true }
        False { return $false }
        Null { return $null }
        default { throw [IO.InvalidDataException]::new('Unsupported diagnostic JSON value.') }
    }
}

function Get-WorkFlowReadableRequestJson {
    param([Parameter(Mandatory)][string]$Body)
    # Root-member order is not fixed. Do not parse an unrelated clipped work
    # snapshot while paging toward the captured request at the end.
    $members = [regex]::Matches($body, '(?m)^  "(?:request|operation)": \{')
    if ($members.Count -gt 1) { throw 'Ambiguous captured request records in the visible diagnostics.' }
    $json = $null
    $start = -1
    $direct = $false
    if ($members.Count -eq 1) {
        $start = $members[0].Index + $members[0].Length - 1
    }
    elseif ($body -cmatch '(?m)^  "method":|^\{[^\r\n]*"method"\s*:') {
        $start = $body.IndexOf('{')
        $direct = $true
    }
    if ($start -ge 0) {
        $depth = 0
        $quoted = $false
        $escaped = $false
        for ($i = $start; $i -lt $body.Length; $i++) {
            $character = $body[$i]
            if ($quoted) {
                if ($character -in @("`r", "`n")) {
                    throw [IO.InvalidDataException]::new('Ambiguous physical row break inside a captured JSON string. Word wrapping can discard whitespace; exact request semantics cannot be recovered by joining rows.')
                }
                if ($escaped) { $escaped = $false }
                elseif ($character -eq '\') { $escaped = $true }
                elseif ($character -eq '"') { $quoted = $false }
                continue
            }
            if ($character -eq '"') { $quoted = $true }
            elseif ($character -eq '{') { $depth++ }
            elseif ($character -eq '}') {
                $depth--
                if ($depth -eq 0) {
                    $json = $body.Substring($start, $i - $start + 1)
                    if ($direct -and $body.Substring($i + 1).Trim()) {
                        throw [IO.InvalidDataException]::new('Unexpected trailing content after the captured diagnostic request.')
                    }
                    break
                }
            }
        }
    }
    if (-not $json) { throw [IO.EndOfStreamException]::new('No complete captured request is visible in these diagnostic pages.') }
    return $json
}

function Get-WorkFlowCapturedRequest {
    param([Parameter(Mandatory)][string]$Frame, [switch]$RequireEncoding)
    $body = (Get-WorkFlowBodyRows $Frame) -join "`n"
    $json = Get-WorkFlowEncodedRequestJson -Body $body -RequireEncoding:$RequireEncoding
    $encoded = $null -ne $json
    if (-not $encoded) { $json = Get-WorkFlowReadableRequestJson -Body $body }
    $document = $null
    try {
        # JSON.NET accepts raw newlines in strings. Validate strictly before conversion.
        $document = [System.Text.Json.JsonDocument]::Parse([string]$json, [System.Text.Json.JsonDocumentOptions]::new())
        $pending = [Collections.Generic.Stack[System.Text.Json.JsonElement]]::new()
        $pending.Push($document.RootElement)
        while ($pending.Count) {
            $element = $pending.Pop()
            if ($element.ValueKind -eq [System.Text.Json.JsonValueKind]::Object) {
                $names = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
                foreach ($property in $element.EnumerateObject()) {
                    if (-not $names.Add($property.Name)) {
                        throw [IO.InvalidDataException]::new('Ambiguous duplicate property in captured diagnostic JSON.')
                    }
                    $pending.Push($property.Value)
                }
            }
            $root = $document.RootElement
            if ($root.ValueKind -ne [System.Text.Json.JsonValueKind]::Object) {
                throw [IO.InvalidDataException]::new('A captured request must be a JSON object.')
            }
            $fields = @($root.EnumerateObject() | ForEach-Object Name)
            foreach ($name in @('method', 'params', 'ifMatch', 'commandId')) {
                if ($name -cnotin $fields) { throw [IO.InvalidDataException]::new('The visible diagnostics do not contain a complete captured request.') }
            }
            if ($encoded -and $fields.Count -ne 4) { throw [IO.InvalidDataException]::new('Encoded request must have exactly four frozen fields.') }
            if ($root.GetProperty('method').ValueKind -ne [System.Text.Json.JsonValueKind]::String -or
                -not $root.GetProperty('method').GetString() -or
                $root.GetProperty('commandId').ValueKind -ne [System.Text.Json.JsonValueKind]::String -or
                -not $root.GetProperty('commandId').GetString() -or
                $root.GetProperty('params').ValueKind -ne [System.Text.Json.JsonValueKind]::Object -or
                $root.GetProperty('ifMatch').ValueKind -ne [System.Text.Json.JsonValueKind]::Array) {
                throw [IO.InvalidDataException]::new('Invalid method/params/ifMatch/commandId request shape.')
            }
            foreach ($guard in $root.GetProperty('ifMatch').EnumerateArray()) {
                $version = [long]0
                $guardFields = if ($guard.ValueKind -eq [System.Text.Json.JsonValueKind]::Object) { @($guard.EnumerateObject() | ForEach-Object Name) } else { @() }
                if ('kind' -cnotin $guardFields -or 'id' -cnotin $guardFields -or 'version' -cnotin $guardFields -or
                    $guard.GetProperty('kind').ValueKind -ne [System.Text.Json.JsonValueKind]::String -or
                    -not $guard.GetProperty('kind').GetString() -or
                    $guard.GetProperty('id').ValueKind -ne [System.Text.Json.JsonValueKind]::String -or
                    -not $guard.GetProperty('id').GetString() -or
                    $guard.GetProperty('version').ValueKind -ne [System.Text.Json.JsonValueKind]::Number -or
                    -not $guard.GetProperty('version').TryGetInt64([ref]$version) -or $version -lt 1) {
                    throw [IO.InvalidDataException]::new('Invalid captured ifMatch guard.')
                }
            }
            return ConvertFrom-WorkFlowJsonElement $root
            elseif ($element.ValueKind -eq [System.Text.Json.JsonValueKind]::Array) {
                foreach ($value in $element.EnumerateArray()) { $pending.Push($value) }
            }
        }
    }
    catch [System.Text.Json.JsonException] {
        throw [IO.InvalidDataException]::new('Invalid or ambiguous captured diagnostic JSON; physical row breaks must not be converted into string content or normalized away.', $_.Exception)
    }
    finally { if ($document) { $document.Dispose() } }
}

function Save-WorkFlowCapturedActionEvidence {
    param(
        [Parameter(Mandatory)][string]$Path,
        $Expected,
        $Actual,
        [string[]]$Frames = @(),
        [string[]]$Rows = @(),
        [string]$CaptureError
    )
    [ordered]@{
        expected = $Expected
        actual = $Actual
        exactMatch = if ($null -ne $Expected -and $null -ne $Actual) { Test-WorkFlowFrozenRequest $Expected $Actual } else { $null }
        error = $CaptureError
        physicalFrames = @($Frames)
        physicalRows = @($Rows)
        physicalJson = $Rows -join "`n"
    } | ConvertTo-Json -Depth 80 | Set-Content -LiteralPath $Path -Encoding utf8NoBOM
}

function Merge-WorkFlowBodyRows {
    param([string[]]$Earlier, [string[]]$Later)
    $matches = [Collections.Generic.List[int]]::new()
    for ($overlap = [Math]::Min($Earlier.Count, $Later.Count); $overlap -ge 3; $overlap--) {
        $equal = $true
        for ($i = 0; $i -lt $overlap; $i++) {
            if ($Earlier[$Earlier.Count - $overlap + $i] -cne $Later[$i]) { $equal = $false; break }
        }
        if ($equal) { $matches.Add($overlap) }
    }
    if ($matches.Count -gt 1) { throw [IO.InvalidDataException]::new('Ambiguous diagnostic page overlap; repeated rows cannot establish the exact encoded bytes.') }
    if ($matches.Count -eq 1) { return ,@($Earlier + @($Later | Select-Object -Skip $matches[0])) }
    throw 'Diagnostic pages do not have a verifiable overlapping row sequence.'
}

function Get-WorkFlowFormField {
    param([string]$Frame, [string]$Label)
    $pattern = '^[> ] ' + [regex]::Escape($Label) + ' \*:(?: (?<value>.*))?$'
    $values = @(foreach ($row in (Get-WorkFlowBodyRows $Frame)) {
        if ($row -cmatch $pattern) { $Matches.value }
    })
    if ($values.Count -ne 1) { throw "Expected one rendered field '$Label'." }
    return [string]$values[0]
}

function Read-WorkFlowJsonSnapshot {
    param([Parameter(Mandatory)][string]$Path)
    $stream = $null
    $reader = $null
    try {
        $stream = [IO.File]::Open($Path, [IO.FileMode]::Open, [IO.FileAccess]::Read,
            [IO.FileShare]::ReadWrite -bor [IO.FileShare]::Delete)
        $reader = [IO.StreamReader]::new($stream, [Text.Encoding]::UTF8)
        return $reader.ReadToEnd() | ConvertFrom-Json -Depth 30
    }
    catch [IO.FileNotFoundException] { return $null }
    catch [IO.IOException] {
        $cause = $_.Exception.GetBaseException()
        # File.Open wraps HRESULT_FROM_WIN32(ERROR_SHARING_VIOLATION) in PowerShell.
        if ($cause -is [IO.IOException] -and $cause.HResult -eq -2147024864) {
            Write-Warning "Snapshot temporarily unavailable (ERROR_SHARING_VIOLATION): $Path"
            return $null
        }
        throw
    }
    finally {
        if ($reader) { $reader.Dispose() }
        elseif ($stream) { $stream.Dispose() }
    }
}

function Read-WorkFlowNewGlobalStatus {
    param([Parameter(Mandatory)][string]$Root, [string[]]$ExcludedFiles = @())
    $files = @(Get-ChildItem -LiteralPath $Root -File -Filter 'global-status-*.json' |
        Where-Object { $_.Name -cnotin $ExcludedFiles })
    if ($files.Count -gt 1) { throw 'More than one new global status source was recorded.' }
    if ($files.Count -eq 0) { return $null }
    $value = Read-WorkFlowJsonSnapshot -Path $files[0].FullName
    if ($null -eq $value) { return $null }
    if (-not $value.source.id -or $files[0].Name -cne "global-status-$($value.source.id).json" -or
        $value.source.role -cne 'human' -or $value.source.context.scope -cne 'Global' -or
        -not $value.source.context.consoleSessionId -or -not $value.conversationId -or
        $value.source.conversationId -cne $value.conversationId) {
        throw 'The global status file does not identify its exact captured human source.'
    }
    return $value
}

function ConvertTo-WorkFlowObserverJson {
    param([Parameter(Mandatory)]$Value)
    # Redirected pwsh can inherit OEM 437 even when its reader expects UTF-8.
    # ASCII JSON escapes preserve every Unicode code point across both readers.
    $Value | ConvertTo-Json -Depth 80 -Compress -EscapeHandling EscapeNonAscii
}

function Test-WorkFlowFrozenRequest {
    param([Parameter(Mandatory)]$Expected, [Parameter(Mandatory)]$Actual)
    $captured = @{ method = $Actual.method; params = $Actual.params; ifMatch = @($Actual.ifMatch); commandId = $Actual.commandId }
    $tokens = @(foreach ($request in @($Expected, $captured)) {
        $reader = [Newtonsoft.Json.JsonTextReader]::new([IO.StringReader]::new(($request | ConvertTo-Json -Depth 80 -Compress)))
        $reader.DateParseHandling = [Newtonsoft.Json.DateParseHandling]::None
        try { ,([Newtonsoft.Json.Linq.JToken]::ReadFrom($reader)) }
        finally { $reader.Dispose() }
    })
    [Newtonsoft.Json.Linq.JToken]::DeepEquals($tokens[0], $tokens[1])
}

function Test-WorkFlowPreviewText {
    param([Parameter(Mandatory)][AllowEmptyCollection()][AllowEmptyString()][string[]]$Rows, [Parameter(Mandatory)][string]$Expected)
    $spaced = ($Rows -join "`n") -replace '\s+', ' '
    $wrapped = $Rows -join ''
    $spaced.Contains($Expected, [StringComparison]::Ordinal) -or $wrapped.Contains($Expected, [StringComparison]::Ordinal)
}

function Get-WorkFlowObservedOperation {
    param([Parameter(Mandatory)]$Response, [Parameter(Mandatory)][string]$OperationId)
    $operation = $Response.data.operation
    if ($Response.status -cne 'ok' -or $operation.kind -cne 'Operation' -or
        $operation.id -cne $OperationId -or -not $operation.status) {
        throw 'operation.get did not return the exact recorded operation view.'
    }
    return $operation
}

function Read-WorkFlowLoadProgress {
    param([string]$Root, [Parameter(Mandatory)]$Process)
    $result = Read-WorkFlowJsonSnapshot -Path (Join-Path $Root 'load-result.json')
    if ($result.failed) { return $result }
    if ($Process.HasExited) {
        return @{ failed = $true; error = "The producer exited before loaded input; inspect $(Join-Path $Root 'load.stderr.txt')." }
    }
    $progress = Read-WorkFlowJsonSnapshot -Path (Join-Path $Root 'load-progress.json')
    if ($null -eq $progress) { return $null }
    if ($progress.completed -le 0 -or $progress.version -le 0 -or -not $progress.storeId) {
        return @{ failed = $true; error = 'Producer progress does not identify an actual committed update.' }
    }
    return $progress
}

function Write-WorkFlowJsonSnapshot {
    param([Parameter(Mandatory)][string]$Path, [Parameter(Mandatory)]$Value)
    $next = "$Path.next"
    $Value | ConvertTo-Json -Depth 30 | Set-Content -LiteralPath $next -Encoding utf8NoBOM
    if (Test-Path -LiteralPath $Path) {
        [IO.File]::Replace($next, $Path, [System.Management.Automation.Language.NullString]::Value)
    }
    else { [IO.File]::Move($next, $Path) }
}

function Stop-WorkFlowLoadProducer {
    param([Parameter(Mandatory)][hashtable]$Context)
    if ($Context.LoadProcess -and -not $Context.LoadProcess.HasExited) {
        Set-Content -LiteralPath (Join-Path $Context.Root 'stop-load') -Value 'stop'
        if (-not $Context.LoadProcess.WaitForExit(20000)) { throw 'The owned load producer did not settle; subsequent mutations must not run.' }
    }
}

function Wait-WorkFlowLoadCheckpoint {
    param(
        [Parameter(Mandatory)][hashtable]$Context,
        [Parameter(Mandatory)][hashtable]$Monitor,
        [double]$TargetSeconds = 0,
        [int]$MinimumUpdates = 1
    )
    # Receipt time, not idle wall time, qualifies the sustained interval.
    while ($Monitor.Clock.Elapsed.TotalSeconds -lt 240) {
        $elapsed = $Monitor.Clock.Elapsed.TotalSeconds
        $progress = Read-WorkFlowLoadProgress -Root $Context.Root -Process $Context.LoadProcess
        if ($progress.failed) { throw $progress.error }
        if ($progress) {
            if ($progress.storeId -cne $Context.Fixture.storeId -or -not $progress.sustained -or
                $null -eq $progress.receiptSpanSeconds -or $progress.receiptSpanSeconds -lt 0 -or
                $progress.completed -lt $Monitor.LastCompleted -or
                $progress.receiptSpanSeconds -lt $Monitor.LastReceiptSpanSeconds) {
                throw 'Sustained producer progress changed authority, mode, or receipt history.'
            }
            $Monitor.LastReceiptSpanSeconds = $progress.receiptSpanSeconds
            if ($progress.completed -gt $Monitor.LastCompleted) {
                $Monitor.LastCompleted = $progress.completed
                $Monitor.LastAdvanceSeconds = $elapsed
            }
            $ready = $progress.receiptSpanSeconds -ge $TargetSeconds -and $progress.completed -ge $MinimumUpdates
            if ($ready -or $elapsed - $Monitor.LastRecordedSeconds -ge 1) {
                @{ observedUtc = [datetime]::UtcNow; observerSeconds = $elapsed; targetSeconds = $TargetSeconds; progress = $progress } |
                    ConvertTo-Json -Depth 8 -Compress |
                    Add-Content -LiteralPath (Join-Path $Context.Root 'load-observations.jsonl') -Encoding utf8NoBOM
                $Monitor.LastRecordedSeconds = $elapsed
            }
            if ($elapsed - $Monitor.LastAdvanceSeconds -ge 15) {
                throw 'The owned sustained producer made no committed progress for fifteen seconds.'
            }
            if ($ready) { return $progress }
        }
        elseif ($Monitor.LastCompleted -gt 0) {
            if ($elapsed - $Monitor.LastAdvanceSeconds -ge 15) {
                throw 'The owned sustained producer made no committed progress for fifteen seconds.'
            }
        }
        elseif ($elapsed -ge 15) {
            throw 'The owned sustained producer did not publish its first committed update within fifteen seconds.'
        }
        Start-Sleep -Milliseconds 200
    }
    throw 'Sustained load did not reach its receipt duration/count checkpoint within the 240-second safety budget.'
}

function Test-WorkFlowLoadContinues {
    param([bool]$Sustained, [bool]$StopRequested, [int]$Completed, [int]$Count, [double]$ElapsedSeconds)
    if ($StopRequested) { return $false }
    if ($Sustained -and $ElapsedSeconds -ge 240) {
        throw 'Sustained load exceeded its 240-second safety budget without a cooperative stop.'
    }
    return $Sustained -or $Completed -lt $Count
}

function Get-WorkFlowSustainedSwitchPlan {
    @(
        @{ name = 'early'; targetSeconds = 0; minimumUpdates = 1; afterStop = $false; indices = @(0, 1, 0, 1) }
        @{ name = 'around-60s'; targetSeconds = 60; minimumUpdates = 1; afterStop = $false; indices = @(0, 1) }
        @{ name = 'around-130s'; targetSeconds = 130; minimumUpdates = 1; afterStop = $false; indices = @(0, 1) }
        @{ name = 'after-stop'; targetSeconds = 181; minimumUpdates = 784; afterStop = $true; indices = @(0, 1) }
    )
}

function Read-WorkFlowFrame {
    param([Parameter(Mandatory)][IO.Stream]$Stream, [int]$TimeoutMs = 15000, [ref]$ByteCount)
    $clock = [Diagnostics.Stopwatch]::StartNew()
    function Read-Bytes([int]$Count) {
        $bytes = [byte[]]::new($Count)
        $offset = 0
        while ($offset -lt $Count) {
            $remaining = $TimeoutMs - [int]$clock.ElapsedMilliseconds
            if ($remaining -le 0) { throw 'Agent Center fixture response exceeded 15 seconds.' }
            $read = $Stream.ReadAsync($bytes, $offset, $Count - $offset)
            if (-not $read.Wait($remaining)) { throw 'Agent Center fixture response timed out.' }
            if ($read.Result -eq 0) { throw 'Agent Center fixture pipe closed mid-frame.' }
            $offset += $read.Result
        }
        return ,$bytes
    }
    $length = [BitConverter]::ToUInt32((Read-Bytes 4), 0)
    if ($length -eq 0 -or $length -gt 1048576) { throw 'Agent Center fixture frame length is invalid.' }
    if ($ByteCount) { $ByteCount.Value = $length }
    [Text.UTF8Encoding]::new($false, $true).GetString((Read-Bytes $length)) | ConvertFrom-Json -Depth 80
}

function Write-WorkFlowFrame {
    param([Parameter(Mandatory)][IO.Stream]$Stream, [Parameter(Mandatory)]$Frame)
    $bytes = [Text.Encoding]::UTF8.GetBytes(($Frame | ConvertTo-Json -Depth 80 -Compress))
    if ($bytes.Length -gt 1048576) { throw 'Agent Center fixture request exceeds the frame budget.' }
    $header = [BitConverter]::GetBytes([uint32]$bytes.Length)
    $Stream.Write($header, 0, $header.Length)
    $Stream.Write($bytes, 0, $bytes.Length)
    $Stream.Flush()
}

function Open-WorkFlowConnection {
    param([Parameter(Mandatory)][string]$StateRoot)
    $pipe = [IO.Pipes.NamedPipeClientStream]::new('.', (Get-WorkFlowPipeName $StateRoot),
        [IO.Pipes.PipeDirection]::InOut, [IO.Pipes.PipeOptions]::Asynchronous)
    try {
        $pipe.Connect(15000)
        Write-WorkFlowFrame $pipe @{
            type = 'hello'; versions = @(1); clientInstanceId = [guid]::NewGuid().ToString()
            clientKind = 'CLI'
        }
        $welcome = Read-WorkFlowFrame $pipe
        if ($welcome.type -cne 'welcome' -or $welcome.version -ne 1 -or -not $welcome.storeId) {
            throw 'Agent Center fixture did not negotiate a store-bound v1 connection.'
        }
        return @{ Pipe = $pipe; Welcome = $welcome }
    }
    catch { $pipe.Dispose(); throw }
}

function Invoke-WorkFlowRequest {
    param(
        [Parameter(Mandatory)]$Connection,
        [Parameter(Mandatory)][string]$Method,
        [hashtable]$Params = @{},
        [object[]]$IfMatch = @(),
        [switch]$Mutation,
        [string]$ReceiptPath
    )
    $request = @{
        type = 'request'; requestId = [guid]::NewGuid().ToString()
        method = $Method; params = $Params; ifMatch = @($IfMatch)
    }
    if ($Mutation) { $request.commandId = [guid]::NewGuid().ToString() }
    Write-WorkFlowFrame $Connection.Pipe $request
    $receivedBytes = 0
    $response = Read-WorkFlowFrame $Connection.Pipe -ByteCount ([ref]$receivedBytes)
    $Connection.LastReceivedBytes = $receivedBytes
    if ($response.type -cne 'response' -or $response.requestId -cne $request.requestId) {
        throw 'Agent Center fixture received an uncorrelated response; no mutation will be replayed.'
    }
    if ($ReceiptPath) {
        @{ request = $request; response = $response; receivedUtc = [DateTime]::UtcNow.ToString('o') } |
            ConvertTo-Json -Depth 80 -Compress | Add-Content -LiteralPath $ReceiptPath -Encoding utf8NoBOM
    }
    if ($response.status -ceq 'pending') {
        if (-not $Mutation -or -not $response.operationId) { throw 'A pending mutation must identify its recorded operation.' }
    }
    elseif ($response.status -cne 'ok') {
        throw "Agent Center fixture request $Method failed: $($response | ConvertTo-Json -Depth 15 -Compress)"
    }
    return $response
}

function Initialize-WorkFlowFixture {
    param(
        [Parameter(Mandatory)]$Connection,
        [Parameter(Mandatory)][string]$EvidenceDirectory,
        [switch]$EnableIntake
    )
    $receipt = Join-Path $EvidenceDirectory 'seed-receipts.jsonl'
    $definitions = @(
        @{ name = 'Harbor Reports'; goal = 'Review the harbor checklist'; directory = 'harbor' }
        @{ name = 'Orchard Notes'; goal = 'Review the orchard checklist'; directory = 'orchard' }
    )
    $works = @()
    foreach ($definition in $definitions) {
        $root = Join-Path $EvidenceDirectory $definition.directory
        New-Item -ItemType Directory -Path $root -Force | Out-Null
        $project = Invoke-WorkFlowRequest $Connection 'project.configure' -Mutation -ReceiptPath $receipt -Params @{
            name = $definition.name; root = $root
            coordinatorCapabilityId = $(if ($EnableIntake -and $definition.directory -eq 'harbor') { 'ite2e-local-intake' } else { 'ite2e-never-run' })
            workerCapabilityId = 'ite2e-never-run'
            checkCapabilityId = 'native-check'
            capabilityIds = @(if ($EnableIntake) { 'ite2e-local-global' })
            limits = @{
                concurrency = 1; executionAttempts = 1; evaluationAttempts = 1
                coordinationTurns = $(if ($EnableIntake) { 3 } else { 1 }); contextRounds = 1
                executionSeconds = 60; coordinationSeconds = 60
            }
        }

        $work = Invoke-WorkFlowRequest $Connection 'work.create_draft' -Mutation -ReceiptPath $receipt -Params @{
            projectId = $project.data.projectId; goal = $definition.goal
            scope = @('reports'); exclusions = @(); context = @(); sourceMessageIds = @()
            criteria = @(@{ id = 'report'; description = 'Readable report'; evidenceRule = 'artifact:report' })
            delivery = @{ kind = 'Report' }
        }
        $works += @{
            projectId = $project.data.projectId; projectName = $definition.name
            id = $work.data.workId; goal = $definition.goal
        }
    }
    $decisionProject = $null
    if ($EnableIntake) {
        $root = Join-Path $EvidenceDirectory 'horizon'
        New-Item -ItemType Directory -Path $root -Force | Out-Null
        $project = Invoke-WorkFlowRequest $Connection 'project.configure' -Mutation -ReceiptPath $receipt -Params @{
            name = 'Horizon Decisions'; root = $root
            coordinatorCapabilityId = 'ite2e-local-decision'; workerCapabilityId = 'ite2e-local-decision'; checkCapabilityId = 'native-check'
            capabilityIds = @('ite2e-local-global')
            limits = @{ concurrency = 1; executionAttempts = 1; evaluationAttempts = 1; coordinationTurns = 3
                contextRounds = 1; executionSeconds = 180; coordinationSeconds = 90 }
        }
        $decisionProject = @{
            projectId = $project.data.projectId; projectName = 'Horizon Decisions'
            goal = 'Produce the human-approved decision report'; root = $root
            prompt = "Please produce the human-approved decision report in $root and ask for the report details before writing."
        }
        Write-WorkFlowJsonSnapshot -Path (Join-Path $EvidenceDirectory 'decision-fixture.json') -Value $decisionProject
        $globalPath = Join-Path $EvidenceDirectory 'global-fixture.json'
        if (Test-Path -LiteralPath $globalPath) {
            $global = Get-Content -LiteralPath $globalPath -Raw | ConvertFrom-Json -AsHashtable
            $global.decisionPrompt = $decisionProject.prompt
            Write-WorkFlowJsonSnapshot -Path $globalPath -Value $global
        }
    }
    @{
        works = $works; storeId = $Connection.Welcome.storeId
        seededThrough = @('project.configure', 'work.create_draft')
        safety = 'Seeded works remain drafts; no work.start during seeding. Separate report execution requires its explicit test-owned grant.'
        scriptedIntakeEnabled = [bool]$EnableIntake
        decisionProject = $decisionProject
    }
}

function Initialize-WorkFlowGlobalFixture {
    param([Parameter(Mandatory)]$Connection, [Parameter(Mandatory)][string]$EvidenceDirectory)
    $root = Join-Path $EvidenceDirectory 'approved'
    New-Item -ItemType Directory -Path $root -Force | Out-Null
    $configuration = @{
        projectRoot = $root; projectName = 'Approved Reports'
        projectPrompt = "Please prepare an execution project for my report in $root. Ask for approval before creating it."
    }
    Write-WorkFlowJsonSnapshot -Path (Join-Path $EvidenceDirectory 'global-fixture.json') -Value $configuration
    @{
        works = @(); storeId = $Connection.Welcome.storeId; global = $configuration
        seededThrough = @(); safety = 'Zero Projects or Works before the actual global hello and project approval.'
    }
}

function Start-WorkFlowTestContext {
            param([Parameter(Mandatory)][hashtable]$Context)
            if ((Get-ItTestPackage) -cne 'Dev' -or $env:ITE2E_EXPECTED_WTA_SHA256 -notmatch '^[0-9a-fA-F]{64}$') {
                throw 'Work-flow proof requires ITE2E_PACKAGE=Dev and an independently verified ITE2E_EXPECTED_WTA_SHA256.'
            }
            $sqlite = Invoke-Native -FilePath 'node.exe' -Arguments @('--no-warnings', '-e', "require('node:sqlite')") -TimeoutSec 10
            if ($sqlite.ExitCode -ne 0) { throw 'Read-only command receipts require Node with node:sqlite, before any fixture launch.' }
            $target = Resolve-ItApp -Package Dev
            $hash = (Get-FileHash -LiteralPath $target.WtaPath -Algorithm SHA256).Hash
            if ($hash -ne $env:ITE2E_EXPECTED_WTA_SHA256) { throw 'The deployed WTA does not match the selected feature build.' }
            if (@(Get-WtProcessesForApp -App $target).Count) {
                throw 'Close existing Dev hosts yourself; the work-flow fixture will not stop them.'
            }
            foreach ($path in @($target.SettingsPath, $target.StatePath)) {
                if ((Test-Path "$path.e2ebak") -or (Test-Path "$path.e2ebak.missing")) {
                    throw "A different test owns a configuration backup: $path"
                }
            }
            $base = if ($env:ITE2E_ARTIFACT_ROOT) { $env:ITE2E_ARTIFACT_ROOT } else { Join-Path $PSScriptRoot '..\..\artifacts' }
            $Context.Root = [IO.Path]::GetFullPath((Join-Path $base "agent-center-work-flow-$([guid]::NewGuid().ToString('N'))"))
            New-Item -ItemType Directory -Path $Context.Root -Force | Out-Null
            $Context.Target = $target
            $Context.SettingsHashes = @{}
            foreach ($path in @($target.SettingsPath, $target.StatePath)) {
                $Context.SettingsHashes[$path] = if (Test-Path -LiteralPath $path) { (Get-FileHash -LiteralPath $path).Hash } else { 'missing' }
            }
            $Context.SettingsHashes | ConvertTo-Json |
                Set-Content -LiteralPath (Join-Path $Context.Root 'settings-before.json') -Encoding utf8NoBOM
            @{ package = $target.Package; version = $target.Version; binary = $target.WtaPath; sha256 = $hash } |
                ConvertTo-Json | Set-Content -LiteralPath (Join-Path $Context.Root 'package.json') -Encoding utf8NoBOM
            $Context.Clipboard = Get-ClipboardSnapshot
            $Context.ClipboardSaved = $true
            $fixture = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..\..\fixtures\Start-AgentCenterNativePaste.ps1'))
            $invoke = "& '$($fixture.Replace("'", "''"))' -WtaPath '$($target.WtaPath.Replace("'", "''"))' -EvidenceDirectory '$($Context.Root.Replace("'", "''"))' -ExpectedSha256 '$hash' -Scenario WorkFlow -NavigationDiagnostics"
            $command = 'pwsh.exe -NoLogo -NoProfile -EncodedCommand ' + [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($invoke))
            $profile = '{9d3472ab-5da5-4880-b4c2-15c093aa4460}'
            $Context.StartAttempted = $true
            $Context.App = Start-Terminal -Package Dev -PassFre $true -Settings @{
                defaultProfile = $profile; startupActions = ''; firstWindowPreference = 'defaultProfile'
                initialCols = 140; initialRows = 45
                profiles = @{ defaults = @{}; list = @(@{
                    guid = $profile; name = 'ItE2E work flow'; commandline = $command
                    startingDirectory = $Context.Root; closeOnExit = 'never'
                }) }
                acpAgent = 'custom:work-flow-no-provider'; acpCustomCommand = 'cmd.exe /d /c exit 77'
                acpModel = ''; autoErrorDetectionEnabled = $false; autoFixEnabled = $false
                agentSessionManagementEnabled = $false; multiLinePasteWarning = $false
                'warning.confirmOnClose' = 'never'
                actions = @(@{ command = 'paste'; keys = 'ctrl+shift+v' })
            }
            $Context.Pane = Get-ActivePane -App $Context.App
            Wait-Until -TimeoutSec 40 -Because 'the isolated work-flow fixture is seeded and renders its draft' -Condition {
                if (-not (Test-Path -LiteralPath (Join-Path $Context.Root 'work-flow.json'))) { return $false }
                Get-NativePasteDraftRows -Frame (Get-WtCapture -App $Context.App -SessionId $Context.Pane.session_id) | Out-Null
                return $true
            } | Out-Null
            $Context.Runtime = Get-Content -LiteralPath (Join-Path $Context.Root 'runtime.json') -Raw | ConvertFrom-Json
            $Context.Fixture = Get-Content -LiteralPath (Join-Path $Context.Root 'work-flow.json') -Raw | ConvertFrom-Json
            if ($Context.Runtime.sha256 -ne $hash -or $Context.Runtime.scenario -cne 'WorkFlow') {
                throw 'The isolated work-flow runtime provenance is inconsistent.'
            }
            $ui = @(Get-CimInstance Win32_Process -Filter "ParentProcessId=$($Context.Runtime.shellPid)" |
                Where-Object { $_.ExecutablePath -eq $target.WtaPath -and $_.CommandLine -match '(?:^|\s)ui(?:\s|$)' })
            if ($ui.Count -ne 1) { throw 'Cannot identify the one test-owned deployed wta ui process.' }
            $Context.UiIdentity = @{ pid = $ui[0].ProcessId; created = $ui[0].CreationDate }
            if (-not $Context.Runtime.navigationDiagnostics -or $Context.Runtime.uiLogFilter -cne (Get-WorkFlowNavigationLogFilter)) {
                throw 'The owned work-flow UI did not record the requested navigation-only log configuration.'
            }
            Save-WorkFlowNavigationLogProvenance -Context $Context
            $Context.Connection = Open-WorkFlowConnection -StateRoot $Context.Runtime.stateRoot
            if ($Context.Connection.Welcome.storeId -cne $Context.Fixture.storeId) { throw 'The fixture authority changed.' }
            $Context.Sequence = 0
            Set-WtPaneFocus -App $Context.App -SessionId $Context.Pane.session_id
            if (-not (Test-WtWindowKeyFocusable -App $Context.App)) {
                throw 'Cannot acquire the test-owned foreground window; no native work-flow input was sent.'
            }
        }

        function Stop-WorkFlowTestContext {
            param([Parameter(Mandatory)][hashtable]$Context)
            $errors = [Collections.Generic.List[string]]::new()
            try {
                if ($Context.LoadProcess -and -not $Context.LoadProcess.HasExited) {
                    Set-Content -LiteralPath (Join-Path $Context.Root 'stop-load') -Value 'stop'
                    if (-not $Context.LoadProcess.WaitForExit(20000)) {
                        Stop-Process -Id $Context.LoadProcess.Id -Force
                        $errors.Add('The test-owned load producer required forced cleanup.')
                    }
                }
                if ($Context.Connection) { $Context.Connection.Pipe.Dispose() }
            }
            catch { $errors.Add($_.Exception.Message) }
            try {
                if ($Context.App) { Stop-Terminal -App $Context.App }
                elseif ($Context.StartAttempted) {
                    Restore-WtConfig -App $Context.Target
                    if (@(Get-WtProcessesForApp -App $Context.Target).Count) {
                        $errors.Add('Launch failed before an owned window was identified; no unidentified host was terminated.')
                    }
                }
            }
            catch { $errors.Add($_.Exception.Message) }
            finally {
                try {
                    if ($Context.ClipboardSaved) { Restore-ClipboardSnapshot -Snapshot $Context.Clipboard }
                }
                catch { $errors.Add($_.Exception.Message) }
            }
            foreach ($path in @($Context.SettingsHashes.Keys)) {
                $actual = if (Test-Path -LiteralPath $path) { (Get-FileHash -LiteralPath $path).Hash } else { 'missing' }
                if ($actual -ne $Context.SettingsHashes[$path]) { $errors.Add("Configuration was not restored: $path") }
            }
            if ($Context.UiIdentity) {
                $current = Get-CimInstance Win32_Process -Filter "ProcessId=$($Context.UiIdentity.pid)"
                if ($current -and $current.CreationDate -eq $Context.UiIdentity.created) { $errors.Add('The test-owned UI process survived teardown.') }
            }
            if ($Context.Root) {
                try { Save-WorkFlowNavigationLogProvenance -Context $Context -Final }
                catch { $errors.Add($_.Exception.Message) }
                @{ errors = @($errors.ToArray()); keyboardCleanupAttempted = $false; nativeInputSent = [bool]$Context.NativeInputSent } |
                    ConvertTo-Json | Set-Content -LiteralPath (Join-Path $Context.Root 'cleanup.json') -Encoding utf8NoBOM
            }
            if ($errors.Count) { throw ($errors -join '; ') }
        }
