function Get-NativePasteInputBox {
    [CmdletBinding()]
    param([Parameter(Mandatory)][AllowEmptyString()][string]$Frame)

    $lines = $Frame -split '\r?\n'
    $candidates = @(for ($i = 0; $i -lt $lines.Count; $i++) {
        $line = $lines[$i]
        $rail = $line -cmatch '^[^│]*│┌'
        if ($rail) { $line = $line.Substring($line.IndexOf('│') + 1) }
        # Enter starts the input title (optionally wrapped by a pseudo-locale).
        # Case-sensitive placement and the token boundary exclude "Agent Center".
        if ($line -cmatch '^[ \t]*┌(?:\[!*[ _]?)?Enter\b[^\r\n]*┐[ \t]*$') {
            @{ titleRow = $i; hasRail = $rail }
        }
    })
    if ($candidates.Count -gt 1) { throw 'The Agent Center capture has ambiguous input boxes.' }
    foreach ($candidate in $candidates) {
        $i = $candidate.titleRow
        $scope = if ($i -gt 0) { $lines[$i - 1] } else { '' }
        if ($candidate.hasRail) {
            if ($scope -cnotmatch '^[^│]*│') { throw 'The Agent Center input scope has no rail boundary.' }
            $scope = $scope.Substring($scope.IndexOf('│') + 1)
        }
        $rows = [Collections.Generic.List[string]]::new()
        for ($j = $i + 1; $j -lt $lines.Count; $j++) {
            $line = $lines[$j]
            if ($candidate.hasRail) {
                if ($line -cnotmatch '^[^│]*│[│└]') { throw 'The Agent Center input box has a missing rail boundary.' }
                $line = $line.Substring($line.IndexOf('│') + 1)
            }
            if ($line -cmatch '^[ \t]*│(.*)│[ \t]*$') {
                # Only ASCII spaces are terminal cell padding; preserve leading
                # whitespace, tabs, and nonbreaking spaces belonging to the draft.
                $rows.Add($Matches[1].TrimEnd([char]' '))
            }
            elseif ($line -cmatch '^[ \t]*└─*┘[ \t]*$' -and $rows.Count) {
                return @{
                    rows = $rows.ToArray(); titleRow = $i; firstRow = $i + 1; endRow = $j
                    hasRail = $candidate.hasRail; scope = $scope.TrimEnd([char]' ')
                }
            }
            else { throw 'The Agent Center input box has missing or malformed draft rows.' }
        }
        throw 'The Agent Center input box has no closing border.'
    }
    throw 'The Agent Center input box is not rendered.'
}

function Get-NativePasteDraftRows {
    [CmdletBinding()]
    param([Parameter(Mandatory)][AllowEmptyString()][string]$Frame)
    return ,(Get-NativePasteInputBox -Frame $Frame).rows
}

function Get-NativePasteCaretPrefix {
    param([Parameter(Mandatory)][string]$DocumentBeforeCaret, [Parameter(Mandatory)]$InputBox)
    $lines = $DocumentBeforeCaret -split '\r?\n'
    $row = $lines.Count - 1
    if ($row -lt $InputBox.firstRow -or $row -ge $InputBox.endRow) {
        throw 'The native caret is outside the captured composer rows.'
    }
    $line = $lines[-1]
    if ($InputBox.hasRail) {
        if ($line -cnotmatch '^[^│]*││') { throw 'The native caret is outside the railed composer.' }
        $line = $line.Substring($line.IndexOf('│') + 1)
    }
    if ($line -cnotmatch '^│') { throw 'The native caret is outside the bordered composer.' }
    return $line.Substring(1)
}
