# Copyright (c) Microsoft Corporation.
# Licensed under the MIT license.

#Requires -Version 7.0

<#
.SYNOPSIS
Deterministic, bounded tmux control-mode stdio fixture. Does not launch a shell.
.DESCRIPTION
Use -CC for DCS framing, -Mode SwapOnInput or RestructureOnInput for a one-shot
layout transition on the first send-keys, and -InvocationLogPath for JSONL input
records. Only protocol bytes are written to stdout. See doc/wtcli-commands.md.
#>
[CmdletBinding()]
param(
    [Alias('CC')]
    [switch]$ControlControl,
    [ValidateSet('Static', 'SwapOnInput', 'RestructureOnInput')]
    [string]$Mode = 'Static',
    [string]$InvocationLogPath,
    [string]$SocketPath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$utf8 = [Text.UTF8Encoding]::new($false, $true)
$inputStream = [Console]::OpenStandardInput()
$outputStream = [Console]::OpenStandardOutput()
$script:windows = [Collections.Generic.SortedDictionary[int, object]]::new()
$script:panes = [Collections.Generic.Dictionary[int, object]]::new()
$script:activeWindow = 0
$script:nextPane = 3
$script:nextWindow = 2
$script:commandNumber = 0
$script:inputBytes = 0
$script:transitionDone = $false
$script:detached = $false
$script:sessionName = 'fixture'
$log = $null

function Write-Protocol([string]$Text) {
    $bytes = $utf8.GetBytes($Text)
    $outputStream.Write($bytes, 0, $bytes.Length)
    $outputStream.Flush()
}

function Write-Response([string[]]$Lines, [bool]$Success = $true, [int]$Flags = 1) {
    $guard = "$(1700000000L + $script:commandNumber) $script:commandNumber $Flags"
    Write-Protocol "%begin $guard`n"
    foreach ($line in $Lines) {
        Write-Protocol "$line`n"
    }
    $end = if ($Success) { '%end' } else { '%error' }
    Write-Protocol "$end $guard`n"
}

function Encode-Bytes([byte[]]$Bytes) {
    $text = [Text.StringBuilder]::new()
    foreach ($byte in $Bytes) {
        if ($byte -lt 32 -or $byte -ge 127 -or $byte -eq 92) {
            [void]$text.Append('\').Append([Convert]::ToString($byte, 8).PadLeft(3, '0'))
        } else {
            [void]$text.Append([char]$byte)
        }
    }
    return $text.ToString()
}

function Read-CommandLine {
    $bytes = [Collections.Generic.List[byte]]::new()
    while (($byte = $inputStream.ReadByte()) -ne -1) {
        if (++$script:inputBytes -gt 4194304 -or $bytes.Count -ge 65536) {
            throw 'Fixture input limit exceeded'
        }
        if ($byte -eq 10) {
            return $utf8.GetString($bytes.ToArray()).TrimEnd("`r")
        }
        $bytes.Add([byte]$byte)
    }
    if ($bytes.Count) {
        throw 'Truncated command at stdin EOF'
    }
    return $null
}

function Split-Commands([string]$Line) {
    $commands = [Collections.Generic.List[object]]::new()
    $words = [Collections.Generic.List[string]]::new()
    $word = [Text.StringBuilder]::new()
    $quote = [char]0
    $started = $false
    for ($i = 0; $i -lt $Line.Length; ++$i) {
        $ch = $Line[$i]
        if ($ch -eq '\' -and $quote -ne "'") {
            if (++$i -ge $Line.Length) { throw 'Unfinished command escape' }
            [void]$word.Append($Line[$i])
            $started = $true
        } elseif ($quote -ne [char]0) {
            if ($ch -eq $quote) { $quote = [char]0 } else { [void]$word.Append($ch) }
        } elseif ($ch -eq "'" -or $ch -eq '"') {
            $quote = $ch
            $started = $true
        } elseif ([char]::IsWhiteSpace($ch) -or $ch -eq ';') {
            if ($started) {
                $words.Add($word.ToString())
                [void]$word.Clear()
                $started = $false
            }
            if ($ch -eq ';' -and $words.Count) {
                $commands.Add($words.ToArray())
                $words.Clear()
            }
        } else {
            [void]$word.Append($ch)
            $started = $true
        }
    }
    if ($quote -ne [char]0) { throw 'Unclosed command quote' }
    if ($started) { $words.Add($word.ToString()) }
    if ($words.Count) { $commands.Add($words.ToArray()) }
    if ($commands.Count -gt 128) { throw 'Too many compound commands' }
    return ,$commands
}

function Get-Option([string[]]$Words, [string]$Name, [string]$Default = '') {
    for ($i = 1; $i -lt $Words.Count; ++$i) {
        if ($Words[$i] -ceq $Name) {
            if ($i + 1 -ge $Words.Count) { throw "Missing value for $Name" }
            return $Words[$i + 1]
        }
        if ($Words[$i].StartsWith($Name, [StringComparison]::Ordinal) -and $Words[$i].Length -gt $Name.Length) {
            return $Words[$i].Substring($Name.Length)
        }
    }
    return $Default
}

function New-Leaf([int]$Id) {
    return [pscustomobject]@{ Kind = 'Leaf'; Id = $Id; First = $null; Second = $null; Ratio = 0.5 }
}

function New-Branch([string]$Kind, $First, $Second) {
    return [pscustomobject]@{ Kind = $Kind; Id = -1; First = $First; Second = $Second; Ratio = 0.5 }
}

function Add-Pane([int]$Id) {
    if ($script:panes.Count -ge 32) { throw 'Fixture pane limit exceeded' }
    $script:panes.Add($Id, [pscustomobject]@{
        Width = 80; Height = 24
        Current = $utf8.GetBytes("`e[32mfixture %$Id 世界`e[0m`nready %$Id")
        Saved = $utf8.GetBytes("saved screen %$Id")
        Alternate = [int]($Id -eq 1)
    })
}

function Get-PaneIds($Node) {
    if ($Node.Kind -eq 'Leaf') { return $Node.Id }
    Get-PaneIds $Node.First
    Get-PaneIds $Node.Second
}

function Get-MinimumSize($Node) {
    if ($Node.Kind -eq 'Leaf') { return @(1, 1) }
    $first = Get-MinimumSize $Node.First
    $second = Get-MinimumSize $Node.Second
    if ($Node.Kind -eq 'Columns') {
        return @((1 + $first[0] + $second[0]), [Math]::Max($first[1], $second[1]))
    }
    return @([Math]::Max($first[0], $second[0]), (1 + $first[1] + $second[1]))
}

function Get-LayoutBody($Node, [int]$Width, [int]$Height, [int]$X = 0, [int]$Y = 0) {
    $prefix = "${Width}x${Height},$X,$Y"
    if ($Node.Kind -eq 'Leaf') {
        $script:panes[$Node.Id].Width = $Width
        $script:panes[$Node.Id].Height = $Height
        return "$prefix,$($Node.Id)"
    }
    $firstMin = Get-MinimumSize $Node.First
    $secondMin = Get-MinimumSize $Node.Second
    if ($Node.Kind -eq 'Columns') {
        $size = [Math]::Clamp([int][Math]::Floor(($Width - 1) * $Node.Ratio), $firstMin[0], ($Width - 1 - $secondMin[0]))
        $first = Get-LayoutBody $Node.First $size $Height $X $Y
        $second = Get-LayoutBody $Node.Second ($Width - $size - 1) $Height ($X + $size + 1) $Y
        return "$prefix{$first,$second}"
    }
    $size = [Math]::Clamp([int][Math]::Floor(($Height - 1) * $Node.Ratio), $firstMin[1], ($Height - 1 - $secondMin[1]))
    $first = Get-LayoutBody $Node.First $Width $size $X $Y
    $second = Get-LayoutBody $Node.Second $Width ($Height - $size - 1) $X ($Y + $size + 1)
    return "$prefix[$first,$second]"
}

function Add-Checksum([string]$Body) {
    $checksum = 0
    foreach ($byte in $utf8.GetBytes($Body)) {
        $checksum = ((($checksum -shr 1) -bor (($checksum -band 1) -shl 15)) + $byte) -band 65535
    }
    return ('{0:x4},{1}' -f $checksum, $Body)
}

function Get-WindowLayouts($Window) {
    $minimum = Get-MinimumSize $Window.Tree
    $Window.Width = [Math]::Max($Window.Width, $minimum[0])
    $Window.Height = [Math]::Max($Window.Height, $minimum[1])
    $full = Add-Checksum (Get-LayoutBody $Window.Tree $Window.Width $Window.Height)
    $visible = if ($Window.Zoom) {
        Add-Checksum (Get-LayoutBody (New-Leaf $Window.Active) $Window.Width $Window.Height)
    } else { $full }
    return @($full, $visible)
}

function Get-LayoutNotification([int]$Id) {
    $layouts = Get-WindowLayouts $script:windows[$Id]
    return "%layout-change @$Id $($layouts[0]) $($layouts[1]) *"
}

function Get-WindowId([string]$Target) {
    if (!$Target) { return $script:activeWindow }
    if ($Target -cmatch '^@([0-9]+)$' -and $script:windows.ContainsKey([int]$Matches[1])) {
        return [int]$Matches[1]
    }
    if ($Target -cmatch '^%([0-9]+)$') {
        $id = [int]$Matches[1]
        foreach ($entry in $script:windows.GetEnumerator()) {
            if (@(Get-PaneIds $entry.Value.Tree) -contains $id) { return $entry.Key }
        }
    }
    throw "Unknown window target: $Target"
}

function Get-PaneId([string]$Target) {
    if ($Target -cmatch '^%([0-9]+)$' -and $script:panes.ContainsKey([int]$Matches[1])) {
        return [int]$Matches[1]
    }
    if (!$Target -or $Target.StartsWith('@')) {
        return $script:windows[(Get-WindowId $Target)].Active
    }
    throw "Unknown pane target: $Target"
}

function Replace-Leaf($Node, [int]$Id, $Replacement) {
    if ($Node.Kind -eq 'Leaf') {
        if ($Node.Id -eq $Id) { return $Replacement }
        return $Node
    }
    $Node.First = Replace-Leaf $Node.First $Id $Replacement
    $Node.Second = Replace-Leaf $Node.Second $Id $Replacement
    if ($null -eq $Node.First) { return $Node.Second }
    if ($null -eq $Node.Second) { return $Node.First }
    return $Node
}

function Find-Parent($Node, [int]$Id) {
    if ($Node.Kind -eq 'Leaf') { return $null }
    if (($Node.First.Kind -eq 'Leaf' -and $Node.First.Id -eq $Id) -or
        ($Node.Second.Kind -eq 'Leaf' -and $Node.Second.Id -eq $Id)) { return $Node }
    $found = Find-Parent $Node.First $Id
    if ($null -ne $found) { return $found }
    return Find-Parent $Node.Second $Id
}

function Expand-Format([string]$Format, [hashtable]$Values) {
    foreach ($match in [regex]::Matches($Format, '#\{([a-z0-9_]+)\}')) {
        $name = $match.Groups[1].Value
        if (!$Values.ContainsKey($name)) { throw "Unsupported format: $name" }
        $Format = $Format.Replace($match.Value, [string]$Values[$name])
    }
    return $Format
}

function Invoke-Transition([string]$Kind) {
    $ids = @($script:panes.Keys | Sort-Object)
    if ($ids.Count -lt 2) { throw 'Layout transition needs at least two panes' }
    if ($Kind -eq 'Swap') {
        # A temporary sentinel cannot collide with the fixture's nonnegative IDs.
        foreach ($window in $script:windows.Values) {
            $window.Tree = Replace-Leaf $window.Tree $ids[0] (New-Leaf -1)
            $window.Tree = Replace-Leaf $window.Tree $ids[-1] (New-Leaf $ids[0])
            $window.Tree = Replace-Leaf $window.Tree -1 (New-Leaf $ids[-1])
            $window.Active = @(Get-PaneIds $window.Tree)[0]
        }
    } else {
        $firstWindow = @($script:windows.Keys)[0]
        $tree = New-Leaf $ids[-1]
        for ($i = $ids.Count - 2; $i -ge 0; --$i) {
            $orientation = if ($i % 2) { 'Columns' } else { 'Rows' }
            $tree = New-Branch $orientation (New-Leaf $ids[$i]) $tree
        }
        foreach ($id in @($script:windows.Keys)) {
            if ($id -ne $firstWindow) {
                [void]$script:windows.Remove($id)
                "%window-close @$id"
            }
        }
        $script:windows[$firstWindow].Tree = $tree
        $script:windows[$firstWindow].Active = $ids[0]
        $script:activeWindow = $firstWindow
    }
    foreach ($entry in $script:windows.GetEnumerator()) {
        $entry.Value.Zoom = $false
        Get-LayoutNotification $entry.Key
        "%window-pane-changed @$($entry.Key) %$($entry.Value.Active)"
    }
    "%session-window-changed `$0 @$script:activeWindow"
}

function Invoke-FixtureCommand([string[]]$Words) {
    $lines = [Collections.Generic.List[string]]::new()
    $events = [Collections.Generic.List[string]]::new()
    $target = Get-Option $Words '-t'
    switch -CaseSensitive ($Words[0]) {
        'refresh-client' {
            $size = Get-Option $Words '-C'
            if ($size -cnotmatch '^([0-9]{1,4})[x,]([0-9]{1,4})$') { throw 'Expected refresh-client -C <columns>x<rows>' }
            $width = [int]$Matches[1]
            $height = [int]$Matches[2]
            if ($width -lt 1 -or $width -gt 1000 -or $height -lt 1 -or $height -gt 1000) { throw 'Invalid client dimensions' }
            foreach ($entry in $script:windows.GetEnumerator()) {
                $entry.Value.Width = $width
                $entry.Value.Height = $height
                $events.Add((Get-LayoutNotification $entry.Key))
            }
        }
        'list-windows' {
            $format = Get-Option $Words '-F' '#{window_id} #{window_active} #{window_layout} #{window_visible_layout}'
            foreach ($entry in $script:windows.GetEnumerator()) {
                $layouts = Get-WindowLayouts $entry.Value
                $lines.Add((Expand-Format $format @{
                    window_id = "@$($entry.Key)"; window_active = [int]($entry.Key -eq $script:activeWindow)
                    window_layout = $layouts[0]; window_visible_layout = $layouts[1]; window_name = $entry.Value.Name
                }))
            }
        }
        'display-message' {
            if ($Words -cnotcontains '-p') { throw 'Only display-message -p is supported' }
            $format = $Words[-1]
            $windowId = Get-WindowId $target
            $window = $script:windows[$windowId]
            $id = Get-PaneId $target
            $pane = $script:panes[$id]
            $lines.Add((Expand-Format $format @{
                session_id = '$0'; session_name = $script:sessionName; socket_path = $SocketPath
                window_id = "@$windowId"; window_name = $window.Name; pane_id = "%$id"
                cursor_x = 2; cursor_y = [Math]::Min(1, $pane.Height - 1); alternate_on = $pane.Alternate
                alternate_saved_x = $(if ($pane.Alternate) { 1 } else { [uint32]::MaxValue })
                alternate_saved_y = $(if ($pane.Alternate) { 0 } else { [uint32]::MaxValue })
                cursor_flag = 1; insert_flag = 0
                keypad_cursor_flag = $pane.Alternate; keypad_flag = 0; mouse_standard_flag = 0
                mouse_button_flag = $pane.Alternate; mouse_any_flag = 0; mouse_utf8_flag = 0
                mouse_sgr_flag = $pane.Alternate; scroll_region_upper = 0; scroll_region_lower = $pane.Height - 1
                wrap_flag = 1; pane_width = $pane.Width; pane_height = $pane.Height
            }))
        }
        'capture-pane' {
            $pane = $script:panes[(Get-PaneId $target)]
            $flags = ($Words | Where-Object { $_ -cmatch '^-[a-zA-Z]+$' }) -join ''
            if (!$flags.Contains('p')) { throw 'Only capture-pane to stdout is supported' }
            if (!$flags.Contains('P')) {
                $bytes = if ($flags.Contains('a')) { $pane.Saved } else { $pane.Current }
                $lines.Add((Encode-Bytes $bytes))
            }
        }
        'send-keys' {
            if ($Words -cnotcontains '-H') { throw 'Only send-keys -H is supported' }
            $id = Get-PaneId $target
            $bytes = [Collections.Generic.List[byte]]::new()
            for ($i = 1; $i -lt $Words.Count; ++$i) {
                if ($Words[$i] -ceq '-H') { continue }
                if ($Words[$i] -ceq '-t') { ++$i; continue }
                if ($Words[$i].StartsWith('-t', [StringComparison]::Ordinal)) { continue }
                if ($Words[$i] -cnotmatch '^[0-9a-fA-F]{2}$') { throw 'Expected hexadecimal byte operand' }
                $bytes.Add([Convert]::ToByte($Words[$i], 16))
            }
            $pane = $script:panes[$id]
            if ($pane.Current.Length + $bytes.Count -gt 65536) { throw 'Fixture pane capture limit exceeded' }
            $pane.Current = [byte[]]($pane.Current + $bytes.ToArray())
            $events.Add("%output %$id $(Encode-Bytes $bytes.ToArray())")
            if (!$script:transitionDone -and $Mode -ne 'Static') {
                $kind = if ($Mode -eq 'SwapOnInput') { 'Swap' } else { 'Restructure' }
                foreach ($event in (Invoke-Transition $kind)) { $events.Add($event) }
                $script:transitionDone = $true
            }
        }
        'split-window' {
            $windowId = Get-WindowId $target
            $window = $script:windows[$windowId]
            $id = Get-PaneId $target
            $newId = $script:nextPane++
            Add-Pane $newId
            $kind = if ($Words -ccontains '-h') { 'Columns' } else { 'Rows' }
            $first = New-Leaf $id
            $second = New-Leaf $newId
            if ($Words -ccontains '-b') { $first, $second = $second, $first }
            $window.Tree = Replace-Leaf $window.Tree $id (New-Branch $kind $first $second)
            $window.Active = $newId
            $window.Zoom = $false
            $events.Add((Get-LayoutNotification $windowId))
            $events.Add("%window-pane-changed @$windowId %$newId")
            if ($Words -ccontains '-P') { $lines.Add("%$newId") }
        }
        'new-window' {
            if ($script:windows.Count -ge 16) { throw 'Fixture window limit exceeded' }
            $id = $script:nextWindow++
            $paneId = $script:nextPane++
            Add-Pane $paneId
            $script:windows.Add($id, [pscustomobject]@{
                Name = Get-Option $Words '-n' "fixture-$id"; Tree = New-Leaf $paneId
                Active = $paneId; Width = 80; Height = 24; Zoom = $false
            })
            $script:activeWindow = $id
            $events.Add("%window-add @$id")
            $events.Add("%session-window-changed `$0 @$id")
            $events.Add("%window-pane-changed @$id %$paneId")
            if ($Words -ccontains '-P') { $lines.Add("@$id") }
        }
        'select-pane' {
            $windowId = Get-WindowId $target
            $id = Get-PaneId $target
            $script:windows[$windowId].Active = $id
            $events.Add("%window-pane-changed @$windowId %$id")
        }
        'select-window' {
            $script:activeWindow = Get-WindowId $target
            $events.Add("%session-window-changed `$0 @$script:activeWindow")
        }
        'resize-pane' {
            $windowId = Get-WindowId $target
            $window = $script:windows[$windowId]
            $id = Get-PaneId $target
            if ($Words -ccontains '-Z') {
                $window.Active = $id
                $window.Zoom = !$window.Zoom
            } elseif ($null -ne ($parent = Find-Parent $window.Tree $id)) {
                $delta = if ($Words -ccontains '-L' -or $Words -ccontains '-U') { -0.1 } else { 0.1 }
                $parent.Ratio = [Math]::Clamp($parent.Ratio + $delta, 0.1, 0.9)
            }
            $events.Add((Get-LayoutNotification $windowId))
        }
        { $_ -ceq 'kill-pane' -or $_ -ceq 'kill-window' } {
            $windowId = Get-WindowId $target
            $window = $script:windows[$windowId]
            $removed = if ($Words[0] -ceq 'kill-window') { @(Get-PaneIds $window.Tree) } else { @((Get-PaneId $target)) }
            foreach ($id in $removed) {
                [void]$script:panes.Remove($id)
                $window.Tree = Replace-Leaf $window.Tree $id $null
                if ($null -eq $window.Tree) { break }
            }
            if ($null -eq $window.Tree) {
                [void]$script:windows.Remove($windowId)
                $events.Add("%window-close @$windowId")
                if (!$script:windows.Count) { $script:detached = $true }
                elseif ($script:activeWindow -eq $windowId) {
                    $script:activeWindow = @($script:windows.Keys)[0]
                    $events.Add("%session-window-changed `$0 @$script:activeWindow")
                }
            } else {
                $window.Active = @(Get-PaneIds $window.Tree)[0]
                $window.Zoom = $false
                $events.Add((Get-LayoutNotification $windowId))
                $events.Add("%window-pane-changed @$windowId %$($window.Active)")
            }
        }
        'detach-client' { $script:detached = $true }
        'rename-session' {
            if ($Words.Count -lt 2 -or [string]::IsNullOrEmpty($Words[-1])) { throw 'Missing session name' }
            $script:sessionName = $Words[-1]
            $events.Add("%session-renamed `$0 $script:sessionName")
        }
        'fixture-swap' { foreach ($event in (Invoke-Transition 'Swap')) { $events.Add($event) } }
        'fixture-restructure' { foreach ($event in (Invoke-Transition 'Restructure')) { $events.Add($event) } }
        default { throw "Unsupported fixture command: $($Words[0])" }
    }
    return [pscustomobject]@{ Lines = $lines.ToArray(); Events = $events.ToArray() }
}

$exitCode = 0
try {
    if ($InvocationLogPath) {
        $log = [IO.StreamWriter]::new($InvocationLogPath, $false, $utf8)
        $log.AutoFlush = $true
        $log.WriteLine((@{ type = 'start'; pid = $PID; cwd = [Environment]::CurrentDirectory; mode = $Mode; cc = [bool]$ControlControl } | ConvertTo-Json -Compress))
    }
    0..2 | ForEach-Object { Add-Pane $_ }
    $script:windows.Add(0, [pscustomobject]@{
        Name = 'fixture-α'; Tree = New-Branch 'Columns' (New-Leaf 0) (New-Leaf 1)
        Active = 0; Width = 80; Height = 24; Zoom = $false
    })
    $script:windows.Add(1, [pscustomobject]@{
        Name = 'fixture-第二'; Tree = New-Leaf 2
        Active = 2; Width = 80; Height = 24; Zoom = $false
    })
    if ($ControlControl) { Write-Protocol "`eP1000p" }
    Write-Response -Lines @() -Flags 0
    Write-Protocol "%session-changed `$0 $script:sessionName`n"
    while (!$script:detached -and $null -ne ($line = Read-CommandLine)) {
        if ($log) { $log.WriteLine((@{ type = 'command'; command = $line } | ConvertTo-Json -Compress)) }
        try {
            $commands = Split-Commands $line
        } catch {
            ++$script:commandNumber
            Write-Response -Lines @($_.Exception.Message) -Success $false
            continue
        }
        foreach ($words in $commands) {
            if (++$script:commandNumber -gt 4096) { throw 'Fixture command limit exceeded' }
            try {
                $result = Invoke-FixtureCommand $words
                Write-Response -Lines $result.Lines
                foreach ($event in $result.Events) { Write-Protocol "$event`n" }
            } catch {
                Write-Response -Lines @(($_.Exception.Message -replace '[\r\n]', ' ')) -Success $false
            }
            if ($script:detached) { break }
        }
    }
} catch {
    [Console]::Error.WriteLine("TmuxBackend: $($_.Exception.Message)")
    $exitCode = 2
} finally {
    Write-Protocol "%exit fixture finished`n"
    if ($ControlControl) { Write-Protocol "`e\" }
    if ($log) { $log.Dispose() }
    $inputStream.Dispose()
    $outputStream.Dispose()
}
exit $exitCode
